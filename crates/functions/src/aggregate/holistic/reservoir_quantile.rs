//! `reservoir_quantile(x, q[, sample_size])` (DuckDB's `reservoir_quantile.cpp`): the
//! quantile of a reservoir sample of at most `sample_size` (default 8192) values.
//!
//! Like DuckDB, the sample holds the first `sample_size` values: `FillReservoir` only
//! replaces an entry when `next_index_to_sample` equals `num_entries_to_skip_b4_next_sample`,
//! and as the skip counter is never advanced (while `SetNextEntry` draws a skip of at least
//! one) that never happens.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::cast::ValueCast;
use rustdb_function::{AggregateFunction, AggregateFunctionSet, AggregateOps, BindInput, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType, Value, Vector};

use crate::aggregate::nested::list::append_list;

/// DuckDB's `ReservoirQuantileBindData`.
#[derive(Debug)]
struct Bounds {
    quantiles: Vec<f64>,
    sample_size: usize,
}

impl FunctionData for Bounds {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The sample of one group (`ReservoirQuantileState`): `values` is `v[0..pos]`, `len` the
/// allocated size.
#[derive(Debug, Default)]
struct Reservoir<T> {
    values: Vec<T>,
    len: usize,
}

impl<T: Copy> Reservoir<T> {
    /// `Resize`: the reservoir only grows.
    fn resize(&mut self, len: usize) {
        self.len = self.len.max(len);
    }

    /// `FillReservoir`: values beyond the sample size are not sampled (see the module docs).
    fn fill(&mut self, value: T) {
        if self.values.len() < self.len {
            self.values.push(value);
        }
    }
}

/// An ordered input type.
trait Element: NativeType + Copy + PartialOrd + Send + Sync {}
impl<T: NativeType + Copy + PartialOrd + Send + Sync> Element for T {}

struct ReservoirQuantile<T> {
    list: bool,
    _marker: PhantomData<T>,
}

/// `std::nth_element` followed by reading the element: the `offset`-th smallest value.
fn nth<T: Element>(values: &mut [T], offset: usize) -> T {
    let (_, value, _) = values.select_nth_unstable_by(offset, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    *value
}

impl<T: Element + 'static> Aggregate for ReservoirQuantile<T> {
    type State = Reservoir<T>;

    fn initialize(&self) -> Reservoir<T> {
        Reservoir {
            values: Vec::new(),
            len: 0,
        }
    }

    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, Reservoir<T>>,
        count: usize,
    ) -> Result<()> {
        let sample_size = input.bind_data::<Bounds>()?.sample_size;
        let values = inputs[0].unified::<T>();
        for row in 0..count {
            if let Some(value) = values.get(row) {
                let state = states.get(row);
                if state.values.is_empty() {
                    state.resize(sample_size);
                }
                state.fill(value);
            }
        }
        Ok(())
    }

    fn combine(
        &self,
        source: &Reservoir<T>,
        target: &mut Reservoir<T>,
        input: AggregateInput<'_>,
    ) -> Result<()> {
        let _ = input;
        if source.values.is_empty() {
            return Ok(());
        }
        if target.values.is_empty() {
            target.resize(source.len);
        }
        for &value in &source.values {
            target.fill(value);
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Reservoir<T>,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if state.values.is_empty() {
            result.set_null(row);
            return Ok(());
        }
        let quantiles = &input.bind_data::<Bounds>()?.quantiles;
        let last = (state.values.len() - 1) as f64;
        #[allow(clippy::cast_sign_loss, reason = "(idx_t)((double)(pos - 1) * quantile)")]
        let offset = |quantile: f64| (last * quantile) as usize;
        if !self.list {
            let value = nth(&mut state.values, offset(quantiles[0]));
            result.values_mut::<T>()[row] = value;
            return Ok(());
        }
        let values: Vec<T> = quantiles
            .iter()
            .map(|&quantile| nth(&mut state.values, offset(quantile)))
            .collect();
        append_list(
            result,
            row,
            |child, offset| {
                child.values_mut::<T>()[offset..offset + values.len()].copy_from_slice(&values);
                Ok(())
            },
            values.len(),
        )
    }
}

fn typed_ops(physical: PhysicalType, list: bool) -> Result<Arc<dyn AggregateOps>> {
    macro_rules! ops {
        ($t:ty) => {
            Arc::new(Typed(ReservoirQuantile::<$t> {
                list,
                _marker: PhantomData,
            })) as Arc<dyn AggregateOps>
        };
    }
    Ok(match physical {
        PhysicalType::Int8 => ops!(i8),
        PhysicalType::Int16 => ops!(i16),
        PhysicalType::Int32 => ops!(i32),
        PhysicalType::Int64 => ops!(i64),
        PhysicalType::Int128 => ops!(i128),
        PhysicalType::Float => ops!(f32),
        PhysicalType::Double => ops!(f64),
        _ => return Err(Error::internal("Unimplemented reservoir quantile aggregate")),
    })
}

/// DuckDB's `CheckReservoirQuantile`.
fn check_quantile(value: &Value) -> Result<f64> {
    if value.is_null() {
        return Err(Error::binder(
            "RESERVOIR_QUANTILE QUANTILE parameter cannot be NULL",
        ));
    }
    let quantile = value
        .default_cast_as(&LogicalType::DOUBLE)
        .ok()
        .and_then(|value| value.as_f64())
        .ok_or_else(|| Error::binder("RESERVOIR_QUANTILE can only take parameters in the range [0, 1]"))?;
    if !(0.0..=1.0).contains(&quantile) {
        return Err(Error::binder(
            "RESERVOIR_QUANTILE can only take parameters in the range [0, 1]",
        ));
    }
    Ok(quantile)
}

/// DuckDB's `BindReservoirQuantile` (and `BindReservoirQuantileDecimal`).
fn bind(function: &mut AggregateFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(quantile) = &input.constants[1] else {
        return Err(Error::binder(
            "RESERVOIR_QUANTILE can only take constant quantile parameters",
        ));
    };
    let quantiles = if quantile.logical_type().id() == LogicalTypeId::List {
        quantile
            .children()
            .unwrap_or_default()
            .iter()
            .map(check_quantile)
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![check_quantile(quantile)?]
    };
    let mut sample_size = 8192;
    if input.argument_types.len() > 2 {
        let Some(size) = &input.constants[2] else {
            return Err(Error::binder(
                "RESERVOIR_QUANTILE can only take constant sample size parameters",
            ));
        };
        if size.is_null() {
            return Err(Error::binder(
                "Size of the RESERVOIR_QUANTILE sample cannot be NULL",
            ));
        }
        let size = size.as_i128().unwrap_or(0);
        if size <= 0 {
            return Err(Error::binder(
                "Size of the RESERVOIR_QUANTILE sample must be bigger than 0",
            ));
        }
        sample_size = usize::try_from(size).unwrap_or(usize::MAX);
    }
    let ty = input.argument_types[0].clone();
    if ty.id() == LogicalTypeId::Decimal {
        // `BindReservoirQuantileDecimal` binds the scalar integer function of the physical
        // type for both forms (the list form then returns the first quantile).
        function.ops = typed_ops(ty.physical_type(), false)?;
        let integer = super::integer_type_of(ty.physical_type())?;
        function.return_type = integer.clone();
        function.arguments[0] = integer;
        function.arguments.truncate(1);
    } else {
        // `EraseArgument` of the quantile (and sample size) arguments.
        while function.arguments.len() > 1 {
            function.erase_argument(function.arguments.len() - 1);
        }
    }
    Ok(Some(Arc::new(Bounds {
        quantiles,
        sample_size,
    })))
}

/// `ReservoirQuantileBindData::Serialize`.
fn serialize_bounds(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &AggregateFunction,
) -> Result<()> {
    let bounds = bind_data
        .and_then(|data| data.as_any().downcast_ref::<Bounds>())
        .ok_or_else(|| Error::internal("reservoir_quantile without its bind data"))?;
    serializer.write_property(100, "quantiles", &bounds.quantiles)?;
    serializer.write_property(101, "sample_size", &(bounds.sample_size as u64))
}

/// The four overloads of a type: scalar and list quantiles, each with and without a
/// sample size.
fn overloads(set: &mut AggregateFunctionSet, ty: &LogicalType) -> Result<()> {
    let physical = if ty.id() == LogicalTypeId::Decimal {
        PhysicalType::Int64
    } else {
        ty.physical_type()
    };
    for (list, quantile_type, return_type) in [
        (false, LogicalType::DOUBLE, ty.clone()),
        (
            true,
            LogicalType::list(LogicalType::DOUBLE),
            LogicalType::list(ty.clone()),
        ),
    ] {
        let mut function = AggregateFunction::new(
            "reservoir_quantile",
            vec![ty.clone(), quantile_type],
            return_type,
            Typed(ReservoirQuantile::<f64> {
                list,
                _marker: PhantomData,
            }),
        )
        .with_bind(bind);
        function.ops = typed_ops(physical, list)?;
        function.serialize = Some(Arc::new(serialize_bounds));
        let mut sized = function.clone();
        sized.arguments.push(LogicalType::INTEGER);
        set.add(function);
        set.add(sized);
    }
    Ok(())
}

/// `reservoir_quantile` over every supported type, in DuckDB's order.
pub fn reservoir_quantile_functions() -> Result<AggregateFunctionSet> {
    let mut set = AggregateFunctionSet::new("reservoir_quantile");
    for ty in [
        LogicalType::simple(LogicalTypeId::Decimal),
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
    ] {
        overloads(&mut set, &ty)?;
    }
    Ok(set)
}
