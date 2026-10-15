//! `histogram(x)` (DuckDB's `histogram.cpp`): counts per distinct value, returned as a
//! MAP ordered by value.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, AggregateOps, States, Typed};
use rustdb_function::{AggregateFunction, BindInput, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Vector};

use crate::aggregate::nested::list::append_list;
use crate::aggregate::stored::{Bytes, Fixed, OrdKey, SortKey, StoredKind};

/// Counts per value, ordered by value.
pub(crate) type Counts<K> = BTreeMap<OrdKey<K>, u64>;

struct Histogram<K>(PhantomData<fn() -> K>);

impl<K: StoredKind> Aggregate for Histogram<K> {
    type State = Option<Counts<K>>;

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
                let counts = states.get(row).get_or_insert_with(BTreeMap::new);
                *counts.entry(OrdKey(K::to_owned(value))).or_insert(0) += 1;
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        if let Some(source) = source {
            let target = target.get_or_insert_with(BTreeMap::new);
            for (key, count) in source {
                *target.entry(key.clone()).or_insert(0) += count;
            }
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
        let Some(counts) = state else {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
            return Ok(());
        };
        write_map(result, row, counts.len(), |keys, values, offset| {
            for (index, (key, count)) in counts.iter().enumerate() {
                K::write(&key.0, keys, offset + index)?;
                values.values_mut::<u64>()[offset + index] = *count;
            }
            Ok(())
        })
    }
}

/// Appends a MAP of `len` entries at row `row`: `write(keys, values, offset)` fills the key
/// and value vectors from `offset`.
pub(crate) fn write_map(
    result: &mut Vector,
    row: usize,
    len: usize,
    write: impl FnOnce(&mut Vector, &mut Vector, usize) -> Result<()>,
) -> Result<()> {
    append_list(
        result,
        row,
        |entries, offset| {
            let [keys, values] = entries
                .struct_children_mut()
                .ok_or_else(|| Error::internal("MAP entries are not a struct"))?
            else {
                return Err(Error::internal("MAP entries need a key and a value"));
            };
            write(keys, values, offset)
        },
        len,
    )
}

/// The histogram operations for values of `ty` (DuckDB's `GetHistogramFunction`).
fn histogram_ops(ty: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match ty.physical_type() {
        P::Bool => Arc::new(Typed(Histogram::<Fixed<bool>>(PhantomData))),
        P::UInt8 => Arc::new(Typed(Histogram::<Fixed<u8>>(PhantomData))),
        P::UInt16 => Arc::new(Typed(Histogram::<Fixed<u16>>(PhantomData))),
        P::UInt32 => Arc::new(Typed(Histogram::<Fixed<u32>>(PhantomData))),
        P::UInt64 => Arc::new(Typed(Histogram::<Fixed<u64>>(PhantomData))),
        P::Int8 => Arc::new(Typed(Histogram::<Fixed<i8>>(PhantomData))),
        P::Int16 => Arc::new(Typed(Histogram::<Fixed<i16>>(PhantomData))),
        P::Int32 => Arc::new(Typed(Histogram::<Fixed<i32>>(PhantomData))),
        P::Int64 => Arc::new(Typed(Histogram::<Fixed<i64>>(PhantomData))),
        P::Float => Arc::new(Typed(Histogram::<Fixed<f32>>(PhantomData))),
        P::Double => Arc::new(Typed(Histogram::<Fixed<f64>>(PhantomData))),
        P::Varchar => Arc::new(Typed(Histogram::<Bytes>(PhantomData))),
        _ => Arc::new(Typed(Histogram::<SortKey<false>>(PhantomData))),
    }
}

/// DuckDB's `HistogramBindFunction`.
pub(crate) fn bind_histogram(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    if ty.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    function.ops = histogram_ops(&ty);
    function.arguments = vec![ty.clone()];
    function.return_type = LogicalType::map(ty, LogicalType::UBIGINT);
    Ok(None)
}

/// `histogram(ANY) -> MAP`.
pub fn histogram_function() -> AggregateFunction {
    AggregateFunction::unbound(
        vec![LogicalType::ANY],
        LogicalType::simple(LogicalTypeId::Map),
        bind_histogram,
    )
}
