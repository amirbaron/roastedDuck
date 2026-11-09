//! Exact quantiles: `quantile_disc`/`quantile`, `quantile_cont`, `median` and `mad`
//! (DuckDB's `quantile.cpp`, `mad.cpp`, `quantile_helpers.hpp` and the interpolators of
//! `quantile_sort_tree.hpp`).
//!
//! States collect every non-NULL value; finalize selects the order statistics with
//! `select_nth_unstable` (DuckDB's `std::nth_element`) and interpolates between the floor
//! and ceiling ranks for continuous quantiles.

use std::cmp::Ordering;
use std::marker::PhantomData;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::{Aggregate, AggregateInput, AggregateOps, States, Typed};
use roastedduck_function::cast::numeric::i128_to_f64;
use roastedduck_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use roastedduck_types::{
    Date, Interval, LogicalType, LogicalTypeId, NativeType, PhysicalType, Timestamp, Value, Vector,
};

use crate::aggregate::nested::list::append_list;
use crate::aggregate::stored::{Bytes, Fixed, FixedOrd, SortKey, StoredKind};
use crate::signature::type_id;
use crate::values::as_double;

/// One requested quantile (DuckDB's `QuantileValue`): its absolute value as DOUBLE, and the
/// unscaled value and scale factor when it was given as a DECIMAL.
#[derive(Clone, Debug)]
pub(crate) struct QuantileValue {
    /// The quantile as given, made positive (DuckDB's `val`): a DECIMAL stays one.
    val: Value,
    pub(crate) dbl: f64,
    decimal: Option<(i128, i128)>,
}

impl PartialEq for QuantileValue {
    fn eq(&self, other: &Self) -> bool {
        // QuantileValue::operator== compares the original Values numerically,
        // including equivalent DECIMAL scales and DOUBLE probability literals.
        roastedduck_function::cast::values_equal(&self.val, &other.val).unwrap_or(false)
    }
}

impl QuantileValue {
    /// DuckDB's `QuantileAbs` followed by the `QuantileValue` constructor.
    fn from_value(value: &Value) -> Result<Self> {
        if let Some((width, scale)) = value.logical_type().decimal_properties() {
            let integral = value
                .as_i128()
                .ok_or_else(|| Error::internal("DECIMAL quantile without an integral value"))?
                .abs();
            let scaling = 10i128.pow(u32::from(scale));
            return Ok(Self {
                val: Value::decimal(integral, width, scale),
                dbl: i128_to_f64(integral) / i128_to_f64(scaling),
                decimal: Some((integral, scaling)),
            });
        }
        let dbl = as_double(value)?.abs();
        Ok(Self {
            val: Value::double(dbl),
            dbl,
            decimal: None,
        })
    }
}

/// The quantiles of a call (DuckDB's `QuantileBindData`): `order` lists the quantile
/// indexes by increasing quantile; `desc` is set for negative quantiles.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuantileBindData {
    pub(crate) quantiles: Vec<QuantileValue>,
    pub(crate) order: Vec<usize>,
    pub(crate) desc: bool,
}

impl FunctionData for QuantileBindData {
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl QuantileBindData {
    /// The bind data of `median`/`mad`: the DECIMAL quantile `0.5`.
    pub(crate) fn median() -> Self {
        Self {
            quantiles: vec![QuantileValue {
                val: Value::decimal(5, 2, 1),
                dbl: 0.5,
                decimal: Some((5, 10)),
            }],
            order: vec![0],
            desc: false,
        }
    }

    fn new(values: &[Value]) -> Result<Self> {
        let mut positive = 0;
        let mut negative = 0;
        let mut quantiles = Vec::with_capacity(values.len());
        for value in values {
            let raw = as_double(value)?;
            positive += usize::from(raw > 0.0);
            negative += usize::from(raw < 0.0);
            quantiles.push(QuantileValue::from_value(value)?);
        }
        if positive > 0 && negative > 0 {
            return Err(Error::binder("QUANTILE parameters must have consistent signs"));
        }
        let mut order: Vec<usize> = (0..quantiles.len()).collect();
        order.sort_by(|&a, &b| quantiles[a].dbl.total_cmp(&quantiles[b].dbl));
        Ok(Self {
            quantiles,
            order,
            desc: negative > 0,
        })
    }
}

/// DuckDB's `CheckQuantile`.
fn check_quantile(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Err(Error::binder("QUANTILE parameter cannot be NULL"));
    }
    let quantile = as_double(value)?;
    if !(-1.0..=1.0).contains(&quantile) && !quantile.is_nan() {
        return Err(Error::binder(
            "QUANTILE can only take parameters in the range [-1, 1]",
        ));
    }
    if quantile.is_nan() {
        return Err(Error::binder("QUANTILE parameter cannot be NaN"));
    }
    Ok(value.clone())
}

/// DuckDB's `BindQuantile`: evaluates the constant quantile argument(s) and removes it.
pub(crate) fn bind_quantile(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Arc<dyn FunctionData>> {
    if input.argument_types.len() < 2 {
        return Err(Error::binder("QUANTILE requires a range argument between [0, 1]"));
    }
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(value) = &input.constants[1] else {
        return Err(Error::binder("QUANTILE can only take constant parameters"));
    };
    if value.is_null() {
        return Err(Error::binder("QUANTILE argument must not be NULL"));
    }
    let values = match value.logical_type().id() {
        LogicalTypeId::List | LogicalTypeId::Array => value
            .children()
            .unwrap_or_default()
            .iter()
            .map(check_quantile)
            .collect::<Result<Vec<_>>>()?,
        _ => vec![check_quantile(value)?],
    };
    function.erase_argument(input.argument_types.len() - 1);
    Ok(Arc::new(QuantileBindData::new(&values)?))
}

/// `QuantileBindData::Serialize`: the quantiles as given, made positive, their order and
/// whether they were negative.
fn serialize_quantile(
    serializer: &mut dyn roastedduck_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &AggregateFunction,
) -> Result<()> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<QuantileBindData>())
        .ok_or_else(|| Error::internal("quantile without its bind data"))?;
    let quantiles: Vec<&Value> = data.quantiles.iter().map(|quantile| &quantile.val).collect();
    let order: Vec<u64> = data.order.iter().map(|&index| index as u64).collect();
    serializer.write_property(100, "quantiles", &quantiles)?;
    serializer.write_property(101, "order", &order)?;
    serializer.write_property(102, "desc", &data.desc)
}

/// The floor/ceiling ranks of a quantile (DuckDB's `QuantileInterpolator`).
#[derive(Clone, Copy, Debug)]
struct Interpolator {
    rn: f64,
    frn: usize,
    crn: usize,
}

impl Interpolator {
    /// The continuous ranks for `n` values.
    fn continuous(q: &QuantileValue, n: usize) -> Self {
        let rn = (n - 1) as f64 * q.dbl;
        Self {
            rn,
            frn: rn.floor() as usize,
            crn: rn.ceil() as usize,
        }
    }

    /// The discrete rank for `n` values (DuckDB's `QuantileInterpolator<true>::Index`).
    fn discrete(q: &QuantileValue, n: usize) -> Result<Self> {
        let floored = if let Some((integral, scaling)) = q.decimal {
            let overflow = || Error::out_of_range("Overflow in multiplication of DECIMAL(38)");
            let scaled_q = (n as i128).checked_mul(integral).ok_or_else(overflow)?;
            let scaled_n = (n as i128).checked_mul(scaling).ok_or_else(overflow)?;
            ((scaled_n - scaled_q) / scaling) as usize
        } else {
            let scaled_q = n as f64 * q.dbl;
            (n as f64 - scaled_q).floor() as usize
        };
        let index = n.saturating_sub(floored).max(1) - 1;
        Ok(Self {
            rn: index as f64,
            frn: index,
            crn: index,
        })
    }

    /// Places the order statistics `frn` and `crn` of `values[begin..]` (in `cmp` order) at
    /// their positions (`std::nth_element`).
    fn select<T>(&self, values: &mut [T], begin: usize, cmp: &impl Fn(&T, &T) -> Ordering) {
        values[begin..].select_nth_unstable_by(self.frn - begin, cmp);
        if self.crn != self.frn {
            values[self.frn..].select_nth_unstable_by(self.crn - self.frn, cmp);
        }
    }
}

/// How a quantile aggregate turns the collected values into results.
trait QuantileKind: Send + Sync + 'static {
    /// The stored values.
    type Stored: StoredKind;

    /// Writes the quantile `q` of `values` into row `row` of `result`; `begin` is the first
    /// index that may still need to move (for lists of increasing quantiles).
    fn write(
        values: &mut [<Self::Stored as StoredKind>::Owned],
        q: &QuantileValue,
        desc: bool,
        begin: usize,
        result: &mut Vector,
        row: usize,
    ) -> Result<usize>;
}

/// The comparator of a quantile selection (DuckDB's `QuantileCompare`).
fn compare<K: StoredKind>(desc: bool) -> impl Fn(&K::Owned, &K::Owned) -> Ordering {
    move |a, b| {
        let ordering = K::cmp(K::borrow(a), K::borrow(b));
        if desc { ordering.reverse() } else { ordering }
    }
}

/// `quantile_disc`: the value at the discrete rank.
struct Discrete<K>(PhantomData<fn() -> K>);

impl<K: StoredKind> QuantileKind for Discrete<K> {
    type Stored = K;

    fn write(
        values: &mut [K::Owned],
        q: &QuantileValue,
        desc: bool,
        begin: usize,
        result: &mut Vector,
        row: usize,
    ) -> Result<usize> {
        let interpolator = Interpolator::discrete(q, values.len())?;
        interpolator.select(values, begin, &compare::<K>(desc));
        K::write(&values[interpolator.frn], result, row)?;
        Ok(interpolator.frn)
    }
}

/// A value type of continuous quantiles: how inputs convert to the result type and how
/// two results are interpolated (DuckDB's `QuantileCast` and `InterpolateOperator`).
trait Continuous: Send + Sync + 'static {
    type Input: FixedOrd;
    type Output: NativeType;

    fn cast(input: Self::Input) -> Result<Self::Output>;

    fn interpolate(lo: Self::Output, d: f64, hi: Self::Output) -> Self::Output;
}

/// `quantile_cont`: interpolates between the floor and ceiling ranks.
struct ContinuousQuantile<C>(PhantomData<fn() -> C>);

impl<C: Continuous> QuantileKind for ContinuousQuantile<C> {
    type Stored = Fixed<C::Input>;

    fn write(
        values: &mut [C::Input],
        q: &QuantileValue,
        desc: bool,
        begin: usize,
        result: &mut Vector,
        row: usize,
    ) -> Result<usize> {
        let interpolator = Interpolator::continuous(q, values.len());
        interpolator.select(values, begin, &compare::<Fixed<C::Input>>(desc));
        let lo = C::cast(values[interpolator.frn])?;
        let value = if interpolator.crn == interpolator.frn {
            lo
        } else {
            let hi = C::cast(values[interpolator.crn])?;
            C::interpolate(lo, interpolator.rn - interpolator.frn as f64, hi)
        };
        result.values_mut::<C::Output>()[row] = value;
        Ok(interpolator.frn)
    }
}

/// DOUBLE interpolation `lo * (1 - d) + hi * d` (with the reference build's fused
/// multiply-add).
#[inline]
fn lerp(lo: f64, d: f64, hi: f64) -> f64 {
    lo.mul_add(1.0 - d, hi * d)
}

/// Integers interpolated as DOUBLE.
struct IntegerToDouble<I>(PhantomData<fn() -> I>);

macro_rules! integer_to_double {
    ($($ty:ty => $convert:expr),*) => {$(
        impl Continuous for IntegerToDouble<$ty> {
            type Input = $ty;
            type Output = f64;

            fn cast(input: $ty) -> Result<f64> {
                let convert: fn($ty) -> f64 = $convert;
                Ok(convert(input))
            }

            fn interpolate(lo: f64, d: f64, hi: f64) -> f64 {
                lerp(lo, d, hi)
            }
        }
    )*};
}
integer_to_double!(i8 => f64::from, i16 => f64::from, i32 => f64::from, i64 => |v| v as f64, i128 => i128_to_f64, f64 => |v| v);

/// FLOAT quantiles stay FLOAT (DuckDB's generic `InterpolateOperator`).
struct FloatQuantile;

impl Continuous for FloatQuantile {
    type Input = f32;
    type Output = f32;

    fn cast(input: f32) -> Result<f32> {
        Ok(input)
    }

    fn interpolate(lo: f32, d: f64, hi: f32) -> f32 {
        let delta = f64::from(hi - lo);
        lo + (delta * d) as f32
    }
}

/// DECIMAL quantiles interpolate the unscaled integers, truncating.
struct DecimalQuantile<I>(PhantomData<fn() -> I>);

macro_rules! decimal_quantile {
    ($($ty:ty),*) => {$(
        impl Continuous for DecimalQuantile<$ty> {
            type Input = $ty;
            type Output = $ty;

            fn cast(input: $ty) -> Result<$ty> {
                Ok(input)
            }

            fn interpolate(lo: $ty, d: f64, hi: $ty) -> $ty {
                let delta = hi.wrapping_sub(lo) as f64;
                lo.wrapping_add((delta * d) as $ty)
            }
        }
    )*};
}
decimal_quantile!(i16, i32, i64);

impl Continuous for DecimalQuantile<i128> {
    type Input = i128;
    type Output = i128;

    fn cast(input: i128) -> Result<i128> {
        Ok(input)
    }

    fn interpolate(lo: i128, d: f64, hi: i128) -> i128 {
        let interpolated = lerp(i128_to_f64(lo), d, i128_to_f64(hi));
        roastedduck_function::cast::numeric::f64_to_i128(interpolated).unwrap_or(0)
    }
}

/// `llround` of a DOUBLE interpolation of two 64-bit values.
#[inline]
fn lerp_i64(lo: i64, d: f64, hi: i64) -> i64 {
    lerp(lo as f64, d, hi as f64).round() as i64
}

/// TIMESTAMP (any precision), TIME and TIME WITH TIME ZONE quantiles: the raw 64-bit
/// values interpolated and rounded.
struct Temporal64;

impl Continuous for Temporal64 {
    type Input = i64;
    type Output = i64;

    fn cast(input: i64) -> Result<i64> {
        Ok(input)
    }

    fn interpolate(lo: i64, d: f64, hi: i64) -> i64 {
        lerp_i64(lo, d, hi)
    }
}

/// DATE quantiles are computed as TIMESTAMP.
struct DateQuantile;

/// DuckDB's `Cast::Operation<date_t, timestamp_t>`.
pub(crate) fn date_to_timestamp(days: i32) -> Result<i64> {
    let date = Date(days);
    if date == Date::INFINITY {
        return Ok(Timestamp::INFINITY.0);
    }
    if date == Date::NEG_INFINITY {
        return Ok(Timestamp::NEG_INFINITY.0);
    }
    Timestamp::from_date(date).map(|ts| ts.0).ok_or_else(|| {
        Error::conversion(format!(
            "Date out of range in timestamp conversion: {}",
            value_text(date)
        ))
    })
}

fn value_text(date: Date) -> String {
    Value::date(date).to_varchar().unwrap_or_default()
}

impl Continuous for DateQuantile {
    type Input = i32;
    type Output = i64;

    fn cast(input: i32) -> Result<i64> {
        date_to_timestamp(input)
    }

    fn interpolate(lo: i64, d: f64, hi: i64) -> i64 {
        lerp_i64(lo, d, hi)
    }
}

/// The collected values of a quantile aggregate (DuckDB's `QuantileState::v`).
struct Quantile<Q, const LIST: bool>(PhantomData<fn() -> Q>);

impl<Q: QuantileKind, const LIST: bool> Aggregate for Quantile<Q, LIST> {
    type State = Vec<<Q::Stored as StoredKind>::Owned>;

    fn initialize(&self) -> Self::State {
        Vec::new()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let values = Q::Stored::prepare(&inputs[0], count)?;
        for row in 0..count {
            if let Some(value) = Q::Stored::get(&values, row) {
                states.get(row).push(Q::Stored::to_owned(value));
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        target.extend_from_slice(source);
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if state.is_empty() {
            result.set_null(row);
            return Ok(());
        }
        let bind = input.bind_data::<QuantileBindData>()?;
        if !LIST {
            Q::write(state, &bind.quantiles[0], bind.desc, 0, result, row)?;
            return Ok(());
        }
        append_list(
            result,
            row,
            |child, offset| {
                let mut lower = 0;
                for &q in &bind.order {
                    lower = Q::write(state, &bind.quantiles[q], bind.desc, lower, child, offset + q)?;
                }
                Ok(())
            },
            bind.quantiles.len(),
        )
    }
}

fn quantile_ops<Q: QuantileKind>(list: bool) -> Arc<dyn AggregateOps> {
    if list {
        Arc::new(Typed(Quantile::<Q, true>(PhantomData)))
    } else {
        Arc::new(Typed(Quantile::<Q, false>(PhantomData)))
    }
}

/// DuckDB's `GetDiscreteQuantileTemplated` type dispatch.
fn discrete_ops(ty: &LogicalType, list: bool) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match ty.physical_type() {
        P::Int8 => quantile_ops::<Discrete<Fixed<i8>>>(list),
        P::Int16 => quantile_ops::<Discrete<Fixed<i16>>>(list),
        P::Int32 => quantile_ops::<Discrete<Fixed<i32>>>(list),
        P::Int64 => quantile_ops::<Discrete<Fixed<i64>>>(list),
        P::Int128 => quantile_ops::<Discrete<Fixed<i128>>>(list),
        P::Float => quantile_ops::<Discrete<Fixed<f32>>>(list),
        P::Double => quantile_ops::<Discrete<Fixed<f64>>>(list),
        P::Interval => quantile_ops::<Discrete<Fixed<Interval>>>(list),
        P::Varchar => quantile_ops::<Discrete<Bytes>>(list),
        _ => quantile_ops::<Discrete<SortKey<false>>>(list),
    }
}

/// A discrete quantile over `ty`: `(input type, result type)` plus operations.
fn discrete_function(ty: &LogicalType, list: bool) -> AggregateFunction {
    let result = if list {
        LogicalType::list(ty.clone())
    } else {
        ty.clone()
    };
    let mut function = AggregateFunction::new(
        "",
        vec![ty.clone()],
        result,
        roastedduck_function::aggregate::Unbound,
    )
    .order_independent();
    function.ops = discrete_ops(ty, list);
    function
}

/// DuckDB's `GetContinuousQuantileTemplated`: the input and result types and operations.
fn continuous_function(ty: &LogicalType, list: bool) -> Result<AggregateFunction> {
    use LogicalTypeId as L;
    let (input, output, ops) = match ty.id() {
        L::TinyInt => (
            ty.clone(),
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<i8>>>(list),
        ),
        L::SmallInt => (
            ty.clone(),
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<i16>>>(list),
        ),
        L::SqlNull | L::Integer => (
            ty.clone(),
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<i32>>>(list),
        ),
        L::BigInt => (
            ty.clone(),
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<i64>>>(list),
        ),
        L::HugeInt => (
            ty.clone(),
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<i128>>>(list),
        ),
        L::Float => (
            ty.clone(),
            ty.clone(),
            quantile_ops::<ContinuousQuantile<FloatQuantile>>(list),
        ),
        L::UTinyInt | L::USmallInt | L::UInteger | L::UBigInt | L::UHugeInt | L::Double => (
            LogicalType::DOUBLE,
            LogicalType::DOUBLE,
            quantile_ops::<ContinuousQuantile<IntegerToDouble<f64>>>(list),
        ),
        L::Decimal => {
            let ops = match ty.physical_type() {
                PhysicalType::Int16 => quantile_ops::<ContinuousQuantile<DecimalQuantile<i16>>>(list),
                PhysicalType::Int32 => quantile_ops::<ContinuousQuantile<DecimalQuantile<i32>>>(list),
                PhysicalType::Int64 => quantile_ops::<ContinuousQuantile<DecimalQuantile<i64>>>(list),
                PhysicalType::Int128 => quantile_ops::<ContinuousQuantile<DecimalQuantile<i128>>>(list),
                _ => {
                    return Err(Error::not_implemented(
                        "Unimplemented continuous quantile DECIMAL aggregate",
                    ));
                }
            };
            (ty.clone(), ty.clone(), ops)
        }
        L::Date => (
            ty.clone(),
            LogicalType::TIMESTAMP,
            quantile_ops::<ContinuousQuantile<DateQuantile>>(list),
        ),
        L::Timestamp
        | L::TimestampTz
        | L::TimestampSec
        | L::TimestampMs
        | L::TimestampNs
        | L::Time
        | L::TimeTz => (
            ty.clone(),
            ty.clone(),
            quantile_ops::<ContinuousQuantile<Temporal64>>(list),
        ),
        _ => {
            return Err(Error::not_implemented(
                "Unimplemented continuous quantile aggregate",
            ));
        }
    };
    let result = if list { LogicalType::list(output) } else { output };
    let mut function =
        AggregateFunction::new("", vec![input], result, roastedduck_function::aggregate::Unbound)
            .order_independent();
    function.ops = ops;
    Ok(function)
}

/// Replaces `function` by `bound`, appending the quantile argument type.
fn install(
    function: &mut AggregateFunction,
    name: &str,
    mut bound: AggregateFunction,
    quantile_type: Option<LogicalType>,
) {
    bound.arguments.extend(quantile_type);
    bound.bind.clone_from(&function.bind);
    bound.serialize = Some(Arc::new(serialize_quantile));
    function.replace_with(bound);
    // The bound function is named after its implementation, also when called through
    // an alias (`fun.name = "quantile_disc"`).
    function.name = name.into();
}

fn quantile_argument(list: bool) -> LogicalType {
    if list {
        LogicalType::list(LogicalType::DOUBLE)
    } else {
        LogicalType::DOUBLE
    }
}

/// DuckDB's `DiscreteQuantileFunction::Bind` and `DiscreteQuantileListFunction::Bind`.
fn bind_discrete(
    list: bool,
) -> impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync + 'static
{
    move |function, input| {
        let bound = discrete_function(&input.argument_types[0], list);
        install(function, "quantile_disc", bound, Some(quantile_argument(list)));
        bind_quantile(function, input).map(Some)
    }
}

/// DuckDB's `ContinuousQuantileFunction::Bind` and `ContinuousQuantileListFunction::Bind`:
/// the overload's declared input type decides the implementation (the actual type for
/// DECIMAL).
fn bind_continuous(
    list: bool,
) -> impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync + 'static
{
    move |function, input| {
        let ty = if function.arguments[0].id() == LogicalTypeId::Decimal {
            input.argument_types[0].clone()
        } else {
            function.arguments[0].clone()
        };
        let bound = continuous_function(&ty, list)?;
        install(function, "quantile_cont", bound, Some(quantile_argument(list)));
        bind_quantile(function, input).map(Some)
    }
}

/// Types `median` interpolates (DuckDB's `CanInterpolate`).
fn can_interpolate(ty: &LogicalType) -> bool {
    use LogicalTypeId as L;
    ty.alias().is_none()
        && matches!(
            ty.id(),
            L::Decimal
                | L::SqlNull
                | L::TinyInt
                | L::SmallInt
                | L::Integer
                | L::UTinyInt
                | L::USmallInt
                | L::UInteger
                | L::UBigInt
                | L::BigInt
                | L::UHugeInt
                | L::HugeInt
                | L::Float
                | L::Double
                | L::Date
                | L::Timestamp
                | L::TimestampTz
                | L::TimestampSec
                | L::TimestampMs
                | L::TimestampNs
                | L::Time
                | L::TimeTz
        )
}

/// DuckDB's `MedianFunction::Bind`.
fn bind_median(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = &input.argument_types[0];
    let bound = if can_interpolate(ty) {
        continuous_function(ty, false)?
    } else {
        discrete_function(ty, false)
    };
    install(function, "median", bound, None);
    Ok(Some(Arc::new(QuantileBindData::median())))
}

/// `median(ANY) -> ANY`.
pub fn median_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "median",
        AggregateFunction::unbound(vec![LogicalType::ANY], LogicalType::ANY, bind_median).order_independent(),
    )
    .renamed("median")
}

/// The `quantile_disc` set (alias `quantile`).
pub fn quantile_disc_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("quantile_disc");
    set.add(
        AggregateFunction::unbound(
            vec![LogicalType::ANY, LogicalType::DOUBLE],
            LogicalType::ANY,
            bind_discrete(false),
        )
        .order_independent(),
    );
    set.add(
        AggregateFunction::unbound(
            vec![LogicalType::ANY, LogicalType::list(LogicalType::DOUBLE)],
            LogicalType::ANY,
            bind_discrete(true),
        )
        .order_independent(),
    );
    set.add(
        AggregateFunction::unbound(vec![LogicalType::ANY], LogicalType::ANY, bind_discrete(false))
            .order_independent(),
    );
    set.renamed("quantile_disc")
}

/// The `quantile_cont` set: DECIMAL and every interpolatable type, scalar and list.
pub fn quantile_cont_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("quantile_cont");
    let decimal = type_id(LogicalTypeId::Decimal);
    for list in [false, true] {
        set.add(
            AggregateFunction::unbound(
                vec![decimal.clone(), quantile_argument(list)],
                decimal.clone(),
                bind_continuous(list),
            )
            .order_independent(),
        );
    }
    for ty in [
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::TIME,
        LogicalType::TIMESTAMP_TZ,
        LogicalType::TIME_TZ,
    ] {
        for list in [false, true] {
            set.add(
                AggregateFunction::unbound(
                    vec![ty.clone(), quantile_argument(list)],
                    ty.clone(),
                    bind_continuous(list),
                )
                .order_independent(),
            );
        }
    }
    set.renamed("quantile_cont")
}

/// A type usable by `mad`: the input, the median type and the absolute deviation (DuckDB's
/// `MadAccessor` specialisations).
trait MadKind: Send + Sync + 'static {
    type Input: FixedOrd;
    /// The median, computed like a continuous quantile.
    type Median: NativeType;
    type Output: FixedOrd;

    fn median(lo: Self::Input, d: f64, hi: Self::Input, exact: bool) -> Result<Self::Median>;

    fn deviation(input: Self::Input, median: Self::Median) -> Result<Self::Output>;

    fn interpolate(lo: Self::Output, d: f64, hi: Self::Output) -> Self::Output;
}

/// `|x - median|` of FLOAT/DOUBLE.
struct FloatMad<F>(PhantomData<fn() -> F>);

impl MadKind for FloatMad<f64> {
    type Input = f64;
    type Median = f64;
    type Output = f64;

    fn median(lo: f64, d: f64, hi: f64, exact: bool) -> Result<f64> {
        Ok(if exact { lo } else { lerp(lo, d, hi) })
    }

    fn deviation(input: f64, median: f64) -> Result<f64> {
        Ok((input - median).abs())
    }

    fn interpolate(lo: f64, d: f64, hi: f64) -> f64 {
        lerp(lo, d, hi)
    }
}

impl MadKind for FloatMad<f32> {
    type Input = f32;
    type Median = f32;
    type Output = f32;

    fn median(lo: f32, d: f64, hi: f32, exact: bool) -> Result<f32> {
        Ok(if exact {
            lo
        } else {
            FloatQuantile::interpolate(lo, d, hi)
        })
    }

    fn deviation(input: f32, median: f32) -> Result<f32> {
        Ok((input - median).abs())
    }

    fn interpolate(lo: f32, d: f64, hi: f32) -> f32 {
        FloatQuantile::interpolate(lo, d, hi)
    }
}

/// `|x - median|` of DECIMAL unscaled values (checked `abs`).
struct DecimalMad<I>(PhantomData<fn() -> I>);

macro_rules! decimal_mad {
    ($($ty:ty),*) => {$(
        impl MadKind for DecimalMad<$ty> {
            type Input = $ty;
            type Median = $ty;
            type Output = $ty;

            fn median(lo: $ty, d: f64, hi: $ty, exact: bool) -> Result<$ty> {
                Ok(if exact { lo } else { DecimalQuantile::<$ty>::interpolate(lo, d, hi) })
            }

            fn deviation(input: $ty, median: $ty) -> Result<$ty> {
                let delta = input.wrapping_sub(median);
                delta
                    .checked_abs()
                    .ok_or_else(|| Error::out_of_range(format!("Overflow on abs({delta})")))
            }

            fn interpolate(lo: $ty, d: f64, hi: $ty) -> $ty {
                DecimalQuantile::<$ty>::interpolate(lo, d, hi)
            }
        }
    )*};
}
decimal_mad!(i16, i32, i64, i128);

/// `|x - median|` of temporal values as an INTERVAL of microseconds; `DATE` inputs are
/// converted to TIMESTAMP first.
struct TemporalMad<const DATE: bool>;

/// DuckDB's `Interval::FromMicro`.
fn interval_from_micros(micros: i64) -> Interval {
    Interval {
        months: 0,
        days: (micros / Interval::MICROS_PER_DAY) as i32,
        micros: micros % Interval::MICROS_PER_DAY,
    }
}

/// DuckDB's `Interval::GetMicro` (without its overflow checks, which cannot trigger for the
/// intervals built here).
fn interval_micros(interval: Interval) -> i64 {
    i64::from(interval.months) * 30 * Interval::MICROS_PER_DAY
        + i64::from(interval.days) * Interval::MICROS_PER_DAY
        + interval.micros
}

impl<const DATE: bool> MadKind for TemporalMad<DATE> {
    type Input = i64;
    type Median = i64;
    type Output = Interval;

    fn median(lo: i64, d: f64, hi: i64, exact: bool) -> Result<i64> {
        let (lo, hi) = if DATE {
            (date_to_timestamp(lo as i32)?, date_to_timestamp(hi as i32)?)
        } else {
            (lo, hi)
        };
        Ok(if exact { lo } else { lerp_i64(lo, d, hi) })
    }

    fn deviation(input: i64, median: i64) -> Result<Interval> {
        let input = if DATE {
            date_to_timestamp(input as i32)?
        } else {
            input
        };
        // `timestamp_t::operator-` checks for overflow (times cannot overflow).
        let delta = input
            .checked_sub(median)
            .ok_or_else(|| Error::out_of_range("Overflow in timestamp subtraction"))?;
        let magnitude = delta
            .checked_abs()
            .ok_or_else(|| Error::out_of_range(format!("Overflow on abs({delta})")))?;
        Ok(interval_from_micros(magnitude))
    }

    /// `lo + (hi - lo) * d` on the intervals' microsecond totals.
    fn interpolate(lo: Interval, d: f64, hi: Interval) -> Interval {
        let lo = interval_micros(lo);
        let delta = interval_micros(hi) - lo;
        interval_from_micros(lo + (delta as f64 * d).round() as i64)
    }
}

/// `mad(x)` (DuckDB's `MedianAbsoluteDeviationOperation`).
struct Mad<M>(PhantomData<fn() -> M>);

impl<M: MadKind> Aggregate for Mad<M> {
    type State = Vec<M::Input>;

    fn initialize(&self) -> Self::State {
        Vec::new()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let values = inputs[0].unified::<M::Input>();
        for row in 0..count {
            if let Some(value) = values.get(row) {
                states.get(row).push(value);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        target.extend_from_slice(source);
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if state.is_empty() {
            result.set_null(row);
            return Ok(());
        }
        let bind = input.bind_data::<QuantileBindData>()?;
        let interpolator = Interpolator::continuous(&bind.quantiles[0], state.len());
        let d = interpolator.rn - interpolator.frn as f64;
        interpolator.select(state, 0, &|a: &M::Input, b: &M::Input| a.sql_cmp(*b));
        let exact = interpolator.frn == interpolator.crn;
        let median = M::median(state[interpolator.frn], d, state[interpolator.crn], exact)?;
        let mut deviations = state
            .iter()
            .map(|&value| M::deviation(value, median))
            .collect::<Result<Vec<_>>>()?;
        interpolator.select(&mut deviations, 0, &|a: &M::Output, b: &M::Output| a.sql_cmp(*b));
        let lo = deviations[interpolator.frn];
        let value = if exact {
            lo
        } else {
            M::interpolate(lo, d, deviations[interpolator.crn])
        };
        result.values_mut::<M::Output>()[row] = value;
        Ok(())
    }
}

/// DuckDB's `GetMedianAbsoluteDeviationAggregateFunctionInternal`.
fn mad_function(ty: &LogicalType) -> Result<AggregateFunction> {
    use LogicalTypeId as L;
    let (output, ops): (LogicalType, Arc<dyn AggregateOps>) = match ty.id() {
        L::Float => (ty.clone(), Arc::new(Typed(Mad::<FloatMad<f32>>(PhantomData)))),
        L::Double => (ty.clone(), Arc::new(Typed(Mad::<FloatMad<f64>>(PhantomData)))),
        L::Decimal => {
            let ops: Arc<dyn AggregateOps> = match ty.physical_type() {
                PhysicalType::Int16 => Arc::new(Typed(Mad::<DecimalMad<i16>>(PhantomData))),
                PhysicalType::Int32 => Arc::new(Typed(Mad::<DecimalMad<i32>>(PhantomData))),
                PhysicalType::Int64 => Arc::new(Typed(Mad::<DecimalMad<i64>>(PhantomData))),
                PhysicalType::Int128 => Arc::new(Typed(Mad::<DecimalMad<i128>>(PhantomData))),
                _ => {
                    return Err(Error::not_implemented(
                        "Unimplemented Median Absolute Deviation DECIMAL aggregate",
                    ));
                }
            };
            (ty.clone(), ops)
        }
        L::Date => (
            LogicalType::INTERVAL,
            Arc::new(Typed(Mad::<DateMad>(PhantomData))),
        ),
        L::Timestamp | L::TimestampTz | L::Time | L::TimeTz => (
            LogicalType::INTERVAL,
            Arc::new(Typed(Mad::<TemporalMad<false>>(PhantomData))),
        ),
        _ => {
            return Err(Error::not_implemented(
                "Unimplemented Median Absolute Deviation aggregate",
            ));
        }
    };
    let mut function = AggregateFunction::new(
        "mad",
        vec![ty.clone()],
        output,
        roastedduck_function::aggregate::Unbound,
    )
    .order_independent();
    function.ops = ops;
    Ok(function.with_bind(|_, _| Ok(Some(Arc::new(QuantileBindData::median())))))
}

/// DATE inputs of `mad`, read as 32-bit days.
struct DateMad;

impl MadKind for DateMad {
    type Input = i32;
    type Median = i64;
    type Output = Interval;

    fn median(lo: i32, d: f64, hi: i32, exact: bool) -> Result<i64> {
        TemporalMad::<true>::median(i64::from(lo), d, i64::from(hi), exact)
    }

    fn deviation(input: i32, median: i64) -> Result<Interval> {
        TemporalMad::<true>::deviation(i64::from(input), median)
    }

    fn interpolate(lo: Interval, d: f64, hi: Interval) -> Interval {
        TemporalMad::<true>::interpolate(lo, d, hi)
    }
}

/// The `mad` set: DECIMAL plus FLOAT, DOUBLE and the temporal types.
pub fn mad_functions() -> Result<AggregateFunctionSet> {
    let mut set = AggregateFunctionSet::new("mad");
    set.add(AggregateFunction::unbound(
        vec![type_id(LogicalTypeId::Decimal)],
        type_id(LogicalTypeId::Decimal),
        |function, input| {
            let bound = mad_function(&input.argument_types[0])?;
            function.replace_with(bound);
            Ok(Some(Arc::new(QuantileBindData::median())))
        },
    ));
    for ty in [
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::TIME,
        LogicalType::TIMESTAMP_TZ,
        LogicalType::TIME_TZ,
    ] {
        set.add(mad_function(&ty)?);
    }
    Ok(set.renamed("mad"))
}
