//! Generic vectorised kernels (DuckDB's `UnaryExecutor` / `BinaryExecutor`).
//!
//! Each helper specialises on the input representations: constant inputs produce a
//! constant result, flat inputs without NULLs run a tight loop the compiler can
//! auto-vectorise, and everything else goes through the selection-aware generic path.

use std::convert::Infallible;

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
    // `Infallible` errors compile to nothing, so the loops stay free of early exits and
    // can be vectorised.
    let Ok(()) = unary_impl::<I, O, Infallible>(input, count, result, |value| Ok(f(value)));
}

/// Like [`unary`], but `f` may fail; the first error aborts the kernel.
pub fn try_unary<I, O>(
    input: &Vector,
    count: usize,
    result: &mut Vector,
    f: impl FnMut(I) -> Result<O>,
) -> Result<()>
where
    I: NativeType,
    O: NativeType,
{
    unary_impl(input, count, result, f)
}

fn unary_impl<I, O, E>(
    input: &Vector,
    count: usize,
    result: &mut Vector,
    mut f: impl FnMut(I) -> std::result::Result<O, E>,
) -> std::result::Result<(), E>
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
        } else if let crate::Sel::Indices(indices) = input.sel {
            // A dictionary (e.g. a filtered scan): no per-row selection dispatch.
            for (out, &slot) in out[..count].iter_mut().zip(&indices[..count]) {
                *out = f(input.values[slot as usize])?;
            }
        } else {
            for (row, out) in out[..count].iter_mut().enumerate() {
                *out = f(input.values[input.slot(row)])?;
            }
        }
        return Ok(());
    }
    if let (crate::Sel::Identity, Some(words)) = (input.sel, input.validity.words()) {
        // Flat with NULLs (`ExecuteFlat`): the result shares the input's mask, and `f`
        // runs on the valid rows a validity word at a time.
        for (block, (out, values)) in out[..count]
            .chunks_mut(64)
            .zip(input.values[..count].chunks(64))
            .enumerate()
        {
            let word = words.get(block).copied().unwrap_or(u64::MAX);
            if word == u64::MAX {
                for (out, &value) in out.iter_mut().zip(values) {
                    *out = f(value)?;
                }
            } else if word != 0 {
                for (bit, (out, &value)) in out.iter_mut().zip(values).enumerate() {
                    if word >> bit & 1 == 1 {
                        *out = f(value)?;
                    }
                }
            }
        }
        let validity = input.validity.clone();
        result.set_validity(validity);
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
    // See `unary`: no error path, so the loops can be vectorised.
    let Ok(()) = binary_impl::<L, R, O, Infallible>(left, right, count, result, |l, r| Ok(f(l, r)));
}

/// Like [`binary`], but `f` may fail; the first error aborts the kernel.
pub fn try_binary<L, R, O>(
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
    f: impl FnMut(L, R) -> Result<O>,
) -> Result<()>
where
    L: NativeType,
    R: NativeType,
    O: NativeType,
{
    binary_impl(left, right, count, result, f)
}

fn binary_impl<L, R, O, E>(
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
    mut f: impl FnMut(L, R) -> std::result::Result<O, E>,
) -> std::result::Result<(), E>
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
            // Dictionaries (e.g. filtered scans): no per-row selection dispatch.
            (crate::Sel::Indices(left), crate::Sel::Indices(right)) => {
                for ((out, &ls), &rs) in out[..count].iter_mut().zip(&left[..count]).zip(&right[..count]) {
                    *out = f(lu.values[ls as usize], ru.values[rs as usize])?;
                }
            }
            (crate::Sel::Indices(left), crate::Sel::Constant) => {
                let r = ru.values[0];
                for (out, &ls) in out[..count].iter_mut().zip(&left[..count]) {
                    *out = f(lu.values[ls as usize], r)?;
                }
            }
            (crate::Sel::Constant, crate::Sel::Indices(right)) => {
                let l = lu.values[0];
                for (out, &rs) in out[..count].iter_mut().zip(&right[..count]) {
                    *out = f(l, ru.values[rs as usize])?;
                }
            }
            (crate::Sel::Indices(left), crate::Sel::Identity) => {
                for ((out, &ls), &r) in out[..count]
                    .iter_mut()
                    .zip(&left[..count])
                    .zip(&ru.values[..count])
                {
                    *out = f(lu.values[ls as usize], r)?;
                }
            }
            (crate::Sel::Identity, crate::Sel::Indices(right)) => {
                for ((out, &l), &rs) in out[..count]
                    .iter_mut()
                    .zip(&lu.values[..count])
                    .zip(&right[..count])
                {
                    *out = f(l, ru.values[rs as usize])?;
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
    let flat_or_constant = |sel: crate::Sel<'_>| matches!(sel, crate::Sel::Identity | crate::Sel::Constant);
    if flat_or_constant(lu.sel) && flat_or_constant(ru.sel) {
        // Flat (or valid constant) inputs with NULLs (`ExecuteFlat`): the result mask is
        // the inputs' masks combined, and `f` runs on the valid rows a word at a time.
        let left_constant = matches!(lu.sel, crate::Sel::Constant);
        let right_constant = matches!(ru.sel, crate::Sel::Constant);
        let left_words = if left_constant { None } else { lu.validity.words() };
        let right_words = if right_constant { None } else { ru.validity.words() };
        let validity = match (left_words, right_words) {
            (Some(left), Some(right)) => ValidityMask::from_words(
                (0..count.div_ceil(64))
                    .map(|word| {
                        left.get(word).copied().unwrap_or(u64::MAX)
                            & right.get(word).copied().unwrap_or(u64::MAX)
                    })
                    .collect(),
            ),
            (Some(_), None) => lu.validity.clone(),
            _ => ru.validity.clone(),
        };
        if let Some(words) = validity.words() {
            for (block, out) in out[..count].chunks_mut(64).enumerate() {
                let word = words.get(block).copied().unwrap_or(u64::MAX);
                if word == 0 {
                    continue;
                }
                let base = block * 64;
                for (bit, out) in out.iter_mut().enumerate() {
                    if word == u64::MAX || word >> bit & 1 == 1 {
                        let row = base + bit;
                        let l = lu.values[if left_constant { 0 } else { row }];
                        let r = ru.values[if right_constant { 0 } else { row }];
                        *out = f(l, r)?;
                    }
                }
            }
        }
        result.set_validity(validity);
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

/// Writes row `i` of `source` (for `i < positions.len()`) into row `positions[i]` of
/// `target`, a flat vector of the same type (DuckDB's `FillSwitch`).
pub fn scatter(source: &Vector, target: &mut Vector, positions: &[u32]) -> Result<()> {
    fn typed<T: NativeType>(source: &Vector, target: &mut Vector, positions: &[u32]) {
        let src = source.unified::<T>();
        let mut nulls = Vec::new();
        let data = target.values_mut::<T>();
        for (row, &position) in positions.iter().enumerate() {
            match src.get(row) {
                Some(value) => data[position as usize] = value,
                None => nulls.push(position as usize),
            }
        }
        let capacity = data.len();
        let validity = target.validity_mut();
        for row in nulls {
            validity.set_invalid(row, capacity);
        }
    }
    use crate::PhysicalType as P;
    match source.logical_type().physical_type() {
        P::Bool => typed::<bool>(source, target, positions),
        P::Int8 => typed::<i8>(source, target, positions),
        P::Int16 => typed::<i16>(source, target, positions),
        P::Int32 => typed::<i32>(source, target, positions),
        P::Int64 => typed::<i64>(source, target, positions),
        P::Int128 => typed::<i128>(source, target, positions),
        P::UInt8 => typed::<u8>(source, target, positions),
        P::UInt16 => typed::<u16>(source, target, positions),
        P::UInt32 => typed::<u32>(source, target, positions),
        P::UInt64 => typed::<u64>(source, target, positions),
        P::UInt128 => typed::<u128>(source, target, positions),
        P::Float => typed::<f32>(source, target, positions),
        P::Double => typed::<f64>(source, target, positions),
        P::Varchar => {
            typed::<crate::StringT>(source, target, positions);
            target.add_heap_references(source);
        }
        _ => {
            // Nested and other types: value-wise copy until specialised paths exist.
            for (row, &position) in positions.iter().enumerate() {
                target.set_value(position as usize, &source.get_value(row))?;
            }
        }
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
