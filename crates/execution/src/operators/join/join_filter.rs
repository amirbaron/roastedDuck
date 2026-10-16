//! The run-time filters a hash join sets on its probe-side scans (DuckDB's
//! `JoinFilterPushdownInfo`): the build keys' minimum and maximum, collected while
//! building, bound the probe keys that can match.

use std::cmp::Ordering;

use rustdb_ast::ExpressionType;
use rustdb_function::cast::ValueCast;
use rustdb_planner::operator::JoinFilter;
use rustdb_storage::statistics::{NumericStats, NumericValue};
use rustdb_types::compare::CompareOp;
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, StringT, Value, Vector};

/// The smallest and largest non-NULL key seen (DuckDB's `min`/`max` aggregates of
/// `JoinFilterPushdownInfo::min_max_aggregates`).
#[derive(Clone, Debug, Default)]
pub(super) enum KeyMinMax {
    /// No non-NULL key yet.
    #[default]
    Empty,
    Numeric(NumericValue, NumericValue),
    Bytes(Vec<u8>, Vec<u8>),
    /// A key type without a usable order.
    Unsupported,
}

impl KeyMinMax {
    /// Extends the range by the first `count` rows of `keys`.
    pub(super) fn update(&mut self, keys: &Vector, count: usize) {
        if matches!(self, Self::Unsupported) {
            return;
        }
        let ty = keys.logical_type();
        macro_rules! numeric {
            ($native:ty) => {{
                let values = keys.unified::<$native>();
                let mut stats = NumericStats::empty(ty);
                stats.update((0..count).filter_map(|row| values.get(row)));
                if let (Some(min), Some(max)) = (stats.min, stats.max)
                    && stats.has_min_max()
                {
                    self.merge(Self::Numeric(min, max));
                }
            }};
        }
        match ty.physical_type() {
            PhysicalType::Int8 => numeric!(i8),
            PhysicalType::Int16 => numeric!(i16),
            PhysicalType::Int32 => numeric!(i32),
            PhysicalType::Int64 => numeric!(i64),
            PhysicalType::Int128 => numeric!(i128),
            PhysicalType::UInt8 => numeric!(u8),
            PhysicalType::UInt16 => numeric!(u16),
            PhysicalType::UInt32 => numeric!(u32),
            PhysicalType::UInt64 => numeric!(u64),
            PhysicalType::UInt128 => numeric!(u128),
            PhysicalType::Float => numeric!(f32),
            PhysicalType::Double => numeric!(f64),
            PhysicalType::Varchar if ty.id() == LogicalTypeId::Varchar => {
                let values = keys.unified::<StringT>();
                let mut bounds: Option<(Vec<u8>, Vec<u8>)> = None;
                for row in 0..count {
                    let Some(value) = values.get(row) else {
                        continue;
                    };
                    let bytes = keys.string_value(&value);
                    match &mut bounds {
                        None => bounds = Some((bytes.to_vec(), bytes.to_vec())),
                        Some((min, max)) => {
                            if bytes < min.as_slice() {
                                *min = bytes.to_vec();
                            }
                            if bytes > max.as_slice() {
                                *max = bytes.to_vec();
                            }
                        }
                    }
                }
                if let Some((min, max)) = bounds {
                    self.merge(Self::Bytes(min, max));
                }
            }
            _ => *self = Self::Unsupported,
        }
    }

    /// Merges the range of another thread.
    pub(super) fn merge(&mut self, other: Self) {
        *self = match (std::mem::take(self), other) {
            (Self::Unsupported, _) | (_, Self::Unsupported) => Self::Unsupported,
            (Self::Empty, other) | (other, Self::Empty) => other,
            (Self::Numeric(min, max), Self::Numeric(other_min, other_max)) => {
                let min = if other_min.compare(min) == Some(Ordering::Less) {
                    other_min
                } else {
                    min
                };
                let max = if other_max.compare(max) == Some(Ordering::Greater) {
                    other_max
                } else {
                    max
                };
                Self::Numeric(min, max)
            }
            (Self::Bytes(min, max), Self::Bytes(other_min, other_max)) => {
                Self::Bytes(min.min(other_min), max.max(other_max))
            }
            _ => Self::Unsupported,
        };
    }

    /// The range as values of the key type.
    fn bounds(&self, ty: &LogicalType) -> Option<(Value, Value)> {
        match self {
            Self::Numeric(min, max) => Some((min.to_value(ty), max.to_value(ty))),
            Self::Bytes(min, max) => Some((
                Value::varchar(&*String::from_utf8_lossy(min)),
                Value::varchar(&*String::from_utf8_lossy(max)),
            )),
            Self::Empty | Self::Unsupported => None,
        }
    }
}

/// The comparison of a condition as a filter operator.
const fn compare_op(comparison: ExpressionType) -> Option<CompareOp> {
    Some(match comparison {
        ExpressionType::CompareEqual => CompareOp::Equal,
        ExpressionType::CompareLessthan => CompareOp::LessThan,
        ExpressionType::CompareLessthanorequalto => CompareOp::LessThanOrEqual,
        ExpressionType::CompareGreaterthan => CompareOp::GreaterThan,
        ExpressionType::CompareGreaterthanorequalto => CompareOp::GreaterThanOrEqual,
        _ => return None,
    })
}

/// Clears the filters before a build (DuckDB resets the dynamic filters per run).
pub(super) fn reset_join_filters(filters: &[JoinFilter]) {
    for target in filters.iter().flat_map(|filter| &filter.targets) {
        target.min.reset();
        target.max.reset();
    }
}

/// Sets the probe-side filters from the build key ranges (DuckDB's
/// `JoinFilterPushdownInfo::FinalizeFilters`): `>= min` and `<= max`, or a single
/// comparison when every key is the same.
pub(super) fn set_join_filters(
    filters: &[JoinFilter],
    ranges: &[KeyMinMax],
    comparisons: &[ExpressionType],
    key_types: &[LogicalType],
) {
    for (filter, range) in filters.iter().zip(ranges) {
        let (Some(&comparison), Some(key_type)) =
            (comparisons.get(filter.condition), key_types.get(filter.condition))
        else {
            continue;
        };
        let Some((min, max)) = range.bounds(key_type) else {
            continue;
        };
        let Some(op) = compare_op(comparison) else {
            continue;
        };
        for target in &filter.targets {
            let cast = |value: &Value| {
                if value.logical_type() == &target.storage_type {
                    Some(value.clone())
                } else {
                    value
                        .try_cast(&target.storage_type)
                        .ok()
                        .filter(|value| !value.is_null())
                }
            };
            let (Some(min), Some(max)) = (cast(&min), cast(&max)) else {
                continue;
            };
            if min == max {
                // A single key: one comparison filter (it also covers equality).
                target.min.set(op, min);
                target.max.reset();
                continue;
            }
            target.min.set(CompareOp::GreaterThanOrEqual, min);
            target.max.set(CompareOp::LessThanOrEqual, max);
        }
    }
}
