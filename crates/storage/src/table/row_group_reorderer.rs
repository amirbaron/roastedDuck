//! Scanning a table's row groups in the order of a column's statistics (DuckDB's
//! `RowGroupReorderer`).
//!
//! An `ORDER BY column LIMIT n` scans the row groups whose minimum (ascending) or maximum
//! (descending) comes first before the others, so the Top-N boundary tightens early and
//! the zone maps of the remaining row groups prune them. Row groups that cannot hold any
//! of the first `n` rows, because the row groups before them already hold `n` qualifying
//! rows, are left out; row groups that an `OFFSET` skips entirely are left out as well.

use std::cmp::Ordering;

use rustdb_row::sort::{NullOrder, OrderType};
use rustdb_types::Value;

use super::chunk_info::ScanOptions;
use super::row_group::RowGroup;
use super::segment_tree::SegmentNode;
use crate::VECTOR_SIZE;
use crate::statistics::BaseStatistics;
use crate::transaction::TransactionData;

/// The statistic that orders the row groups (DuckDB's `OrderByStatistics`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderByStatistics {
    Min,
    Max,
}

impl OrderByStatistics {
    fn opposite(self) -> Self {
        match self {
            Self::Min => Self::Max,
            Self::Max => Self::Min,
        }
    }
}

/// How the statistics of the order column are read (DuckDB's `OrderByColumnType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderByColumnType {
    Numeric,
    String,
}

/// The order a scan reads its row groups in (DuckDB's `RowGroupOrderOptions`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGroupOrderOptions {
    /// The physical index of the order column.
    pub column: usize,
    pub order_by: OrderByStatistics,
    pub order_type: OrderType,
    pub null_order: NullOrder,
    pub column_type: OrderByColumnType,
    /// LIMIT plus OFFSET, if the rows above the scan are limited.
    pub row_limit: Option<u64>,
    /// Row groups the OFFSET skips entirely.
    pub row_group_offset: u64,
    /// NULL-only row groups the OFFSET skips (`NULLS FIRST`).
    pub leading_null_group_offset: u64,
}

/// What an OFFSET leaves of the row groups (DuckDB's `OffsetPruningResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OffsetPruningResult {
    /// The OFFSET still to apply to the rows of the remaining row groups.
    pub offset_remainder: u64,
    pub pruned_row_group_count: u64,
    pub leading_null_group_offset: u64,
}

/// A row group of a table for offset pruning: its row count, exact if no version
/// information can hide rows (DuckDB's `PartitionStatistics`), and the statistics of the
/// order column.
#[derive(Debug)]
pub struct RowGroupPartition {
    pub count: u64,
    pub exact: bool,
    pub stats: Option<BaseStatistics>,
}

fn is_null_only(stats: &BaseStatistics) -> bool {
    !stats.can_have_no_null()
}

/// Whether `v1` comes after `v2` in the scan order (`CompareValues`); never for NULLs.
fn compare_values(v1: &Value, v2: Option<&Value>, order: OrderByStatistics) -> bool {
    let Some(v2) = v2 else {
        return false;
    };
    match v1.compare_not_distinct(v2) {
        Some(Ordering::Less) => order == OrderByStatistics::Max,
        Some(Ordering::Greater) => order == OrderByStatistics::Min,
        _ => false,
    }
}

fn equal_values(v1: &Value, v2: &Value) -> bool {
    v1.compare_not_distinct(v2) == Some(Ordering::Equal)
}

/// The minimum or maximum of `stats` (`RowGroupReorderer::RetrieveStat`); `None` when
/// unknown.
pub fn retrieve_stat(
    stats: &BaseStatistics,
    order_by: OrderByStatistics,
    column_type: OrderByColumnType,
) -> Option<Value> {
    match column_type {
        OrderByColumnType::Numeric => {
            if !stats.numeric()?.has_min_max() {
                return None;
            }
            match order_by {
                OrderByStatistics::Min => stats.numeric_min(),
                OrderByStatistics::Max => stats.numeric_max(),
            }
        }
        OrderByColumnType::String => {
            // Without non-NULL values the bounds mean nothing.
            if !stats.can_have_no_null() {
                return None;
            }
            let string = stats.string()?;
            Some(Value::blob(match order_by {
                OrderByStatistics::Min => string.min_bytes(),
                OrderByStatistics::Max => string.max_bytes(),
            }))
        }
    }
}

/// The rows of `row_group` the transaction sees (`RowGroup::GetVisibleRowCount`).
fn visible_row_count(row_group: &RowGroup, transaction: TransactionData) -> u64 {
    let count = row_group.count();
    let Some(versions) = row_group.version_info() else {
        return count;
    };
    let options = ScanOptions::new(transaction);
    let mut sel = Vec::new();
    let mut visible = 0;
    for (vector_index, start) in (0..count).step_by(VECTOR_SIZE as usize).enumerate() {
        let max_count = (count - start).min(VECTOR_SIZE) as usize;
        visible += versions.sel_vector(options, vector_index, &mut sel, max_count) as u64;
    }
    visible
}

/// How many rows of `row_group` surely have a non-NULL order value
/// (`GetMinimumQualifyingTupleCount`).
fn minimum_qualifying_tuple_count(
    row_group: &RowGroup,
    stats: &BaseStatistics,
    column_type: OrderByColumnType,
    transaction: TransactionData,
) -> u64 {
    if !stats.can_have_null() {
        return visible_row_count(row_group, transaction);
    }
    if column_type == OrderByColumnType::Numeric {
        let Some(numeric) = stats.numeric() else {
            return 0;
        };
        if !numeric.has_min_max() {
            return 0;
        }
        return if numeric.is_constant() { 1 } else { 2 };
    }
    // String bounds may be truncated: nothing is known about the values.
    0
}

/// A row group with trustworthy statistics, keyed by its order statistic.
struct OrderedEntry {
    key: Value,
    position: usize,
    row_group: SegmentNode<RowGroup>,
    stats: BaseStatistics,
}

/// Appends the row groups of `entries` (in scan order) until the row groups before one
/// surely hold `row_limit` qualifying rows (`AddRowGroups`).
fn add_row_groups(
    entries: &[&OrderedEntry],
    ordered: &mut Vec<usize>,
    row_limit: u64,
    column_type: OrderByColumnType,
    stat_type: OrderByStatistics,
    transaction: TransactionData,
) {
    let opposite = stat_type.opposite();
    let mut last_unresolved = 0;
    let mut last_unresolved_boundary = retrieve_stat(&entries[0].stats, opposite, column_type);
    let mut qualifying_tuples = 0;
    let mut qualify_later = 0;
    let mut last_unresolved_sum = minimum_qualifying_tuple_count(
        &entries[0].row_group.node,
        &entries[0].stats,
        column_type,
        transaction,
    );
    for (index, entry) in entries.iter().enumerate() {
        while last_unresolved != index {
            if !compare_values(&entry.key, last_unresolved_boundary.as_ref(), stat_type) {
                if equal_values(&entry.key, &entries[index - 1].key) {
                    // The same order value: a qualifying row can only be counted later.
                    qualify_later += 1;
                } else {
                    // The row groups overlap: one more row surely qualifies.
                    qualifying_tuples += qualify_later + 1;
                    qualify_later = 0;
                }
                break;
            }
            // The row groups do not overlap: the earlier rows all qualify.
            qualifying_tuples = last_unresolved_sum;
            last_unresolved += 1;
            let upcoming = entries[last_unresolved];
            last_unresolved_sum += minimum_qualifying_tuple_count(
                &upcoming.row_group.node,
                &upcoming.stats,
                column_type,
                transaction,
            );
            last_unresolved_boundary = retrieve_stat(&upcoming.stats, opposite, column_type);
        }
        if qualifying_tuples >= row_limit {
            return;
        }
        ordered.push(entry.position);
    }
}

/// The positions of the row groups of `row_groups` in the order `options` asks for
/// (`RowGroupReorderer::GetRootSegment`): NULL-only row groups and row groups without
/// usable statistics come last (`NULLS LAST`) or first (`NULLS FIRST`).
pub fn order_row_groups(
    row_groups: &[SegmentNode<RowGroup>],
    options: &RowGroupOrderOptions,
    transaction: TransactionData,
) -> Vec<usize> {
    let mut null_only = Vec::new();
    let mut ambiguous = Vec::new();
    let mut entries = Vec::new();
    for (position, row_group) in row_groups.iter().enumerate() {
        let Ok(stats) = row_group.node.column_statistics(options.column) else {
            ambiguous.push(position);
            continue;
        };
        if is_null_only(&stats) {
            null_only.push(position);
            continue;
        }
        let key = retrieve_stat(&stats, options.order_by, options.column_type);
        let Some(key) =
            key.filter(|_| !(options.null_order == NullOrder::NullsFirst && stats.can_have_null()))
        else {
            // No trustworthy order statistic, or NULLs that NULLS FIRST must scan first.
            ambiguous.push(position);
            continue;
        };
        entries.push(OrderedEntry {
            key,
            position,
            row_group: row_group.clone(),
            stats,
        });
    }
    // Equal keys keep the storage order (a multimap's insertion order).
    entries.sort_by(|a, b| a.key.compare_not_distinct(&b.key).unwrap_or(Ordering::Equal));
    let mut scan_order: Vec<&OrderedEntry> = entries.iter().collect();
    if options.order_type == OrderType::Descending {
        scan_order.reverse();
    }
    let remaining = scan_order
        .get(options.row_group_offset as usize..)
        .unwrap_or_default();

    let mut ordered = Vec::with_capacity(row_groups.len());
    let add_ordered = |ordered: &mut Vec<usize>| {
        if remaining.is_empty() {
            return;
        }
        match options.row_limit {
            Some(row_limit) => add_row_groups(
                remaining,
                ordered,
                row_limit,
                options.column_type,
                options.order_by,
                transaction,
            ),
            None => ordered.extend(remaining.iter().map(|entry| entry.position)),
        }
    };
    if options.null_order == NullOrder::NullsFirst {
        ordered.extend(null_only.iter().skip(options.leading_null_group_offset as usize));
        ordered.extend(&ambiguous);
        add_ordered(&mut ordered);
    } else {
        add_ordered(&mut ordered);
        ordered.extend(&ambiguous);
        ordered.extend(&null_only);
    }
    ordered
}

/// The row groups an OFFSET skips entirely, scanning in the order of `order_by` statistics
/// (`RowGroupReorderer::GetOffsetAfterPruning`).
pub fn offset_after_pruning(
    order_by: OrderByStatistics,
    column_type: OrderByColumnType,
    order_type: OrderType,
    null_order: NullOrder,
    row_offset: u64,
    partitions: Vec<RowGroupPartition>,
) -> OffsetPruningResult {
    let no_pruning = |offset_remainder, leading_null_group_offset| OffsetPruningResult {
        offset_remainder,
        pruned_row_group_count: 0,
        leading_null_group_offset,
    };
    let mut entries: Vec<(Value, u64, BaseStatistics)> = Vec::new();
    let mut new_row_offset = row_offset;
    let mut leading_null_group_offset = 0;
    let mut encountered_maybe_null_group = false;
    for partition in partitions {
        if !partition.exact {
            return no_pruning(row_offset, 0);
        }
        let Some(stats) = partition.stats else {
            return no_pruning(new_row_offset, leading_null_group_offset);
        };
        if null_order == NullOrder::NullsFirst && is_null_only(&stats) {
            if new_row_offset < partition.count {
                return no_pruning(new_row_offset, leading_null_group_offset);
            }
            new_row_offset -= partition.count;
            leading_null_group_offset += 1;
            continue;
        }
        let Some(value) = retrieve_stat(&stats, order_by, column_type) else {
            if null_order == NullOrder::NullsLast && is_null_only(&stats) {
                // Scanned after every row group with values, beyond the offset.
                continue;
            }
            if null_order == NullOrder::NullsFirst {
                encountered_maybe_null_group = true;
                continue;
            }
            return no_pruning(new_row_offset, leading_null_group_offset);
        };
        if null_order == NullOrder::NullsFirst && stats.can_have_null() {
            // Scanned before the row groups without NULLs; their NULL count is unknown.
            encountered_maybe_null_group = true;
            continue;
        }
        entries.push((value, partition.count, stats));
    }
    if null_order == NullOrder::NullsFirst && encountered_maybe_null_group {
        return no_pruning(new_row_offset, leading_null_group_offset);
    }
    entries.sort_by(|a, b| a.0.compare_not_distinct(&b.0).unwrap_or(Ordering::Equal));
    if order_type == OrderType::Descending {
        entries.reverse();
    }
    let mut result = find_offset_prunable_row_groups(&entries, order_by, column_type, new_row_offset);
    result.leading_null_group_offset = leading_null_group_offset;
    result
}

/// The leading row groups (in scan order) that hold only rows before the offset
/// (`FindOffsetPrunableChunks`).
fn find_offset_prunable_row_groups(
    entries: &[(Value, u64, BaseStatistics)],
    order_by: OrderByStatistics,
    column_type: OrderByColumnType,
    row_offset: u64,
) -> OffsetPruningResult {
    let mut result = OffsetPruningResult {
        offset_remainder: row_offset,
        pruned_row_group_count: 0,
        leading_null_group_offset: 0,
    };
    let Some(first) = entries.first() else {
        return result;
    };
    let opposite = order_by.opposite();
    let mut last_unresolved = 0;
    let mut last_unresolved_boundary = retrieve_stat(&first.2, opposite, column_type);
    let mut seen_tuples = 0;
    for (index, (key, count, _)) in entries.iter().enumerate() {
        seen_tuples += count;
        while last_unresolved != index {
            if !compare_values(key, last_unresolved_boundary.as_ref(), order_by) {
                break;
            }
            // The row groups do not overlap.
            let (_, pruned_count, pruned_stats) = &entries[last_unresolved];
            if pruned_stats.can_have_null() {
                // Only a prefix of row groups without NULLs can be skipped.
                return result;
            }
            result.pruned_row_group_count += 1;
            result.offset_remainder -= pruned_count;
            last_unresolved += 1;
            let Some(upcoming) = entries.get(last_unresolved) else {
                return result;
            };
            last_unresolved_boundary = retrieve_stat(&upcoming.2, opposite, column_type);
        }
        if seen_tuples > row_offset {
            break;
        }
    }
    result
}
