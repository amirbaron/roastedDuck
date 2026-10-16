//! Compressed materialization (DuckDB's `CompressedMaterialization`): the inputs of
//! materializing operators (aggregates, joins, DISTINCT, ORDER BY) whose statistics allow
//! it are compressed by a projection below the operator, integers to the offset from the
//! column minimum in a smaller type and short strings into an integer, and decompressed by
//! a projection above it.
//!
//! DuckDB rewrites the bindings of the whole plan with a `ColumnBindingReplacer` that
//! stops at the new projection. Here the operator's own expressions are rewritten at
//! once, and the rewrites are queued for its ancestors, which apply them when the
//! propagation returns to them (see [`StatisticsPropagator::propagate_child`]).

use std::collections::{HashMap, HashSet};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_function::ScalarFunction;
use rustdb_planner::operator::LogicalAggregate;
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_storage::statistics::{BaseStatistics, NumericValue};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::StatisticsPropagator;
use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::expression_util::visit_column_refs;
use crate::optimizer::OptimizerType;

/// Joins are compressed only with a build side of at least this many rows
/// (`JOIN_BUILD_CARDINALITY_THRESHOLD`)...
const JOIN_BUILD_CARDINALITY_THRESHOLD: u64 = 1_048_576;
/// ...and, with fewer build columns than this (`JOIN_BUILD_COLUMN_COUNT_THRESHOLD`)...
const JOIN_BUILD_COLUMN_COUNT_THRESHOLD: usize = 20;
/// ...if the join produces at most this many rows per build row
/// (`JOIN_CARDINALITY_RATIO_THRESHOLD`).
const JOIN_CARDINALITY_RATIO_THRESHOLD: f64 = 8.0;

/// The types integers compress to (`CMUtils::IntegralTypes`), in size order.
fn integral_types() -> [LogicalType; 4] {
    [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
    ]
}

/// The types strings compress to (`CMUtils::StringTypes`), in size order.
fn string_types() -> [LogicalType; 5] {
    [
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
    ]
}

/// A child of the materializing operator (DuckDB's `CMChildInfo`).
struct ChildInfo {
    /// The child's bindings and types before compressing.
    bindings_before: Vec<ColumnBinding>,
    types: Vec<LogicalType>,
    /// Whether each binding may be compressed.
    can_compress: Vec<bool>,
}

impl ChildInfo {
    fn new(op: &LogicalOperator, referenced_bindings: &HashSet<ColumnBinding>) -> Self {
        let bindings_before = op.column_bindings();
        let can_compress = bindings_before
            .iter()
            .map(|binding| !referenced_bindings.contains(binding))
            .collect();
        Self {
            bindings_before,
            types: op.types.clone(),
            can_compress,
        }
    }
}

/// What becomes of an incoming binding (DuckDB's `CMBindingInfo`).
struct BindingInfo {
    /// The operator's output binding.
    binding: ColumnBinding,
    /// The type before compressing.
    ty: LogicalType,
    needs_decompression: bool,
    stats: Option<BaseStatistics>,
}

impl BindingInfo {
    const fn new(binding: ColumnBinding, ty: LogicalType) -> Self {
        Self {
            binding,
            ty,
            needs_decompression: false,
            stats: None,
        }
    }
}

/// DuckDB's `CompressedMaterializationInfo`.
struct CompressInfo {
    /// Incoming binding -> output binding.
    binding_map: HashMap<ColumnBinding, BindingInfo>,
    child_indexes: Vec<usize>,
    child_info: Vec<ChildInfo>,
}

impl CompressInfo {
    fn new(
        op: &LogicalOperator,
        child_indexes: Vec<usize>,
        referenced_bindings: &HashSet<ColumnBinding>,
    ) -> Self {
        let child_info = child_indexes
            .iter()
            .filter_map(|&index| op.children.get(index))
            .map(|child| ChildInfo::new(child, referenced_bindings))
            .collect();
        Self {
            binding_map: HashMap::new(),
            child_indexes,
            child_info,
        }
    }
}

/// A compressing expression and the statistics of its result (DuckDB's
/// `CompressExpression`).
struct CompressExpression {
    expression: Expression,
    stats: Option<BaseStatistics>,
}

/// Adds the bindings `expression` references (DuckDB's `GetReferencedBindings`).
fn referenced_bindings_of(expression: &Expression, referenced: &mut HashSet<ColumnBinding>) {
    visit_column_refs(expression, &mut |column| {
        referenced.insert(column.binding);
    });
}

/// The column binding of a column reference.
fn column_binding(expression: &Expression) -> Option<ColumnBinding> {
    match &expression.kind {
        ExpressionKind::ColumnRef(column) => Some(column.binding),
        _ => None,
    }
}

/// `max - min` of an integer range, if it does not overflow (`GetIntegralRangeValue`
/// evaluates the subtraction in the column type).
fn integral_range(min: NumericValue, max: NumericValue) -> Option<u128> {
    macro_rules! signed {
        ($min:expr, $max:expr) => {
            $max.checked_sub($min)
                .and_then(|range| u128::try_from(range).ok())
        };
    }
    match (min, max) {
        (NumericValue::I8(min), NumericValue::I8(max)) => signed!(min, max),
        (NumericValue::I16(min), NumericValue::I16(max)) => signed!(min, max),
        (NumericValue::I32(min), NumericValue::I32(max)) => signed!(min, max),
        (NumericValue::I64(min), NumericValue::I64(max)) => signed!(min, max),
        (NumericValue::I128(min), NumericValue::I128(max)) => signed!(min, max),
        (NumericValue::U8(min), NumericValue::U8(max)) => signed!(min, max),
        (NumericValue::U16(min), NumericValue::U16(max)) => signed!(min, max),
        (NumericValue::U32(min), NumericValue::U32(max)) => signed!(min, max),
        (NumericValue::U64(min), NumericValue::U64(max)) => signed!(min, max),
        (NumericValue::U128(min), NumericValue::U128(max)) => signed!(min, max),
        _ => None,
    }
}

/// Statistics of `ty` with the validity and distinct count of `stats`
/// (`CreateEmpty` + `CopyBase`).
fn empty_stats_like(ty: &LogicalType, stats: &BaseStatistics) -> BaseStatistics {
    let mut result = BaseStatistics::create_empty(ty.clone());
    result.set_validity(stats.can_have_null(), stats.can_have_no_null());
    result.set_distinct_count(stats.distinct_count());
    result
}

/// `value` as a value of the unsigned compression type `ty`, which holds it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the type was chosen to hold the value"
)]
fn unsigned_value(ty: &LogicalType, value: u64) -> Value {
    match ty.id() {
        LogicalTypeId::UTinyInt => Value::utinyint(value as u8),
        LogicalTypeId::USmallInt => Value::usmallint(value as u16),
        LogicalTypeId::UInteger => Value::uinteger(value as u32),
        _ => Value::ubigint(value),
    }
}

/// Sets numeric statistics' range (`NumericStats::SetMin` / `SetMax`).
fn set_range(stats: &mut BaseStatistics, min: &Value, max: &Value) {
    if let Some(numeric) = stats.numeric_mut() {
        numeric.min = NumericValue::from_value(min);
        numeric.max = NumericValue::from_value(max);
    }
}

impl StatisticsPropagator<'_> {
    /// Compresses the inputs of `op` if it materializes them (DuckDB's
    /// `CompressedMaterialization::Compress`).
    pub(super) fn compress_materialization(&mut self, op: &mut LogicalOperator) -> Result<()> {
        if self
            .optimizer
            .config
            .disabled
            .contains(&OptimizerType::CompressedMaterialization)
            || crate::topn::can_optimize(op)
        {
            return Ok(());
        }
        if !matches!(
            op.operator_type(),
            LogicalOperatorType::LogicalAggregateAndGroupBy
                | LogicalOperatorType::LogicalComparisonJoin
                | LogicalOperatorType::LogicalDistinct
                | LogicalOperatorType::LogicalOrderBy
        ) {
            return Ok(());
        }
        op.resolve_operator_types()?;
        match op.operator_type() {
            LogicalOperatorType::LogicalAggregateAndGroupBy => self.compress_aggregate(op),
            LogicalOperatorType::LogicalComparisonJoin => self.compress_comparison_join(op),
            LogicalOperatorType::LogicalDistinct => self.compress_distinct(op),
            _ => self.compress_order(op),
        }
    }

    /// DuckDB's `CompressAggregate`.
    fn compress_aggregate(&mut self, op: &mut LogicalOperator) -> Result<()> {
        let LogicalKind::Aggregate(aggregate) = &op.kind else {
            return Ok(());
        };
        if aggregate.grouping_sets.len() > 1 {
            return Ok(());
        }
        let mut group_binding_set = HashSet::new();
        for group in &aggregate.groups {
            if let Some(binding) = column_binding(group)
                && !group_binding_set.insert(binding)
            {
                // A duplicate group: no compression.
                return Ok(());
            }
        }
        if aggregate.groups.is_empty() || aggregate.group_stats.is_empty() {
            return Ok(());
        }
        let group_count = aggregate.groups.len();
        let mut referenced_bindings = HashSet::new();
        let mut group_bindings: Vec<Option<ColumnBinding>> = vec![None; group_count];
        let mut needs_decompression = vec![false; group_count];
        let mut stored_group_stats: Vec<Option<BaseStatistics>> = vec![None; group_count];
        for group_idx in 0..group_count {
            let LogicalKind::Aggregate(aggregate) = &mut op.kind else {
                return Ok(());
            };
            let group = &aggregate.groups[group_idx];
            if let Some(binding) = column_binding(group) {
                group_bindings[group_idx] = Some(binding);
                continue;
            }
            // Non-column groups are compressed directly; their inputs stay as they are.
            referenced_bindings_of(group, &mut referenced_bindings);
            let Some(group_stats) = aggregate.group_stats.get(group_idx).cloned().flatten() else {
                continue;
            };
            if let Some(compressed) = self.compress_expression(group.clone(), &group_stats)? {
                needs_decompression[group_idx] = true;
                stored_group_stats[group_idx] = Some(group_stats);
                aggregate.groups[group_idx] = compressed.expression;
                aggregate.group_stats[group_idx] = compressed.stats;
            }
        }
        // Anything the aggregates reference is not compressed either.
        for expression in &op.expressions {
            let ExpressionKind::Aggregate(aggregate) = &expression.kind else {
                continue;
            };
            for child in &aggregate.children {
                referenced_bindings_of(child, &mut referenced_bindings);
            }
            if let Some(filter) = &aggregate.filter {
                referenced_bindings_of(filter, &mut referenced_bindings);
            }
            if let Some(order_bys) = &aggregate.order_bys {
                for order in &order_bys.orders {
                    if column_binding(&order.expression).is_none() {
                        referenced_bindings_of(&order.expression, &mut referenced_bindings);
                    }
                }
            }
        }
        let mut info = CompressInfo::new(op, vec![0], &referenced_bindings);
        let bindings_out = op.column_bindings();
        for group_idx in 0..group_count {
            let mut binding_info = BindingInfo::new(bindings_out[group_idx], op.types[group_idx].clone());
            binding_info.needs_decompression = needs_decompression[group_idx];
            if needs_decompression[group_idx] {
                binding_info.stats = stored_group_stats[group_idx].take();
                info.binding_map
                    .entry(bindings_out[group_idx])
                    .or_insert(binding_info);
            } else if let Some(binding) = group_bindings[group_idx] {
                info.binding_map.entry(binding).or_insert(binding_info);
            }
        }
        self.create_projections(op, &mut info)?;
        self.update_aggregate_stats(op);
        Ok(())
    }

    /// The group statistics of a compressed aggregate are those of the compressed
    /// columns (DuckDB's `UpdateAggregateStats`).
    fn update_aggregate_stats(&self, op: &mut LogicalOperator) {
        if op.operator_type() != LogicalOperatorType::LogicalProjection {
            return;
        }
        let Some(LogicalKind::Aggregate(aggregate)) = op.children.first_mut().map(|child| &mut child.kind)
        else {
            return;
        };
        let LogicalAggregate {
            groups, group_stats, ..
        } = &mut **aggregate;
        for (group, stats) in groups.iter().zip(group_stats.iter_mut()) {
            let Some(binding) = column_binding(group) else {
                continue;
            };
            let Some(current) = stats.as_ref() else {
                continue;
            };
            if &group.return_type == current.logical_type() {
                continue;
            }
            if let Some(compressed) = self.statistics_map.get(&binding) {
                *stats = Some(compressed.clone());
            }
        }
    }

    /// DuckDB's `CompressComparisonJoin`.
    fn compress_comparison_join(&mut self, op: &mut LogicalOperator) -> Result<()> {
        let LogicalKind::ComparisonJoin(join) = &op.kind else {
            return Ok(());
        };
        if join.join_type == JoinType::Mark || op.children.len() != 2 {
            return Ok(());
        }
        let build_cardinality = match op.children[1].estimated_cardinality {
            Some(cardinality) => cardinality,
            None => op.children[1].estimate_cardinality(),
        };
        if build_cardinality < JOIN_BUILD_CARDINALITY_THRESHOLD {
            return Ok(());
        }
        if op.children[1].types.len() < JOIN_BUILD_COLUMN_COUNT_THRESHOLD {
            let join_cardinality = match op.estimated_cardinality {
                Some(cardinality) => cardinality,
                None => op.estimate_cardinality(),
            };
            #[allow(clippy::cast_precision_loss, reason = "a cardinality ratio")]
            let ratio = join_cardinality as f64 / build_cardinality as f64;
            if ratio > JOIN_CARDINALITY_RATIO_THRESHOLD {
                return Ok(());
            }
        }
        let LogicalKind::ComparisonJoin(join) = &op.kind else {
            return Ok(());
        };
        let is_delim_join = op.operator_type() == LogicalOperatorType::LogicalDelimJoin;
        let mut probe_compress_bindings = HashSet::new();
        let mut referenced_bindings = HashSet::new();
        for condition in &join.conditions {
            if join.conditions.len() == 1
                && !is_delim_join
                && let (Some(left), Some(right)) =
                    (column_binding(&condition.left), column_binding(&condition.right))
                && let (Some(left_stats), Some(right_stats)) =
                    (self.statistics_map.get(&left), self.statistics_map.get(&right))
            {
                // Both sides must be compressed with the same statistics to compare.
                let mut merged = left_stats.clone();
                merged.merge(right_stats);
                if self
                    .compress_expression(condition.left.clone(), &merged)?
                    .is_some()
                {
                    if let Some(stats) = self.statistics_map.get_mut(&left) {
                        stats.merge(&merged);
                    }
                    if let Some(stats) = self.statistics_map.get_mut(&right) {
                        stats.merge(&merged);
                    }
                    probe_compress_bindings.insert(left);
                    continue;
                }
            }
            referenced_bindings_of(&condition.left, &mut referenced_bindings);
            referenced_bindings_of(&condition.right, &mut referenced_bindings);
        }
        if is_delim_join {
            for column in &join.duplicate_eliminated_columns {
                referenced_bindings_of(column, &mut referenced_bindings);
            }
        }
        // The probe side is streamed: only its join keys are compressed.
        for binding in op.children[0].column_bindings() {
            if !probe_compress_bindings.contains(&binding) {
                referenced_bindings.insert(binding);
            }
        }
        let mut info = CompressInfo::new(op, vec![0, 1], &referenced_bindings);
        let bindings_out = op.column_bindings();
        for child in &op.children {
            // Joins keep their input bindings (`PopulateBindingMap`).
            for binding in child.column_bindings() {
                for (index, binding_out) in bindings_out.iter().enumerate() {
                    if *binding_out == binding {
                        info.binding_map
                            .entry(binding)
                            .or_insert_with(|| BindingInfo::new(*binding_out, op.types[index].clone()));
                    }
                }
            }
        }
        self.create_projections(op, &mut info)
    }

    /// DuckDB's `CompressDistinct`.
    fn compress_distinct(&mut self, op: &mut LogicalOperator) -> Result<()> {
        let LogicalKind::Distinct {
            distinct_targets,
            order_by,
            ..
        } = &op.kind
        else {
            return Ok(());
        };
        let mut referenced_bindings = HashSet::new();
        for target in distinct_targets {
            if column_binding(target).is_none() {
                referenced_bindings_of(target, &mut referenced_bindings);
            }
        }
        if let Some(order_by) = order_by {
            for order in &order_by.orders {
                if column_binding(&order.expression).is_none() {
                    referenced_bindings_of(&order.expression, &mut referenced_bindings);
                }
            }
        }
        self.compress_pass_through(op, &referenced_bindings)
    }

    /// DuckDB's `CompressOrder`.
    fn compress_order(&mut self, op: &mut LogicalOperator) -> Result<()> {
        let LogicalKind::Order { orders, .. } = &op.kind else {
            return Ok(());
        };
        let mut referenced_bindings = HashSet::new();
        for order in orders {
            if column_binding(&order.expression).is_none() {
                referenced_bindings_of(&order.expression, &mut referenced_bindings);
            }
        }
        self.compress_pass_through(op, &referenced_bindings)
    }

    /// Compresses the child of an operator that outputs its input bindings (DISTINCT,
    /// ORDER BY).
    fn compress_pass_through(
        &mut self,
        op: &mut LogicalOperator,
        referenced_bindings: &HashSet<ColumnBinding>,
    ) -> Result<()> {
        let mut info = CompressInfo::new(op, vec![0], referenced_bindings);
        for (binding, ty) in op.column_bindings().into_iter().zip(op.types.iter()) {
            info.binding_map
                .entry(binding)
                .or_insert_with(|| BindingInfo::new(binding, ty.clone()));
        }
        self.create_projections(op, &mut info)
    }

    /// DuckDB's `UpdateBindingInfo`.
    fn update_binding_info(
        &self,
        info: &mut CompressInfo,
        binding: ColumnBinding,
        needs_decompression: bool,
    ) {
        if let Some(binding_info) = info.binding_map.get_mut(&binding) {
            binding_info.needs_decompression = needs_decompression;
            if let Some(stats) = self.statistics_map.get(&binding) {
                binding_info.stats = Some(stats.clone());
            }
        }
    }

    /// DuckDB's `CreateProjections`.
    fn create_projections(&mut self, op: &mut LogicalOperator, info: &mut CompressInfo) -> Result<()> {
        let mut compressed_anything = false;
        for i in 0..info.child_indexes.len() {
            let Some(compress_exprs) = self.try_compress_child(info, i)? else {
                continue;
            };
            let child_index = info.child_indexes[i];
            self.create_compress_projection(op, child_index, compress_exprs, info)?;
            compressed_anything = true;
        }
        if compressed_anything {
            self.create_decompress_projection(op, info)?;
        }
        Ok(())
    }

    /// The compressing (or passing) expression of every column of child `i`, if any is
    /// compressed (DuckDB's `TryCompressChild`).
    fn try_compress_child(
        &mut self,
        info: &mut CompressInfo,
        i: usize,
    ) -> Result<Option<Vec<CompressExpression>>> {
        let child_info = &info.child_info[i];
        let columns: Vec<(ColumnBinding, LogicalType, bool)> = child_info
            .bindings_before
            .iter()
            .zip(&child_info.types)
            .zip(&child_info.can_compress)
            .map(|((binding, ty), can_compress)| (*binding, ty.clone(), *can_compress))
            .collect();
        let mut compress_exprs = Vec::with_capacity(columns.len());
        let mut compressed_anything = false;
        for (binding, ty, can_compress) in columns {
            let compressed = match self.statistics_map.get(&binding) {
                Some(stats) if can_compress => {
                    let stats = stats.clone();
                    self.compress_expression(
                        Expression::column_ref(String::new(), ty.clone(), binding, 0),
                        &stats,
                    )?
                }
                _ => None,
            };
            let is_compressed = compressed.is_some();
            compress_exprs.push(compressed.unwrap_or_else(|| CompressExpression {
                expression: Expression::column_ref(String::new(), ty, binding, 0),
                stats: self.statistics_map.get(&binding).cloned(),
            }));
            self.update_binding_info(info, binding, is_compressed);
            compressed_anything |= is_compressed;
        }
        if !compressed_anything {
            // Groups compressed directly still need decompressing.
            compressed_anything = info
                .binding_map
                .values()
                .any(|binding| binding.needs_decompression);
        }
        Ok(compressed_anything.then_some(compress_exprs))
    }

    /// Puts a projection computing `compress_exprs` between `op` and child `child_index`
    /// (DuckDB's `CreateCompressProjection`).
    fn create_compress_projection(
        &mut self,
        op: &mut LogicalOperator,
        child_index: usize,
        compress_exprs: Vec<CompressExpression>,
        info: &mut CompressInfo,
    ) -> Result<()> {
        let Some(info_index) = info.child_indexes.iter().position(|&index| index == child_index) else {
            return Ok(());
        };
        let (expressions, stats): (Vec<Expression>, Vec<Option<BaseStatistics>>) = compress_exprs
            .into_iter()
            .map(|compressed| (compressed.expression, compressed.stats))
            .unzip();
        let table_index = self.optimizer.generate_table_index();
        let child = std::mem::replace(
            &mut op.children[child_index],
            LogicalOperator::new(LogicalKind::CrossProduct),
        );
        let estimate = child.estimated_cardinality;
        let mut projection = LogicalOperator::projection(table_index, expressions, child);
        projection.estimated_cardinality = estimate;
        projection.resolve_operator_types()?;
        let bindings_after = projection.column_bindings();
        let new_types = projection.types.clone();
        op.children[child_index] = projection;

        let bindings_before = &info.child_info[info_index].bindings_before;
        let mut replacer = ColumnBindingReplacer::default();
        for ((old, new), ty) in bindings_before.iter().zip(&bindings_after).zip(&new_types) {
            replacer
                .replacement_bindings
                .push(ReplacementBinding::with_type(*old, *new, ty.clone()));
            self.statistics_map.remove(old);
        }
        // The operator itself now reads the projection; its ancestors follow.
        op.for_each_expression_mut(|expression| replacer.visit_expression(expression));

        // Re-key the binding map.
        for replacement in &replacer.replacement_bindings {
            if let Some(mut binding_info) = info.binding_map.remove(&replacement.old_binding) {
                if binding_info.binding == replacement.old_binding {
                    binding_info.binding = replacement.new_binding;
                }
                info.binding_map
                    .entry(replacement.new_binding)
                    .or_insert(binding_info);
            }
        }
        for (binding, stats) in bindings_after.iter().zip(stats) {
            if let Some(stats) = stats {
                self.statistics_map.entry(*binding).or_insert(stats);
            }
        }
        self.replacements.push(replacer);
        Ok(())
    }

    /// Puts a projection decompressing the compressed columns on top of `op` (DuckDB's
    /// `CreateDecompressProjection`).
    fn create_decompress_projection(&mut self, op: &mut LogicalOperator, info: &CompressInfo) -> Result<()> {
        op.resolve_operator_types()?;
        let bindings = op.column_bindings();
        let mut decompress_exprs = Vec::with_capacity(bindings.len());
        let mut statistics = Vec::with_capacity(bindings.len());
        for (binding, ty) in bindings.iter().zip(&op.types) {
            let mut expression = Expression::column_ref(String::new(), ty.clone(), *binding, 0);
            let mut stats = None;
            for binding_info in info
                .binding_map
                .values()
                .filter(|entry| entry.binding == *binding)
            {
                stats = binding_info.stats.as_ref();
                if binding_info.needs_decompression {
                    let input_stats = stats.ok_or_else(|| {
                        Error::internal("compressed materialization: decompression without statistics")
                    })?;
                    expression = self.decompress_expression(expression, &binding_info.ty, input_stats)?;
                }
            }
            statistics.push(stats.cloned());
            decompress_exprs.push(expression);
        }
        let table_index = self.optimizer.generate_table_index();
        let estimate = op.estimated_cardinality;
        let materializing = std::mem::replace(op, LogicalOperator::new(LogicalKind::CrossProduct));
        let mut projection = LogicalOperator::projection(table_index, decompress_exprs, materializing);
        projection.estimated_cardinality = estimate;
        projection.resolve_operator_types()?;
        let new_bindings = projection.column_bindings();
        let mut replacer = ColumnBindingReplacer::default();
        for (((old, new), ty), stats) in bindings
            .iter()
            .zip(&new_bindings)
            .zip(&projection.types)
            .zip(statistics)
        {
            replacer
                .replacement_bindings
                .push(ReplacementBinding::with_type(*old, *new, ty.clone()));
            if let Some(stats) = stats {
                self.statistics_map.insert(*new, stats);
            }
        }
        *op = projection;
        self.replacements.push(replacer);
        Ok(())
    }

    /// A built-in internal function by exact argument types (DuckDB's
    /// `CM*Fun::GetFunction`; these functions refuse to bind from SQL).
    fn internal_function(&self, name: &str, arguments: &[LogicalType]) -> Result<ScalarFunction> {
        let entry = self
            .optimizer
            .context()
            .get_entry(
                "system",
                "main",
                rustdb_catalog::CatalogType::ScalarFunction,
                name,
                true,
            )?
            .ok_or_else(|| Error::internal(format!("scalar function {name} not found")))?;
        let rustdb_catalog::EntryKind::ScalarFunction(functions) = &entry.entry.kind else {
            return Err(Error::internal(format!("{name} is not a scalar function")));
        };
        functions
            .functions
            .iter()
            .find(|function| function.arguments == arguments)
            .cloned()
            .ok_or_else(|| Error::internal(format!("no overload of {name} for {arguments:?}")))
    }

    /// The compressed form of `input` given its statistics, if smaller (DuckDB's
    /// `GetCompressExpression`).
    fn compress_expression(
        &self,
        input: Expression,
        stats: &BaseStatistics,
    ) -> Result<Option<CompressExpression>> {
        let ty = input.return_type.clone();
        if &ty != stats.logical_type() {
            return Ok(None);
        }
        if ty.is_integral() {
            self.integral_compress(input, stats)
        } else if ty.id() == LogicalTypeId::Varchar {
            self.string_compress(input, stats)
        } else {
            Ok(None)
        }
    }

    /// DuckDB's `GetIntegralCompress`: the offset from the minimum in the smallest type
    /// holding the range.
    fn integral_compress(
        &self,
        input: Expression,
        stats: &BaseStatistics,
    ) -> Result<Option<CompressExpression>> {
        let ty = input.return_type.clone();
        let size = ty.physical_type().size();
        if size == 1 {
            return Ok(None);
        }
        let (cast_type, range, min) = if !stats.can_have_no_null() {
            // All NULL.
            (LogicalType::UTINYINT, 0u64, Value::null(ty.clone()))
        } else if let (Some(numeric), Some(min)) = (
            stats.numeric().filter(|numeric| numeric.has_min_max()),
            stats.numeric_min(),
        ) {
            let (Some(low), Some(high)) = (numeric.min, numeric.max) else {
                return Ok(None);
            };
            let Some(range) = integral_range(low, high).and_then(|range| u64::try_from(range).ok()) else {
                return Ok(None);
            };
            let cast_type = if u8::try_from(range).is_ok() {
                LogicalType::UTINYINT
            } else if u16::try_from(range).is_ok() {
                LogicalType::USMALLINT
            } else if u32::try_from(range).is_ok() {
                LogicalType::UINTEGER
            } else {
                LogicalType::UBIGINT
            };
            (cast_type, range, min)
        } else {
            return Ok(None);
        };
        if cast_type.physical_type().size() == size {
            return Ok(None);
        }
        debug_assert!(integral_types().contains(&cast_type));
        let name = format!(
            "__internal_compress_integral_{}",
            cast_type.to_string().to_lowercase()
        );
        let function = self.internal_function(&name, &[ty.clone(), ty])?;
        let expression = Expression::function(function, vec![input, Expression::constant(min)], None, false);
        let mut compress_stats = empty_stats_like(&cast_type, stats);
        set_range(
            &mut compress_stats,
            &unsigned_value(&cast_type, 0),
            &unsigned_value(&cast_type, range),
        );
        Ok(Some(CompressExpression {
            expression,
            stats: Some(compress_stats),
        }))
    }

    /// DuckDB's `GetStringCompress`: strings shorter than an integer type, packed into it.
    fn string_compress(
        &self,
        input: Expression,
        stats: &BaseStatistics,
    ) -> Result<Option<CompressExpression>> {
        let (mut cast_type, max_string_length) = if !stats.can_have_no_null() {
            (LogicalType::UTINYINT, 0)
        } else if let Some(string) = stats.string().filter(|string| string.has_max_string_length) {
            let max_string_length = string.max_string_length;
            let Some(cast_type) = string_types()
                .into_iter()
                .find(|ty| (max_string_length as usize) < ty.physical_type().size())
            else {
                return Ok(None);
            };
            (cast_type, max_string_length)
        } else {
            return Ok(None);
        };
        let mut compress_stats = empty_stats_like(&cast_type, stats);
        if cast_type.id() == LogicalTypeId::USmallInt {
            let first_byte = |bytes: [u8; 8]| if max_string_length == 0 { 0 } else { bytes[0] };
            let (min_numeric, max_numeric) = stats
                .string()
                .map_or((0, 0), |string| (first_byte(string.min), first_byte(string.max)));
            let mut min_value = Value::usmallint(u16::from(min_numeric));
            let mut max_value = Value::usmallint(u16::from(max_numeric) + 1);
            if max_numeric < u8::MAX {
                cast_type = LogicalType::UTINYINT;
                compress_stats = empty_stats_like(&cast_type, stats);
                min_value = Value::utinyint(min_numeric);
                max_value = Value::utinyint(max_numeric + 1);
            }
            set_range(&mut compress_stats, &min_value, &max_value);
        }
        let name = format!(
            "__internal_compress_string_{}",
            cast_type.to_string().to_lowercase()
        );
        let function = self.internal_function(&name, &[LogicalType::VARCHAR])?;
        Ok(Some(CompressExpression {
            expression: Expression::function(function, vec![input], None, false),
            stats: Some(compress_stats),
        }))
    }

    /// DuckDB's `GetDecompressExpression`.
    fn decompress_expression(
        &self,
        input: Expression,
        result_type: &LogicalType,
        stats: &BaseStatistics,
    ) -> Result<Expression> {
        if result_type.is_integral() {
            let min = if stats.can_have_no_null() {
                stats
                    .numeric_min()
                    .ok_or_else(|| Error::internal("decompression without a minimum"))?
            } else {
                Value::null(result_type.clone())
            };
            let name = format!(
                "__internal_decompress_integral_{}",
                result_type.to_string().to_lowercase()
            );
            let function =
                self.internal_function(&name, &[input.return_type.clone(), result_type.clone()])?;
            let mut expression =
                Expression::function(function, vec![input, Expression::constant(min)], None, false);
            expression.return_type = result_type.clone();
            Ok(expression)
        } else if result_type.id() == LogicalTypeId::Varchar {
            let function = self.internal_function(
                "__internal_decompress_string",
                std::slice::from_ref(&input.return_type),
            )?;
            let mut expression = Expression::function(function, vec![input], None, false);
            expression.return_type = result_type.clone();
            Ok(expression)
        } else {
            Err(Error::internal(
                "Type other than integral/string marked for decompression!",
            ))
        }
    }
}
