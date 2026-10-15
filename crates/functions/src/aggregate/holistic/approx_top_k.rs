//! `approx_top_k(x, k)` (DuckDB's `approx_top_k.cpp`): the Filtered Space-Saving
//! algorithm ("A parallel space saving algorithm for frequent items and the Hurwitz zeta
//! distribution" with the filter of "Estimating Top-k Destinations in Data Streams").
//!
//! Values are identified by their bytes (VARCHAR) or their sort key (other types) and
//! DuckDB's string hash, so ties resolve in the same order as in DuckDB.

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_row::hash::hash_bytes;
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::{LogicalType, LogicalTypeId, Value, Vector};

use crate::aggregate::nested::list::append_list;

/// One monitored value (DuckDB's `ApproxTopKValue`).
#[derive(Clone, Debug, Default)]
struct Monitored {
    count: u64,
    /// Position in `order`.
    index: usize,
    key: Vec<u8>,
    hash: u64,
    value: Option<Value>,
}

/// DuckDB's `InternalApproxTopKState`.
#[derive(Clone, Debug, Default)]
struct TopK {
    k: usize,
    capacity: usize,
    /// Monitored values; `order[i]` indexes them by decreasing count.
    stored: Vec<Monitored>,
    order: Vec<usize>,
    lookup: HashMap<Vec<u8>, usize>,
    filter: Vec<u64>,
    filter_mask: u64,
}

impl TopK {
    fn new(k: usize) -> Self {
        const MONITORED_VALUES_RATIO: usize = 3;
        const FILTER_RATIO: usize = 8;
        let capacity = k * MONITORED_VALUES_RATIO;
        let filter_size = (capacity * FILTER_RATIO).next_power_of_two();
        Self {
            k,
            capacity,
            stored: vec![Monitored::default(); capacity],
            order: Vec::with_capacity(capacity),
            lookup: HashMap::new(),
            filter: vec![0; filter_size],
            filter_mask: filter_size as u64 - 1,
        }
    }

    /// `IncrementCount`: keeps `order` sorted by decreasing count.
    fn increment(&mut self, slot: usize, increment: u64) {
        self.stored[slot].count += increment;
        loop {
            let index = self.stored[slot].index;
            if index == 0 || self.stored[self.order[index]].count <= self.stored[self.order[index - 1]].count
            {
                break;
            }
            let (left, right) = (self.order[index], self.order[index - 1]);
            self.stored[left].index = index - 1;
            self.stored[right].index = index;
            self.order.swap(index, index - 1);
        }
    }

    /// `InsertOrReplaceEntry`.
    fn insert_or_replace(&mut self, key: &[u8], hash: u64, value: &Value, increment: u64) {
        if self.order.len() < self.capacity {
            let slot = self.order.len();
            self.stored[slot].index = slot;
            self.order.push(slot);
        }
        let Some(&slot) = self.order.last() else {
            return;
        };
        if self.stored[slot].count > 0 {
            let filter_slot = (hash & self.filter_mask) as usize;
            if self.filter[filter_slot] + increment < self.stored[slot].count {
                self.filter[filter_slot] += increment;
                return;
            }
            let evicted = (self.stored[slot].hash & self.filter_mask) as usize;
            self.filter[evicted] = self.stored[slot].count;
            let old_key = std::mem::take(&mut self.stored[slot].key);
            self.lookup.remove(&old_key);
        }
        let entry = &mut self.stored[slot];
        entry.key = key.to_vec();
        entry.hash = hash;
        entry.value = Some(value.clone());
        self.lookup.insert(key.to_vec(), slot);
        self.increment(slot, increment);
    }

    /// One input value (`ApproxTopKOperation::Operation` after initialisation).
    fn add(&mut self, key: &[u8], value: &Value) {
        if let Some(&slot) = self.lookup.get(key) {
            self.increment(slot, 1);
        } else {
            self.insert_or_replace(key, hash_bytes(key), value, 1);
        }
    }
}

/// The `k` argument of row `row`, validated like DuckDB.
fn k_value(vector: &Vector, row: usize) -> Result<usize> {
    const MAX_APPROX_K: i64 = 1_000_000;
    let Some(k) = vector.unified::<i64>().get(row) else {
        return Err(Error::invalid_input(
            "Invalid input for approx_top_k: k value cannot be NULL",
        ));
    };
    if k <= 0 {
        return Err(Error::invalid_input(
            "Invalid input for approx_top_k: k value must be > 0",
        ));
    }
    if k >= MAX_APPROX_K {
        return Err(Error::invalid_input(format!(
            "Invalid input for approx_top_k: k value must be < {MAX_APPROX_K}"
        )));
    }
    Ok(k as usize)
}

struct ApproxTopK;

impl Aggregate for ApproxTopK {
    type State = Option<Box<TopK>>;

    fn initialize(&self) -> Self::State {
        None
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let values = &inputs[0];
        let keys = if values.logical_type().id() == LogicalTypeId::Varchar {
            None
        } else {
            let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
            Some(create_sort_keys(&[values], &modifiers, count)?)
        };
        for row in 0..count {
            if values.is_null(row) {
                continue;
            }
            let state = states.get(row);
            if state.as_ref().is_none_or(|state| state.order.is_empty()) {
                *state = Some(Box::new(TopK::new(k_value(&inputs[1], row)?)));
            }
            let key: Vec<u8> = match &keys {
                Some(keys) => keys.key(row).to_vec(),
                None => match crate::scalar::executor::str_at(values, row) {
                    Some(bytes) => bytes.to_vec(),
                    None => continue,
                },
            };
            let value = values.get_value(row);
            if let Some(state) = state {
                state.add(&key, &value);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        let Some(source) = source.as_ref().filter(|source| !source.order.is_empty()) else {
            return Ok(());
        };
        let min_source = source.stored[source.order[source.order.len() - 1]].count;
        let min_target = match target {
            Some(target) if !target.order.is_empty() => {
                if source.k != target.k {
                    return Err(Error::not_implemented(
                        "Approx Top K - cannot combine approx_top_K with different k values. K values must be the \
                         same for all entries within the same group",
                    ));
                }
                target.stored[target.order[target.order.len() - 1]].count
            }
            _ => {
                *target = Some(Box::new(TopK::new(source.k)));
                0
            }
        };
        let Some(target) = target else {
            return Ok(());
        };
        for index in 0..target.order.len() {
            let slot = target.order[index];
            let increment = source
                .lookup
                .get(&target.stored[slot].key)
                .map_or(min_source, |&source_slot| source.stored[source_slot].count);
            if increment != 0 {
                target.increment(slot, increment);
            }
        }
        for &source_slot in &source.order {
            let entry = &source.stored[source_slot];
            if target.lookup.contains_key(&entry.key) {
                continue;
            }
            let new_count = entry.count + min_target;
            let increment = if target.order.len() >= target.capacity {
                let current_min = target.order.last().map_or(0, |&slot| target.stored[slot].count);
                if new_count <= current_min {
                    continue;
                }
                new_count - current_min
            } else {
                new_count
            };
            let Some(value) = &entry.value else {
                continue;
            };
            target.insert_or_replace(&entry.key, entry.hash, value, increment);
        }
        for (target_filter, source_filter) in target.filter.iter_mut().zip(&source.filter) {
            *target_filter += source_filter;
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        let Some(state) = state.as_ref().filter(|state| !state.order.is_empty()) else {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
            return Ok(());
        };
        let len = state.order.len().min(state.k);
        append_list(
            result,
            row,
            |child, offset| {
                for (position, &slot) in state.order[..len].iter().enumerate() {
                    if let Some(value) = &state.stored[slot].value {
                        child.set_value(offset + position, value)?;
                    }
                }
                Ok(())
            },
            len,
        )
    }
}

/// DuckDB's `ApproxTopKBind`.
fn bind_approx_top_k(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input
        .argument_types
        .iter()
        .any(|ty| ty.id() == LogicalTypeId::Unknown)
    {
        return Err(crate::parameter_not_resolved());
    }
    function.arguments[0] = input.argument_types[0].clone();
    function.return_type = LogicalType::list(input.argument_types[0].clone());
    Ok(None)
}

/// `approx_top_k(value, k)`.
pub fn approx_top_k_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "approx_top_k",
        AggregateFunction::new(
            "approx_top_k",
            vec![LogicalType::ANY, LogicalType::BIGINT],
            LogicalType::list(LogicalType::ANY),
            Typed(ApproxTopK),
        )
        .with_bind(bind_approx_top_k),
    )
}
