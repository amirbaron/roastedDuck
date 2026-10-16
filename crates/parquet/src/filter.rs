//! Row group pruning (the zone map and bloom filter checks of DuckDB's
//! `ParquetReader::PrepareRowGroupBuffer`, `CheckParquetStringFilter`,
//! `CheckParquetFloatFilter` and `ParquetStatisticsUtils::BloomFilterExcludes`).

use std::cmp::Ordering;

use rustdb_common::Result;
use rustdb_function::cast::CastFunctionSet;
use rustdb_storage::TableFilter;
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, NumericValue, StatsData};
use rustdb_types::compare::CompareOp;
use rustdb_types::{LogicalTypeId, Value, Vector};

use crate::metadata::{read_bloom_filter, value_hash};
use crate::reader::ParquetReader;
use crate::schema::SchemaKind;
use crate::statistics::transform_column_statistics;
use crate::thrift::{ColumnMetaData, Statistics};
use crate::writer::bloom::BloomFilter;

/// `StringValueComparison`: compares the first `len` bytes of `data` with `comparison`.
fn string_value_comparison(data: &[u8], len: usize, comparison: &[u8]) -> Ordering {
    for i in 0..len {
        let left = data.get(i).copied().unwrap_or(0);
        let right = comparison.get(i).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// `StringStats::CheckZonemap` over the complete min/max strings of a column chunk.
fn check_string_zonemap(min: &[u8], max: &[u8], op: CompareOp, constant: &[u8]) -> FilterPropagateResult {
    use FilterPropagateResult::{FilterAlwaysFalse, FilterAlwaysTrue, NoPruningPossible};
    let min_comp = string_value_comparison(constant, min.len().min(constant.len()), min);
    let max_comp = string_value_comparison(constant, max.len().min(constant.len()), max);
    match op {
        CompareOp::Equal => {
            if min_comp != Ordering::Less && max_comp != Ordering::Greater {
                NoPruningPossible
            } else {
                FilterAlwaysFalse
            }
        }
        CompareOp::NotEqual => {
            if min_comp == Ordering::Less || max_comp == Ordering::Greater {
                FilterAlwaysTrue
            } else {
                NoPruningPossible
            }
        }
        CompareOp::GreaterThanOrEqual | CompareOp::GreaterThan => {
            if max_comp == Ordering::Greater {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
        CompareOp::LessThan | CompareOp::LessThanOrEqual => {
            if min_comp == Ordering::Less {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
    }
}

/// `CheckParquetStringFilter`: string comparisons use the full-length statistics.
fn check_string_filter(
    stats: &BaseStatistics,
    parquet: &Statistics,
    filter: &TableFilter,
) -> FilterPropagateResult {
    use FilterPropagateResult::{FilterAlwaysFalse, FilterAlwaysTrue, NoPruningPossible};
    match filter {
        TableFilter::And(children) => {
            let mut result = FilterAlwaysTrue;
            for child in children {
                let child_result = check_string_filter(stats, parquet, child);
                if child_result == FilterAlwaysFalse {
                    return FilterAlwaysFalse;
                }
                if child_result != result {
                    result = NoPruningPossible;
                }
            }
            result
        }
        TableFilter::ConstantComparison { op, constant } => {
            let (Some(min), Some(max)) = (&parquet.min_value, &parquet.max_value) else {
                return filter.check_statistics(stats);
            };
            let text = constant.to_varchar().unwrap_or_default();
            check_string_zonemap(min, max, *op, text.as_bytes())
        }
        _ => filter.check_statistics(stats),
    }
}

/// `CheckParquetFloatFilter`: NaNs lie outside the min/max of the file.
fn check_float_filter(
    stats: &BaseStatistics,
    filter: &TableFilter,
    ty: &rustdb_types::LogicalType,
) -> FilterPropagateResult {
    let mut nan_stats = BaseStatistics::create_unknown(ty.clone());
    let nan = if ty.id() == LogicalTypeId::Float {
        Value::float(f32::NAN)
    } else {
        Value::double(f64::NAN)
    };
    if let StatsData::Numeric(numeric) = nan_stats.data_mut() {
        numeric.min = NumericValue::from_value(&nan);
        numeric.max = NumericValue::from_value(&nan);
    }
    let nan_prune = filter.check_statistics(&nan_stats);
    let prune = filter.check_statistics(stats);
    if prune == FilterPropagateResult::NoPruningPossible
        || nan_prune == FilterPropagateResult::NoPruningPossible
    {
        return FilterPropagateResult::NoPruningPossible;
    }
    if prune == nan_prune {
        prune
    } else {
        FilterPropagateResult::NoPruningPossible
    }
}

/// `HasFilterConstants`: whether the filter tests equality with a constant.
fn has_filter_constants(filter: &TableFilter) -> bool {
    match filter {
        TableFilter::ConstantComparison { op, constant } => *op == CompareOp::Equal && !constant.is_null(),
        TableFilter::And(children) | TableFilter::Or(children) => children.iter().any(has_filter_constants),
        _ => false,
    }
}

/// `ApplyBloomFilter`: whether the filter excludes every value of the bloom filter.
fn apply_bloom_filter(filter: &TableFilter, bloom: &BloomFilter) -> bool {
    match filter {
        TableFilter::ConstantComparison { op, constant } => {
            let hash = value_hash(constant);
            hash > 0 && !bloom.check(hash) && *op == CompareOp::Equal
        }
        TableFilter::And(children) => children.iter().any(|child| apply_bloom_filter(child, bloom)),
        TableFilter::Or(children) => children.iter().all(|child| apply_bloom_filter(child, bloom)),
        _ => false,
    }
}

/// `ParquetStatisticsUtils::BloomFilterSupported`.
fn bloom_filter_supported(id: LogicalTypeId) -> bool {
    use LogicalTypeId as L;
    matches!(
        id,
        L::TinyInt
            | L::UTinyInt
            | L::SmallInt
            | L::USmallInt
            | L::Integer
            | L::UInteger
            | L::BigInt
            | L::UBigInt
            | L::Float
            | L::Double
            | L::Varchar
            | L::Blob
    )
}

/// `BloomFilterExcludes`.
fn bloom_filter_excludes(
    reader: &ParquetReader,
    filter: &TableFilter,
    meta: &ColumnMetaData,
) -> Result<bool> {
    if !has_filter_constants(filter) {
        return Ok(false);
    }
    let Some(bloom) = read_bloom_filter(reader, meta)? else {
        return Ok(false);
    };
    Ok(apply_bloom_filter(filter, &bloom))
}

/// Whether the statistics of row group `group_index` show that no row passes `filters`
/// (pairs of a file column and the filter on it).
pub fn prune_row_group(
    reader: &ParquetReader,
    group_index: usize,
    filters: &[(usize, &TableFilter)],
    can_have_nan: bool,
    casts: &CastFunctionSet,
) -> Result<bool> {
    let metadata = reader.metadata();
    let Some(group) = metadata.row_groups.get(group_index) else {
        return Ok(false);
    };
    for &(file_column, filter) in filters {
        let Some(schema) = reader.root_schema.children.get(file_column) else {
            continue;
        };
        let Some(stats) = transform_column_statistics(schema, &group.columns, can_have_nan, casts)? else {
            continue;
        };
        let is_leaf = schema.kind == SchemaKind::Column && schema.children.is_empty();
        let meta = group
            .columns
            .get(schema.column_index)
            .and_then(|column| column.meta_data.as_ref());
        let parquet_stats = meta.and_then(|meta| meta.statistics.as_ref());
        let has_min_max =
            parquet_stats.is_some_and(|stats| stats.min_value.is_some() && stats.max_value.is_some());
        let type_id = schema.logical_type.id();
        let mut result = match parquet_stats {
            Some(parquet) if is_leaf && has_min_max && type_id == LogicalTypeId::Varchar => {
                check_string_filter(&stats, parquet, filter)
            }
            Some(_)
                if is_leaf
                    && has_min_max
                    && matches!(type_id, LogicalTypeId::Float | LogicalTypeId::Double)
                    && can_have_nan =>
            {
                check_float_filter(&stats, filter, &schema.logical_type)
            }
            _ => filter.check_statistics(&stats),
        };
        if result == FilterPropagateResult::NoPruningPossible
            && is_leaf
            && bloom_filter_supported(type_id)
            && let Some(meta) = meta
            && bloom_filter_excludes(reader, filter, meta)?
        {
            result = FilterPropagateResult::FilterAlwaysFalse;
        }
        if result == FilterPropagateResult::FilterAlwaysFalse {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a constant value passes `filter` (for filename and hive partition columns).
pub fn constant_passes(filter: &TableFilter, value: &Value) -> Result<bool> {
    let vector = Vector::constant(value)?;
    let mut sel = vec![0u32];
    filter.select(&vector, 1, &mut sel)?;
    Ok(!sel.is_empty())
}
