//! `histogram(x, bins)` and `histogram_exact(x, bins)` (DuckDB's `binned_histogram.cpp`),
//! plus the `is_histogram_other_bin` scalar function.

use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, AggregateOps, States, Typed};
use rustdb_function::cast::values_not_distinct;
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData, ScalarFunction};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, PhysicalType, Value, Vector};

use crate::aggregate::nested::histogram::{histogram_function, write_map};
use crate::aggregate::stored::{Bytes, Fixed, SortKey, StoredKind};
use crate::limits::{infinity, maximum_value};

/// Sorted, distinct bin boundaries and the count of each bin (plus the "other" bin).
struct BinState<V> {
    boundaries: Vec<V>,
    counts: Vec<u64>,
}

impl<V: Clone> Clone for BinState<V> {
    fn clone(&self) -> Self {
        Self {
            boundaries: self.boundaries.clone(),
            counts: self.counts.clone(),
        }
    }
}

/// `EXACT` counts only values equal to a boundary; otherwise a value counts towards the
/// first boundary at or above it.
struct BinnedHistogram<K, const EXACT: bool>(PhantomData<fn() -> K>);

impl<K: StoredKind, const EXACT: bool> BinnedHistogram<K, EXACT> {
    /// DuckDB's `InitializeBins`: reads the bin list of row `row`.
    fn initialize(bins: &Vector, row: usize) -> Result<BinState<K::Owned>> {
        let entries = bins.unified::<ListEntry>();
        let Some(entry) = entries.get(row) else {
            return Err(Error::binder("Histogram bin list cannot be NULL"));
        };
        let (child, size) = bins
            .list_child()
            .ok_or_else(|| Error::internal("histogram bins are not a list"))?;
        let child_values = K::prepare(child, size)?;
        let mut boundaries = Vec::with_capacity(entry.length as usize);
        for index in entry.range() {
            let value = K::get(&child_values, index)
                .ok_or_else(|| Error::binder("Histogram bin entry cannot be NULL"))?;
            boundaries.push(K::to_owned(value));
        }
        boundaries.sort_by(|a, b| K::cmp(K::borrow(a), K::borrow(b)));
        boundaries.dedup_by(|a, b| K::cmp(K::borrow(a), K::borrow(b)).is_eq());
        Ok(BinState {
            boundaries,
            counts: vec![0; entry.length as usize + 1],
        })
    }

    /// The bin of `value` (DuckDB's `HistogramRange`/`HistogramExact::GetBin`).
    fn bin(state: &BinState<K::Owned>, value: K::Ref<'_>) -> usize {
        let position = state
            .boundaries
            .partition_point(|boundary| K::cmp(K::borrow(boundary), value).is_lt());
        if EXACT
            && state
                .boundaries
                .get(position)
                .is_none_or(|boundary| !K::cmp(K::borrow(boundary), value).is_eq())
        {
            return state.boundaries.len();
        }
        position
    }
}

impl<K: StoredKind, const EXACT: bool> Aggregate for BinnedHistogram<K, EXACT> {
    type State = Option<BinState<K::Owned>>;

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
        let values = K::prepare(&inputs[0], count)?;
        for row in 0..count {
            let Some(value) = K::get(&values, row) else {
                continue;
            };
            let state = states.get(row);
            let state = match state {
                Some(state) => state,
                None => state.insert(Self::initialize(&inputs[1], row)?),
            };
            let bin = Self::bin(state, value);
            state.counts[bin] += 1;
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        let Some(source) = source else {
            return Ok(());
        };
        let Some(target) = target else {
            *target = Some(source.clone());
            return Ok(());
        };
        let same = target.boundaries.len() == source.boundaries.len()
            && target
                .boundaries
                .iter()
                .zip(&source.boundaries)
                .all(|(a, b)| K::cmp(K::borrow(a), K::borrow(b)).is_eq());
        if !same {
            return Err(Error::not_implemented(
                "Histogram - cannot combine histograms with different bin boundaries. Bin boundaries must be the \
                 same for all histograms within the same group",
            ));
        }
        if target.counts.len() != source.counts.len() {
            return Err(Error::internal(
                "Histogram combine - bin boundaries are the same but counts are different",
            ));
        }
        for (target, source) in target.counts.iter_mut().zip(&source.counts) {
            *target += source;
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
        let Some(state) = state else {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
            return Ok(());
        };
        let key_type = result
            .logical_type()
            .map_key_value()
            .map(|(key, _)| key.clone())
            .ok_or_else(|| Error::internal("histogram result is not a MAP"))?;
        let other = state.counts.last().copied().unwrap_or(0);
        let other_value = if other > 0 && supports_other_bucket(&key_type) {
            Some(other_bucket_value(&key_type)?)
        } else {
            None
        };
        let len = state.boundaries.len() + usize::from(other_value.is_some());
        write_map(result, row, len, |keys, values, offset| {
            for (index, boundary) in state.boundaries.iter().enumerate() {
                K::write(boundary, keys, offset + index)?;
                values.values_mut::<u64>()[offset + index] = state.counts[index];
            }
            if let Some(other_value) = &other_value {
                let position = offset + state.boundaries.len();
                keys.set_value(position, other_value)?;
                values.values_mut::<u64>()[position] = other;
            }
            Ok(())
        })
    }
}

/// Whether the histogram of a type gets an "other" bin (DuckDB's `SupportsOtherBucket`).
fn supports_other_bucket(ty: &LogicalType) -> bool {
    use LogicalTypeId as L;
    ty.alias().is_none()
        && matches!(
            ty.id(),
            L::TinyInt
                | L::SmallInt
                | L::Integer
                | L::BigInt
                | L::HugeInt
                | L::Float
                | L::Double
                | L::Decimal
                | L::UTinyInt
                | L::USmallInt
                | L::UInteger
                | L::UBigInt
                | L::UHugeInt
                | L::Time
                | L::TimeTz
                | L::Date
                | L::Timestamp
                | L::TimestampTz
                | L::TimestampSec
                | L::TimestampMs
                | L::TimestampNs
                | L::Varchar
                | L::Blob
                | L::Struct
                | L::List
        )
}

/// The key of the "other" bin (DuckDB's `OtherBucketValue`).
fn other_bucket_value(ty: &LogicalType) -> Result<Value> {
    use LogicalTypeId as L;
    match ty.id() {
        L::Date
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampSec
        | L::TimestampMs
        | L::TimestampNs
        | L::Float
        | L::Double => infinity(ty),
        L::Varchar => Ok(Value::varchar("")),
        L::Blob => Ok(Value::blob(b"")),
        L::Struct => {
            let fields = ty
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| (name.clone(), Value::null(child.clone())))
                .collect();
            Ok(Value::struct_value(fields))
        }
        L::List => Ok(Value::list(
            ty.child_type().cloned().unwrap_or_default(),
            Vec::new(),
        )),
        _ => maximum_value(ty),
    }
}

fn binned_ops<const EXACT: bool>(ty: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match ty.physical_type() {
        P::Bool => Arc::new(Typed(BinnedHistogram::<Fixed<bool>, EXACT>(PhantomData))),
        P::UInt8 => Arc::new(Typed(BinnedHistogram::<Fixed<u8>, EXACT>(PhantomData))),
        P::UInt16 => Arc::new(Typed(BinnedHistogram::<Fixed<u16>, EXACT>(PhantomData))),
        P::UInt32 => Arc::new(Typed(BinnedHistogram::<Fixed<u32>, EXACT>(PhantomData))),
        P::UInt64 => Arc::new(Typed(BinnedHistogram::<Fixed<u64>, EXACT>(PhantomData))),
        P::Int8 => Arc::new(Typed(BinnedHistogram::<Fixed<i8>, EXACT>(PhantomData))),
        P::Int16 => Arc::new(Typed(BinnedHistogram::<Fixed<i16>, EXACT>(PhantomData))),
        P::Int32 => Arc::new(Typed(BinnedHistogram::<Fixed<i32>, EXACT>(PhantomData))),
        P::Int64 => Arc::new(Typed(BinnedHistogram::<Fixed<i64>, EXACT>(PhantomData))),
        P::Float => Arc::new(Typed(BinnedHistogram::<Fixed<f32>, EXACT>(PhantomData))),
        P::Double => Arc::new(Typed(BinnedHistogram::<Fixed<f64>, EXACT>(PhantomData))),
        P::Varchar => Arc::new(Typed(BinnedHistogram::<Bytes, EXACT>(PhantomData))),
        _ => Arc::new(Typed(BinnedHistogram::<SortKey<false>, EXACT>(PhantomData))),
    }
}

/// DuckDB's `HistogramBinBindFunction`: DECIMAL inputs are binned as DOUBLE.
fn bind_binned<const EXACT: bool>(
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
    let mut ty = input.argument_types[0].clone();
    if ty.id() == LogicalTypeId::Decimal {
        ty = LogicalType::DOUBLE;
    }
    function.ops = binned_ops::<EXACT>(&ty);
    function.arguments = vec![ty.clone(), LogicalType::list(ty.clone())];
    function.return_type = LogicalType::map(ty, LogicalType::UBIGINT);
    Ok(None)
}

fn binned_function<const EXACT: bool>() -> AggregateFunction {
    AggregateFunction::unbound(
        vec![LogicalType::ANY, LogicalType::list(LogicalType::ANY)],
        LogicalType::simple(LogicalTypeId::Map),
        bind_binned::<EXACT>,
    )
}

/// The `histogram` set: binned first, then the plain histogram.
pub fn histogram_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("histogram");
    set.add(binned_function::<false>());
    set.add(histogram_function());
    set.renamed("histogram")
}

/// `histogram_exact(ANY, ANY[]) -> MAP`.
pub fn histogram_exact_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single("histogram_exact", binned_function::<true>()).renamed("histogram_exact")
}

/// `is_histogram_other_bin(ANY) -> BOOLEAN` (DuckDB's `IsHistogramOtherBinFunction`).
pub fn is_histogram_other_bin_function() -> ScalarFunction {
    ScalarFunction::new(
        "is_histogram_other_bin",
        vec![LogicalType::ANY],
        LogicalType::BOOLEAN,
        |args, _, result| {
            let input = args.column(0);
            let ty = input.logical_type();
            if !supports_other_bucket(ty) {
                *result = Vector::constant(&Value::boolean(false))?;
                return Ok(());
            }
            let other = other_bucket_value(ty)?;
            let count = args.len();
            *result = Vector::flat(LogicalType::BOOLEAN, count.max(1));
            for row in 0..count {
                let value = input.get_value(row);
                if value.is_null() {
                    let capacity = result.capacity();
                    result.validity_mut().set_invalid(row, capacity);
                } else {
                    result.values_mut::<bool>()[row] = values_not_distinct(&value, &other);
                }
            }
            Ok(())
        },
    )
}
