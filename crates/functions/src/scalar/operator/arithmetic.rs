//! The arithmetic operators `+`, `-`, `*`, `/`, `//` and `%` (DuckDB's `arithmetic.cpp`,
//! `add.cpp`, `subtract.cpp` and `multiply.cpp`), including DECIMAL arithmetic with
//! bind-time result widths and the date/time/interval combinations.

use std::fmt::Display;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::kernels::try_binary;
use rustdb_types::{
    DataChunk, Date, Interval, LogicalType, LogicalTypeId, NativeType, PhysicalType, Time, TimeTz, Timestamp,
    Vector,
};

use crate::operator::{
    CheckedInt, checked_add, checked_multiply, checked_subtract, interval_add, interval_subtract,
};
use crate::scalar::executor::{Str, StrOut, binary, unary};
use crate::signature::numeric_types;
use crate::temporal;

/// A binary scalar kernel.
type Kernel = Arc<dyn Fn(&DataChunk, &mut Vector) -> Result<()> + Send + Sync>;

/// Wraps a kernel into a function.
fn function(
    name: &str,
    arguments: Vec<LogicalType>,
    return_type: LogicalType,
    kernel: impl Fn(&DataChunk, &mut Vector) -> Result<()> + Send + Sync + 'static,
) -> ScalarFunction {
    ScalarFunction::new(name, arguments, return_type, move |args, _, result| {
        kernel(args, result)
    })
}

/// Replaces a function's kernel (DuckDB's `SetFunctionCallback` in bind callbacks).
fn set_kernel(target: &mut ScalarFunction, kernel: Kernel) {
    target.kernel = Arc::new(move |args, _, result| kernel(args, result));
}

/// `f(left, right)` over two columns of `T` with a fallible operation. Generic over the
/// operation (not a function pointer) so that it is inlined into the row loop.
fn binary_kernel<T: NativeType>(f: impl Fn(T, T) -> Result<T> + Copy + Send + Sync + 'static) -> Kernel {
    Arc::new(move |args: &DataChunk, result: &mut Vector| {
        try_binary::<T, T, T>(args.column(0), args.column(1), args.len(), result, f)
    })
}

/// Dispatches `$body` with `$T` bound to the integer type of `$physical`; other types get a
/// kernel that fails (DuckDB throws when building such a function).
macro_rules! with_int {
    ($physical:expr, $T:ident => $body:expr) => {
        crate::dispatch::with_integer_type!($physical, $T => $body, _ => unsupported_kernel($physical))
    };
}

/// A kernel for a physical type the operator does not support.
fn unsupported_kernel(physical: PhysicalType) -> Kernel {
    Arc::new(move |_: &DataChunk, _: &mut Vector| {
        Err(Error::not_implemented(format!(
            "Unimplemented type for GetScalarBinaryFunction: {}",
            physical.name()
        )))
    })
}

// ---------------------------------------------------------------------------------------
// DECIMAL arithmetic
// ---------------------------------------------------------------------------------------

/// The unscaled storage of a DECIMAL.
trait DecimalStorage: CheckedInt + Into<i128> {
    /// `10^width - 1` for the widest DECIMAL stored in this type.
    const MAX: Self;
}

macro_rules! decimal_storage {
    ($($ty:ty => $max:expr),*) => {$(
        impl DecimalStorage for $ty {
            const MAX: Self = $max;
        }
    )*};
}
decimal_storage!(i16 => 9999, i32 => 999_999_999, i64 => 999_999_999_999_999_999, i128 => 99_999_999_999_999_999_999_999_999_999_999_999_999);

/// The DECIMAL overflow error (DuckDB's `Decimal*OverflowCheck`).
fn decimal_overflow<T: DecimalStorage + Display>(
    verb: &str,
    op: &str,
    left: T,
    right: T,
    hint: &str,
) -> Error {
    if T::PHYSICAL == PhysicalType::Int128 {
        if verb == "multiplication" {
            Error::out_of_range(format!(
                "Overflow in multiplication of DECIMAL(38) ({left} * {right}). You might want to add an explicit \
                 cast to a decimal with a smaller scale."
            ))
        } else {
            Error::out_of_range(format!(
                "Overflow in {verb} of DECIMAL(38) ({left} {op} {right});"
            ))
        }
    } else {
        Error::out_of_range(format!(
            "Overflow in {verb} of DECIMAL(18) ({left} {op} {right}). You might want to add an explicit cast to a \
             {hint}."
        ))
    }
}

/// Whether `value` lies within the DECIMAL range of its storage type.
fn in_decimal_range<T: DecimalStorage>(value: T) -> bool {
    let value: i128 = value.into();
    let max: i128 = T::MAX.into();
    (-max..=max).contains(&value)
}

fn decimal_add_checked<T: DecimalStorage>(left: T, right: T) -> Result<T> {
    left.checked_add_(right)
        .filter(|&value| in_decimal_range(value))
        .ok_or_else(|| decimal_overflow("addition", "+", left, right, "bigger decimal"))
}

fn decimal_subtract_checked<T: DecimalStorage>(left: T, right: T) -> Result<T> {
    left.checked_sub_(right)
        .filter(|&value| in_decimal_range(value))
        .ok_or_else(|| decimal_overflow("subtract", "-", left, right, "bigger decimal"))
}

fn decimal_multiply_checked<T: DecimalStorage>(left: T, right: T) -> Result<T> {
    left.checked_mul_(right)
        .filter(|&value| in_decimal_range(value))
        .ok_or_else(|| decimal_overflow("multiplication", "*", left, right, "bigger decimal"))
}

/// Bind data of DECIMAL arithmetic.
type DecimalArithmetic = rustdb_function::DecimalArithmeticBindData;

/// DuckDB's `SerializeDecimalArithmetic` (of `+`, `-` and `*`): the overflow check and
/// the bound types.
fn serialize_decimal_arithmetic(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    function: &ScalarFunction,
) -> Result<()> {
    let check_overflow = bind_data
        .and_then(|data| data.as_any().downcast_ref::<DecimalArithmetic>())
        .is_some_and(|data| data.check_overflow);
    serializer.write_property(100, "check_overflow", &check_overflow)?;
    serializer.write_property(101, "return_type", &function.return_type)?;
    serializer.write_property(102, "arguments", &function.arguments)
}

/// The DECIMAL `(width, scale)` of an argument type.
fn decimal_properties(ty: &LogicalType) -> Result<(u8, u8)> {
    if ty.id() == LogicalTypeId::SqlNull {
        return Ok((0, 0));
    }
    ty.numeric_decimal_properties()
        .ok_or_else(|| Error::internal(format!("Could not convert type {ty} to a decimal.")))
}

/// DuckDB's `BindDecimalArithmetic`: the result width and scale of `+`, `-` and `%`.
fn bind_decimal_arithmetic(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
    is_modulo: bool,
) -> Result<DecimalArithmetic> {
    let mut check_overflow = false;
    let (mut max_width, mut max_scale, mut max_width_over_scale) = (0u8, 0u8, 0u8);
    for ty in input.argument_types {
        if ty.id() == LogicalTypeId::Unknown {
            continue;
        }
        let (width, scale) = decimal_properties(ty)?;
        max_width = max_width.max(width);
        max_scale = max_scale.max(scale);
        max_width_over_scale = max_width_over_scale.max(width - scale);
    }
    let mut required_width = (max_scale + max_width_over_scale).max(max_width);
    if !is_modulo {
        required_width += 1;
        if required_width > 18 && max_width <= 18 {
            check_overflow = true;
            required_width = 18;
        }
    }
    if required_width > LogicalType::MAX_DECIMAL_WIDTH {
        check_overflow = true;
        required_width = LogicalType::MAX_DECIMAL_WIDTH;
    }
    let result_type = LogicalType::decimal(required_width, max_scale);
    for (index, ty) in input.argument_types.iter().enumerate() {
        let scale = ty.numeric_decimal_properties().map_or(0, |(_, scale)| scale);
        function.arguments[index] = if scale == max_scale && ty.physical_type() == result_type.physical_type()
        {
            ty.clone()
        } else {
            result_type.clone()
        };
    }
    function.return_type = result_type;
    Ok(DecimalArithmetic { check_overflow })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DecimalOp {
    Add,
    Subtract,
}

/// DuckDB's `BindDecimalAddSubtract`.
fn bind_decimal_add_subtract(
    op: DecimalOp,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync {
    move |function, input| {
        let data = bind_decimal_arithmetic(function, input, false)?;
        macro_rules! kernel {
            ($T:ty) => {
                match (op, data.check_overflow) {
                    (DecimalOp::Add, true) => binary_kernel::<$T>(decimal_add_checked::<$T>),
                    (DecimalOp::Add, false) => binary_kernel::<$T>(|l, r| Ok(l.wrapping_add(r))),
                    (DecimalOp::Subtract, true) => binary_kernel::<$T>(decimal_subtract_checked::<$T>),
                    (DecimalOp::Subtract, false) => binary_kernel::<$T>(|l, r| Ok(l.wrapping_sub(r))),
                }
            };
        }
        let kernel = match function.return_type.physical_type() {
            PhysicalType::Int16 => kernel!(i16),
            PhysicalType::Int32 => kernel!(i32),
            PhysicalType::Int64 => kernel!(i64),
            _ => kernel!(i128),
        };
        set_kernel(function, kernel);
        Ok(Some(Arc::new(data)))
    }
}

/// DuckDB's `BindDecimalMultiply`.
fn bind_decimal_multiply(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let mut check_overflow = false;
    let (mut result_width, mut result_scale, mut max_width) = (0u8, 0u8, 0u8);
    for ty in input.argument_types {
        if ty.id() == LogicalTypeId::Unknown {
            continue;
        }
        let (width, scale) = decimal_properties(ty)
            .map_err(|_| Error::internal(format!("Could not convert type {ty} to a decimal?")))?;
        max_width = max_width.max(width);
        result_width = result_width.saturating_add(width);
        result_scale = result_scale.saturating_add(scale);
    }
    if result_scale > LogicalType::MAX_DECIMAL_WIDTH {
        return Err(Error::out_of_range(format!(
            "Needed scale {result_scale} to accurately represent the multiplication result, but this is out of \
             range of the DECIMAL type. Max scale is 38; could not perform an accurate multiplication. Either add a \
             cast to DOUBLE, or add an explicit cast to a decimal with a lower scale."
        )));
    }
    if result_width > 18 && max_width <= 18 && result_scale < 18 {
        check_overflow = true;
        result_width = 18;
    }
    if result_width > LogicalType::MAX_DECIMAL_WIDTH {
        check_overflow = true;
        result_width = LogicalType::MAX_DECIMAL_WIDTH;
    }
    let result_type = LogicalType::decimal(result_width, result_scale);
    for (index, ty) in input.argument_types.iter().enumerate() {
        function.arguments[index] = if ty.physical_type() == result_type.physical_type() {
            ty.clone()
        } else {
            let scale = ty.numeric_decimal_properties().map_or(0, |(_, scale)| scale);
            LogicalType::decimal(result_width, scale)
        };
    }
    macro_rules! kernel {
        ($T:ty) => {
            if check_overflow {
                binary_kernel::<$T>(decimal_multiply_checked::<$T>)
            } else {
                binary_kernel::<$T>(|l, r| Ok(l.wrapping_mul(r)))
            }
        };
    }
    let kernel = match result_type.physical_type() {
        PhysicalType::Int16 => kernel!(i16),
        PhysicalType::Int32 => kernel!(i32),
        PhysicalType::Int64 => kernel!(i64),
        _ => kernel!(i128),
    };
    function.return_type = result_type;
    set_kernel(function, kernel);
    Ok(Some(Arc::new(DecimalArithmetic { check_overflow })))
}

// ---------------------------------------------------------------------------------------
// +, -, *
// ---------------------------------------------------------------------------------------

/// Unary `+`: returns its argument (DuckDB's `NopFunction`).
fn unary_plus(ty: &LogicalType) -> ScalarFunction {
    let mut function = function("+", vec![ty.clone()], ty.clone(), |args, result| {
        *result = args.column(0).clone();
        Ok(())
    });
    if ty.id() == LogicalTypeId::Decimal {
        function = function.with_bind(|function, input| {
            function.arguments[0] = input.argument_types[0].clone();
            function.return_type = input.argument_types[0].clone();
            Ok(None)
        });
    }
    function
}

/// `-x` of an integer (DuckDB's `NegateOperator`).
fn negate_int<T: CheckedInt>(value: T) -> Result<T> {
    value
        .negate_()
        .ok_or_else(|| Error::out_of_range("Overflow in negation of numeric value!"))
}

fn negate_kernel(physical: PhysicalType) -> Kernel {
    match physical {
        PhysicalType::Float => Arc::new(|args: &DataChunk, result: &mut Vector| {
            unary::<f32, f32>(args, result, |v, out| {
                *out = -v;
                Ok(true)
            })
        }),
        PhysicalType::Double => Arc::new(|args: &DataChunk, result: &mut Vector| {
            unary::<f64, f64>(args, result, |v, out| {
                *out = -v;
                Ok(true)
            })
        }),
        physical => with_int!(physical, T => Arc::new(|args: &DataChunk, result: &mut Vector| {
            unary::<T, T>(args, result, |v, out| {
                *out = negate_int(v)?;
                Ok(true)
            })
        }) as Kernel),
    }
}

/// Unary `-` (DuckDB's `SubtractFunction::GetFunction(type)`).
fn unary_minus(ty: &LogicalType) -> ScalarFunction {
    if ty.id() == LogicalTypeId::Interval {
        return function("-", vec![ty.clone()], ty.clone(), |args, result| {
            unary::<Interval, Interval>(args, result, |v, out| {
                *out = temporal::interval_negate(v)?;
                Ok(true)
            })
        });
    }
    if ty.id() == LogicalTypeId::Decimal {
        return function("-", vec![ty.clone()], ty.clone(), |_, _| Err(unbound())).with_bind(
            |function, input| {
                let ty = input.argument_types[0].clone();
                set_kernel(function, negate_kernel(ty.physical_type()));
                function.arguments[0] = ty.clone();
                function.return_type = ty;
                Ok(None)
            },
        );
    }
    let kernel = negate_kernel(ty.physical_type());
    function("-", vec![ty.clone()], ty.clone(), move |args, result| {
        kernel(args, result)
    })
}

fn unbound() -> Error {
    Error::internal("function used without being bound")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arith {
    Add,
    Subtract,
    Multiply,
}

impl Arith {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
        }
    }
}

/// A DECIMAL bind callback.
type DecimalBind =
    dyn Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync;

/// `left op right` for two values of the same numeric type.
fn numeric_binary(op: Arith, ty: &LogicalType) -> ScalarFunction {
    let name = op.symbol();
    let args = vec![ty.clone(), ty.clone()];
    if ty.id() == LogicalTypeId::Decimal {
        let bind: Box<DecimalBind> = match op {
            Arith::Add => Box::new(bind_decimal_add_subtract(DecimalOp::Add)),
            Arith::Subtract => Box::new(bind_decimal_add_subtract(DecimalOp::Subtract)),
            Arith::Multiply => Box::new(bind_decimal_multiply),
        };
        let mut function =
            function(name, args, ty.clone(), |_, _| Err(unbound())).with_bind(move |f, i| bind(f, i));
        function.serialize = Some(Arc::new(serialize_decimal_arithmetic));
        return function;
    }
    let kernel: Kernel = match ty.physical_type() {
        PhysicalType::Float => float_kernel::<f32>(op),
        PhysicalType::Double => float_kernel::<f64>(op),
        physical => with_int!(physical, T => match op {
            Arith::Add => binary_kernel::<T>(checked_add::<T>),
            Arith::Subtract => binary_kernel::<T>(checked_subtract::<T>),
            Arith::Multiply => binary_kernel::<T>(checked_multiply::<T>),
        }),
    };
    function(name, args, ty.clone(), move |args, result| kernel(args, result))
}

fn float_kernel<T>(op: Arith) -> Kernel
where
    T: NativeType + std::ops::Add<Output = T> + std::ops::Sub<Output = T> + std::ops::Mul<Output = T>,
{
    match op {
        Arith::Add => binary_kernel::<T>(|l, r| Ok(l + r)),
        Arith::Subtract => binary_kernel::<T>(|l, r| Ok(l - r)),
        Arith::Multiply => binary_kernel::<T>(|l, r| Ok(l * r)),
    }
}

/// A binary function over typed columns with a fallible per-row operation.
fn typed<A: NativeType, B: NativeType, O: NativeType>(
    name: &str,
    left: LogicalType,
    right: LogicalType,
    output: LogicalType,
    op: impl Fn(A, B) -> Result<O> + Copy + Send + Sync + 'static,
) -> ScalarFunction {
    function(name, vec![left, right], output, move |args, result| {
        binary::<A, B, O>(args, result, |a, b, out| {
            *out = op(a, b)?;
            Ok(true)
        })
    })
}

/// The `+` set (DuckDB's `OperatorAddFun`).
pub fn add_functions() -> ScalarFunctionSet {
    use LogicalType as T;
    let mut set = ScalarFunctionSet::new("+");
    for ty in numeric_types() {
        set.add(unary_plus(&ty));
        set.add(numeric_binary(Arith::Add, &ty));
    }
    set.add(typed("+", T::DATE, T::INTEGER, T::DATE, |d: Date, n: i32| {
        temporal::date_add_days(d, n)
    }));
    set.add(typed("+", T::INTEGER, T::DATE, T::DATE, |n: i32, d: Date| {
        temporal::date_add_days(d, n)
    }));
    set.add(typed("+", T::INTERVAL, T::INTERVAL, T::INTERVAL, interval_add));
    set.add(typed(
        "+",
        T::DATE,
        T::INTERVAL,
        T::TIMESTAMP,
        temporal::date_add_interval_timestamp,
    ));
    set.add(typed(
        "+",
        T::INTERVAL,
        T::DATE,
        T::TIMESTAMP,
        |i: Interval, d: Date| temporal::date_add_interval_timestamp(d, i),
    ));
    set.add(typed(
        "+",
        T::TIME,
        T::INTERVAL,
        T::TIME,
        |t: Time, i: Interval| Ok(temporal::time_add_interval(t, i).0),
    ));
    set.add(typed(
        "+",
        T::INTERVAL,
        T::TIME,
        T::TIME,
        |i: Interval, t: Time| Ok(temporal::time_add_interval(t, i).0),
    ));
    set.add(typed(
        "+",
        T::TIMESTAMP,
        T::INTERVAL,
        T::TIMESTAMP,
        temporal::timestamp_add_interval,
    ));
    set.add(typed(
        "+",
        T::INTERVAL,
        T::TIMESTAMP,
        T::TIMESTAMP,
        |i: Interval, t: Timestamp| temporal::timestamp_add_interval(t, i),
    ));
    set.add(typed(
        "+",
        T::TIME_TZ,
        T::INTERVAL,
        T::TIME_TZ,
        |t: TimeTz, i: Interval| {
            Ok(TimeTz::new(
                temporal::time_add_interval(t.time(), i).0,
                t.offset(),
            ))
        },
    ));
    set.add(typed(
        "+",
        T::INTERVAL,
        T::TIME_TZ,
        T::TIME_TZ,
        |i: Interval, t: TimeTz| {
            Ok(TimeTz::new(
                temporal::time_add_interval(t.time(), i).0,
                t.offset(),
            ))
        },
    ));
    set.add(typed("+", T::TIME, T::DATE, T::TIMESTAMP, |t: Time, d: Date| {
        temporal::date_add_time(d, t)
    }));
    set.add(typed(
        "+",
        T::DATE,
        T::TIME,
        T::TIMESTAMP,
        temporal::date_add_time,
    ));
    set.add(typed(
        "+",
        T::TIME_TZ,
        T::DATE,
        T::TIMESTAMP_TZ,
        |t: TimeTz, d: Date| temporal::date_add_time_tz(d, t),
    ));
    set.add(typed(
        "+",
        T::DATE,
        T::TIME_TZ,
        T::TIMESTAMP_TZ,
        temporal::date_add_time_tz,
    ));
    // Lists concatenate (DuckDB adds `ListConcatFun` to the set).
    for mut function in crate::scalar::string::concat::list_concat_functions().functions {
        function.name = Arc::from("+");
        set.add(function);
    }
    set.add(bignum_binary("+", false));
    set
}

/// BIGNUM `+`/`-` (DuckDB's `BignumAdd` / `BignumSubtract`).
fn bignum_binary(name: &str, subtract: bool) -> ScalarFunction {
    use rustdb_function::bignum::Intermediate;
    let mut function = ScalarFunction::new(
        name,
        vec![LogicalType::BIGNUM, LogicalType::BIGNUM],
        LogicalType::BIGNUM,
        move |args, _, result| {
            binary::<Str, Str, StrOut>(args, result, |left, right, out| {
                let left = Intermediate::from_bignum(left);
                let mut right = Intermediate::from_bignum(right);
                if subtract {
                    right.negate_in_place();
                }
                out.extend_from_slice(&Intermediate::add(&left, &right)?);
                Ok(true)
            })
        },
    );
    function.null_handling = rustdb_function::NullHandling::Default;
    function
}

/// BIGNUM negation (DuckDB's `BignumNegate`).
fn bignum_negate() -> ScalarFunction {
    use rustdb_function::bignum::Intermediate;
    ScalarFunction::new(
        "-",
        vec![LogicalType::BIGNUM],
        LogicalType::BIGNUM,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |value, out| {
                out.extend_from_slice(&Intermediate::from_bignum(value).negate());
                Ok(true)
            })
        },
    )
}

/// The `-` set (DuckDB's `OperatorSubtractFun`).
pub fn subtract_functions() -> ScalarFunctionSet {
    use LogicalType as T;
    let mut set = ScalarFunctionSet::new("-");
    for ty in numeric_types() {
        set.add(unary_minus(&ty));
        set.add(numeric_binary(Arith::Subtract, &ty));
    }
    set.add(bignum_negate());
    set.add(bignum_binary("-", true));
    set.add(typed("-", T::DATE, T::DATE, T::BIGINT, |l: Date, r: Date| {
        Ok(i64::from(l.0) - i64::from(r.0))
    }));
    set.add(typed(
        "-",
        T::DATE,
        T::INTEGER,
        T::DATE,
        temporal::date_subtract_days,
    ));
    set.add(typed(
        "-",
        T::TIMESTAMP,
        T::TIMESTAMP,
        T::INTERVAL,
        temporal::timestamp_difference,
    ));
    set.add(typed(
        "-",
        T::INTERVAL,
        T::INTERVAL,
        T::INTERVAL,
        interval_subtract,
    ));
    set.add(typed(
        "-",
        T::DATE,
        T::INTERVAL,
        T::TIMESTAMP,
        |d: Date, i: Interval| temporal::date_add_interval_timestamp(d, temporal::interval_invert(i)),
    ));
    set.add(typed(
        "-",
        T::TIME,
        T::INTERVAL,
        T::TIME,
        |t: Time, mut i: Interval| {
            i.micros = i.micros.wrapping_neg();
            Ok(temporal::time_add_interval(t, i).0)
        },
    ));
    set.add(typed(
        "-",
        T::TIMESTAMP,
        T::INTERVAL,
        T::TIMESTAMP,
        |t: Timestamp, i: Interval| temporal::timestamp_add_interval(t, temporal::interval_invert(i)),
    ));
    set.add(typed(
        "-",
        T::TIME_TZ,
        T::INTERVAL,
        T::TIME_TZ,
        |t: TimeTz, mut i: Interval| {
            i.micros = i.micros.wrapping_neg();
            Ok(TimeTz::new(
                temporal::time_add_interval(t.time(), i).0,
                t.offset(),
            ))
        },
    ));
    set.add(unary_minus(&T::INTERVAL));
    set
}

/// The `*` set (DuckDB's `OperatorMultiplyFun`).
pub fn multiply_functions() -> ScalarFunctionSet {
    use LogicalType as T;
    let mut set = ScalarFunctionSet::new("*");
    for ty in numeric_types() {
        set.add(numeric_binary(Arith::Multiply, &ty));
    }
    set.add(typed(
        "*",
        T::INTERVAL,
        T::DOUBLE,
        T::INTERVAL,
        temporal::interval_multiply_double_checked,
    ));
    set.add(typed(
        "*",
        T::DOUBLE,
        T::INTERVAL,
        T::INTERVAL,
        |d: f64, i: Interval| temporal::interval_multiply_double_checked(i, d),
    ));
    set.add(typed(
        "*",
        T::BIGINT,
        T::INTERVAL,
        T::INTERVAL,
        |n: i64, i: Interval| temporal::interval_multiply_bigint(i, n),
    ));
    set.add(typed(
        "*",
        T::INTERVAL,
        T::BIGINT,
        T::INTERVAL,
        temporal::interval_multiply_bigint,
    ));
    set
}

// ---------------------------------------------------------------------------------------
// /, //, %
// ---------------------------------------------------------------------------------------

/// An integer type for `//` and `%`.
trait IntegerDivide: CheckedInt {
    const SIGNED_MIN: Option<Self>;
    fn minus_one() -> Option<Self>;
    fn div(self, other: Self) -> Self;
    fn rem(self, other: Self) -> Self;
}

macro_rules! integer_divide {
    (signed: $($ty:ty),*) => {$(
        impl IntegerDivide for $ty {
            const SIGNED_MIN: Option<Self> = Some(<$ty>::MIN);
            fn minus_one() -> Option<Self> { Some(-1) }
            fn div(self, other: Self) -> Self { self.wrapping_div(other) }
            fn rem(self, other: Self) -> Self { self.wrapping_rem(other) }
        }
    )*};
    (unsigned: $($ty:ty),*) => {$(
        impl IntegerDivide for $ty {
            const SIGNED_MIN: Option<Self> = None;
            fn minus_one() -> Option<Self> { None }
            fn div(self, other: Self) -> Self { self / other }
            fn rem(self, other: Self) -> Self { self % other }
        }
    )*};
}
integer_divide!(signed: i8, i16, i32, i64, i128);
integer_divide!(unsigned: u8, u16, u32, u64, u128);

/// `left / right` or `left % right` of integers: division by zero is NULL, `MIN / -1`
/// overflows (DuckDB's `BinaryNumericDivideWrapper`; unsigned types use
/// `BinaryZeroIsNullWrapper`).
fn integer_division<T: IntegerDivide>(remainder: bool) -> Kernel {
    Arc::new(move |args: &DataChunk, result: &mut Vector| {
        binary::<T, T, T>(args, result, |left, right, out| {
            if Some(left) == T::SIGNED_MIN && Some(right) == T::minus_one() {
                return Err(Error::out_of_range(format!(
                    "Overflow in division of {left} / {right}"
                )));
            }
            if right == T::ZERO {
                return Ok(false);
            }
            *out = if remainder {
                left.rem(right)
            } else {
                left.div(right)
            };
            Ok(true)
        })
    })
}

/// `/` or `%` of floating point values; with `ieee` division by zero follows IEEE 754,
/// otherwise it is NULL (DuckDB's `BindBinaryFloatingPoint`; `ieee_floating_point_ops`
/// defaults to true).
fn float_division<T>(remainder: bool) -> Kernel
where
    T: NativeType + PartialEq + std::ops::Div<Output = T> + std::ops::Rem<Output = T>,
{
    // `%` on floats is C's `fmod`, which Rust's `%` matches exactly.
    if remainder {
        binary_kernel::<T>(|l, r| Ok(l % r))
    } else {
        binary_kernel::<T>(|l, r| Ok(l / r))
    }
}

/// DuckDB's `BindBinaryFloatingPoint`: without `ieee_floating_point_ops`, division and
/// modulo by zero give NULL.
fn bind_float_division(
    remainder: bool,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync {
    move |function, input| {
        let ieee = match input.client.settings() {
            Some(settings) => settings
                .current_setting("ieee_floating_point_ops")?
                .as_bool()
                .unwrap_or(true),
            None => true,
        };
        if !ieee {
            let kernel = match function.return_type.physical_type() {
                PhysicalType::Float => zero_is_null_float::<f32>(remainder),
                _ => zero_is_null_float::<f64>(remainder),
            };
            set_kernel(function, kernel);
        }
        Ok(None)
    }
}

fn division_kernel(physical: PhysicalType, remainder: bool) -> Kernel {
    match physical {
        PhysicalType::Float => float_division::<f32>(remainder),
        PhysicalType::Double => float_division::<f64>(remainder),
        physical => with_int!(physical, T => integer_division::<T>(remainder)),
    }
}

/// Integer division by zero is NULL even for floats in `//` (DuckDB's
/// `GetBinaryFunctionIgnoreZero`).
fn zero_is_null_float<T>(remainder: bool) -> Kernel
where
    T: NativeType + PartialEq + Default + std::ops::Div<Output = T> + std::ops::Rem<Output = T>,
{
    Arc::new(move |args: &DataChunk, result: &mut Vector| {
        binary::<T, T, T>(args, result, |left, right, out| {
            if right == T::default() {
                return Ok(false);
            }
            *out = if remainder { left % right } else { left / right };
            Ok(true)
        })
    })
}

/// The `/` set (DuckDB's `OperatorFloatDivideFun`).
pub fn divide_functions() -> ScalarFunctionSet {
    use LogicalType as T;
    let mut set = ScalarFunctionSet::new("/");
    for ty in [T::FLOAT, T::DOUBLE] {
        let kernel = division_kernel(ty.physical_type(), false);
        set.add(
            function("/", vec![ty.clone(), ty.clone()], ty, move |args, result| {
                kernel(args, result)
            })
            .with_bind(bind_float_division(false)),
        );
    }
    set.add(function(
        "/",
        vec![T::INTERVAL, T::DOUBLE],
        T::INTERVAL,
        |args, result| {
            binary::<Interval, f64, Interval>(args, result, |interval, divisor, out| {
                if divisor == 0.0 {
                    return Ok(false);
                }
                *out = temporal::interval_multiply_double(interval, 1.0 / divisor)
                    .ok_or_else(|| Error::out_of_range("Overflow in INTERVAL division"))?;
                Ok(true)
            })
        },
    ));
    set
}

/// The `//` set (DuckDB's `OperatorIntegerDivideFun`).
pub fn integer_divide_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("//");
    for ty in numeric_types() {
        let kernel = match ty.id() {
            LogicalTypeId::Decimal => continue,
            LogicalTypeId::Float => zero_is_null_float::<f32>(false),
            LogicalTypeId::Double => zero_is_null_float::<f64>(false),
            _ => division_kernel(ty.physical_type(), false),
        };
        set.add(function(
            "//",
            vec![ty.clone(), ty.clone()],
            ty,
            move |args, result| kernel(args, result),
        ));
    }
    set
}

/// DuckDB's `BindDecimalModulo`: DECIMAL modulo, through DOUBLE when it could overflow.
fn bind_decimal_modulo(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let data = bind_decimal_arithmetic(function, input, true)?;
    if data.check_overflow {
        function.arguments.fill(LogicalType::DOUBLE);
        function.return_type = LogicalType::DOUBLE;
        set_kernel(function, zero_is_null_float::<f64>(true));
    } else {
        let kernel = division_kernel(function.return_type.physical_type(), true);
        set_kernel(function, kernel);
    }
    Ok(Some(Arc::new(data)))
}

/// The `%` set (DuckDB's `OperatorModuloFun`).
pub fn modulo_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("%");
    for ty in numeric_types() {
        let args = vec![ty.clone(), ty.clone()];
        if ty.id() == LogicalTypeId::Decimal {
            set.add(function("%", args, ty, |_, _| Err(unbound())).with_bind(bind_decimal_modulo));
            continue;
        }
        let float = matches!(ty.id(), LogicalTypeId::Float | LogicalTypeId::Double);
        let kernel = division_kernel(ty.physical_type(), true);
        let modulo = function("%", args, ty, move |args, result| kernel(args, result));
        set.add(if float {
            modulo.with_bind(bind_float_division(true))
        } else {
            modulo
        });
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_function::FunctionState;
    use rustdb_types::Value;

    fn call(function: &ScalarFunction, left: &Value, right: &Value) -> Result<Value> {
        let args = DataChunk::new(vec![Vector::constant(left)?, Vector::constant(right)?], 1);
        let mut result = Vector::flat(function.return_type.clone(), 0);
        (function.kernel)(&args, &mut FunctionState::default(), &mut result)?;
        Ok(result.get_value(0))
    }

    #[test]
    fn integer_overflow_matches_duckdb() {
        let add = numeric_binary(Arith::Add, &LogicalType::INTEGER);
        assert_eq!(
            call(&add, &Value::integer(1), &Value::integer(2)).unwrap(),
            Value::integer(3)
        );
        let error = call(&add, &Value::integer(i32::MAX), &Value::integer(1)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Out of Range Error: Overflow in addition of INT32 (2147483647 + 1)!"
        );
        let mul = numeric_binary(Arith::Multiply, &LogicalType::TINYINT);
        let error = call(&mul, &Value::tinyint(-128), &Value::tinyint(-1)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Out of Range Error: Overflow in multiplication of INT8 (-128 * -1)!"
        );
    }

    #[test]
    fn division_by_zero_is_null() {
        let kernel = division_kernel(PhysicalType::Int32, false);
        let args = DataChunk::new(
            vec![
                Vector::constant(&Value::integer(7)).unwrap(),
                Vector::constant(&Value::integer(0)).unwrap(),
            ],
            1,
        );
        let mut result = Vector::flat(LogicalType::INTEGER, 0);
        kernel(&args, &mut result).unwrap();
        assert!(result.get_value(0).is_null());
    }
}
