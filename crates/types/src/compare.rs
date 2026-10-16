//! Vectorised comparisons with DuckDB's semantics
//! (`common/vector_operations/comparison_operators.cpp`).
//!
//! Floats order NaN above every number and treat NaN as equal to NaN; strings compare
//! byte-wise; booleans order `false < true`.

use rustdb_common::{Error, Result};

use crate::kernels::binary;
use crate::{
    Interval, LogicalType, NativeType, PhysicalType, Sel, StringT, Vector, VectorKind, partition_rows,
};

/// A comparison between two values of the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareOp {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

impl CompareOp {
    /// The same comparison with its operands swapped (`a < b` ⇔ `b > a`).
    #[must_use]
    pub const fn flip(self) -> Self {
        match self {
            Self::Equal | Self::NotEqual => self,
            Self::LessThan => Self::GreaterThan,
            Self::LessThanOrEqual => Self::GreaterThanOrEqual,
            Self::GreaterThan => Self::LessThan,
            Self::GreaterThanOrEqual => Self::LessThanOrEqual,
        }
    }
}

/// `==` and `>` as DuckDB defines them for a native type.
pub trait SqlOrd: NativeType {
    fn sql_eq(self, other: Self) -> bool;
    fn sql_gt(self, other: Self) -> bool;
}

macro_rules! sql_ord_total {
    ($($ty:ty),*) => {$(
        impl SqlOrd for $ty {
            #[inline]
            fn sql_eq(self, other: Self) -> bool { self == other }
            #[inline]
            fn sql_gt(self, other: Self) -> bool { self > other }
        }
    )*};
}
sql_ord_total!(bool, i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

macro_rules! sql_ord_float {
    ($($ty:ty),*) => {$(
        impl SqlOrd for $ty {
            #[inline]
            fn sql_eq(self, other: Self) -> bool {
                (self.is_nan() && other.is_nan()) || self == other
            }
            #[inline]
            fn sql_gt(self, other: Self) -> bool {
                if other.is_nan() {
                    false
                } else if self.is_nan() {
                    true
                } else {
                    self > other
                }
            }
        }
    )*};
}
sql_ord_float!(f32, f64);

/// Intervals compare by their normalised `(months, days, micros)`, so `1 month = 30 days`
/// (`Interval::Equals` and `Interval::GreaterThan`).
impl SqlOrd for Interval {
    #[inline]
    fn sql_eq(self, other: Self) -> bool {
        self.normalized_eq(other)
    }
    #[inline]
    fn sql_gt(self, other: Self) -> bool {
        self > other
    }
}

#[inline]
fn apply_bytes(op: CompareOp, left: &[u8], right: &[u8]) -> bool {
    match op {
        CompareOp::Equal => left == right,
        CompareOp::NotEqual => left != right,
        CompareOp::GreaterThan => left > right,
        CompareOp::LessThan => left < right,
        CompareOp::GreaterThanOrEqual => left >= right,
        CompareOp::LessThanOrEqual => left <= right,
    }
}

fn compare_native<T: SqlOrd>(
    op: CompareOp,
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
) {
    // One loop per operator, so that the comparison is not dispatched per row.
    match op {
        CompareOp::Equal => binary::<T, T, bool>(left, right, count, result, SqlOrd::sql_eq),
        CompareOp::NotEqual => binary::<T, T, bool>(left, right, count, result, |l, r| !l.sql_eq(r)),
        CompareOp::GreaterThan => binary::<T, T, bool>(left, right, count, result, SqlOrd::sql_gt),
        CompareOp::LessThan => binary::<T, T, bool>(left, right, count, result, |l, r| r.sql_gt(l)),
        CompareOp::GreaterThanOrEqual => {
            binary::<T, T, bool>(left, right, count, result, |l, r| !r.sql_gt(l));
        }
        CompareOp::LessThanOrEqual => binary::<T, T, bool>(left, right, count, result, |l, r| !l.sql_gt(r)),
    }
}

fn compare_strings(op: CompareOp, left: &Vector, right: &Vector, count: usize, result: &mut Vector) {
    // Equal strings must share length and prefix; check those before touching heap bytes.
    binary::<StringT, StringT, bool>(left, right, count, result, |l, r| {
        if matches!(op, CompareOp::Equal | CompareOp::NotEqual)
            && (l.len() != r.len() || l.prefix() != r.prefix())
        {
            return op == CompareOp::NotEqual;
        }
        apply_bytes(op, left.string_value(&l), right.string_value(&r))
    });
}

/// LIST/STRUCT/ARRAY comparisons: NULL at the top level gives NULL; inside, values
/// compare element-wise with NULLs last and a shorter list before a longer one sharing
/// its prefix (DuckDB's nested `PositionComparator`). Row by row through `Value`s.
fn compare_nested(
    op: CompareOp,
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
) -> Result<()> {
    let mut out = Vector::flat(LogicalType::BOOLEAN, count);
    for row in 0..count {
        if left.is_null(row) || right.is_null(row) {
            out.validity_mut().set_invalid(row, count);
            continue;
        }
        let ordering = left
            .get_value(row)
            .compare_not_distinct(&right.get_value(row))
            .ok_or_else(|| Error::internal("comparison of differently stored nested values"))?;
        out.values_mut::<bool>()[row] = match op {
            CompareOp::Equal => ordering.is_eq(),
            CompareOp::NotEqual => ordering.is_ne(),
            CompareOp::LessThan => ordering.is_lt(),
            CompareOp::LessThanOrEqual => ordering.is_le(),
            CompareOp::GreaterThan => ordering.is_gt(),
            CompareOp::GreaterThanOrEqual => ordering.is_ge(),
        };
    }
    *result = out;
    Ok(())
}

/// Computes `left <op> right` into a BOOLEAN vector; NULL inputs give NULL.
pub fn compare(
    op: CompareOp,
    left: &Vector,
    right: &Vector,
    count: usize,
    result: &mut Vector,
) -> Result<()> {
    if left.logical_type().physical_type() != right.logical_type().physical_type() {
        return Err(Error::internal(format!(
            "comparison between different types {} and {}",
            left.logical_type(),
            right.logical_type()
        )));
    }
    *result = Vector::flat(LogicalType::BOOLEAN, 0);
    match left.logical_type().physical_type() {
        PhysicalType::Bool => compare_native::<bool>(op, left, right, count, result),
        PhysicalType::Int8 => compare_native::<i8>(op, left, right, count, result),
        PhysicalType::Int16 => compare_native::<i16>(op, left, right, count, result),
        PhysicalType::Int32 => compare_native::<i32>(op, left, right, count, result),
        PhysicalType::Int64 => compare_native::<i64>(op, left, right, count, result),
        PhysicalType::Int128 => compare_native::<i128>(op, left, right, count, result),
        PhysicalType::UInt8 => compare_native::<u8>(op, left, right, count, result),
        PhysicalType::UInt16 => compare_native::<u16>(op, left, right, count, result),
        PhysicalType::UInt32 => compare_native::<u32>(op, left, right, count, result),
        PhysicalType::UInt64 => compare_native::<u64>(op, left, right, count, result),
        PhysicalType::UInt128 => compare_native::<u128>(op, left, right, count, result),
        PhysicalType::Float => compare_native::<f32>(op, left, right, count, result),
        PhysicalType::Double => compare_native::<f64>(op, left, right, count, result),
        PhysicalType::Interval => compare_native::<Interval>(op, left, right, count, result),
        PhysicalType::Varchar => compare_strings(op, left, right, count, result),
        PhysicalType::List | PhysicalType::Struct | PhysicalType::Array => {
            compare_nested(op, left, right, count, result)?;
        }
        other => {
            return Err(Error::not_implemented(format!(
                "comparison of physical type {}",
                other.name()
            )));
        }
    }
    Ok(())
}

/// Splits rows `0..count` by `left <op> right` without materialising a BOOLEAN vector
/// (DuckDB's `VectorOperations::Equals`/`GreaterThan`/... with selection vectors). NULL
/// rows go to `false_sel`. The position of row `i` is `positions[i]` (`i` without
/// positions). Returns the number of matching rows.
pub fn select(
    op: CompareOp,
    left: &Vector,
    right: &Vector,
    count: usize,
    positions: Option<&[u32]>,
    true_sel: Option<&mut Vec<u32>>,
    false_sel: Option<&mut Vec<u32>>,
) -> Result<usize> {
    let physical = left.logical_type().physical_type();
    if physical != right.logical_type().physical_type() {
        return Err(Error::internal(format!(
            "comparison between different types {} and {}",
            left.logical_type(),
            right.logical_type()
        )));
    }
    if left.kind() == VectorKind::Sequence || right.kind() == VectorKind::Sequence {
        let (left, right) = (left.flattened(count), right.flattened(count));
        return select(op, &left, &right, count, positions, true_sel, false_sel);
    }
    let args = (left, right, count, positions, true_sel, false_sel);
    Ok(match physical {
        PhysicalType::Bool => select_native::<bool>(op, args),
        PhysicalType::Int8 => select_native::<i8>(op, args),
        PhysicalType::Int16 => select_native::<i16>(op, args),
        PhysicalType::Int32 => select_native::<i32>(op, args),
        PhysicalType::Int64 => select_native::<i64>(op, args),
        PhysicalType::Int128 => select_native::<i128>(op, args),
        PhysicalType::UInt8 => select_native::<u8>(op, args),
        PhysicalType::UInt16 => select_native::<u16>(op, args),
        PhysicalType::UInt32 => select_native::<u32>(op, args),
        PhysicalType::UInt64 => select_native::<u64>(op, args),
        PhysicalType::UInt128 => select_native::<u128>(op, args),
        PhysicalType::Float => select_native::<f32>(op, args),
        PhysicalType::Double => select_native::<f64>(op, args),
        PhysicalType::Interval => select_native::<Interval>(op, args),
        _ => {
            let (left, right, count, positions, true_sel, false_sel) = args;
            let mut result = Vector::flat(LogicalType::BOOLEAN, 0);
            compare(op, left, right, count, &mut result)?;
            let values = result.unified::<bool>();
            partition_rows(count, positions, true_sel, false_sel, |row| {
                values.get(row) == Some(true)
            })
        }
    })
}

/// The arguments of [`select`] after dispatching on the type.
type SelectArgs<'a, 'b> = (
    &'a Vector,
    &'a Vector,
    usize,
    Option<&'a [u32]>,
    Option<&'b mut Vec<u32>>,
    Option<&'b mut Vec<u32>>,
);

fn select_native<T: SqlOrd>(op: CompareOp, args: SelectArgs<'_, '_>) -> usize {
    // One loop per operator, so that the comparison is not dispatched per row.
    match op {
        CompareOp::Equal => select_loop::<T>(args, SqlOrd::sql_eq),
        CompareOp::NotEqual => select_loop::<T>(args, |l, r| !l.sql_eq(r)),
        CompareOp::GreaterThan => select_loop::<T>(args, SqlOrd::sql_gt),
        CompareOp::LessThan => select_loop::<T>(args, |l, r| r.sql_gt(l)),
        CompareOp::GreaterThanOrEqual => select_loop::<T>(args, |l, r| !r.sql_gt(l)),
        CompareOp::LessThanOrEqual => select_loop::<T>(args, |l, r| !l.sql_gt(r)),
    }
}

#[inline]
fn select_loop<T: NativeType>(
    (left, right, count, positions, true_sel, false_sel): SelectArgs<'_, '_>,
    predicate: impl Fn(T, T) -> bool,
) -> usize {
    let (lu, ru) = (left.unified::<T>(), right.unified::<T>());
    if !(lu.validity.is_all_valid() && ru.validity.is_all_valid()) {
        return partition_rows(count, positions, true_sel, false_sel, |row| {
            match (lu.get(row), ru.get(row)) {
                (Some(l), Some(r)) => predicate(l, r),
                _ => false,
            }
        });
    }
    match (lu.sel, ru.sel) {
        (Sel::Identity, Sel::Identity) => {
            let (l, r) = (&lu.values[..count], &ru.values[..count]);
            partition_rows(count, positions, true_sel, false_sel, |row| {
                predicate(l[row], r[row])
            })
        }
        (Sel::Identity, Sel::Constant) => {
            let (l, r) = (&lu.values[..count], ru.values[0]);
            partition_rows(count, positions, true_sel, false_sel, |row| predicate(l[row], r))
        }
        (Sel::Constant, Sel::Identity) => {
            let (l, r) = (lu.values[0], &ru.values[..count]);
            partition_rows(count, positions, true_sel, false_sel, |row| predicate(l, r[row]))
        }
        _ => partition_rows(count, positions, true_sel, false_sel, |row| {
            predicate(lu.values[lu.slot(row)], ru.values[ru.slot(row)])
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn select_splits_rows_and_sends_nulls_to_false() {
        let mut left = Vector::flat(LogicalType::INTEGER, 4);
        left.values_mut::<i32>().copy_from_slice(&[1, 5, 3, 7]);
        left.set_null(3);
        let right = Vector::constant(&Value::integer(3)).unwrap();
        let (mut yes, mut no) = (Vec::new(), Vec::new());
        let positions = [10, 11, 12, 13];
        let matched = select(
            CompareOp::GreaterThanOrEqual,
            &left,
            &right,
            4,
            Some(&positions),
            Some(&mut yes),
            Some(&mut no),
        )
        .unwrap();
        assert_eq!(matched, 2);
        assert_eq!(yes, [11, 12]);
        assert_eq!(no, [10, 13]);
    }

    fn bools(vector: &Vector, count: usize) -> Vec<Option<bool>> {
        (0..count).map(|row| vector.get_value(row).as_bool()).collect()
    }

    #[test]
    fn nan_orders_above_numbers_and_equals_itself() {
        let mut left = Vector::flat(LogicalType::DOUBLE, 3);
        left.values_mut::<f64>()
            .copy_from_slice(&[f64::NAN, 1.0, f64::NAN]);
        let mut right = Vector::flat(LogicalType::DOUBLE, 3);
        right
            .values_mut::<f64>()
            .copy_from_slice(&[f64::NAN, f64::NAN, 5.0]);
        let mut result = Vector::flat(LogicalType::BOOLEAN, 0);
        compare(CompareOp::Equal, &left, &right, 3, &mut result).unwrap();
        assert_eq!(bools(&result, 3), [Some(true), Some(false), Some(false)]);
        compare(CompareOp::GreaterThan, &left, &right, 3, &mut result).unwrap();
        assert_eq!(bools(&result, 3), [Some(false), Some(false), Some(true)]);
        compare(CompareOp::LessThanOrEqual, &left, &right, 3, &mut result).unwrap();
        assert_eq!(bools(&result, 3), [Some(true), Some(true), Some(false)]);
    }

    #[test]
    fn strings_compare_bytewise() {
        let mut left = Vector::flat(LogicalType::VARCHAR, 3);
        let mut right = Vector::flat(LogicalType::VARCHAR, 3);
        for (row, (l, r)) in [
            ("abc", "abd"),
            ("a long shared prefix x", "a long shared prefix x"),
            ("b", ""),
        ]
        .into_iter()
        .enumerate()
        {
            left.set_value(row, &Value::varchar(l)).unwrap();
            right.set_value(row, &Value::varchar(r)).unwrap();
        }
        let mut result = Vector::flat(LogicalType::BOOLEAN, 0);
        compare(CompareOp::LessThan, &left, &right, 3, &mut result).unwrap();
        assert_eq!(bools(&result, 3), [Some(true), Some(false), Some(false)]);
        compare(CompareOp::Equal, &left, &right, 3, &mut result).unwrap();
        assert_eq!(bools(&result, 3), [Some(false), Some(true), Some(false)]);
    }
}
