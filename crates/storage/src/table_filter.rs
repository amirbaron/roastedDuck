//! Filters pushed down into table scans (DuckDB's `planner/table_filter.hpp` and
//! `planner/filter/*`).
//!
//! A scan checks each filter against segment and row-group statistics (zone maps) to skip
//! data, then applies it row by row to the scanned vectors before the other columns are
//! fetched. Filters on arbitrary expressions are evaluated through the
//! [`ExpressionFilter`] hook, which the execution layer implements.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_types::compare::{CompareOp, SqlOrd};

use rustdb_types::{NativeType, PhysicalType, Sel, StringT, ValidityMask, Value, Vector};

use crate::statistics::{BaseStatistics, FilterPropagateResult, NumericValue, StatsData};

/// A filter over arbitrary expressions, evaluated by the execution layer
/// (DuckDB's `ExpressionFilter`).
pub trait ExpressionFilter: Send + Sync + fmt::Debug {
    /// Keeps the rows of `sel` (positions into `vector`) for which the expression is true.
    fn select(&self, vector: &Vector, count: usize, sel: &mut Vec<u32>) -> Result<()>;

    /// What statistics say about the expression; by default nothing.
    fn check_statistics(&self, stats: &BaseStatistics) -> FilterPropagateResult {
        let _ = stats;
        FilterPropagateResult::NoPruningPossible
    }

    /// The filter as SQL text over `column_name`.
    fn to_sql(&self, column_name: &str) -> String;

    /// The filter as `Any`, so that layers above storage can recognize their own
    /// filter types.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A comparison whose constant is set at run time, e.g. by a join (DuckDB's
/// `DynamicFilterData`). Until it is set the filter passes everything.
#[derive(Debug, Default)]
pub struct DynamicFilterData {
    filter: Mutex<Option<(CompareOp, Value)>>,
}

impl DynamicFilterData {
    /// Sets (or replaces) the comparison.
    pub fn set(&self, op: CompareOp, constant: Value) {
        *self.filter.lock().unwrap_or_else(PoisonError::into_inner) = Some((op, constant));
    }

    /// The current comparison, if any.
    pub fn get(&self) -> Option<(CompareOp, Value)> {
        self.filter.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Clears the comparison (DuckDB's `DynamicFilterData::Reset`).
    pub fn reset(&self) {
        *self.filter.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The selectivity a [`SelectivityOptionalFilter`] measured in one scan (DuckDB's
/// `SelectivityOptionalFilterState::SelectivityStats`). Each scan has its own: cloning
/// the filter for a scan starts fresh statistics.
#[derive(Debug, Default)]
pub struct SelectivityStats {
    tuples_accepted: AtomicU64,
    tuples_processed: AtomicU64,
    vectors_processed: AtomicU64,
    paused: AtomicBool,
}

impl Clone for SelectivityStats {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// A filter evaluated on rows only while it removes enough of them (DuckDB's
/// `SelectivityOptionalFilter`): after `check_n` vectors with a selectivity of at least
/// `threshold` (the fraction of rows kept), the scan stops evaluating it.
#[derive(Clone, Debug)]
pub struct SelectivityOptionalFilter {
    pub child: TableFilter,
    pub threshold: f32,
    pub check_n: u64,
    stats: SelectivityStats,
}

impl SelectivityOptionalFilter {
    /// DuckDB's `MIN_MAX_THRESHOLD` and `MIN_MAX_CHECK_N` for join min/max filters.
    pub const MIN_MAX_THRESHOLD: f32 = 0.75;
    pub const MIN_MAX_CHECK_N: u64 = 30;
    /// DuckDB's `BF_THRESHOLD` and `BF_CHECK_N` for join bloom filters.
    pub const BF_THRESHOLD: f32 = 0.25;
    pub const BF_CHECK_N: u64 = 75;

    pub fn new(child: TableFilter, threshold: f32, check_n: u64) -> Self {
        Self {
            child,
            threshold,
            check_n,
            stats: SelectivityStats::default(),
        }
    }

    /// DuckDB's `SelectivityOptionalFilter::FilterSelection` with `SelectivityStats::Update`.
    fn select(&self, vector: &Vector, count: usize, sel: &mut Vec<u32>) -> Result<()> {
        let stats = &self.stats;
        if stats.paused.load(AtomicOrdering::Relaxed) {
            return Ok(());
        }
        let before = sel.len() as u64;
        self.child.select(vector, count, sel)?;
        let vectors = stats.vectors_processed.load(AtomicOrdering::Relaxed);
        if vectors < self.check_n {
            let accepted = stats
                .tuples_accepted
                .fetch_add(sel.len() as u64, AtomicOrdering::Relaxed)
                + sel.len() as u64;
            let processed = stats.tuples_processed.fetch_add(before, AtomicOrdering::Relaxed) + before;
            stats
                .vectors_processed
                .store(vectors + 1, AtomicOrdering::Relaxed);
            if vectors + 1 == self.check_n {
                #[allow(clippy::cast_precision_loss, reason = "a selectivity ratio")]
                let selectivity = if processed == 0 {
                    1.0
                } else {
                    accepted as f64 / processed as f64
                };
                if selectivity >= f64::from(self.threshold) {
                    stats.paused.store(true, AtomicOrdering::Relaxed);
                }
            }
        }
        Ok(())
    }
}

/// The filters operators set on a scan during execution, by operator (DuckDB's
/// `DynamicTableFilterSet`); keyed by position in the scanned columns.
#[derive(Debug, Default)]
pub struct DynamicTableFilterSet {
    filters: Mutex<Vec<(usize, TableFilterSet)>>,
}

impl DynamicTableFilterSet {
    /// Drops the filters operator `op` set (`ClearFilters`).
    pub fn clear_filters(&self, op: usize) {
        self.filters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(key, _)| *key != op);
    }

    /// Adds a filter of operator `op` on scan column `column` (`PushFilter`).
    pub fn push_filter(&self, op: usize, column: usize, filter: TableFilter) {
        let mut filters = self.filters.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, set)) = filters.iter_mut().find(|(key, _)| *key == op) {
            set.push_filter(column, filter);
            return;
        }
        let mut set = TableFilterSet::default();
        set.push_filter(column, filter);
        filters.push((op, set));
    }

    /// Whether any operator set a filter (`HasFilters`).
    pub fn has_filters(&self) -> bool {
        !self
            .filters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// `existing` with every operator's filters AND-ed in, or `None` without any filter
    /// (`GetFinalTableFilters`).
    pub fn final_table_filters(&self, existing: Option<&TableFilterSet>) -> Option<TableFilterSet> {
        let mut result = existing.cloned().unwrap_or_default();
        for (_, set) in self.filters.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            for (&column, filter) in &set.filters {
                result.push_filter(column, filter.clone());
            }
        }
        (!result.is_empty()).then_some(result)
    }
}

/// A filter on one column of a table scan (DuckDB's `TableFilter` hierarchy).
#[derive(Clone, Debug)]
pub enum TableFilter {
    /// `column <op> constant`; the constant is never NULL.
    ConstantComparison {
        op: CompareOp,
        constant: Value,
    },
    IsNull,
    IsNotNull,
    /// True if any child is true.
    Or(Vec<TableFilter>),
    /// True if every child is true.
    And(Vec<TableFilter>),
    /// A filter on field `child_index` of a STRUCT column.
    StructExtract {
        child_index: usize,
        child_name: String,
        child_filter: Box<TableFilter>,
    },
    /// Only used to skip row groups and segments; rows are not filtered by it.
    Optional(Option<Box<TableFilter>>),
    /// An optional filter that also filters rows while it is selective (inside an AND;
    /// on its own, like any optional filter, it only prunes).
    SelectivityOptional(Box<SelectivityOptionalFilter>),
    /// `column IN (constants...)`.
    In(Vec<Value>),
    /// A comparison filled in during execution.
    Dynamic(Arc<DynamicFilterData>),
    /// An arbitrary boolean expression over the column.
    Expression(Arc<dyn ExpressionFilter>),
}

fn comparison_name(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Equal => "=",
        CompareOp::NotEqual => "!=",
        CompareOp::LessThan => "<",
        CompareOp::LessThanOrEqual => "<=",
        CompareOp::GreaterThan => ">",
        CompareOp::GreaterThanOrEqual => ">=",
    }
}

impl TableFilter {
    /// What the statistics of a set of values say about this filter
    /// (`TableFilter::CheckStatistics`).
    pub fn check_statistics(&self, stats: &BaseStatistics) -> FilterPropagateResult {
        use FilterPropagateResult::{FilterAlwaysFalse, FilterAlwaysTrue, NoPruningPossible};
        match self {
            Self::ConstantComparison { op, constant } => {
                if !stats.can_have_no_null() {
                    return FilterAlwaysFalse;
                }
                let result = check_constants(stats, *op, std::slice::from_ref(constant));
                if result == FilterAlwaysTrue && stats.can_have_null() {
                    NoPruningPossible
                } else {
                    result
                }
            }
            Self::In(values) => {
                if !stats.can_have_no_null() {
                    return FilterAlwaysFalse;
                }
                check_constants(stats, CompareOp::Equal, values)
            }
            Self::IsNull => {
                if !stats.can_have_null() {
                    FilterAlwaysFalse
                } else if !stats.can_have_no_null() {
                    FilterAlwaysTrue
                } else {
                    NoPruningPossible
                }
            }
            Self::IsNotNull => {
                if !stats.can_have_no_null() {
                    FilterAlwaysFalse
                } else if !stats.can_have_null() {
                    FilterAlwaysTrue
                } else {
                    NoPruningPossible
                }
            }
            Self::Or(children) => {
                for child in children {
                    match child.check_statistics(stats) {
                        NoPruningPossible => return NoPruningPossible,
                        FilterAlwaysTrue => return FilterAlwaysTrue,
                        _ => {}
                    }
                }
                FilterAlwaysFalse
            }
            Self::And(children) => {
                let mut result = FilterAlwaysTrue;
                for child in children {
                    let child_result = child.check_statistics(stats);
                    if child_result == FilterAlwaysFalse {
                        return FilterAlwaysFalse;
                    }
                    if child_result != result {
                        result = NoPruningPossible;
                    }
                }
                result
            }
            Self::StructExtract {
                child_index,
                child_filter,
                ..
            } => match stats
                .struct_children()
                .and_then(|children| children.get(*child_index))
            {
                Some(child) => child_filter.check_statistics(child),
                None => NoPruningPossible,
            },
            Self::Optional(child) => child
                .as_ref()
                .map_or(NoPruningPossible, |child| child.check_statistics(stats)),
            Self::SelectivityOptional(filter) => filter.child.check_statistics(stats),
            Self::Dynamic(data) => match data.get() {
                Some((op, constant)) => Self::ConstantComparison { op, constant }.check_statistics(stats),
                None => NoPruningPossible,
            },
            Self::Expression(expression) => expression.check_statistics(stats),
        }
    }

    /// Whether this filter is only used for pruning (`OPTIONAL_FILTER`).
    pub fn is_optional(&self) -> bool {
        matches!(self, Self::Optional(_) | Self::SelectivityOptional(_))
    }

    /// Keeps the rows of `sel` (positions into the first `count` rows of `vector`) that
    /// pass the filter (`ColumnSegment::FilterSelection`).
    /// Whether the filter is a fixed function of the value that never passes NULLs, so a
    /// codec may evaluate it on stored values alone (comparisons, IN lists and their ANDs).
    pub fn is_static_value_filter(&self) -> bool {
        match self {
            Self::ConstantComparison { .. } | Self::In(_) => true,
            Self::And(children) => !children.is_empty() && children.iter().all(Self::is_static_value_filter),
            _ => false,
        }
    }

    pub fn select(&self, vector: &Vector, count: usize, sel: &mut Vec<u32>) -> Result<()> {
        match self {
            Self::ConstantComparison { op, constant } => select_constant(vector, *op, constant, sel),
            Self::IsNull | Self::IsNotNull => {
                let want_null = matches!(self, Self::IsNull);
                let vector_sel = vector.sel();
                let validity = vector.validity();
                if validity.is_all_valid() {
                    if want_null {
                        sel.clear();
                    }
                    return Ok(());
                }
                sel.retain(|&row| validity.is_valid(vector_sel.slot(row as usize)) != want_null);
                Ok(())
            }
            Self::And(children) => {
                for child in children {
                    child.select(vector, count, sel)?;
                }
                Ok(())
            }
            Self::Or(children) => {
                let mut passed = vec![false; count];
                for child in children {
                    let mut child_sel = sel.clone();
                    child.select(vector, count, &mut child_sel)?;
                    for row in child_sel {
                        passed[row as usize] = true;
                    }
                }
                sel.retain(|&row| passed[row as usize]);
                Ok(())
            }
            Self::StructExtract {
                child_index,
                child_filter,
                ..
            } => {
                let child = vector
                    .flattened(count)
                    .struct_children()
                    .and_then(|children| children.get(*child_index).cloned())
                    .ok_or_else(|| Error::internal("struct filter on a non-struct vector"))?;
                child_filter.select(&child, count, sel)
            }
            Self::Optional(_) => Ok(()),
            Self::SelectivityOptional(filter) => filter.select(vector, count, sel),
            Self::In(values) => {
                let mut passed = vec![false; count];
                for value in values {
                    let mut value_sel = sel.clone();
                    select_constant(vector, CompareOp::Equal, value, &mut value_sel)?;
                    for row in value_sel {
                        passed[row as usize] = true;
                    }
                }
                sel.retain(|&row| passed[row as usize]);
                Ok(())
            }
            Self::Dynamic(data) => match data.get() {
                Some((op, constant)) => select_constant(vector, op, &constant, sel),
                None => Ok(()),
            },
            Self::Expression(expression) => expression.select(vector, count, sel),
        }
    }

    /// The filter as SQL text over `column_name` (`TableFilter::ToString`).
    pub fn to_sql(&self, column_name: &str) -> String {
        match self {
            Self::ConstantComparison { op, constant } => {
                format!(
                    "{column_name}{}{}",
                    comparison_name(*op),
                    constant.to_sql_string()
                )
            }
            Self::IsNull => format!("{column_name} IS NULL"),
            Self::IsNotNull => format!("{column_name} IS NOT NULL"),
            Self::Or(children) => children
                .iter()
                .map(|child| child.to_sql(column_name))
                .collect::<Vec<_>>()
                .join(" OR "),
            Self::And(children) => children
                .iter()
                .map(|child| child.to_sql(column_name))
                .collect::<Vec<_>>()
                .join(" AND "),
            Self::StructExtract {
                child_index,
                child_name,
                child_filter,
            } => {
                if child_name.is_empty() {
                    child_filter.to_sql(&format!("struct_extract_at({column_name},{})", child_index + 1))
                } else {
                    child_filter.to_sql(&format!("{column_name}.{child_name}"))
                }
            }
            Self::Optional(child) => child.as_ref().map_or_else(String::new, |child| {
                format!("optional: {}", child.to_sql(column_name))
            }),
            Self::SelectivityOptional(filter) => format!("optional: {}", filter.child.to_sql(column_name)),
            Self::In(values) => {
                let list = values
                    .iter()
                    .map(rustdb_types::Value::to_sql_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{column_name} IN ({list})")
            }
            // DuckDB's `DynamicFilter::ToString`.
            Self::Dynamic(_) => format!("Dynamic Filter ({column_name})"),
            Self::Expression(expression) => expression.to_sql(column_name),
        }
    }
}

fn check_constants(stats: &BaseStatistics, op: CompareOp, constants: &[Value]) -> FilterPropagateResult {
    match stats.data() {
        StatsData::Numeric(numeric) => {
            let values: Vec<NumericValue> = constants.iter().filter_map(NumericValue::from_value).collect();
            if values.len() != constants.len() {
                return FilterPropagateResult::NoPruningPossible;
            }
            numeric.check_zonemap(op, &values)
        }
        StatsData::String(string) if stats.logical_type().physical_type() == PhysicalType::Varchar => {
            let bytes: Vec<&[u8]> = constants.iter().filter_map(Value::as_bytes).collect();
            if bytes.len() != constants.len() {
                return FilterPropagateResult::NoPruningPossible;
            }
            string.check_zonemap(op, &bytes)
        }
        _ => FilterPropagateResult::NoPruningPossible,
    }
}

/// Keeps the rows of `sel` for which `keep(row)` holds, without branching on the
/// outcome (DuckDB's `SelectFlatLoop` writes true matches the same way).
#[inline]
fn retain_rows(sel: &mut Vec<u32>, keep: impl Fn(usize) -> bool) {
    let mut kept = 0;
    for index in 0..sel.len() {
        let row = sel[index];
        sel[kept] = row;
        kept += usize::from(keep(row as usize));
    }
    sel.truncate(kept);
}

/// Keeps the rows whose value satisfies `matches`, monomorphised per comparison.
#[inline]
fn select_values<T>(
    values: &[T],
    data_sel: Sel<'_>,
    validity: &ValidityMask,
    sel: &mut Vec<u32>,
    matches: impl Fn(&T) -> bool,
) {
    let all_valid = validity.is_all_valid();
    match (data_sel, all_valid) {
        (Sel::Identity, true) => retain_rows(sel, |row| matches(&values[row])),
        (_, true) => retain_rows(sel, |row| matches(&values[data_sel.slot(row)])),
        (_, false) => retain_rows(sel, |row| {
            let slot = data_sel.slot(row);
            validity.is_valid(slot) && matches(&values[slot])
        }),
    }
}

fn select_native<T: SqlOrd + NativeType>(vector: &Vector, op: CompareOp, constant: T, sel: &mut Vec<u32>) {
    let data = vector.unified::<T>();
    let (values, data_sel, validity) = (data.values, data.sel, data.validity);
    match op {
        CompareOp::Equal => select_values(values, data_sel, validity, sel, |&v| v.sql_eq(constant)),
        CompareOp::NotEqual => select_values(values, data_sel, validity, sel, |&v| !v.sql_eq(constant)),
        CompareOp::GreaterThan => select_values(values, data_sel, validity, sel, |&v| v.sql_gt(constant)),
        CompareOp::LessThan => select_values(values, data_sel, validity, sel, |&v| constant.sql_gt(v)),
        CompareOp::GreaterThanOrEqual => {
            select_values(values, data_sel, validity, sel, |&v| !constant.sql_gt(v));
        }
        CompareOp::LessThanOrEqual => {
            select_values(values, data_sel, validity, sel, |&v| !v.sql_gt(constant));
        }
    }
}

fn select_strings(vector: &Vector, op: CompareOp, constant: &[u8], sel: &mut Vec<u32>) {
    let data = vector.unified::<StringT>();
    let (values, data_sel, validity) = (data.values, data.sel, data.validity);
    // SAFETY: `constant` outlives `target`, which is only compared within this call.
    let target = unsafe { StringT::from_raw(constant) };
    let equal = |v: &StringT| {
        v.quick_eq(&target)
            .unwrap_or_else(|| vector.string_value(v) == constant)
    };
    match op {
        CompareOp::Equal => select_values(values, data_sel, validity, sel, equal),
        CompareOp::NotEqual => select_values(values, data_sel, validity, sel, |v| !equal(v)),
        CompareOp::LessThan => select_values(values, data_sel, validity, sel, |v| {
            vector.string_value(v) < constant
        }),
        CompareOp::LessThanOrEqual => select_values(values, data_sel, validity, sel, |v| {
            vector.string_value(v) <= constant
        }),
        CompareOp::GreaterThan => select_values(values, data_sel, validity, sel, |v| {
            vector.string_value(v) > constant
        }),
        CompareOp::GreaterThanOrEqual => {
            select_values(values, data_sel, validity, sel, |v| {
                vector.string_value(v) >= constant
            });
        }
    }
}

fn select_constant(vector: &Vector, op: CompareOp, constant: &Value, sel: &mut Vec<u32>) -> Result<()> {
    let physical = vector.logical_type().physical_type();
    if physical == PhysicalType::Varchar {
        let bytes = constant
            .as_bytes()
            .ok_or_else(|| Error::internal("string table filter with a non-string constant"))?;
        select_strings(vector, op, bytes, sel);
        return Ok(());
    }
    let numeric = NumericValue::from_value(constant).ok_or_else(|| {
        Error::invalid_input(format!(
            "Invalid type for filter pushed down to table comparison: {}",
            vector.logical_type()
        ))
    })?;
    match numeric {
        NumericValue::Bool(c) => select_native(vector, op, c, sel),
        NumericValue::I8(c) => select_native(vector, op, c, sel),
        NumericValue::I16(c) => select_native(vector, op, c, sel),
        NumericValue::I32(c) => select_native(vector, op, c, sel),
        NumericValue::I64(c) => select_native(vector, op, c, sel),
        NumericValue::I128(c) => select_native(vector, op, c, sel),
        NumericValue::U8(c) => select_native(vector, op, c, sel),
        NumericValue::U16(c) => select_native(vector, op, c, sel),
        NumericValue::U32(c) => select_native(vector, op, c, sel),
        NumericValue::U64(c) => select_native(vector, op, c, sel),
        NumericValue::U128(c) => select_native(vector, op, c, sel),
        NumericValue::F32(c) => select_native(vector, op, c, sel),
        NumericValue::F64(c) => select_native(vector, op, c, sel),
    }
    Ok(())
}

/// The filters of one scan, keyed by the filtered column's position in the scan's column
/// list (DuckDB's `TableFilterSet`).
#[derive(Clone, Debug, Default)]
pub struct TableFilterSet {
    pub filters: BTreeMap<usize, TableFilter>,
}

impl TableFilterSet {
    /// Adds a filter on scan column `column`, AND-ing it with an existing one
    /// (`TableFilterSet::PushFilter`).
    pub fn push_filter(&mut self, column: usize, filter: TableFilter) {
        match self.filters.remove(&column) {
            None => {
                self.filters.insert(column, filter);
            }
            Some(TableFilter::And(mut children)) => {
                children.push(filter);
                self.filters.insert(column, TableFilter::And(children));
            }
            Some(existing) => {
                self.filters
                    .insert(column, TableFilter::And(vec![existing, filter]));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::LogicalType;

    #[test]
    fn constant_filters_select_rows() {
        let mut vector = Vector::flat(LogicalType::INTEGER, 5);
        vector.values_mut::<i32>().copy_from_slice(&[1, 5, 3, 9, 5]);
        vector.set_value(2, &Value::null(LogicalType::INTEGER)).unwrap();
        let filter = TableFilter::ConstantComparison {
            op: CompareOp::GreaterThanOrEqual,
            constant: Value::integer(5),
        };
        let mut sel: Vec<u32> = (0..5).collect();
        filter.select(&vector, 5, &mut sel).unwrap();
        assert_eq!(sel, [1, 3, 4]);
        let mut sel: Vec<u32> = (0..5).collect();
        TableFilter::IsNull.select(&vector, 5, &mut sel).unwrap();
        assert_eq!(sel, [2]);
        let mut sel: Vec<u32> = (0..5).collect();
        TableFilter::In(vec![Value::integer(1), Value::integer(9)])
            .select(&vector, 5, &mut sel)
            .unwrap();
        assert_eq!(sel, [0, 3]);
        assert_eq!(filter.to_sql("x"), "x>=5");
    }

    #[test]
    fn statistics_prune_filters() {
        let mut stats = BaseStatistics::create_empty(LogicalType::INTEGER);
        stats.set_has_no_null_fast();
        stats.numeric_mut().unwrap().update([10i32, 20]);
        let equal = |c| TableFilter::ConstantComparison {
            op: CompareOp::Equal,
            constant: Value::integer(c),
        };
        assert_eq!(
            equal(30).check_statistics(&stats),
            FilterPropagateResult::FilterAlwaysFalse
        );
        assert_eq!(
            TableFilter::And(vec![equal(15), TableFilter::IsNotNull]).check_statistics(&stats),
            FilterPropagateResult::NoPruningPossible
        );
        assert_eq!(
            TableFilter::IsNull.check_statistics(&stats),
            FilterPropagateResult::FilterAlwaysFalse
        );
    }
}
