//! Value comparisons with DuckDB's semantics, including nested types
//! (`NestedEquals`, `DistinctGreaterThan`, ...).
//!
//! Within nested values NULL sorts after every value, lists compare element-wise and then
//! by length, structs field by field, intervals after normalisation and floats with NaN
//! above every number.

use std::cmp::Ordering;

use roastedduck_common::Result;
use roastedduck_types::{PhysicalType, Vector};

use crate::format::{FormatKind, VectorFormat};

/// DuckDB's `interval_t`, as stored in vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Interval {
    pub(crate) months: i32,
    pub(crate) days: i32,
    pub(crate) micros: i64,
}

const MICROS_PER_DAY: i64 = 86_400_000_000;
const DAYS_PER_MONTH: i64 = 30;

impl Interval {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        let mut months = [0; 4];
        let mut days = [0; 4];
        let mut micros = [0; 8];
        months.copy_from_slice(&bytes[0..4]);
        days.copy_from_slice(&bytes[4..8]);
        micros.copy_from_slice(&bytes[8..16]);
        Self {
            months: i32::from_ne_bytes(months),
            days: i32::from_ne_bytes(days),
            micros: i64::from_ne_bytes(micros),
        }
    }

    /// `(months, days, micros)` with micros carried into days and days into months
    /// (`interval_t::Normalize`).
    pub(crate) fn normalize(self) -> (i64, i64, i64) {
        let carry_days = self.micros / MICROS_PER_DAY;
        let micros = self.micros - carry_days * MICROS_PER_DAY;
        let days = i64::from(self.days) + carry_days;
        let carry_months = days / DAYS_PER_MONTH;
        let days = days - carry_months * DAYS_PER_MONTH;
        (i64::from(self.months) + carry_months, days, micros)
    }
}

macro_rules! read {
    ($ty:ty, $bytes:expr) => {{
        let mut buffer = [0u8; size_of::<$ty>()];
        buffer.copy_from_slice($bytes);
        <$ty>::from_ne_bytes(buffer)
    }};
}

fn compare_floats<T: PartialOrd + Copy>(lhs: T, rhs: T, lhs_nan: bool, rhs_nan: bool) -> Ordering {
    match (lhs_nan, rhs_nan) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => lhs.partial_cmp(&rhs).unwrap_or(Ordering::Equal),
    }
}

/// Compares two fixed-width values of physical type `physical`.
pub(crate) fn compare_fixed(physical: PhysicalType, lhs: &[u8], rhs: &[u8]) -> Ordering {
    match physical {
        PhysicalType::Bool | PhysicalType::UInt8 => lhs[0].cmp(&rhs[0]),
        PhysicalType::Int8 => read!(i8, lhs).cmp(&read!(i8, rhs)),
        PhysicalType::Int16 => read!(i16, lhs).cmp(&read!(i16, rhs)),
        PhysicalType::UInt16 => read!(u16, lhs).cmp(&read!(u16, rhs)),
        PhysicalType::Int32 => read!(i32, lhs).cmp(&read!(i32, rhs)),
        PhysicalType::UInt32 => read!(u32, lhs).cmp(&read!(u32, rhs)),
        PhysicalType::Int64 => read!(i64, lhs).cmp(&read!(i64, rhs)),
        PhysicalType::UInt64 => read!(u64, lhs).cmp(&read!(u64, rhs)),
        PhysicalType::Int128 => read!(i128, lhs).cmp(&read!(i128, rhs)),
        PhysicalType::UInt128 => read!(u128, lhs).cmp(&read!(u128, rhs)),
        PhysicalType::Float => {
            let (l, r) = (read!(f32, lhs), read!(f32, rhs));
            compare_floats(l, r, l.is_nan(), r.is_nan())
        }
        PhysicalType::Double => {
            let (l, r) = (read!(f64, lhs), read!(f64, rhs));
            compare_floats(l, r, l.is_nan(), r.is_nan())
        }
        PhysicalType::Interval => Interval::from_bytes(lhs)
            .normalize()
            .cmp(&Interval::from_bytes(rhs).normalize()),
        _ => lhs.cmp(rhs),
    }
}

/// Compares logical row `lhs_row` of `lhs` with `rhs_row` of `rhs` (same type); NULL is
/// greater than every value and equal to NULL.
pub(crate) fn compare_rows(
    lhs: &VectorFormat<'_>,
    lhs_row: usize,
    rhs: &VectorFormat<'_>,
    rhs_row: usize,
) -> Ordering {
    let (lhs_slot, rhs_slot) = (lhs.slot(lhs_row), rhs.slot(rhs_row));
    match (lhs.slot_is_valid(lhs_slot), rhs.slot_is_valid(rhs_slot)) {
        (false, false) => return Ordering::Equal,
        (false, true) => return Ordering::Greater,
        (true, false) => return Ordering::Less,
        (true, true) => {}
    }
    match lhs.kind {
        FormatKind::Fixed(width) => compare_fixed(
            lhs.vector.logical_type().physical_type(),
            lhs.slot_bytes(lhs_slot, width),
            rhs.slot_bytes(rhs_slot, width),
        ),
        FormatKind::Varchar => lhs.string_bytes(lhs_slot).cmp(rhs.string_bytes(rhs_slot)),
        FormatKind::Struct => lhs
            .children
            .iter()
            .zip(&rhs.children)
            .map(|(l, r)| compare_rows(l, lhs_row, r, rhs_row))
            .find(|ordering| ordering.is_ne())
            .unwrap_or(Ordering::Equal),
        FormatKind::List | FormatKind::Array(_) => {
            let (lhs_offset, lhs_len) = lhs.list_entry(lhs_slot);
            let (rhs_offset, rhs_len) = rhs.list_entry(rhs_slot);
            let (lhs_child, rhs_child) = (&lhs.children[0], &rhs.children[0]);
            (0..lhs_len.min(rhs_len))
                .map(|i| compare_rows(lhs_child, lhs_offset + i, rhs_child, rhs_offset + i))
                .find(|ordering| ordering.is_ne())
                .unwrap_or_else(|| lhs_len.cmp(&rhs_len))
        }
    }
}

/// Compares row `lhs_row` of `lhs` with row `rhs_row` of `rhs` (vectors of the same type)
/// with NULL above every value, recursively.
pub fn compare_values(lhs: &Vector, lhs_row: usize, rhs: &Vector, rhs_row: usize) -> Result<Ordering> {
    let lhs = VectorFormat::new(lhs, lhs_row + 1)?;
    let rhs = VectorFormat::new(rhs, rhs_row + 1)?;
    Ok(compare_rows(&lhs, lhs_row, &rhs, rhs_row))
}

/// `lhs IS DISTINCT FROM rhs` for one row each.
pub fn values_distinct(lhs: &Vector, lhs_row: usize, rhs: &Vector, rhs_row: usize) -> Result<bool> {
    compare_values(lhs, lhs_row, rhs, rhs_row).map(Ordering::is_ne)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_compare_normalised() {
        let month = Interval {
            months: 1,
            days: 0,
            micros: 0,
        };
        let thirty_days = Interval {
            months: 0,
            days: 30,
            micros: 0,
        };
        assert_eq!(month.normalize(), thirty_days.normalize());
        let hours = Interval {
            months: 0,
            days: 0,
            micros: 25 * 3_600_000_000,
        };
        assert_eq!(hours.normalize(), (0, 1, 3_600_000_000));
    }

    #[test]
    fn floats_order_nan_last() {
        let nan = f64::NAN.to_ne_bytes();
        let one = 1.0f64.to_ne_bytes();
        assert_eq!(compare_fixed(PhysicalType::Double, &nan, &one), Ordering::Greater);
        assert_eq!(compare_fixed(PhysicalType::Double, &nan, &nan), Ordering::Equal);
        let zero = 0.0f64.to_ne_bytes();
        let negative_zero = (-0.0f64).to_ne_bytes();
        assert_eq!(
            compare_fixed(PhysicalType::Double, &zero, &negative_zero),
            Ordering::Equal
        );
    }
}
