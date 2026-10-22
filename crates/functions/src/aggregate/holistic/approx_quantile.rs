//! `approx_quantile(x, q)` (DuckDB's `approximate_quantile.cpp`): quantiles from a
//! t-digest with compression 100. Values are digested as DOUBLE (DECIMALs as their
//! unscaled integers, TIME WITH TIME ZONE as its sort key) and results are cast back,
//! clamping to the type's range.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::cast::ValueCast;
use rustdb_function::cast::numeric::TryCastTo;
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType, TimeTz, Value, Vector};

use super::tdigest::TDigest;
use crate::aggregate::nested::list::append_list;

/// The requested quantiles (DuckDB's `ApproximateQuantileBindData`; stored as FLOAT).
#[derive(Debug)]
struct Quantiles(Vec<f32>);

impl FunctionData for Quantiles {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A value digested as DOUBLE (DuckDB's `ApproxQuantileCoding`).
trait Coding: NativeType + Copy + Send + Sync {
    fn encode(self) -> f64;
    /// The value for a digest result, clamped to the type's range.
    fn decode(value: f64) -> Self;
}

macro_rules! numeric_coding {
    ($($ty:ty),*) => {$(
        impl Coding for $ty {
            #[allow(clippy::cast_precision_loss, reason = "Cast::Operation<T, double>")]
            fn encode(self) -> f64 {
                self as f64
            }

            fn decode(value: f64) -> Self {
                match TryCastTo::<$ty>::try_cast_to(value) {
                    Some(result) => result,
                    None if value < 0.0 => <$ty>::MIN,
                    None => <$ty>::MAX,
                }
            }
        }
    )*};
}
numeric_coding!(i8, i16, i32, i64, i128, f32, f64);

impl Coding for TimeTz {
    #[allow(clippy::cast_precision_loss, reason = "Cast::Operation<uint64_t, double>")]
    fn encode(self) -> f64 {
        self.sort_key() as f64
    }

    fn decode(value: f64) -> Self {
        const OFFSET_BITS: u32 = 24;
        const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1;
        match TryCastTo::<u64>::try_cast_to(value) {
            Some(sort_key) => {
                let offset = sort_key & OFFSET_MASK;
                let micros = (sort_key >> OFFSET_BITS).wrapping_sub(offset * 1_000_000);
                TimeTz((micros << OFFSET_BITS) | offset)
            }
            None if value < 0.0 => TimeTz::new(rustdb_types::Time(0), TimeTz::MAX_OFFSET),
            None => TimeTz::new(rustdb_types::Time(86_400_000_000), TimeTz::MIN_OFFSET),
        }
    }
}

/// The digest of one group (`ApproxQuantileState`).
#[derive(Debug, Default)]
struct State {
    digest: Option<Box<TDigest>>,
    count: u64,
}

struct ApproxQuantile<T> {
    list: bool,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Coding + 'static> Aggregate for ApproxQuantile<T> {
    type State = State;

    fn initialize(&self) -> State {
        State::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, State>,
        count: usize,
    ) -> Result<()> {
        let values = inputs[0].unified::<T>();
        for row in 0..count {
            let Some(value) = values.get(row) else {
                continue;
            };
            let encoded = value.encode();
            if !encoded.is_finite() {
                continue;
            }
            let state = states.get(row);
            state
                .digest
                .get_or_insert_with(|| Box::new(TDigest::new(100.0)))
                .add(encoded);
            state.count += 1;
        }
        Ok(())
    }

    fn combine(&self, source: &State, target: &mut State, _: AggregateInput<'_>) -> Result<()> {
        if source.count == 0 {
            return Ok(());
        }
        if let Some(digest) = &source.digest {
            target
                .digest
                .get_or_insert_with(|| Box::new(TDigest::new(100.0)))
                .merge(digest);
        }
        target.count += source.count;
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut State,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        let (Some(digest), true) = (state.digest.as_mut(), state.count > 0) else {
            result.set_null(row);
            return Ok(());
        };
        let quantiles = &input.bind_data::<Quantiles>()?.0;
        digest.process();
        if !self.list {
            let value = digest.quantile(f64::from(quantiles[0]));
            result.values_mut::<T>()[row] = T::decode(value);
            return Ok(());
        }
        let values: Vec<T> = quantiles
            .iter()
            .map(|&quantile| T::decode(digest.quantile(f64::from(quantile))))
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

/// DuckDB's `CheckApproxQuantile`.
fn check_quantile(value: &Value) -> Result<f32> {
    if value.is_null() {
        return Err(Error::binder("APPROXIMATE QUANTILE parameter cannot be NULL"));
    }
    #[allow(clippy::cast_possible_truncation, reason = "Value::GetValue<float>")]
    let quantile = value
        .default_cast_as(&LogicalType::DOUBLE)
        .ok()
        .and_then(|value| value.as_f64())
        .ok_or_else(|| Error::binder("APPROXIMATE QUANTILE can only take parameters in range [0, 1]"))?
        as f32;
    if !(0.0..=1.0).contains(&quantile) {
        return Err(Error::binder(
            "APPROXIMATE QUANTILE can only take parameters in range [0, 1]",
        ));
    }
    Ok(quantile)
}

/// The ops of the scalar (`list == false`) or list form for a value type.
fn typed_ops(
    physical: PhysicalType,
    ty: &LogicalType,
    list: bool,
) -> Result<Arc<dyn rustdb_function::AggregateOps>> {
    macro_rules! ops {
        ($t:ty) => {
            Arc::new(Typed(ApproxQuantile::<$t> {
                list,
                _marker: std::marker::PhantomData,
            })) as Arc<dyn rustdb_function::AggregateOps>
        };
    }
    if ty.id() == LogicalTypeId::TimeTz {
        return Ok(ops!(TimeTz));
    }
    Ok(match physical {
        PhysicalType::Int8 => ops!(i8),
        PhysicalType::Int16 => ops!(i16),
        PhysicalType::Int32 => ops!(i32),
        PhysicalType::Int64 => ops!(i64),
        PhysicalType::Int128 => ops!(i128),
        PhysicalType::Float => ops!(f32),
        PhysicalType::Double => ops!(f64),
        _ => return Err(Error::internal("Unimplemented quantile aggregate")),
    })
}

/// DuckDB's `BindApproxQuantile` (and the DECIMAL variants, which run on the unscaled
/// values and keep the DECIMAL type).
fn bind(function: &mut AggregateFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(quantile) = &input.constants[1] else {
        return Err(Error::binder(
            "APPROXIMATE QUANTILE can only take constant quantile parameters",
        ));
    };
    if quantile.is_null() {
        return Err(Error::binder(
            "APPROXIMATE QUANTILE parameter list cannot be NULL",
        ));
    }
    let quantiles = match quantile.logical_type().id() {
        LogicalTypeId::List | LogicalTypeId::Array => quantile
            .children()
            .unwrap_or_default()
            .iter()
            .map(check_quantile)
            .collect::<Result<Vec<_>>>()?,
        _ => vec![check_quantile(quantile)?],
    };
    let list = function.return_type.id() == LogicalTypeId::List;
    let ty = input.argument_types[0].clone();
    // `EraseArgument`; a DECIMAL call then gets a new function (without the original
    // arguments).
    function.erase_argument(1);
    if ty.id() == LogicalTypeId::Decimal {
        function.original_arguments.clear();
        function.ops = typed_ops(ty.physical_type(), &ty, list)?;
        if list {
            function.return_type = LogicalType::list(ty.clone());
            function.arguments[0] = ty;
        } else {
            // `GetApproximateQuantileDecimalAggregateFunction`: the integer function of
            // the physical type, so the input is cast to that integer type.
            let integer = super::integer_type_of(ty.physical_type())?;
            function.return_type = integer.clone();
            function.arguments[0] = integer;
        }
    }
    Ok(Some(Arc::new(Quantiles(quantiles))))
}

/// `ApproximateQuantileBindData::Serialize`.
fn serialize_quantiles(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &AggregateFunction,
) -> Result<()> {
    let quantiles = bind_data
        .and_then(|data| data.as_any().downcast_ref::<Quantiles>())
        .ok_or_else(|| Error::internal("approx_quantile without its bind data"))?;
    serializer.write_property(100, "quantiles", &quantiles.0)
}

fn overload(ty: &LogicalType, list: bool) -> Result<AggregateFunction> {
    let (quantile_type, return_type) = if list {
        (
            LogicalType::list(LogicalType::FLOAT),
            LogicalType::list(ty.clone()),
        )
    } else {
        (LogicalType::FLOAT, ty.clone())
    };
    // DECIMAL overloads get the ops of their physical type at bind time.
    let ops = if ty.id() == LogicalTypeId::Decimal {
        typed_ops(PhysicalType::Int64, &LogicalType::BIGINT, list)?
    } else {
        typed_ops(ty.physical_type(), ty, list)?
    };
    let mut function = AggregateFunction::new(
        "approx_quantile",
        vec![ty.clone(), quantile_type],
        return_type,
        Typed(ApproxQuantile::<f64> {
            list,
            _marker: std::marker::PhantomData,
        }),
    )
    .with_bind(bind);
    function.ops = ops;
    function.serialize = Some(Arc::new(serialize_quantiles));
    Ok(function)
}

/// `approx_quantile` over every supported type, in DuckDB's order.
pub fn approx_quantile_functions() -> Result<AggregateFunctionSet> {
    let mut set = AggregateFunctionSet::new("approx_quantile");
    let decimal = LogicalType::simple(LogicalTypeId::Decimal);
    set.add(overload(&decimal, false)?);
    for ty in [
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::DOUBLE,
        LogicalType::DATE,
        LogicalType::TIME,
        LogicalType::TIME_TZ,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_TZ,
    ] {
        set.add(overload(&ty, false)?);
    }
    set.add(overload(&decimal, true)?);
    for ty in [
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
        LogicalType::DATE,
        LogicalType::TIME,
        LogicalType::TIME_TZ,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_TZ,
    ] {
        set.add(overload(&ty, true)?);
    }
    Ok(set)
}
