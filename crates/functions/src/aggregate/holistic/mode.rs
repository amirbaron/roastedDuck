//! `mode` and `entropy` (DuckDB's `mode.cpp`): frequency counts per distinct value.
//!
//! `mode` returns the most frequent value, ties broken by first occurrence. `entropy` sums
//! `p * log2(1 / p)` over the distinct values; DuckDB iterates a hash map, so the summation
//! order (and thus the last bits of the result) may differ.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, AggregateOps, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData, NullHandling};
use rustdb_types::{Interval, LogicalType, PhysicalType, Vector};

use crate::aggregate::stored::{Bytes, Fixed, OrdKey, SortKey, StoredKind};

/// Occurrences of a value and the position of its first occurrence (`ModeAttr`).
#[derive(Clone, Copy, Debug)]
struct ModeAttr {
    count: u64,
    first_row: u64,
}

/// Value frequencies plus the number of values seen (`ModeState`).
struct ModeState<K: StoredKind> {
    frequencies: Option<BTreeMap<OrdKey<K>, ModeAttr>>,
    count: u64,
}

impl<K: StoredKind> ModeState<K> {
    fn add(&mut self, value: K::Ref<'_>, times: u64) {
        let count = self.count;
        let attr = self
            .frequencies
            .get_or_insert_with(BTreeMap::new)
            .entry(OrdKey(K::to_owned(value)))
            .or_insert(ModeAttr {
                count: 0,
                first_row: u64::MAX,
            });
        attr.count += times;
        attr.first_row = attr.first_row.min(count);
        self.count += times;
    }

    fn merge(&mut self, source: &Self) {
        let Some(frequencies) = &source.frequencies else {
            return;
        };
        let target = self.frequencies.get_or_insert_with(BTreeMap::new);
        for (key, attr) in frequencies {
            let entry = target.entry(key.clone()).or_insert(ModeAttr {
                count: 0,
                first_row: u64::MAX,
            });
            entry.count += attr.count;
            entry.first_row = entry.first_row.min(attr.first_row);
        }
        self.count += source.count;
    }

    /// The most frequent value, ties broken by first occurrence (`ModeState::Scan`).
    fn mode(&self) -> Option<&K::Owned> {
        let frequencies = self.frequencies.as_ref()?;
        let mut best: Option<(&OrdKey<K>, &ModeAttr)> = None;
        for (key, attr) in frequencies {
            let better = best.is_none_or(|(_, best)| {
                attr.count > best.count || (attr.count == best.count && attr.first_row < best.first_row)
            });
            if better {
                best = Some((key, attr));
            }
        }
        best.map(|(key, _)| &key.0)
    }

    /// DuckDB's `FinalizeEntropy`.
    fn entropy(&self) -> f64 {
        let Some(frequencies) = &self.frequencies else {
            return 0.0;
        };
        let count = self.count as f64;
        frequencies.values().fold(0.0, |entropy, attr| {
            let occurrences = attr.count as f64;
            (occurrences / count).mul_add((count / occurrences).log2(), entropy)
        })
    }
}

/// Counts values; `ENTROPY` finalizes to the entropy instead of the mode.
struct Frequencies<K, const ENTROPY: bool>(PhantomData<fn() -> K>);

impl<K: StoredKind, const ENTROPY: bool> Aggregate for Frequencies<K, ENTROPY> {
    type State = ModeState<K>;

    fn initialize(&self) -> Self::State {
        ModeState {
            frequencies: None,
            count: 0,
        }
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let input = &inputs[0];
        let values = K::prepare(input, count)?;
        if input.is_constant()
            && let Some(state) = states.as_single()
        {
            if let Some(value) = K::get(&values, 0) {
                state.add(value, count as u64);
            }
            return Ok(());
        }
        for row in 0..count {
            if let Some(value) = K::get(&values, row) {
                states.get(row).add(value, 1);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if ENTROPY {
            result.values_mut::<f64>()[row] = state.entropy();
            return Ok(());
        }
        if let Some(value) = state.mode() {
            K::write(value, result, row)
        } else {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
            Ok(())
        }
    }
}

fn ops<K: StoredKind, const ENTROPY: bool>() -> Arc<dyn AggregateOps> {
    Arc::new(Typed(Frequencies::<K, ENTROPY>(PhantomData)))
}

/// DuckDB's `GetModeAggregate` type dispatch.
fn mode_ops(ty: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match ty.physical_type() {
        P::Int8 => ops::<Fixed<i8>, false>(),
        P::UInt8 => ops::<Fixed<u8>, false>(),
        P::Int16 => ops::<Fixed<i16>, false>(),
        P::UInt16 => ops::<Fixed<u16>, false>(),
        P::Int32 => ops::<Fixed<i32>, false>(),
        P::UInt32 => ops::<Fixed<u32>, false>(),
        P::Int64 => ops::<Fixed<i64>, false>(),
        P::UInt64 => ops::<Fixed<u64>, false>(),
        P::Int128 => ops::<Fixed<i128>, false>(),
        P::UInt128 => ops::<Fixed<u128>, false>(),
        P::Float => ops::<Fixed<f32>, false>(),
        P::Double => ops::<Fixed<f64>, false>(),
        P::Interval => ops::<Fixed<Interval>, false>(),
        P::Varchar => ops::<Bytes, false>(),
        _ => ops::<SortKey<false>, false>(),
    }
}

/// DuckDB's `GetEntropyFunction` type dispatch.
fn entropy_ops(ty: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match ty.physical_type() {
        P::UInt16 => ops::<Fixed<u16>, true>(),
        P::UInt32 => ops::<Fixed<u32>, true>(),
        P::UInt64 => ops::<Fixed<u64>, true>(),
        P::Int16 => ops::<Fixed<i16>, true>(),
        P::Int32 => ops::<Fixed<i32>, true>(),
        P::Int64 => ops::<Fixed<i64>, true>(),
        P::Float => ops::<Fixed<f32>, true>(),
        P::Double => ops::<Fixed<f64>, true>(),
        P::Varchar => ops::<Bytes, true>(),
        _ => ops::<SortKey<false>, true>(),
    }
}

#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind_mode(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    function.ops = mode_ops(&ty);
    function.arguments = vec![ty.clone()];
    function.return_type = ty;
    Ok(None)
}

#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind_entropy(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    function.ops = entropy_ops(&ty);
    function.arguments = vec![ty];
    function.return_type = LogicalType::DOUBLE;
    function.null_handling = NullHandling::Special;
    Ok(None)
}

/// `mode(ANY) -> ANY`.
pub fn mode_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "mode",
        AggregateFunction::unbound(vec![LogicalType::ANY], LogicalType::ANY, bind_mode),
    )
    .renamed("mode")
}

/// `entropy(ANY) -> DOUBLE`.
pub fn entropy_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "entropy",
        AggregateFunction::unbound(vec![LogicalType::ANY], LogicalType::DOUBLE, bind_entropy),
    )
    .renamed("entropy")
}
