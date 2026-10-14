//! Generic vectorised kernels (DuckDB's `UnaryExecutor` / `BinaryExecutor`).
//!
//! Each helper specialises on the input representations: constant inputs produce a
//! constant result, flat inputs without NULLs run a tight loop the compiler can
//! auto-vectorise, and everything else goes through the selection-aware generic path.

use rustdb_common::Result;

use crate::{NativeType, ValidityMask, Vector, VectorKind};

fn is_constant(vector: &Vector) -> bool {
    vector.kind() == VectorKind::Constant
}

/// Applies `f` to every non-NULL row of `input`, writing into `result`.
/// NULL rows produce NULL.
pub fn unary<I, O>(input: &Vector, count: usize, result: &mut Vector, mut f: impl FnMut(I) -> O)
where
    I: NativeType,
    O: NativeType,
{
    // An infallible closure lifted into the fallible path cannot fail.
    let _ = try_unary(input, count, result, |value| Ok(f(value)));
}

/// Like [`unary`], but `f` may fail; the first error aborts the kernel.
pub fn try_unary<I, O>(
    input: &Vector,
    count: usize,
    result: &mut Vector,
    mut f: impl FnMut(I) -> Result<O>,
) -> Result<()>
where
    I: NativeType,
    O: NativeType,
{
    if is_constant(input) {
        result.reset_flat(1);
        match input.unified::<I>().get(0) {
            Some(value) => result.values_mut::<O>()[0] = f(value)?,
            None => result.validity_mut().set_invalid(0, 1),
        }
        result.make_constant();
        return Ok(());
    }
    result.reset_flat(count);
    let input = input.unified::<I>();
    let out = result.values_mut::<O>();
    if input.validity.is_all_valid() {
        if let crate::Sel::Identity = input.sel {
            for (out, &value) in out[..count].iter_mut().zip(&input.values[..count]) {
                *out = f(value)?;
            }
        } else {
            for (row, out) in out[..count].iter_mut().enumerate() {
                *out = f(input.values[input.slot(row)])?;
            }
        }
        return Ok(());
    }
    let mut nulls = Vec::new();
    for (row, out) in out[..count].iter_mut().enumerate() {
        match input.get(row) {
            Some(value) => *out = f(value)?,
            None => nulls.push(row),
        }
    }
    let validity = result.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    Ok(())
}

/// Applies `f` to every row where both inputs are non-NULL.
pub fn binary<L, R, O>(
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
    mut f: impl FnMut(L, R) -> O,
) where
    L: NativeType,
    R: NativeType,
    O: NativeType,
{
    let _ = try_binary(left, right, count, result, |l, r| Ok(f(l, r)));
}

/// Like [`binary`], but `f` may fail; the first error aborts the kernel.
pub fn try_binary<L, R, O>(
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
    mut f: impl FnMut(L, R) -> Result<O>,
) -> Result<()>
where
    L: NativeType,
    R: NativeType,
    O: NativeType,
{
    if is_constant(left) && is_constant(right) {
        result.reset_flat(1);
        match (left.unified::<L>().get(0), right.unified::<R>().get(0)) {
            (Some(l), Some(r)) => result.values_mut::<O>()[0] = f(l, r)?,
            _ => result.validity_mut().set_invalid(0, 1),
        }
        result.make_constant();
        return Ok(());
    }
    result.reset_flat(count);
    let (lu, ru) = (left.unified::<L>(), right.unified::<R>());
    // A constant NULL on either side makes every row NULL.
    if (is_constant(left) && !lu.is_valid(0)) || (is_constant(right) && !ru.is_valid(0)) {
        *result.validity_mut() = ValidityMask::from_words(vec![0; count.div_ceil(64)]);
        return Ok(());
    }
    let out = result.values_mut::<O>();
    let all_valid = lu.validity.is_all_valid() && ru.validity.is_all_valid();
    if all_valid {
        match (lu.sel, ru.sel) {
            (crate::Sel::Identity, crate::Sel::Identity) => {
                for ((out, &l), &r) in out[..count]
                    .iter_mut()
                    .zip(&lu.values[..count])
                    .zip(&ru.values[..count])
                {
                    *out = f(l, r)?;
                }
            }
            (crate::Sel::Identity, crate::Sel::Constant) => {
                let r = ru.values[0];
                for (out, &l) in out[..count].iter_mut().zip(&lu.values[..count]) {
                    *out = f(l, r)?;
                }
            }
            (crate::Sel::Constant, crate::Sel::Identity) => {
                let l = lu.values[0];
                for (out, &r) in out[..count].iter_mut().zip(&ru.values[..count]) {
                    *out = f(l, r)?;
                }
            }
            _ => {
                for (row, out) in out[..count].iter_mut().enumerate() {
                    *out = f(lu.values[lu.slot(row)], ru.values[ru.slot(row)])?;
                }
            }
        }
        return Ok(());
    }
    let mut nulls = Vec::new();
    for (row, out) in out[..count].iter_mut().enumerate() {
        match (lu.get(row), ru.get(row)) {
            (Some(l), Some(r)) => *out = f(l, r)?,
            _ => nulls.push(row),
        }
    }
    let validity = result.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LogicalType, SelectionVector, Value};

    fn int_vector(values: &[Option<i32>]) -> Vector {
        let mut vector = Vector::flat(LogicalType::INTEGER, values.len());
        for (row, value) in values.iter().enumerate() {
            let value = value.map_or(Value::null(LogicalType::INTEGER), Value::integer);
            vector.set_value(row, &value).unwrap();
        }
        vector
    }

    #[test]
    fn binary_handles_every_representation() {
        let left = int_vector(&[Some(1), Some(2), None, Some(4)]);
        let right = Vector::constant(&Value::integer(10)).unwrap();
        let mut result = Vector::flat(LogicalType::INTEGER, 0);
        binary::<i32, i32, i32>(&left, &right, 4, &mut result, |l, r| l + r);
        let values: Vec<_> = (0..4).map(|row| result.get_value(row)).collect();
        assert_eq!(
            values,
            vec![
                Value::integer(11),
                Value::integer(12),
                Value::null(LogicalType::INTEGER),
                Value::integer(14)
            ]
        );

        let sliced = left.slice(&SelectionVector::new(vec![3, 0]));
        binary::<i32, i32, i32>(&sliced, &sliced, 2, &mut result, |l, r| l * r);
        assert_eq!(result.get_value(0), Value::integer(16));
        assert_eq!(result.get_value(1), Value::integer(1));

        let null = Vector::constant(&Value::null(LogicalType::INTEGER)).unwrap();
        binary::<i32, i32, i32>(&left, &null, 4, &mut result, |l, r| l + r);
        assert!((0..4).all(|row| result.is_null(row)));
    }

    #[test]
    fn constants_stay_constant() {
        let input = Vector::constant(&Value::integer(20)).unwrap();
        let mut result = Vector::flat(LogicalType::BIGINT, 0);
        unary::<i32, i64>(&input, 2048, &mut result, |v| i64::from(v) * 2);
        assert_eq!(result.kind(), VectorKind::Constant);
        assert_eq!(result.get_value(1000), Value::bigint(40));
    }
}
