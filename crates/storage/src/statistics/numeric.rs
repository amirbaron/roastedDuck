//! Min/max statistics of numeric columns (DuckDB's `NumericStats`).

use std::cmp::Ordering;

use rustdb_types::compare::{CompareOp, SqlOrd};
use rustdb_types::{Date, LogicalType, LogicalTypeId, NativeType, PhysicalType, Value};

use super::FilterPropagateResult;

/// A single value of a numeric physical type (DuckDB's `NumericValueUnion`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumericValue {
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    F32(f32),
    F64(f64),
}

impl NumericValue {
    /// DuckDB's ordering of two values of the same physical type (NaN is the largest
    /// float). Values of different physical types are unordered.
    pub fn compare(self, other: Self) -> Option<Ordering> {
        fn ord<T: SqlOrd>(left: T, right: T) -> Ordering {
            if left.sql_eq(right) {
                Ordering::Equal
            } else if left.sql_gt(right) {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        Some(match (self, other) {
            (Self::Bool(l), Self::Bool(r)) => ord(l, r),
            (Self::I8(l), Self::I8(r)) => ord(l, r),
            (Self::I16(l), Self::I16(r)) => ord(l, r),
            (Self::I32(l), Self::I32(r)) => ord(l, r),
            (Self::I64(l), Self::I64(r)) => ord(l, r),
            (Self::I128(l), Self::I128(r)) => ord(l, r),
            (Self::U8(l), Self::U8(r)) => ord(l, r),
            (Self::U16(l), Self::U16(r)) => ord(l, r),
            (Self::U32(l), Self::U32(r)) => ord(l, r),
            (Self::U64(l), Self::U64(r)) => ord(l, r),
            (Self::U128(l), Self::U128(r)) => ord(l, r),
            (Self::F32(l), Self::F32(r)) => ord(l, r),
            (Self::F64(l), Self::F64(r)) => ord(l, r),
            _ => return None,
        })
    }

    /// The value of a non-NULL numeric `value`, if it has a numeric physical type.
    pub fn from_value(value: &Value) -> Option<Self> {
        if value.is_null() {
            return None;
        }
        Some(match value.logical_type().physical_type() {
            PhysicalType::Bool => Self::Bool(value.as_bool()?),
            PhysicalType::Int8 => Self::I8(value.as_i8()?),
            PhysicalType::Int16 => Self::I16(value.as_i16()?),
            PhysicalType::Int32 => Self::I32(value.as_i32()?),
            PhysicalType::Int64 => Self::I64(value.as_i64()?),
            PhysicalType::Int128 => Self::I128(value.as_i128()?),
            PhysicalType::UInt8 => Self::U8(value.as_u8()?),
            PhysicalType::UInt16 => Self::U16(value.as_u16()?),
            PhysicalType::UInt32 => Self::U32(value.as_u32()?),
            PhysicalType::UInt64 => Self::U64(value.as_u64()?),
            PhysicalType::UInt128 => Self::U128(value.as_u128()?),
            PhysicalType::Float => Self::F32(value.as_f32()?),
            PhysicalType::Double => Self::F64(value.as_f64()?),
            _ => return None,
        })
    }

    /// The value as a `Value` of type `ty`.
    pub fn to_value(self, ty: &LogicalType) -> Value {
        let value = match self {
            Self::Bool(v) => Value::boolean(v),
            Self::I8(v) => Value::tinyint(v),
            Self::I16(v) => Value::smallint(v),
            Self::I32(v) => Value::integer(v),
            Self::I64(v) => Value::bigint(v),
            Self::I128(v) => Value::hugeint(v),
            Self::U8(v) => Value::utinyint(v),
            Self::U16(v) => Value::usmallint(v),
            Self::U32(v) => Value::uinteger(v),
            Self::U64(v) => Value::ubigint(v),
            Self::U128(v) => Value::uhugeint(v),
            Self::F32(v) => Value::float(v),
            Self::F64(v) => Value::double(v),
        };
        value.reinterpret(ty.clone())
    }
}

/// A native type whose values can be tracked in numeric statistics.
pub trait StatsNative: NativeType + SqlOrd {
    fn to_numeric(self) -> NumericValue;
    fn from_numeric(value: NumericValue) -> Option<Self>;
}

macro_rules! stats_native {
    ($($ty:ty => $variant:ident),* $(,)?) => {$(
        impl StatsNative for $ty {
            #[inline]
            fn to_numeric(self) -> NumericValue {
                NumericValue::$variant(self)
            }
            #[inline]
            fn from_numeric(value: NumericValue) -> Option<Self> {
                match value {
                    NumericValue::$variant(v) => Some(v),
                    _ => None,
                }
            }
        }
    )*};
}

stats_native! {
    bool => Bool, i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128,
    u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128, f32 => F32, f64 => F64,
}

/// Minimum and maximum of a numeric column; `None` means unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NumericStats {
    pub min: Option<NumericValue>,
    pub max: Option<NumericValue>,
}

impl NumericStats {
    /// Statistics of no values: `min` is the type's largest value and `max` its smallest,
    /// so that any update replaces both (DuckDB's `NumericStats::CreateEmpty`).
    pub fn empty(ty: &LogicalType) -> Self {
        Self {
            min: maximum_value(ty),
            max: minimum_value(ty),
        }
    }

    /// Statistics that say nothing about the values.
    pub const fn unknown() -> Self {
        Self { min: None, max: None }
    }

    /// Whether both bounds are known and form a non-empty range.
    pub fn has_min_max(&self) -> bool {
        match (self.min, self.max) {
            (Some(min), Some(max)) => min.compare(max) != Some(Ordering::Greater),
            _ => false,
        }
    }

    /// Whether every value equals the single bound.
    pub fn is_constant(&self) -> bool {
        match (self.min, self.max) {
            (Some(min), Some(max)) => max.compare(min) != Some(Ordering::Greater),
            _ => false,
        }
    }

    /// Widens the range by `values` (DuckDB's `UpdateNumericStats` in a loop).
    #[inline]
    pub fn update<T: StatsNative>(&mut self, values: impl IntoIterator<Item = T>) {
        let (Some(min), Some(max)) = (
            self.min.and_then(T::from_numeric),
            self.max.and_then(T::from_numeric),
        ) else {
            return;
        };
        let (mut min, mut max) = (min, max);
        for value in values {
            if min.sql_gt(value) {
                min = value;
            }
            if value.sql_gt(max) {
                max = value;
            }
        }
        self.min = Some(min.to_numeric());
        self.max = Some(max.to_numeric());
    }

    /// Merges the range of `other` into this one.
    pub fn merge(&mut self, other: &Self) {
        self.min = match (self.min, other.min) {
            (Some(own), Some(theirs)) if theirs.compare(own) == Some(Ordering::Less) => Some(theirs),
            (Some(own), Some(_)) => Some(own),
            _ => None,
        };
        self.max = match (self.max, other.max) {
            (Some(own), Some(theirs)) if theirs.compare(own) == Some(Ordering::Greater) => Some(theirs),
            (Some(own), Some(_)) => Some(own),
            _ => None,
        };
    }

    /// Whether `x <op> constant` can hold for values in the range (DuckDB's
    /// `NumericStats::CheckZonemap`). Several constants are OR-ed (`IN` lists).
    pub fn check_zonemap(&self, op: CompareOp, constants: &[NumericValue]) -> FilterPropagateResult {
        if !self.has_min_max() {
            return FilterPropagateResult::NoPruningPossible;
        }
        let (Some(min), Some(max)) = (self.min, self.max) else {
            return FilterPropagateResult::NoPruningPossible;
        };
        for &constant in constants {
            match check_range(op, min, max, constant) {
                FilterPropagateResult::FilterAlwaysFalse => {}
                other => return other,
            }
        }
        FilterPropagateResult::FilterAlwaysFalse
    }
}

fn check_range(
    op: CompareOp,
    min: NumericValue,
    max: NumericValue,
    constant: NumericValue,
) -> FilterPropagateResult {
    use FilterPropagateResult::{FilterAlwaysFalse, FilterAlwaysTrue, NoPruningPossible};
    let (Some(min_cmp), Some(max_cmp)) = (constant.compare(min), constant.compare(max)) else {
        return NoPruningPossible;
    };
    let exact = min_cmp == Ordering::Equal && max_cmp == Ordering::Equal;
    let in_range = min_cmp != Ordering::Less && max_cmp != Ordering::Greater;
    match op {
        CompareOp::Equal => {
            if exact {
                FilterAlwaysTrue
            } else if in_range {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
        CompareOp::NotEqual => {
            if !in_range {
                FilterAlwaysTrue
            } else if exact {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
        // x >= C: true everywhere if min >= C, possible if max >= C.
        CompareOp::GreaterThanOrEqual => {
            if min_cmp != Ordering::Greater {
                FilterAlwaysTrue
            } else if max_cmp != Ordering::Greater {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
        CompareOp::GreaterThan => {
            if min_cmp == Ordering::Less {
                FilterAlwaysTrue
            } else if max_cmp == Ordering::Less {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
        CompareOp::LessThanOrEqual => {
            if max_cmp != Ordering::Less {
                FilterAlwaysTrue
            } else if min_cmp != Ordering::Less {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
        CompareOp::LessThan => {
            if max_cmp == Ordering::Greater {
                FilterAlwaysTrue
            } else if min_cmp == Ordering::Greater {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
    }
}

const MICROS_PER_SEC: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400_000_000;
const NANOS_PER_DAY: i64 = 86_400_000_000_000;
/// `dtime_tz_t::MAX_OFFSET` (±15:59:59).
const TIME_TZ_MAX_OFFSET: i64 = 16 * 60 * 60 - 1;
const TIME_TZ_OFFSET_BITS: u32 = 24;

fn decimal_bound(ty: &LogicalType, positive: bool) -> Option<NumericValue> {
    let (width, _) = ty.decimal_properties()?;
    let bound = 10i128.pow(u32::from(width)) - 1;
    let bound = if positive { bound } else { -bound };
    Some(match ty.physical_type() {
        PhysicalType::Int16 => NumericValue::I16(bound as i16),
        PhysicalType::Int32 => NumericValue::I32(bound as i32),
        PhysicalType::Int64 => NumericValue::I64(bound as i64),
        _ => NumericValue::I128(bound),
    })
}

/// The smallest value of a numeric type (DuckDB's `Value::MinimumValue`).
pub fn minimum_value(ty: &LogicalType) -> Option<NumericValue> {
    use LogicalTypeId as L;
    let timestamp_min = || Date::from_ymd(-290_308, 12, 22).map(|date| i64::from(date.0) * MICROS_PER_DAY);
    Some(match ty.id() {
        L::Boolean => NumericValue::Bool(false),
        L::TinyInt => NumericValue::I8(i8::MIN),
        L::SmallInt => NumericValue::I16(i16::MIN),
        L::Integer | L::SqlNull => NumericValue::I32(i32::MIN),
        L::BigInt => NumericValue::I64(i64::MIN),
        L::HugeInt | L::Uuid => NumericValue::I128(i128::MIN),
        L::UHugeInt => NumericValue::U128(0),
        L::UTinyInt => NumericValue::U8(0),
        L::USmallInt => NumericValue::U16(0),
        L::UInteger => NumericValue::U32(0),
        L::UBigInt => NumericValue::U64(0),
        L::Date => NumericValue::I32(Date::from_ymd(-5_877_641, 6, 25)?.0),
        // TIME_TZ: dtime_tz_t(00:00:00, +15:59:59) encodes the offset as MAX_OFFSET - offset = 0.
        L::Time | L::TimeNs | L::TimeTz => NumericValue::I64(0),
        L::Timestamp | L::TimestampTz => NumericValue::I64(timestamp_min()?),
        L::TimestampSec => NumericValue::I64(timestamp_min()? / MICROS_PER_SEC),
        L::TimestampMs => NumericValue::I64(timestamp_min()? / 1000),
        L::TimestampNs => NumericValue::I64(i64::MIN / NANOS_PER_DAY * NANOS_PER_DAY),
        L::Float => NumericValue::F32(f32::MIN),
        L::Double => NumericValue::F64(f64::MIN),
        L::Decimal => return decimal_bound(ty, false),
        L::Enum => match ty.physical_type() {
            PhysicalType::UInt8 => NumericValue::U8(0),
            PhysicalType::UInt16 => NumericValue::U16(0),
            _ => NumericValue::U32(0),
        },
        _ => return None,
    })
}

/// The largest value of a numeric type (DuckDB's `Value::MaximumValue`).
pub fn maximum_value(ty: &LogicalType) -> Option<NumericValue> {
    use LogicalTypeId as L;
    let timestamp_max = i64::MAX - 1;
    Some(match ty.id() {
        L::Boolean => NumericValue::Bool(true),
        L::TinyInt => NumericValue::I8(i8::MAX),
        L::SmallInt => NumericValue::I16(i16::MAX),
        L::Integer | L::SqlNull => NumericValue::I32(i32::MAX),
        L::BigInt => NumericValue::I64(i64::MAX),
        L::HugeInt | L::Uuid => NumericValue::I128(i128::MAX),
        L::UHugeInt => NumericValue::U128(u128::MAX),
        L::UTinyInt => NumericValue::U8(u8::MAX),
        L::USmallInt => NumericValue::U16(u16::MAX),
        L::UInteger => NumericValue::U32(u32::MAX),
        L::UBigInt => NumericValue::U64(u64::MAX),
        L::Date => NumericValue::I32(Date::from_ymd(5_881_580, 7, 10)?.0),
        L::Time => NumericValue::I64(MICROS_PER_DAY),
        L::TimeNs => NumericValue::I64(NANOS_PER_DAY),
        L::Timestamp | L::TimestampTz | L::TimestampNs => NumericValue::I64(timestamp_max),
        L::TimestampSec => NumericValue::I64(timestamp_max / MICROS_PER_SEC),
        L::TimestampMs => NumericValue::I64(timestamp_max / 1000),
        // dtime_tz_t(24:00:00, -15:59:59).
        L::TimeTz => NumericValue::I64(
            (MICROS_PER_DAY << TIME_TZ_OFFSET_BITS) | (TIME_TZ_MAX_OFFSET + TIME_TZ_MAX_OFFSET),
        ),
        L::Float => NumericValue::F32(f32::MAX),
        L::Double => NumericValue::F64(f64::MAX),
        L::Decimal => return decimal_bound(ty, true),
        L::Enum => {
            let size = ty.enum_values().map_or(0, <[String]>::len) as u64;
            let last = size.saturating_sub(1);
            match ty.physical_type() {
                PhysicalType::UInt8 => NumericValue::U8(last as u8),
                PhysicalType::UInt16 => NumericValue::U16(last as u16),
                _ => NumericValue::U32(last as u32),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stats_widen_on_update() {
        let mut stats = NumericStats::empty(&LogicalType::INTEGER);
        assert_eq!(stats.min, Some(NumericValue::I32(i32::MAX)));
        assert!(!stats.has_min_max());
        stats.update([5i32, -3, 7]);
        assert_eq!(stats.min, Some(NumericValue::I32(-3)));
        assert_eq!(stats.max, Some(NumericValue::I32(7)));
        assert!(stats.has_min_max());
    }

    #[test]
    fn zonemaps_prune_like_duckdb() {
        let stats = NumericStats {
            min: Some(NumericValue::I32(10)),
            max: Some(NumericValue::I32(20)),
        };
        let check = |op, c| stats.check_zonemap(op, &[NumericValue::I32(c)]);
        assert_eq!(
            check(CompareOp::Equal, 5),
            FilterPropagateResult::FilterAlwaysFalse
        );
        assert_eq!(
            check(CompareOp::Equal, 15),
            FilterPropagateResult::NoPruningPossible
        );
        assert_eq!(
            check(CompareOp::GreaterThan, 20),
            FilterPropagateResult::FilterAlwaysFalse
        );
        assert_eq!(
            check(CompareOp::GreaterThan, 9),
            FilterPropagateResult::FilterAlwaysTrue
        );
        assert_eq!(
            check(CompareOp::LessThanOrEqual, 20),
            FilterPropagateResult::FilterAlwaysTrue
        );
        assert_eq!(
            check(CompareOp::NotEqual, 25),
            FilterPropagateResult::FilterAlwaysTrue
        );
    }

    #[test]
    fn nan_is_the_largest_float() {
        let mut stats = NumericStats::empty(&LogicalType::DOUBLE);
        stats.update([1.0f64, f64::NAN, -2.0]);
        assert_eq!(stats.min, Some(NumericValue::F64(-2.0)));
        assert!(matches!(stats.max, Some(NumericValue::F64(v)) if v.is_nan()));
    }
}
