//! The run-time filters a hash join sets on its probe-side scans (DuckDB's
//! `JoinFilterPushdownInfo`): the build keys' minimum and maximum, collected while
//! building, bound the probe keys that can match.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_function::cast::ValueCast;
use rustdb_planner::operator::{JoinFilter, JoinFilterTarget};

use super::bloom_filter::{BloomFilter, BloomTableFilter};
use rustdb_storage::TableFilter;
use rustdb_storage::statistics::{NumericStats, NumericValue};
use rustdb_storage::table_filter::SelectivityOptionalFilter;

use crate::join_hashtable::JoinHashTable;
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

/// Drops the filters `key` set on the probe scans (DuckDB's `GetGlobalState` clears them:
/// a join can run again, e.g. in a recursive CTE).
pub(super) fn clear_join_filters(filters: &[JoinFilter], key: usize) {
    for target in filters.iter().flat_map(|filter| &filter.targets) {
        target.dynamic_filters.clear_filters(key);
    }
}

/// What [`set_join_filters`] derives the filters from.
pub(super) struct JoinFilterInput<'a> {
    /// The join, identifying its filters in the scans' filter sets.
    pub key: usize,
    pub filters: &'a [JoinFilter],
    /// The build key range of each filter.
    pub ranges: &'a [KeyMinMax],
    /// The comparison, build key type and probe key type of each condition.
    pub comparisons: &'a [ExpressionType],
    pub key_types: &'a [LogicalType],
    pub probe_types: &'a [LogicalType],
    /// The finished hash table, whose keys form IN filters.
    pub table: &'a JoinHashTable,
    /// `dynamic_or_filter_threshold`.
    pub in_filter_threshold: u64,
    /// The probe side's estimated cardinality.
    pub probe_cardinality: u64,
    /// Whether the join uses a perfect hash table (which prunes well with min/max).
    pub is_perfect_hashtable: bool,
}

impl JoinFilterInput<'_> {
    /// DuckDB's `CanUseBloomFilter`: a single equality condition, a filtered build side
    /// and a probe side larger than the build side, without a perfect hash table.
    #[allow(clippy::cast_precision_loss, reason = "a cardinality ratio")]
    fn can_use_bloom_filter(&self, filter: &JoinFilter, op: CompareOp) -> bool {
        if self.is_perfect_hashtable || self.comparisons.len() != 1 || op != CompareOp::Equal {
            return false;
        }
        let build_to_probe_ratio = self.probe_cardinality as f64 / self.table.count() as f64;
        filter.build_side_has_filter && build_to_probe_ratio > 1.0
    }
}

/// Sets the probe-side filters from the build keys (DuckDB's
/// `JoinFilterPushdownInfo::FinalizeFilters`): with a small hash table an optional IN
/// filter of the keys; a single comparison when every key is the same, else
/// selectivity-optional `>= min` and `<= max` filters as the comparison allows. Returns
/// the targets that also get a bloom filter once the hash table is built (DuckDB's
/// `deferred_bloom_filters`).
pub(super) fn set_join_filters(input: &JoinFilterInput<'_>) -> Result<Vec<(usize, usize)>> {
    let mut bloom_targets = Vec::new();
    for (filter_index, (filter, range)) in input.filters.iter().zip(input.ranges).enumerate() {
        let (Some(&comparison), Some(key_type)) = (
            input.comparisons.get(filter.condition),
            input.key_types.get(filter.condition),
        ) else {
            continue;
        };
        let Some(op) = compare_op(comparison) else {
            continue;
        };
        for (target_index, target) in filter.targets.iter().enumerate() {
            // The range cast to the scanned type (`DefaultTryCastAs`).
            let cast = |value: &Value| {
                if value.logical_type() == &target.storage_type {
                    Some(value.clone())
                } else {
                    value.try_cast(&target.storage_type).ok()
                }
            };
            let Some((min, max)) = range.bounds(key_type) else {
                continue;
            };
            let (Some(min), Some(max)) = (cast(&min), cast(&max)) else {
                continue;
            };
            if min.is_null() || max.is_null() {
                continue;
            }
            let push = |filter: TableFilter| {
                target
                    .dynamic_filters
                    .push_filter(input.key, target.column_index, filter);
            };
            let count = input.table.count() as u64;
            if count > 1
                && count <= input.in_filter_threshold
                && op == CompareOp::Equal
                && let Some(values) =
                    in_filter_values(input.table, filter.condition, key_type, &target.storage_type)?
            {
                push(TableFilter::Optional(Some(Box::new(TableFilter::In(values)))));
            }
            if min == max {
                // A single key: one comparison (it also covers equality).
                push(TableFilter::ConstantComparison { op, constant: min });
                continue;
            }
            let selectivity_optional = |op: CompareOp, constant: Value| {
                TableFilter::SelectivityOptional(Box::new(SelectivityOptionalFilter::new(
                    TableFilter::ConstantComparison { op, constant },
                    SelectivityOptionalFilter::MIN_MAX_THRESHOLD,
                    SelectivityOptionalFilter::MIN_MAX_CHECK_N,
                )))
            };
            if matches!(
                op,
                CompareOp::Equal | CompareOp::GreaterThan | CompareOp::GreaterThanOrEqual
            ) {
                push(selectivity_optional(CompareOp::GreaterThanOrEqual, min));
            }
            if matches!(
                op,
                CompareOp::Equal | CompareOp::LessThan | CompareOp::LessThanOrEqual
            ) {
                push(selectivity_optional(CompareOp::LessThanOrEqual, max));
            }
            let has_cast = input.probe_types.get(filter.condition) != Some(&target.storage_type);
            if !has_cast && input.can_use_bloom_filter(filter, op) {
                bloom_targets.push((filter_index, target_index));
            }
        }
    }
    Ok(bloom_targets)
}

/// Pushes `column IN BF(key)` into a target scan (DuckDB's `PushBloomFilter`), as a
/// selectivity-optional filter.
pub(super) fn push_bloom_filter(
    target: &JoinFilterTarget,
    key: usize,
    bloom_filter: &Arc<BloomFilter>,
    key_column_name: &[u8],
) {
    let filter = TableFilter::Expression(Arc::new(BloomTableFilter::new(
        Arc::clone(bloom_filter),
        key_column_name.into(),
    )));
    target.dynamic_filters.push_filter(
        key,
        target.column_index,
        TableFilter::SelectivityOptional(Box::new(SelectivityOptionalFilter::new(
            filter,
            SelectivityOptionalFilter::BF_THRESHOLD,
            SelectivityOptionalFilter::BF_CHECK_N,
        ))),
    );
}

/// The distinct build keys of condition `condition` cast to `storage_type`, for an IN
/// filter (DuckDB's `PushInFilter`); `None` if a key does not cast, a key is NULL, or
/// the integer keys form a dense range (which the min/max filters cover).
fn in_filter_values(
    table: &JoinHashTable,
    condition: usize,
    key_type: &LogicalType,
    storage_type: &LogicalType,
) -> Result<Option<Vec<Value>>> {
    let mut values: Vec<Value> = Vec::new();
    let mut seen: HashSet<Value> = HashSet::new();
    let mut castable = true;
    table.scan_key_column_at(condition, key_type, |rows, keys| {
        for row in 0..rows.len() {
            let mut value = keys.get_value(row);
            if value.logical_type() != storage_type {
                let Ok(cast) = value.try_cast(storage_type) else {
                    castable = false;
                    return Ok(false);
                };
                value = cast;
            }
            if seen.insert(value.clone()) {
                values.push(value);
            }
        }
        Ok(true)
    })?;
    if !castable || values.is_empty() || values.iter().any(Value::is_null) {
        return Ok(None);
    }
    // `FilterCombiner::IsDenseRange` sorts integer lists.
    let integral = storage_type.is_integral() && storage_type.id() != LogicalTypeId::UHugeInt;
    if integral {
        values.sort_by_key(|value| value.as_i128().unwrap_or_default());
        let dense = values.windows(2).all(|pair| {
            pair[1]
                .as_i128()
                .zip(pair[0].as_i128())
                .and_then(|(b, a)| b.checked_sub(a))
                == Some(1)
        });
        if dense {
            return Ok(None);
        }
    }
    Ok(Some(values))
}
