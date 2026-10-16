//! `min` and `max`, including the top-N forms `min(x, n)`/`max(x, n)` (DuckDB's
//! `minmax.cpp`).

use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{
    Aggregate, AggregateInput, AggregateOps, States, Typed, Unary, UnaryAggregate,
};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::aggregate::heap::{MIN_MAX_N_MAX_VALUE, TopN, comparator};
use crate::aggregate::stored::{Bytes, Fixed, FixedOrd, SortKey, StoredKind};
use crate::dispatch::with_numeric_type;

/// `min`/`max` over a fixed-width numeric type: keeps the best value seen.
struct NumericMinMax<T, const MAX: bool>(PhantomData<fn() -> T>);

impl<T: FixedOrd, const MAX: bool> NumericMinMax<T, MAX> {
    #[inline]
    fn better(new: T, current: T) -> bool {
        if MAX {
            current.sql_less(new)
        } else {
            new.sql_less(current)
        }
    }
}

impl<T: FixedOrd, const MAX: bool> UnaryAggregate for NumericMinMax<T, MAX> {
    type Input = T;
    type State = Option<T>;
    type Output = T;

    #[inline]
    fn update(state: &mut Option<T>, value: T) -> Result<()> {
        match state {
            Some(current) if !Self::better(value, *current) => {}
            _ => *state = Some(value),
        }
        Ok(())
    }

    fn update_repeated(state: &mut Option<T>, value: T, _: usize) -> Result<()> {
        Self::update(state, value)
    }

    fn combine(source: &Option<T>, target: &mut Option<T>) -> Result<()> {
        if let Some(value) = *source {
            Self::update(target, value)?;
        }
        Ok(())
    }

    fn finalize(state: &Option<T>) -> Result<Option<T>> {
        Ok(*state)
    }
}

/// `min`/`max` over values stored by a [`StoredKind`] (strings, intervals and sort keys).
/// `GREATER` picks the greatest stored value (`max` over non-key types).
struct StoredMinMax<K, const GREATER: bool>(PhantomData<fn() -> K>);

impl<K: StoredKind, const GREATER: bool> StoredMinMax<K, GREATER> {
    fn offer(state: &mut Option<K::Owned>, value: K::Ref<'_>) {
        match state {
            None => *state = Some(K::to_owned(value)),
            Some(current) => {
                let ordering = K::cmp(value, K::borrow(current));
                let better = if GREATER {
                    ordering.is_gt()
                } else {
                    ordering.is_lt()
                };
                if better {
                    K::assign(current, value);
                }
            }
        }
    }
}

impl<K: StoredKind, const GREATER: bool> Aggregate for StoredMinMax<K, GREATER> {
    type State = Option<K::Owned>;

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
        let input = K::prepare(&inputs[0], count)?;
        for row in 0..count {
            if let Some(value) = K::get(&input, row) {
                Self::offer(states.get(row), value);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        if let Some(value) = source {
            Self::offer(target, K::borrow(value));
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
        if let Some(value) = state {
            K::write(value, result, row)
        } else {
            result.set_null(row);
            Ok(())
        }
    }
}

/// The `min` (or `max`) aggregate over `ty` (DuckDB's `GetMinMaxOperator`).
pub fn min_max_function<const MAX: bool>(ty: &LogicalType) -> AggregateFunction {
    use rustdb_types::PhysicalType as P;
    let name = if MAX { "max" } else { "min" };
    let args = vec![ty.clone()];
    let ret = ty.clone();
    let function = match ty.physical_type() {
        P::Varchar => AggregateFunction::new(name, args, ret, Typed(StoredMinMax::<Bytes, MAX>(PhantomData))),
        P::Interval => AggregateFunction::new(
            name,
            args,
            ret,
            Typed(StoredMinMax::<Fixed<rustdb_types::Interval>, MAX>(PhantomData)),
        ),
        physical => with_numeric_type!(physical, T => AggregateFunction::new(
            name,
            args,
            ret,
            Unary::<NumericMinMax<T, MAX>>::default(),
        ), _ => {
            // Nested values compare as sort keys: ascending keys for min, descending
            // keys (with the same "smallest key wins" rule) for max.
            AggregateFunction::new(name, args, ret, Typed(StoredMinMax::<SortKey<MAX>, false>(PhantomData)))
        }),
    };
    function.order_independent().distinct_independent()
}

/// DuckDB's `BindMinMax`.
fn bind_min_max<const MAX: bool>(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    if ty.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    function.replace_with(min_max_function::<MAX>(&ty));
    Ok(None)
}

/// The state of `min(x, n)`: the best `n` values, once `n` is known.
struct TopNState<V> {
    heap: Option<TopN<V>>,
}

impl<V> Default for TopNState<V> {
    fn default() -> Self {
        Self { heap: None }
    }
}

/// `min(x, n)`/`max(x, n)` (DuckDB's `MinMaxNState` + `MinMaxNOperation`).
struct MinMaxN<K, const MAX: bool>(PhantomData<fn() -> K>);

/// Validates `n` of a top-N aggregate (DuckDB's checks in `MinMaxNUpdate`).
pub(crate) fn check_top_n(n: Option<i64>, what: &str) -> Result<usize> {
    let n =
        n.ok_or_else(|| Error::invalid_input(format!("Invalid input for {what}: n value cannot be NULL")))?;
    if n <= 0 {
        return Err(Error::invalid_input(format!(
            "Invalid input for {what}: n value must be > 0"
        )));
    }
    if n >= MIN_MAX_N_MAX_VALUE {
        return Err(Error::invalid_input(format!(
            "Invalid input for {what}: n value must be < {MIN_MAX_N_MAX_VALUE}"
        )));
    }
    Ok(n as usize)
}

/// Merges one top-N heap into another (DuckDB's `MinMaxNOperation::Combine`).
pub(crate) fn combine_top_n<V: Clone>(
    source: Option<&TopN<V>>,
    target: &mut Option<TopN<V>>,
    mut insert: impl FnMut(&mut TopN<V>, &V),
) -> Result<()> {
    let Some(source) = source else {
        return Ok(());
    };
    let target = match target {
        Some(target) if target.capacity() != source.capacity() => {
            return Err(Error::invalid_input(
                "Mismatched n values in min/max/arg_min/arg_max",
            ));
        }
        Some(target) => target,
        None => target.insert(TopN::new(source.capacity())),
    };
    for entry in source.entries() {
        insert(target, entry);
    }
    Ok(())
}

impl<K: StoredKind, const MAX: bool> MinMaxN<K, MAX> {
    fn less() -> impl Fn(&K::Owned, &K::Owned) -> bool {
        comparator(MAX, |a: &K::Owned, b: &K::Owned| {
            K::cmp(K::borrow(a), K::borrow(b))
        })
    }

    fn insert(heap: &mut TopN<K::Owned>, value: K::Ref<'_>) {
        let better = |top: &K::Owned| {
            let ordering = K::cmp(value, K::borrow(top));
            if MAX { ordering.is_gt() } else { ordering.is_lt() }
        };
        heap.insert_with(
            &Self::less(),
            better,
            || K::to_owned(value),
            |slot| K::assign(slot, value),
        );
    }
}

impl<K: StoredKind, const MAX: bool> Aggregate for MinMaxN<K, MAX> {
    type State = TopNState<K::Owned>;

    fn initialize(&self) -> Self::State {
        TopNState::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let values = K::prepare(&inputs[0], count)?;
        let n = inputs[1].unified::<i64>();
        for row in 0..count {
            let Some(value) = K::get(&values, row) else {
                continue;
            };
            let state = states.get(row);
            let heap = match &mut state.heap {
                Some(heap) => heap,
                None => state.heap.insert(TopN::new(check_top_n(n.get(row), "MIN/MAX")?)),
            };
            Self::insert(heap, value);
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        combine_top_n(source.heap.as_ref(), &mut target.heap, |heap, value| {
            Self::insert(heap, K::borrow(value));
        })
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        let Some(heap) = state.heap.as_mut().filter(|heap| !heap.is_empty()) else {
            result.set_null(row);
            return Ok(());
        };
        let sorted = heap.sorted(&Self::less());
        write_list(result, row, sorted.len(), |child, index| {
            K::write(&sorted[index], child, index)
        })
    }
}

/// Appends a list of `len` elements to a LIST result at `row`; `write(child, i)` writes
/// element `i` at child row `i` of the appended range (rows are offset internally).
pub(crate) fn write_list(
    result: &mut Vector,
    row: usize,
    len: usize,
    mut write: impl FnMut(&mut Vector, usize) -> Result<()>,
) -> Result<()> {
    let child_type = result
        .logical_type()
        .child_type()
        .cloned()
        .ok_or_else(|| Error::internal("list aggregate result has no child type"))?;
    // Elements are written into a scratch vector and appended, so `write` sees rows
    // `0..len` regardless of the list's current size.
    let mut scratch = Vector::flat(child_type, len.max(1));
    for index in 0..len {
        write(&mut scratch, index)?;
    }
    append_to_list(result, row, &scratch, len)
}

/// Appends the first `len` rows of the flat vector `elements` as the list in row `row`.
pub(crate) fn append_to_list(result: &mut Vector, row: usize, elements: &Vector, len: usize) -> Result<()> {
    let offset = result.list_child().map_or(0, |(_, size)| size);
    {
        let child = result
            .list_child_mut()
            .ok_or_else(|| Error::internal("list aggregate result is not a list"))?;
        if child.capacity() < offset + len {
            child.resize((offset + len).max(child.capacity() * 2));
        }
        let positions: Vec<u32> = (offset..offset + len).map(|position| position as u32).collect();
        rustdb_types::kernels::scatter(elements, child, &positions)?;
    }
    result.set_list_size(offset + len);
    result.values_mut::<ListEntry>()[row] = ListEntry::new(offset, len);
    Ok(())
}

/// DuckDB's `MinMaxNBind`.
fn bind_min_max_n<const MAX: bool>(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    use rustdb_types::PhysicalType as P;
    if input
        .argument_types
        .iter()
        .any(|ty| ty.id() == LogicalTypeId::Unknown)
    {
        return Err(crate::parameter_not_resolved());
    }
    let ty = input.argument_types[0].clone();
    let ops: Arc<dyn AggregateOps> = match ty.physical_type() {
        P::Varchar => Arc::new(Typed(MinMaxN::<Bytes, MAX>(PhantomData))),
        P::Int32 => Arc::new(Typed(MinMaxN::<Fixed<i32>, MAX>(PhantomData))),
        P::Int64 => Arc::new(Typed(MinMaxN::<Fixed<i64>, MAX>(PhantomData))),
        P::Float => Arc::new(Typed(MinMaxN::<Fixed<f32>, MAX>(PhantomData))),
        P::Double => Arc::new(Typed(MinMaxN::<Fixed<f64>, MAX>(PhantomData))),
        _ => Arc::new(Typed(MinMaxN::<SortKey<false>, MAX>(PhantomData))),
    };
    function.ops = ops;
    function.arguments = vec![ty.clone(), LogicalType::BIGINT];
    function.return_type = LogicalType::list(ty);
    Ok(None)
}

fn min_max_set<const MAX: bool>(name: &str) -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new(name);
    let mut function =
        AggregateFunction::unbound(vec![LogicalType::ANY], LogicalType::ANY, bind_min_max::<MAX>);
    function.collation = rustdb_function::AggregateCollation::MinMax;
    set.add(function);
    set.add(AggregateFunction::unbound(
        vec![LogicalType::ANY, LogicalType::BIGINT],
        LogicalType::list(LogicalType::ANY),
        bind_min_max_n::<MAX>,
    ));
    set.renamed(name)
}

/// The `min` set.
pub fn min_functions() -> AggregateFunctionSet {
    min_max_set::<false>("min")
}

/// The `max` set.
pub fn max_functions() -> AggregateFunctionSet {
    min_max_set::<true>("max")
}
