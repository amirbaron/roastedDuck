//! The numeric math functions of DuckDB's `extension/core_functions/scalar/math/numeric.cpp`:
//! `abs`, `sign`, `ceil`, `floor`, `round`, `trunc`, `pow`, `sqrt`, the logarithms,
//! trigonometry, `gamma`, `factorial`, `gcd`, `lcm`, `nextafter`, `bit_count`, ...
//!
//! Floating point kernels call the same C library functions as DuckDB (Rust's `f64`
//! methods lower to the platform `libm`), so results match bit for bit on one platform.
#![allow(
    clippy::float_cmp,
    reason = "DuckDB's operators compare floating point values exactly"
)]

use std::fmt::Display;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, ScalarKernel};
use rustdb_types::{DataChunk, Date, LogicalType, LogicalTypeId, NativeType, PhysicalType, Timestamp, Value, Vector};

use crate::scalar::executor::{Str, binary, unary};
use crate::signature::numeric_types;

/// DuckDB's `PI`.
const PI: f64 = std::f64::consts::PI;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

// ---------------------------------------------------------------------------------------
// Kernel helpers
// ---------------------------------------------------------------------------------------

/// A kernel applying `op` to every row of a one-argument call.
fn kernel<A: NativeType, R: NativeType>(op: impl Fn(A) -> Result<R> + Copy + Send + Sync + 'static) -> ScalarKernel {
    Arc::new(move |args: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
        unary::<A, R>(args, result, |value, out| {
            *out = op(value)?;
            Ok(true)
        })
    })
}

/// A kernel applying `op` to every row of a two-argument call.
fn binary_kernel<A: NativeType, B: NativeType, R: NativeType>(
    op: impl Fn(A, B) -> Result<R> + Copy + Send + Sync + 'static,
) -> ScalarKernel {
    Arc::new(move |args: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
        binary::<A, B, R>(args, result, |left, right, out| {
            *out = op(left, right)?;
            Ok(true)
        })
    })
}

/// DuckDB's `ScalarFunction::NopFunction`: the result is the input.
fn nop_kernel() -> ScalarKernel {
    Arc::new(|args: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
        *result = args.column(0).clone();
        Ok(())
    })
}

/// The kernel of an overload whose bind callback installs the real one.
fn unbound_kernel() -> ScalarKernel {
    Arc::new(|_: &DataChunk, _: &mut FunctionState, _: &mut Vector| {
        Err(Error::internal("function used without being bound"))
    })
}

fn function(name: &str, arguments: Vec<LogicalType>, return_type: LogicalType, kernel: ScalarKernel) -> ScalarFunction {
    let mut function = ScalarFunction::new(name, arguments, return_type, |_, _, _| Ok(()));
    function.kernel = kernel;
    function
}

/// A set with the single overload `name(DOUBLE) -> DOUBLE`.
fn double_function(name: &str, op: impl Fn(f64) -> Result<f64> + Copy + Send + Sync + 'static) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(function(
        name,
        vec![LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        kernel::<f64, f64>(op),
    ));
    set
}

/// A set with the single overload `name(DOUBLE, DOUBLE) -> DOUBLE`.
fn double_binary_function(
    name: &str,
    op: impl Fn(f64, f64) -> Result<f64> + Copy + Send + Sync + 'static,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(function(
        name,
        vec![LogicalType::DOUBLE, LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        binary_kernel::<f64, f64, f64>(op),
    ));
    set
}

fn decimal_properties(ty: &LogicalType) -> Result<(u8, u8)> {
    ty.decimal_properties()
        .ok_or_else(|| Error::internal(format!("Could not get the decimal properties of {ty}")))
}

/// Dispatches `$body` with `$T` bound to the storage type of a DECIMAL with physical type
/// `$physical` (anything wider than BIGINT is HUGEINT, as in DuckDB's `default:` branches).
macro_rules! with_decimal_storage {
    ($physical:expr, $T:ident => $body:expr) => {{
        match $physical {
            PhysicalType::Int16 => {
                type $T = i16;
                $body
            }
            PhysicalType::Int32 => {
                type $T = i32;
                $body
            }
            PhysicalType::Int64 => {
                type $T = i64;
                $body
            }
            _ => {
                type $T = i128;
                $body
            }
        }
    }};
}

// ---------------------------------------------------------------------------------------
// Signed integer helpers (DuckDB's `TryAbsOperator`, `GreatestCommonDivisor`)
// ---------------------------------------------------------------------------------------

/// A signed integer type with the wrapping operations DuckDB's integer kernels rely on.
trait SignedInt: NativeType + Display + Ord {
    const MIN: Self;
    const ZERO: Self;
    const ONE: Self;
    const NEG_ONE: Self;
    fn wrapping_abs_(self) -> Self;
    fn wrapping_rem_(self, other: Self) -> Self;
    fn wrapping_div_(self, other: Self) -> Self;
    fn checked_mul_(self, other: Self) -> Option<Self>;
    fn count_ones_(self) -> u32;
}

macro_rules! signed_int {
    ($($ty:ty),*) => {$(
        impl SignedInt for $ty {
            const MIN: Self = <$ty>::MIN;
            const ZERO: Self = 0;
            const ONE: Self = 1;
            const NEG_ONE: Self = -1;
            #[inline]
            fn wrapping_abs_(self) -> Self { self.wrapping_abs() }
            #[inline]
            fn wrapping_rem_(self, other: Self) -> Self { self.wrapping_rem(other) }
            #[inline]
            fn wrapping_div_(self, other: Self) -> Self { self.wrapping_div(other) }
            #[inline]
            fn checked_mul_(self, other: Self) -> Option<Self> { self.checked_mul(other) }
            #[inline]
            fn count_ones_(self) -> u32 { self.count_ones() }
        }
    )*};
}
signed_int!(i8, i16, i32, i64, i128);

/// `abs(x)` failing on the minimum value (DuckDB's `TryAbsOperator`).
fn try_abs<T: SignedInt>(value: T) -> Result<T> {
    if value == T::MIN {
        return Err(Error::out_of_range(format!("Overflow on abs({value})")));
    }
    Ok(value.wrapping_abs_())
}

/// DuckDB's `GreatestCommonDivisor`.
fn greatest_common_divisor<T: SignedInt>(left: T, right: T) -> Result<T> {
    if (left == T::MIN && right == T::NEG_ONE) || (left == T::NEG_ONE && right == T::MIN) {
        return Ok(T::ONE);
    }
    let (mut a, mut b) = (left, right);
    loop {
        if a == T::ZERO {
            return try_abs(b);
        }
        b = b.wrapping_rem_(a);
        if b == T::ZERO {
            return try_abs(a);
        }
        a = a.wrapping_rem_(b);
    }
}

/// DuckDB's `LeastCommonMultipleOperator`.
fn least_common_multiple<T: SignedInt>(left: T, right: T) -> Result<T> {
    if left == T::ZERO || right == T::ZERO {
        return Ok(T::ZERO);
    }
    let divisor = greatest_common_divisor(left, right)?;
    let result = left
        .checked_mul_(right.wrapping_div_(divisor))
        .ok_or_else(|| Error::out_of_range("lcm value is out of range"))?;
    try_abs(result)
}

// ---------------------------------------------------------------------------------------
// abs / @
// ---------------------------------------------------------------------------------------

/// `AbsOperator` on DECIMAL storage.
fn decimal_abs_kernel(physical: PhysicalType) -> ScalarKernel {
    with_decimal_storage!(physical, T => kernel::<T, T>(|value: T| Ok(value.wrapping_abs())))
}

/// `@` / `abs` (DuckDB's `AbsOperatorFun`).
pub fn abs_functions() -> ScalarFunctionSet {
    const NAME: &str = "@";
    let mut set = ScalarFunctionSet::new(NAME);
    for ty in numeric_types() {
        let kernel = match ty.id() {
            LogicalTypeId::Decimal => {
                set.add(
                    function(NAME, vec![ty.clone()], ty.clone(), unbound_kernel()).with_bind(|function, input| {
                        let ty = input.argument_types[0].clone();
                        function.kernel = decimal_abs_kernel(ty.physical_type());
                        function.arguments[0] = ty.clone();
                        function.return_type = ty;
                        Ok(None)
                    }),
                );
                continue;
            }
            LogicalTypeId::TinyInt => kernel::<i8, i8>(try_abs),
            LogicalTypeId::SmallInt => kernel::<i16, i16>(try_abs),
            LogicalTypeId::Integer => kernel::<i32, i32>(try_abs),
            LogicalTypeId::BigInt => kernel::<i64, i64>(try_abs),
            LogicalTypeId::HugeInt => kernel::<i128, i128>(try_abs),
            LogicalTypeId::Float => kernel::<f32, f32>(|value| Ok(value.abs())),
            LogicalTypeId::Double => kernel::<f64, f64>(|value| Ok(value.abs())),
            _ => nop_kernel(),
        };
        set.add(function(NAME, vec![ty.clone()], ty, kernel));
    }
    set
}

// ---------------------------------------------------------------------------------------
// bit_count, sign
// ---------------------------------------------------------------------------------------

/// The number of set bits, as DuckDB's `int8_t` counter (128 wraps to -128).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "DuckDB counts into an int8_t"
)]
fn bit_count<T: SignedInt>(value: T) -> Result<i8> {
    Ok(value.count_ones_() as i8)
}

/// `bit_count` (DuckDB's `BitCountFun`).
pub fn bit_count_functions() -> ScalarFunctionSet {
    const NAME: &str = "bit_count";
    let mut set = ScalarFunctionSet::new(NAME);
    let integers: [(LogicalType, ScalarKernel); 5] = [
        (LogicalType::TINYINT, kernel::<i8, i8>(bit_count)),
        (LogicalType::SMALLINT, kernel::<i16, i8>(bit_count)),
        (LogicalType::INTEGER, kernel::<i32, i8>(bit_count)),
        (LogicalType::BIGINT, kernel::<i64, i8>(bit_count)),
        (LogicalType::HUGEINT, kernel::<i128, i8>(bit_count)),
    ];
    for (ty, kernel) in integers {
        set.add(function(NAME, vec![ty], LogicalType::TINYINT, kernel));
    }
    set.add(ScalarFunction::new(
        NAME,
        vec![LogicalType::BIT],
        LogicalType::BIGINT,
        |args, _, result| {
            unary::<Str, i64>(args, result, |bits, out| {
                let ones: u32 = bits.iter().skip(1).map(|byte| byte.count_ones()).sum();
                let padding = bits.first().copied().unwrap_or_default();
                *out = i64::from(ones) - i64::from(padding);
                Ok(true)
            })
        },
    ));
    set
}

/// DuckDB's `SignOperator` (NaN has sign 0).
fn sign<T: PartialOrd + Default>(value: T) -> Result<i8> {
    Ok(match value.partial_cmp(&T::default()) {
        Some(std::cmp::Ordering::Greater) => 1,
        Some(std::cmp::Ordering::Less) => -1,
        _ => 0,
    })
}

/// `sign` (DuckDB's `SignFun`).
pub fn sign_functions() -> ScalarFunctionSet {
    const NAME: &str = "sign";
    let mut set = ScalarFunctionSet::new(NAME);
    for ty in numeric_types() {
        if ty.id() == LogicalTypeId::Decimal {
            continue;
        }
        let kernel = crate::dispatch::with_numeric_type!(
            ty.physical_type(),
            T => kernel::<T, i8>(sign::<T>),
            _ => unbound_kernel(),
        );
        set.add(function(NAME, vec![ty], LogicalType::TINYINT, kernel));
    }
    set
}

// ---------------------------------------------------------------------------------------
// ceil, floor, round, trunc
// ---------------------------------------------------------------------------------------

/// The unscaled storage of a DECIMAL, with the wrapping arithmetic of DuckDB's kernels.
trait DecimalInt: NativeType + PartialOrd {
    const ZERO: Self;
    const ONE: Self;
    /// `UnsafeNumericCast<T>(POWERS_OF_TEN[exponent])`.
    fn power_of_ten(exponent: u32) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn mul(self, other: Self) -> Self;
    fn div(self, other: Self) -> Self;
}

macro_rules! decimal_int {
    ($($ty:ty),*) => {$(
        impl DecimalInt for $ty {
            const ZERO: Self = 0;
            const ONE: Self = 1;
            #[inline]
            fn power_of_ten(exponent: u32) -> Self { <$ty>::wrapping_pow(10, exponent) }
            #[inline]
            fn add(self, other: Self) -> Self { self.wrapping_add(other) }
            #[inline]
            fn sub(self, other: Self) -> Self { self.wrapping_sub(other) }
            #[inline]
            fn mul(self, other: Self) -> Self { self.wrapping_mul(other) }
            #[inline]
            fn div(self, other: Self) -> Self { self.wrapping_div(other) }
        }
    )*};
}
decimal_int!(i16, i32, i64, i128);

/// How a DECIMAL kernel drops digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rounding {
    Ceil,
    Floor,
    Round,
    Trunc,
}

impl Rounding {
    /// Drops digits by dividing by `divide` and multiplies the result by `multiply`
    /// (DuckDB's `CeilDecimalOperator`, `FloorDecimalOperator`, `RoundDecimalOperator`,
    /// `TruncDecimalOperator` and their precision variants).
    #[inline]
    fn apply<T: DecimalInt>(self, value: T, divide: T, multiply: T) -> T {
        match self {
            Self::Ceil => {
                if value <= T::ZERO {
                    value.div(divide)
                } else {
                    value.sub(T::ONE).div(divide).add(T::ONE)
                }
            }
            Self::Floor => {
                if value < T::ZERO {
                    value.add(T::ONE).div(divide).sub(T::ONE)
                } else {
                    value.div(divide)
                }
            }
            Self::Round => {
                let addition = divide.div(T::ONE.add(T::ONE));
                let value = if value < T::ZERO {
                    value.sub(addition)
                } else {
                    value.add(addition)
                };
                value.div(divide).mul(multiply)
            }
            Self::Trunc => value.div(divide).mul(multiply),
        }
    }
}

/// A DECIMAL kernel dividing by `10^divide` and multiplying by `10^multiply`.
fn decimal_rounding_kernel(physical: PhysicalType, rounding: Rounding, divide: u32, multiply: u32) -> ScalarKernel {
    with_decimal_storage!(physical, T => {
        let divide = T::power_of_ten(divide);
        let multiply = T::power_of_ten(multiply);
        kernel::<T, T>(move |value: T| Ok(rounding.apply(value, divide, multiply)))
    })
}

/// DuckDB's `BindGenericRoundFunctionDecimal`: `DECIMAL(w, s) -> DECIMAL(w, 0)`.
fn bind_decimal_rounding(rounding: Rounding) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult + Send + Sync {
    move |function, input| {
        let ty = input.argument_types[0].clone();
        let (width, scale) = decimal_properties(&ty)?;
        function.kernel = if scale == 0 {
            nop_kernel()
        } else {
            decimal_rounding_kernel(ty.physical_type(), rounding, u32::from(scale), 0)
        };
        function.arguments[0] = ty;
        function.return_type = LogicalType::decimal(width, 0);
        Ok(None)
    }
}

/// DuckDB's `BindDecimalRoundPrecision`: `round`/`trunc(DECIMAL, INTEGER)` with a constant
/// precision.
fn bind_decimal_precision(rounding: Rounding) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult + Send + Sync {
    move |function, input| {
        let ty = input.argument_types[0].clone();
        if input.argument_types[1].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let unsupported = || {
            Error::not_implemented(format!(
                "{}(DECIMAL, INTEGER) with non-constant precision is not supported",
                function.name.to_uppercase()
            ))
        };
        let Some(value) = input.constants.get(1).cloned().flatten() else {
            return Err(unsupported());
        };
        let value = value.default_cast_as(&LogicalType::INTEGER)?;
        if value.is_null() {
            return Err(unsupported());
        }
        let round_value = value
            .as_i32()
            .ok_or_else(|| Error::internal("INTEGER cast did not produce an integer"))?;
        let (width, scale) = decimal_properties(&ty)?;
        let physical = ty.physical_type();
        let (target_scale, kernel) = if round_value < 0 {
            let kernel = if round_value <= -(i32::from(width) - i32::from(scale)) {
                // Every digit is dropped: the result is the constant 0.
                let zero = Value::decimal(0, width, 0);
                Arc::new(move |_: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
                    *result = Vector::constant(&zero)?;
                    Ok(())
                }) as ScalarKernel
            } else {
                let multiply = round_value.unsigned_abs();
                decimal_rounding_kernel(physical, rounding, multiply + u32::from(scale), multiply)
            };
            (0, kernel)
        } else if round_value >= i32::from(scale) {
            (scale, nop_kernel())
        } else {
            let target = u8::try_from(round_value).map_err(|_| Error::internal("precision out of range"))?;
            (
                target,
                decimal_rounding_kernel(physical, rounding, u32::from(scale - target), 0),
            )
        };
        function.kernel = kernel;
        function.arguments[0] = ty;
        function.return_type = LogicalType::decimal(width, target_scale);
        Ok(None)
    }
}

/// `round`/`trunc(float, precision)` computed in double precision (DuckDB's
/// `RoundOperatorPrecision` and `TruncOperatorPrecision`). `as_input` converts the
/// precision like `TA(precision)` does; `truncate` selects `trunc`.
fn float_precision(input: f64, precision: i32, as_input: fn(f64) -> f64, truncate: bool) -> f64 {
    let op = if truncate { f64::trunc } else { f64::round };
    if precision < 0 {
        let modifier = 10f64.powf(-as_input(f64::from(precision)));
        let value = op(input / modifier) * modifier;
        if value.is_finite() {
            value
        } else if truncate {
            input
        } else {
            0.0
        }
    } else {
        let modifier = 10f64.powf(as_input(f64::from(precision)));
        let value = op(input * modifier) / modifier;
        if value.is_finite() { value } else { input }
    }
}

#[allow(clippy::cast_possible_truncation, reason = "TA(precision) for TA = float")]
fn as_float(value: f64) -> f64 {
    f64::from(value as f32)
}

fn as_double(value: f64) -> f64 {
    value
}

#[allow(clippy::cast_possible_truncation, reason = "LossyNumericCast<float>")]
fn float_round_precision(input: f32, precision: i32) -> Result<f32> {
    Ok(float_precision(f64::from(input), precision, as_float, false) as f32)
}

#[allow(clippy::cast_possible_truncation, reason = "LossyNumericCast<float>")]
fn float_trunc_precision(input: f32, precision: i32) -> Result<f32> {
    Ok(float_precision(f64::from(input), precision, as_float, true) as f32)
}

fn double_round_precision(input: f64, precision: i32) -> Result<f64> {
    Ok(float_precision(input, precision, as_double, false))
}

fn double_trunc_precision(input: f64, precision: i32) -> Result<f64> {
    Ok(float_precision(input, precision, as_double, true))
}

/// `trunc(integer, precision)` (DuckDB's `TruncIntegerOperator`).
trait TruncPrecision: NativeType {
    fn trunc_precision(self, precision: i32) -> Self;
}

/// `round(integer, precision)` (DuckDB's `RoundIntegerOperator`).
trait RoundPrecision: NativeType {
    fn round_precision(self, precision: i32) -> Self;
}

/// Implements the integer precision operators; `$wide` is the type of DuckDB's
/// `POWERS_OF_TEN` entries the arithmetic happens in, `$cached` their count.
macro_rules! integer_precision {
    ($wide:ty, $cached:expr; $($ty:ty),*) => {$(
        impl TruncPrecision for $ty {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_lossless,
                clippy::cast_sign_loss,
                clippy::cast_possible_wrap,
                reason = "DuckDB computes in the power-of-ten type and casts back unchecked"
            )]
            fn trunc_precision(self, precision: i32) -> Self {
                if precision >= 0 {
                    return self;
                }
                if precision <= -$cached {
                    return 0;
                }
                let power = <$wide>::pow(10, precision.unsigned_abs());
                let result = (self as $wide) / power;
                if result == 0 { 0 } else { result.wrapping_mul(power) as $ty }
            }
        }
    )*};
}
integer_precision!(i64, 19; i8, i16, i32, i64);
integer_precision!(u64, 19; u8, u16, u32, u64);
integer_precision!(i128, 39; i128);
integer_precision!(u128, 39; u128);

macro_rules! round_precision {
    ($wide:ty, $cached:expr; $($ty:ty),*) => {$(
        impl RoundPrecision for $ty {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_lossless,
                reason = "DuckDB computes in the power-of-ten type and casts back unchecked"
            )]
            fn round_precision(self, precision: i32) -> Self {
                if precision >= 0 {
                    return self;
                }
                if precision <= -$cached {
                    return 0;
                }
                let power = <$wide>::pow(10, precision.unsigned_abs());
                let mut addition = power / 2;
                if self < 0 {
                    addition = -addition;
                }
                addition = addition.wrapping_add(self as $wide);
                addition /= power;
                if addition == 0 { 0 } else { addition.wrapping_mul(power) as $ty }
            }
        }
    )*};
}
round_precision!(i64, 19; i8, i16, i32, i64);
round_precision!(i128, 39; i128);

/// The overloads `name(T)` and `name(T, INTEGER)` of `round`/`trunc`.
fn rounding_overloads(name: &str, ty: &LogicalType, unary: ScalarKernel, precision: ScalarKernel) -> [ScalarFunction; 2] {
    [
        function(name, vec![ty.clone()], ty.clone(), unary),
        function(name, vec![ty.clone(), LogicalType::INTEGER], ty.clone(), precision),
    ]
}

/// The DECIMAL overloads of `round`/`trunc`.
fn decimal_rounding_overloads(name: &str, ty: &LogicalType, rounding: Rounding) -> [ScalarFunction; 2] {
    [
        function(name, vec![ty.clone()], ty.clone(), unbound_kernel()).with_bind(bind_decimal_rounding(rounding)),
        function(name, vec![ty.clone(), LogicalType::INTEGER], ty.clone(), unbound_kernel())
            .with_bind(bind_decimal_precision(rounding)),
    ]
}

/// `trunc` (DuckDB's `TruncFun`).
pub fn trunc_functions() -> ScalarFunctionSet {
    const NAME: &str = "trunc";
    let mut set = ScalarFunctionSet::new(NAME);
    for ty in numeric_types() {
        let overloads = match ty.id() {
            LogicalTypeId::Float => rounding_overloads(
                NAME,
                &ty,
                kernel::<f32, f32>(|value| Ok(value.trunc())),
                binary_kernel::<f32, i32, f32>(float_trunc_precision),
            ),
            LogicalTypeId::Double => rounding_overloads(
                NAME,
                &ty,
                kernel::<f64, f64>(|value| Ok(value.trunc())),
                binary_kernel::<f64, i32, f64>(double_trunc_precision),
            ),
            LogicalTypeId::Decimal => decimal_rounding_overloads(NAME, &ty, Rounding::Trunc),
            _ => {
                let precision = crate::dispatch::with_integer_type!(
                    ty.physical_type(),
                    T => binary_kernel::<T, i32, T>(|value: T, precision| Ok(value.trunc_precision(precision))),
                    _ => unbound_kernel(),
                );
                rounding_overloads(NAME, &ty, nop_kernel(), precision)
            }
        };
        for overload in overloads {
            set.add(overload);
        }
    }
    set
}

/// `round` (DuckDB's `RoundFun`); unsigned integers have no overloads.
pub fn round_functions() -> ScalarFunctionSet {
    const NAME: &str = "round";
    let mut set = ScalarFunctionSet::new(NAME);
    for ty in numeric_types() {
        let overloads = match ty.physical_type() {
            _ if ty.id() == LogicalTypeId::Decimal => decimal_rounding_overloads(NAME, &ty, Rounding::Round),
            PhysicalType::Float => rounding_overloads(
                NAME,
                &ty,
                kernel::<f32, f32>(|value| Ok(value.round())),
                binary_kernel::<f32, i32, f32>(float_round_precision),
            ),
            PhysicalType::Double => rounding_overloads(
                NAME,
                &ty,
                kernel::<f64, f64>(|value| Ok(value.round())),
                binary_kernel::<f64, i32, f64>(double_round_precision),
            ),
            PhysicalType::Int8 => integer_round_overloads::<i8>(NAME, &ty),
            PhysicalType::Int16 => integer_round_overloads::<i16>(NAME, &ty),
            PhysicalType::Int32 => integer_round_overloads::<i32>(NAME, &ty),
            PhysicalType::Int64 => integer_round_overloads::<i64>(NAME, &ty),
            PhysicalType::Int128 => integer_round_overloads::<i128>(NAME, &ty),
            _ => continue,
        };
        for overload in overloads {
            set.add(overload);
        }
    }
    set
}

fn integer_round_overloads<T: RoundPrecision>(name: &str, ty: &LogicalType) -> [ScalarFunction; 2] {
    rounding_overloads(
        name,
        ty,
        nop_kernel(),
        binary_kernel::<T, i32, T>(|value: T, precision| Ok(value.round_precision(precision))),
    )
}

/// `ceil`/`floor` for FLOAT, DOUBLE and DECIMAL (integers have no overloads).
fn ceil_floor_functions(name: &str, rounding: Rounding) -> ScalarFunctionSet {
    let ceil = rounding == Rounding::Ceil;
    let mut set = ScalarFunctionSet::new(name);
    set.add(function(
        name,
        vec![LogicalType::FLOAT],
        LogicalType::FLOAT,
        if ceil {
            kernel::<f32, f32>(|value| Ok(value.ceil()))
        } else {
            kernel::<f32, f32>(|value| Ok(value.floor()))
        },
    ));
    set.add(function(
        name,
        vec![LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        if ceil {
            kernel::<f64, f64>(|value| Ok(value.ceil()))
        } else {
            kernel::<f64, f64>(|value| Ok(value.floor()))
        },
    ));
    let decimal = crate::signature::type_id(LogicalTypeId::Decimal);
    set.add(
        function(name, vec![decimal.clone()], decimal, unbound_kernel()).with_bind(bind_decimal_rounding(rounding)),
    );
    set
}

/// `ceil` (DuckDB's `CeilFun`).
pub fn ceil_functions() -> ScalarFunctionSet {
    ceil_floor_functions("ceil", Rounding::Ceil)
}

/// `floor` (DuckDB's `FloorFun`).
pub fn floor_functions() -> ScalarFunctionSet {
    ceil_floor_functions("floor", Rounding::Floor)
}

// ---------------------------------------------------------------------------------------
// Powers, roots and logarithms
// ---------------------------------------------------------------------------------------

/// The logarithm checks shared by `ln`, `log10` and `log2`.
fn check_logarithm(input: f64) -> Result<()> {
    if input < 0.0 {
        return Err(Error::out_of_range("cannot take logarithm of a negative number"));
    }
    if input == 0.0 {
        return Err(Error::out_of_range("cannot take logarithm of zero"));
    }
    Ok(())
}

fn log10(input: f64) -> Result<f64> {
    check_logarithm(input)?;
    Ok(input.log10())
}

/// `log(b, x)` (DuckDB's `LogBaseOperator`).
fn log_base(base: f64, input: f64) -> Result<f64> {
    let divisor = log10(base)?;
    if divisor == 0.0 {
        return Err(Error::out_of_range("divison by zero in based logarithm"));
    }
    Ok(log10(input)? / divisor)
}

fn sqrt(input: f64) -> Result<f64> {
    if input < 0.0 {
        return Err(Error::out_of_range("cannot take square root of a negative number"));
    }
    Ok(input.sqrt())
}

/// `log` (DuckDB's `LogFun`): base 10, or an explicit base.
pub fn log_functions() -> ScalarFunctionSet {
    let mut set = double_function("log", log10);
    set.add(function(
        "log",
        vec![LogicalType::DOUBLE, LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        binary_kernel::<f64, f64, f64>(log_base),
    ));
    set
}

// ---------------------------------------------------------------------------------------
// Trigonometry
// ---------------------------------------------------------------------------------------

/// DuckDB's `NoInfiniteDoubleWrapper`: infinities are an error, NaN passes through.
fn no_infinite(input: f64, op: impl Fn(f64) -> Result<f64>) -> Result<f64> {
    if !input.is_finite() {
        if input.is_nan() {
            return Ok(input);
        }
        return Err(Error::out_of_range(format!(
            "input value {input:.6} is out of range for numeric function"
        )));
    }
    op(input)
}

/// `cot(x)` (DuckDB's `NoInfiniteNoZeroDoubleWrapper<CotOperator>`).
fn cot(input: f64) -> Result<f64> {
    no_infinite(input, |input| {
        if input == 0.0 {
            return Err(Error::out_of_range(format!(
                "input value {input:.6} is out of range for numeric function cotangent"
            )));
        }
        Ok(1.0 / input.tan())
    })
}

fn asin(input: f64) -> Result<f64> {
    no_infinite(input, |input| {
        if !(-1.0..=1.0).contains(&input) {
            return Err(Error::invalid_input("ASIN is undefined outside [-1,1]"));
        }
        Ok(input.asin())
    })
}

fn acos(input: f64) -> Result<f64> {
    no_infinite(input, |input| {
        if !(-1.0..=1.0).contains(&input) {
            return Err(Error::invalid_input("ACOS is undefined outside [-1,1]"));
        }
        Ok(input.acos())
    })
}

/// `atanh(x)`; DuckDB returns +infinity at both -1 and 1.
fn atanh(input: f64) -> Result<f64> {
    if input < -1.0 || input > 1.0 {
        return Err(Error::invalid_input("ATANH is undefined outside [-1,1]"));
    }
    if input == -1.0 || input == 1.0 {
        return Ok(f64::INFINITY);
    }
    Ok(input.atanh())
}

// ---------------------------------------------------------------------------------------
// gamma, factorial, even, nextafter
// ---------------------------------------------------------------------------------------

#[cfg(unix)]
mod libm {
    unsafe extern "C" {
        fn tgamma(x: f64) -> f64;
        fn lgamma_r(x: f64, sign: *mut i32) -> f64;
    }

    /// C's `tgamma`, which DuckDB's `gamma` calls.
    pub(super) fn gamma(x: f64) -> f64 {
        // SAFETY: `tgamma` is a pure C math function without preconditions.
        unsafe { tgamma(x) }
    }

    /// C's `lgamma` (through the reentrant `lgamma_r`, which computes the same value).
    pub(super) fn log_gamma(x: f64) -> f64 {
        let mut sign = 0;
        // SAFETY: `lgamma_r` only writes the sign of gamma(x) through the valid pointer.
        unsafe { lgamma_r(x, &raw mut sign) }
    }
}

fn gamma(input: f64) -> Result<f64> {
    if input == 0.0 {
        return Err(Error::out_of_range("cannot take gamma of zero"));
    }
    Ok(libm::gamma(input))
}

fn log_gamma(input: f64) -> Result<f64> {
    if input == 0.0 {
        return Err(Error::out_of_range("cannot take log gamma of zero"));
    }
    Ok(libm::log_gamma(input))
}

/// `n!` as a HUGEINT (DuckDB's `FactorialOperator`).
fn factorial(input: i32) -> Result<i128> {
    if input < 0 {
        return Err(Error::out_of_range("factorial of a negative number is undefined"));
    }
    let mut result: i128 = 1;
    for i in 2..=input {
        result = result
            .checked_mul(i128::from(i))
            .ok_or_else(|| Error::out_of_range("Value out of range"))?;
    }
    Ok(result)
}

/// Rounds away from zero to the next even integer (DuckDB's `EvenOperator`).
fn even(input: f64) -> Result<f64> {
    let mut value = if input >= 0.0 { input.ceil() } else { -(-input).ceil() };
    if (value / 2.0).floor() * 2.0 != value {
        if input >= 0.0 {
            value += 1.0;
        } else {
            value -= 1.0;
        }
    }
    Ok(value)
}

/// C's `nextafter`/`nextafterf`: the next representable value after `$x` towards `$y`.
macro_rules! next_after {
    ($name:ident, $ty:ty) => {
        fn $name(x: $ty, y: $ty) -> Result<$ty> {
            if x.is_nan() || y.is_nan() {
                return Ok(x + y);
            }
            if x == y {
                return Ok(y);
            }
            if x == 0.0 {
                return Ok(<$ty>::from_bits(1).copysign(y));
            }
            let bits = x.to_bits();
            let bits = if (y > x) == (x > 0.0) { bits + 1 } else { bits - 1 };
            Ok(<$ty>::from_bits(bits))
        }
    };
}
next_after!(next_after_f64, f64);
next_after!(next_after_f32, f32);

// ---------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------

/// A set of `name(FLOAT) -> BOOLEAN` and `name(DOUBLE) -> BOOLEAN`.
fn float_predicate(name: &str, f32_op: fn(f32) -> bool, f64_op: fn(f64) -> bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(function(
        name,
        vec![LogicalType::FLOAT],
        LogicalType::BOOLEAN,
        kernel::<f32, bool>(move |value| Ok(f32_op(value))),
    ));
    set.add(function(
        name,
        vec![LogicalType::DOUBLE],
        LogicalType::BOOLEAN,
        kernel::<f64, bool>(move |value| Ok(f64_op(value))),
    ));
    set
}

/// `isinf`/`isfinite`: the float predicates plus DATE, TIMESTAMP and TIMESTAMPTZ.
fn finiteness(name: &str, finite: bool) -> ScalarFunctionSet {
    let mut set = if finite {
        float_predicate(name, f32::is_finite, f64::is_finite)
    } else {
        float_predicate(name, f32::is_infinite, f64::is_infinite)
    };
    set.add(function(
        name,
        vec![LogicalType::DATE],
        LogicalType::BOOLEAN,
        kernel::<Date, bool>(move |value| Ok(value.is_finite() == finite)),
    ));
    for ty in [LogicalType::TIMESTAMP, LogicalType::TIMESTAMP_TZ] {
        set.add(function(
            name,
            vec![ty],
            LogicalType::BOOLEAN,
            kernel::<Timestamp, bool>(move |value| Ok(value.is_finite() == finite)),
        ));
    }
    set
}

/// A set of signed-integer overloads `name(BIGINT, BIGINT)` and `name(HUGEINT, HUGEINT)`.
fn integer_binary(name: &str, i64_op: fn(i64, i64) -> Result<i64>, i128_op: fn(i128, i128) -> Result<i128>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(function(
        name,
        vec![LogicalType::BIGINT, LogicalType::BIGINT],
        LogicalType::BIGINT,
        binary_kernel::<i64, i64, i64>(i64_op),
    ));
    set.add(function(
        name,
        vec![LogicalType::HUGEINT, LogicalType::HUGEINT],
        LogicalType::HUGEINT,
        binary_kernel::<i128, i128, i128>(i128_op),
    ));
    set
}

/// Every numeric math function set with the aliases DuckDB registers for it, as
/// `(set, aliases)`.
pub fn numeric_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let mut pi = ScalarFunctionSet::new("pi");
    pi.add(ScalarFunction::new("pi", vec![], LogicalType::DOUBLE, |_, _, result| {
        *result = Vector::constant(&Value::double(PI))?;
        Ok(())
    }));
    let mut factorial_set = ScalarFunctionSet::new("!__postfix");
    factorial_set.add(function(
        "!__postfix",
        vec![LogicalType::INTEGER],
        LogicalType::HUGEINT,
        kernel::<i32, i128>(factorial),
    ));
    let mut next_after = double_binary_function("nextafter", next_after_f64);
    next_after.add(function(
        "nextafter",
        vec![LogicalType::FLOAT, LogicalType::FLOAT],
        LogicalType::FLOAT,
        binary_kernel::<f32, f32, f32>(next_after_f32),
    ));
    vec![
        (abs_functions(), &["abs"]),
        (double_binary_function("**", |base, exponent| Ok(base.powf(exponent))), &["pow", "power", "^"]),
        (factorial_set, &["factorial"]),
        (double_function("acos", acos), &[]),
        (double_function("asin", asin), &[]),
        (double_function("atan", |x| Ok(x.atan())), &[]),
        (double_binary_function("atan2", |y, x| Ok(y.atan2(x))), &[]),
        (bit_count_functions(), &[]),
        (double_function("cbrt", |x| Ok(x.cbrt())), &[]),
        (ceil_functions(), &["ceiling"]),
        (double_function("cos", |x| no_infinite(x, |x| Ok(x.cos()))), &[]),
        (double_function("cot", cot), &[]),
        (double_function("degrees", |x| Ok(x * (180.0 / PI))), &[]),
        (double_function("even", even), &[]),
        (double_function("exp", |x| Ok(x.exp())), &[]),
        (floor_functions(), &[]),
        (finiteness("isfinite", true), &[]),
        (finiteness("isinf", false), &[]),
        (float_predicate("isnan", f32::is_nan, f64::is_nan), &[]),
        (double_function("gamma", gamma), &[]),
        (
            integer_binary(
                "greatest_common_divisor",
                greatest_common_divisor::<i64>,
                greatest_common_divisor::<i128>,
            ),
            &["gcd"],
        ),
        (
            integer_binary(
                "least_common_multiple",
                least_common_multiple::<i64>,
                least_common_multiple::<i128>,
            ),
            &["lcm"],
        ),
        (double_function("lgamma", log_gamma), &[]),
        (
            double_function("ln", |x| {
                check_logarithm(x)?;
                Ok(x.ln())
            }),
            &[],
        ),
        (
            double_function("log2", |x| {
                check_logarithm(x)?;
                Ok(x.log2())
            }),
            &[],
        ),
        (double_function("log10", log10), &[]),
        (log_functions(), &[]),
        (next_after, &[]),
        (pi, &[]),
        (double_function("radians", |x| Ok(x * (PI / 180.0))), &[]),
        (round_functions(), &[]),
        (sign_functions(), &[]),
        (float_predicate("signbit", f32::is_sign_negative, f64::is_sign_negative), &[]),
        (double_function("sin", |x| no_infinite(x, |x| Ok(x.sin()))), &[]),
        (double_function("sqrt", sqrt), &[]),
        (double_function("tan", |x| no_infinite(x, |x| Ok(x.tan()))), &[]),
        (trunc_functions(), &[]),
        (double_function("cosh", |x| Ok(x.cosh())), &[]),
        (double_function("sinh", |x| Ok(x.sinh())), &[]),
        (double_function("tanh", |x| Ok(x.tanh())), &[]),
        (double_function("acosh", |x| Ok(x.acosh())), &[]),
        (double_function("asinh", |x| Ok(x.asinh())), &[]),
        (double_function("atanh", atanh), &[]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_round_wraps_like_duckdb() {
        assert_eq!(125i8.round_precision(-1), -126);
        assert_eq!((-125i8).round_precision(-1), 126);
        assert_eq!(i64::MAX.round_precision(-1), -9_223_372_036_854_775_800);
        assert_eq!(1234i32.round_precision(-20), 0);
        assert_eq!(1299u16.trunc_precision(-2), 1200);
    }

    #[test]
    fn decimal_rounding() {
        // 1.55 as DECIMAL(3,2) rounded to one digit.
        assert_eq!(Rounding::Round.apply(155i16, 10, 1), 16);
        assert_eq!(Rounding::Round.apply(-155i16, 10, 1), -16);
        assert_eq!(Rounding::Ceil.apply(-150i32, 100, 1), -1);
        assert_eq!(Rounding::Floor.apply(-1i64, 100, 1), -1);
        assert_eq!(Rounding::Trunc.apply(-1555i128, 100, 10), -150);
    }

    #[test]
    fn gcd_and_lcm() {
        assert_eq!(greatest_common_divisor(i64::MIN, -1).ok(), Some(1));
        assert!(greatest_common_divisor(i64::MIN, 0).is_err());
        assert_eq!(least_common_multiple(6i64, -4).ok(), Some(12));
    }

    #[test]
    fn misc() {
        assert_eq!(even(-2.5).ok(), Some(-4.0));
        assert_eq!(even(1.0).ok(), Some(2.0));
        assert_eq!(factorial(21).ok(), Some(51_090_942_171_709_440_000));
        assert!(factorial(34).is_err());
        assert_eq!(next_after_f64(1.0, 2.0).ok(), Some(1.0 + f64::EPSILON));
        assert_eq!(next_after_f32(0.0, -1.0).ok(), Some(-f32::from_bits(1)));
        assert_eq!(sign(f64::NAN).ok(), Some(0));
        assert_eq!(bit_count(-1i128).ok(), Some(-128));
    }
}
