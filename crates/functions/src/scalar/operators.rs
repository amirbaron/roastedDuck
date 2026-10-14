//! Arithmetic operators `+`, `-`, `*` (DuckDB's `src/function/scalar/operator`).

use std::fmt::Display;

use rustdb_common::Error;
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::kernels::{binary, try_binary};
use rustdb_types::{LogicalType, NativeType};

/// Integer types with checked arithmetic.
trait CheckedInt: NativeType + Display {
    fn add(self, other: Self) -> Option<Self>;
    fn sub(self, other: Self) -> Option<Self>;
    fn mul(self, other: Self) -> Option<Self>;
}

macro_rules! checked_int {
    ($($ty:ty),*) => {$(
        impl CheckedInt for $ty {
            fn add(self, other: Self) -> Option<Self> { self.checked_add(other) }
            fn sub(self, other: Self) -> Option<Self> { self.checked_sub(other) }
            fn mul(self, other: Self) -> Option<Self> { self.checked_mul(other) }
        }
    )*};
}
checked_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

#[derive(Clone, Copy)]
enum ArithOp {
    Add,
    Subtract,
    Multiply,
}

impl ArithOp {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
        }
    }

    const fn noun(self) -> &'static str {
        match self {
            Self::Add => "addition",
            Self::Subtract => "subtraction",
            Self::Multiply => "multiplication",
        }
    }
}

/// `Overflow in addition of INT32 (2147483647 + 1)!`
fn overflow<T: NativeType + Display>(op: ArithOp, left: T, right: T) -> Error {
    Error::out_of_range(format!(
        "Overflow in {} of {} ({left} {} {right})!",
        op.noun(),
        T::PHYSICAL.name(),
        op.symbol()
    ))
}

fn checked_function<T: CheckedInt>(op: ArithOp, ty: &LogicalType) -> ScalarFunction {
    let apply = move |left: T, right: T| {
        let result = match op {
            ArithOp::Add => left.add(right),
            ArithOp::Subtract => left.sub(right),
            ArithOp::Multiply => left.mul(right),
        };
        result.ok_or_else(|| overflow(op, left, right))
    };
    ScalarFunction::new(
        op.symbol(),
        vec![ty.clone(), ty.clone()],
        ty.clone(),
        move |args, _, result| {
            try_binary::<T, T, T>(args.column(0), args.column(1), args.len(), result, apply)
        },
    )
}

fn float_function<T>(op: ArithOp, ty: &LogicalType) -> ScalarFunction
where
    T: NativeType + std::ops::Add<Output = T> + std::ops::Sub<Output = T> + std::ops::Mul<Output = T>,
{
    let apply = move |left: T, right: T| match op {
        ArithOp::Add => left + right,
        ArithOp::Subtract => left - right,
        ArithOp::Multiply => left * right,
    };
    ScalarFunction::new(
        op.symbol(),
        vec![ty.clone(), ty.clone()],
        ty.clone(),
        move |args, _, result| {
            binary::<T, T, T>(args.column(0), args.column(1), args.len(), result, apply);
            Ok(())
        },
    )
}

fn arithmetic_set(op: ArithOp) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(op.symbol());
    set.add(checked_function::<i8>(op, &LogicalType::TINYINT));
    set.add(checked_function::<i16>(op, &LogicalType::SMALLINT));
    set.add(checked_function::<i32>(op, &LogicalType::INTEGER));
    set.add(checked_function::<i64>(op, &LogicalType::BIGINT));
    set.add(checked_function::<i128>(op, &LogicalType::HUGEINT));
    set.add(checked_function::<u8>(op, &LogicalType::UTINYINT));
    set.add(checked_function::<u16>(op, &LogicalType::USMALLINT));
    set.add(checked_function::<u32>(op, &LogicalType::UINTEGER));
    set.add(checked_function::<u64>(op, &LogicalType::UBIGINT));
    set.add(checked_function::<u128>(op, &LogicalType::UHUGEINT));
    set.add(float_function::<f32>(op, &LogicalType::FLOAT));
    set.add(float_function::<f64>(op, &LogicalType::DOUBLE));
    set
}

/// The `+`, `-` and `*` function sets.
pub fn arithmetic_functions() -> Vec<ScalarFunctionSet> {
    [ArithOp::Add, ArithOp::Subtract, ArithOp::Multiply]
        .into_iter()
        .map(arithmetic_set)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_common::Result;
    use rustdb_function::FunctionState;
    use rustdb_types::{DataChunk, Value, Vector};

    fn call(function: &ScalarFunction, left: &Value, right: &Value) -> Result<Value> {
        let args = DataChunk::new(vec![Vector::constant(left)?, Vector::constant(right)?], 1);
        let mut result = Vector::flat(function.return_type.clone(), 0);
        (function.kernel)(&args, &mut FunctionState::default(), &mut result)?;
        Ok(result.get_value(0))
    }

    #[test]
    fn integer_overflow_matches_duckdb() {
        let add = checked_function::<i32>(ArithOp::Add, &LogicalType::INTEGER);
        assert_eq!(
            call(&add, &Value::integer(1), &Value::integer(2)).unwrap(),
            Value::integer(3)
        );
        let error = call(&add, &Value::integer(i32::MAX), &Value::integer(1)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Out of Range Error: Overflow in addition of INT32 (2147483647 + 1)!"
        );
        let mul = checked_function::<i8>(ArithOp::Multiply, &LogicalType::TINYINT);
        let error = call(&mul, &Value::tinyint(-128), &Value::tinyint(-1)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Out of Range Error: Overflow in multiplication of INT8 (-128 * -1)!"
        );
    }
}
