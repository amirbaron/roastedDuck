//! Grouped Top-N without a window (DuckDB's `TopNWindowElimination`): a filter
//! `row_number() OVER (PARTITION BY g ORDER BY v) <= n` keeps the `n` first rows of each
//! partition, which an aggregate `arg_min(payload, v, n)` grouped by `g` (unnested back
//! into rows) computes without sorting the partitions. Several payload columns travel as
//! one `struct_pack`, or, for a table scan, as the row id that a join with a second scan
//! of the table turns back into the columns (late materialization).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rustdb_ast::{ExpressionType, JoinType, OrderByNullType, OrderType};
use rustdb_common::{Error, Result};
use rustdb_planner::expression::{BoundUnnestExpression, BoundWindowExpression};
use rustdb_planner::operator::{JoinCondition, LogicalAggregate, LogicalComparisonJoin};
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionClass, ExpressionKind, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_storage::statistics::BaseStatistics;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::expression_nullability::NotNullExpressionAnalyzer;
use crate::expression_util::visit_column_refs;
use crate::late_materialization::get_or_insert_row_id;
use crate::optimizer::{Optimizer, bind_aggregate_function};
use crate::remove_unused_columns::remove_unused_columns;

/// The largest `n` the top-N aggregates accept, exclusive (`MIN_MAX_N_MAX_VALUE`).
const MIN_MAX_N_MAX_VALUE: i64 = 1_000_000;

/// The name of the table scan's row id column.
const ROW_ID_NAME: &str = "rowid";

/// How the aggregate carries the selected rows' columns (DuckDB's `TopNPayloadType`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadType {
    SingleColumn,
    StructPack,
}

/// DuckDB's `TopNWindowEliminationParameters`.
struct Parameters {
    order_type: OrderType,
    payload_type: PayloadType,
    limit: i64,
    include_row_number: bool,
    can_be_null: bool,
    partition_count: usize,
    row_ids_may_have_duplicates: bool,
}

/// A [`ColumnBindingReplacer`] that leaves the subtree of one operator alone (DuckDB's
/// `ColumnBindingReplacer::stop_operator`).
#[derive(Default)]
struct Replacer {
    replacer: ColumnBindingReplacer,
    /// The [`LogicalOperator::node_id`] of the operator not to rewrite.
    stop_operator: Option<u64>,
}

impl Replacer {
    fn visit_operator(&self, op: &mut LogicalOperator) {
        self.replacer.visit_operator_until(op, self.stop_operator);
    }
}

/// The distinct column references of `expression`, in first-seen order (DuckDB's
/// `column_references` after `VisitExpression`).
fn referenced_bindings(expression: &Expression) -> Vec<ColumnBinding> {
    let mut bindings = Vec::new();
    visit_column_refs(expression, &mut |column| {
        if !bindings.contains(&column.binding) {
            bindings.push(column.binding);
        }
    });
    bindings
}

/// The single column `expression` references (DuckDB's `ExtractSingleBinding`), which
/// must be the whole expression with `require_direct_column_ref`.
fn single_binding(expression: &Expression, require_direct_column_ref: bool) -> Option<ColumnBinding> {
    if require_direct_column_ref && expression.expression_type != ExpressionType::BoundColumnRef {
        return None;
    }
    match referenced_bindings(expression).as_slice() {
        [binding] => Some(*binding),
        _ => None,
    }
}

/// The window function call of a window operator's expression.
fn window_call(expression: &Expression) -> Option<&BoundWindowExpression> {
    match &expression.kind {
        ExpressionKind::Window(window) => Some(window),
        _ => None,
    }
}

/// The first table index of an operator (DuckDB's `GetTableIndex()[0]`).
fn first_table_index(op: &LogicalOperator) -> Result<u64> {
    op.table_indexes()
        .first()
        .copied()
        .ok_or_else(|| Error::internal("TopNWindowElimination: operator without a table index"))
}

/// DuckDB's `GetGroupIdx`.
fn group_table_index(op: &LogicalOperator) -> Result<u64> {
    match &op.kind {
        LogicalKind::Aggregate(aggregate) => Ok(aggregate.group_index),
        LogicalKind::ComparisonJoin(_) => op
            .children
            .first()
            .ok_or_else(|| Error::internal("TopNWindowElimination: join without children"))
            .and_then(first_table_index),
        _ => first_table_index(op),
    }
}

/// DuckDB's `GetAggregateIdx`.
fn aggregate_table_index(op: &LogicalOperator) -> Result<u64> {
    match &op.kind {
        LogicalKind::Aggregate(aggregate) => Ok(aggregate.aggregate_index),
        LogicalKind::ComparisonJoin(_) => op
            .children
            .first()
            .ok_or_else(|| Error::internal("TopNWindowElimination: join without children"))
            .and_then(first_table_index),
        _ => first_table_index(op),
    }
}

/// The type of the aggregate's result column (DuckDB's `GetAggregateType`).
fn aggregate_type(op: &LogicalOperator) -> Result<LogicalType> {
    let index = match &op.kind {
        LogicalKind::Unnest { .. } => op.children.first().map_or(0, |child| child.types.len()),
        LogicalKind::Aggregate(aggregate) => aggregate.groups.len(),
        _ => {
            return Err(Error::internal(
                "Unnest or aggregate expected to extract aggregate type.",
            ));
        }
    };
    op.types
        .get(index)
        .cloned()
        .ok_or_else(|| Error::internal("TopNWindowElimination: aggregate column out of range"))
}

/// The binding of the row number column (DuckDB's `GetRowNumberColumnBinding`).
fn row_number_binding(op: &LogicalOperator) -> Result<ColumnBinding> {
    let missing = || Error::internal("TopNWindowElimination: missing row number column");
    match &op.kind {
        LogicalKind::Unnest { .. } => {
            let offset = op.children.first().map_or(0, |child| child.types.len()) + 1;
            op.column_bindings().get(offset).copied().ok_or_else(missing)
        }
        LogicalKind::Projection { table_index } => Ok(ColumnBinding::new(
            *table_index,
            (op.types.len() as u64).checked_sub(1).ok_or_else(missing)?,
        )),
        LogicalKind::ComparisonJoin(_) => op.column_bindings().last().copied().ok_or_else(missing),
        _ => Err(Error::internal("Operator type not supported.")),
    }
}

/// The CTE definitions and references of a subtree (DuckDB's `GatherLocalCTEInfo`).
fn gather_local_cte_info(
    op: &LogicalOperator,
    definitions: &mut HashSet<u64>,
    references: &mut HashSet<u64>,
) {
    match &op.kind {
        LogicalKind::MaterializedCte { table_index, .. } | LogicalKind::RecursiveCte { table_index, .. } => {
            definitions.insert(*table_index);
        }
        LogicalKind::CteRef { cte_index, .. } => {
            references.insert(*cte_index);
        }
        _ => {}
    }
    for child in &op.children {
        gather_local_cte_info(child, definitions, references);
    }
}

/// Whether a subtree reads a CTE defined outside it (DuckDB's `HasExternalCTEReferences`).
fn has_external_cte_references(op: &LogicalOperator) -> bool {
    let mut definitions = HashSet::new();
    let mut references = HashSet::new();
    gather_local_cte_info(op, &mut definitions, &mut references);
    references
        .iter()
        .any(|cte_index| !definitions.contains(cte_index))
}

/// Whether an operator selects its output columns by position (DuckDB's
/// `HasProjectionMap`).
fn has_projection_map(op: &LogicalOperator) -> bool {
    match &op.kind {
        LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
            !projection_map.is_empty()
        }
        LogicalKind::ComparisonJoin(join) => {
            !join.left_projection_map.is_empty() || !join.right_projection_map.is_empty()
        }
        LogicalKind::DependentJoin(dependent) => {
            !dependent.join.left_projection_map.is_empty() || !dependent.join.right_projection_map.is_empty()
        }
        _ => false,
    }
}

/// Whether any operator of a subtree has a projection map (DuckDB's `HasProjectionMaps`).
fn has_projection_maps(op: &LogicalOperator) -> bool {
    has_projection_map(op) || op.children.iter().any(has_projection_maps)
}

/// The operator `path` (child positions) below `op`.
fn descendant_mut<'a>(op: &'a mut LogicalOperator, path: &[usize]) -> Result<&'a mut LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node
            .children
            .get_mut(index)
            .ok_or_else(|| Error::internal("TopNWindowElimination: broken operator path"))?;
    }
    Ok(node)
}

/// The operator `path` (child positions) below `op`.
fn descendant<'a>(op: &'a LogicalOperator, path: &[usize]) -> Result<&'a LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node
            .children
            .get(index)
            .ok_or_else(|| Error::internal("TopNWindowElimination: broken operator path"))?;
    }
    Ok(node)
}

/// The types of a set of bindings, from the operators below that create them (DuckDB's
/// `ColumnBindingTyper`).
fn binding_types(op: &LogicalOperator, bindings: &[ColumnBinding], types: &mut [Option<LogicalType>]) {
    for table_index in op.table_indexes() {
        for (binding, ty) in bindings.iter().zip(types.iter_mut()) {
            if binding.table_index == table_index
                && let Some(column_type) = op.types.get(binding.column_index as usize)
            {
                *ty = Some(column_type.clone());
            }
        }
    }
    for child in &op.children {
        binding_types(child, bindings, types);
    }
}

/// A column of the late-materialized scan (DuckDB's `LHSColumnInfo`).
struct LhsColumnInfo {
    name: String,
    ty: LogicalType,
    binding: ColumnBinding,
}

/// DuckDB's `GetLHSColumnInfo`: the name, type and binding of column `column` of the
/// late-materialized scan (possibly under a projection).
fn lhs_column_info(op: &LogicalOperator, column: usize) -> Result<LhsColumnInfo> {
    let missing = || Error::internal("TopNWindowElimination: late materialization column out of range");
    let ty = op.types.get(column).cloned().ok_or_else(missing)?;
    let binding = op.column_bindings().get(column).copied().ok_or_else(missing)?;
    let (get_op, get_binding) = if let LogicalKind::Get(_) = &op.kind {
        (op, binding)
    } else {
        let Some(ExpressionKind::ColumnRef(column_ref)) = op.expressions.get(column).map(|e| &e.kind) else {
            return Err(missing());
        };
        (op.children.first().ok_or_else(missing)?, column_ref.binding)
    };
    let LogicalKind::Get(get) = &get_op.kind else {
        return Err(missing());
    };
    let column_index = get
        .column_ids
        .get(get_binding.column_index as usize)
        .ok_or_else(missing)?;
    let name = if column_index.is_row_id_column() {
        ROW_ID_NAME.to_owned()
    } else {
        get.names
            .get(column_index.index as usize)
            .cloned()
            .unwrap_or_default()
    };
    Ok(LhsColumnInfo { name, ty, binding })
}

/// Rewrites the grouped Top-N windows of `plan` (DuckDB's `TopNWindowElimination::Optimize`).
pub(crate) fn eliminate_topn_windows(
    optimizer: &Optimizer,
    plan: &mut LogicalOperator,
    stats: &HashMap<ColumnBinding, BaseStatistics>,
) -> Result<()> {
    TopNWindowElimination { optimizer, stats }.optimize(plan)
}

struct TopNWindowElimination<'a> {
    optimizer: &'a Optimizer,
    stats: &'a HashMap<ColumnBinding, BaseStatistics>,
}

impl TopNWindowElimination<'_> {
    fn optimize(&self, op: &mut LogicalOperator) -> Result<()> {
        let mut replacer = Replacer::default();
        self.optimize_internal(op, &mut replacer)?;
        if !replacer.replacer.replacement_bindings.is_empty() {
            replacer.visit_operator(op);
        }
        Ok(())
    }

    fn optimize_internal(&self, op: &mut LogicalOperator, replacer: &mut Replacer) -> Result<()> {
        let Some(projection_depth) = can_optimize(op) else {
            // Look for the pattern further down.
            if op.children.len() > 1 {
                // Each child's rewrite only updates `op`, which then stops the others.
                for index in 0..op.children.len() {
                    let mut child_replacer = Replacer::default();
                    self.optimize_internal(&mut op.children[index], &mut child_replacer)?;
                    if !child_replacer.replacer.replacement_bindings.is_empty() {
                        child_replacer.visit_operator(op);
                        replacer
                            .replacer
                            .replacement_bindings
                            .extend(child_replacer.replacer.replacement_bindings);
                        replacer.stop_operator = Some(op.node_id());
                    }
                }
            } else if let Some(child) = op.children.first_mut() {
                self.optimize_internal(child, replacer)?;
            }
            return Ok(());
        };
        // A filter over projections over the window.
        if op.types.len() != op.column_bindings().len() {
            op.resolve_operator_types()?;
        }
        let topmost_bindings = op.column_bindings();
        let topmost_types = op.types.clone();
        let Some(mut new_bindings) = traverse_projection_bindings(op, &topmost_bindings, projection_depth)
        else {
            return Ok(());
        };

        // Take the window out of the plan.
        let mut original = std::mem::replace(op, LogicalOperator::new(LogicalKind::CrossProduct));
        let filter_expression = original
            .expressions
            .first()
            .cloned()
            .ok_or_else(|| Error::internal("TopNWindowElimination: filter without expression"))?;
        let mut window = original.children.swap_remove(0);
        for _ in 0..projection_depth {
            window = window.children.swap_remove(0);
        }
        let LogicalKind::Window { window_index } = window.kind else {
            return Err(Error::internal("TopNWindowElimination: expected a window"));
        };

        let mut group_projection_indexes = BTreeMap::new();
        let mut aggregate_payload = generate_aggregate_payload(
            &new_bindings,
            &mut window,
            window_index,
            &mut group_projection_indexes,
        )?;
        let mut params = Self::extract_parameters(
            &window,
            window_index,
            &filter_expression,
            &new_bindings,
            &aggregate_payload,
        )?;

        let mut late_mat_lhs = None;
        if params.payload_type == PayloadType::StructPack {
            // Late materialization can save the struct packing.
            late_mat_lhs =
                self.try_prepare_late_materialization(&mut window, &mut aggregate_payload, &mut params)?;
            if late_mat_lhs.is_some() && aggregate_payload.len() == 1 {
                params.payload_type = PayloadType::SingleColumn;
            }
        }
        let used_late_materialization = late_mat_lhs.is_some();

        // The window's input may hold more windows to rewrite.
        if let Some(child) = window.children.first_mut() {
            self.optimize(child)?;
        }

        let mut new_op = self.create_aggregate_operator(window, aggregate_payload, &params)?;
        new_op = self.try_create_unnest_operator(new_op, &params)?;
        new_op = self.create_projection_operator(new_op, &params, &group_projection_indexes)?;
        if let Some(lhs) = late_mat_lhs {
            new_op = self.construct_join(lhs, new_op, group_projection_indexes.len(), &params)?;
        }
        new_op = self.update_topmost_bindings(
            window_index,
            new_op,
            &topmost_types,
            &group_projection_indexes,
            &topmost_bindings,
            &mut new_bindings,
            replacer,
            &params,
        )?;
        replacer.stop_operator = Some(new_op.node_id());

        // Prune what the rewrite left unused, unless positional projection maps could
        // lose a column.
        if !used_late_materialization
            && !has_external_cte_references(&new_op)
            && !has_projection_maps(&new_op)
        {
            remove_unused_columns(self.optimizer, &mut new_op)?;
        }
        *op = new_op;
        Ok(())
    }

    /// DuckDB's `CreateAggregateExpression`: `min`/`max`, `arg_min`/`arg_max`, with `n`
    /// and NULLs-last variants as needed.
    fn create_aggregate_expression(
        &self,
        mut aggregate_params: Vec<Expression>,
        requires_arg: bool,
        params: &Parameters,
    ) -> Result<Expression> {
        // A nullable value needs the NULLs-last function to keep the window's ordering.
        let change_to_arg = !requires_arg && params.can_be_null && params.limit > 1;
        if change_to_arg {
            let value = aggregate_params
                .first()
                .cloned()
                .ok_or_else(|| Error::internal("TopNWindowElimination: missing aggregate value"))?;
            aggregate_params.insert(1, value);
        }
        let use_arg = requires_arg || change_to_arg;
        let mut name = String::from(if use_arg { "arg_" } else { "" });
        name.push_str(if params.order_type == OrderType::Ascending {
            "min"
        } else {
            "max"
        });
        if params.can_be_null && use_arg {
            name.push_str("_nulls_last");
        }
        bind_aggregate_function(self.optimizer.context(), &name, aggregate_params)
    }

    /// DuckDB's `CreateAggregateOperator`: the aggregate grouped by the partitions.
    fn create_aggregate_operator(
        &self,
        mut window: LogicalOperator,
        mut args: Vec<Expression>,
        params: &Parameters,
    ) -> Result<LogicalOperator> {
        let window_expression = window.expressions.swap_remove(0);
        let ExpressionKind::Window(window_call) = window_expression.kind else {
            return Err(Error::internal(
                "TopNWindowElimination: expected a window function",
            ));
        };
        let BoundWindowExpression {
            partitions,
            mut orders,
            ..
        } = *window_call;
        if orders.len() != 1 {
            return Err(Error::internal("TopNWindowElimination: expected one ORDER BY"));
        }

        let mut aggregate_params = Vec::with_capacity(3);
        let use_arg = !args.is_empty();
        if args.len() == 1 {
            aggregate_params.push(args.swap_remove(0));
        } else if args.len() > 1 {
            // Several columns travel as a struct.
            aggregate_params.push(self.optimizer.bind_scalar_function("struct_pack", args)?);
        }
        aggregate_params.push(orders.swap_remove(0).expression);
        if params.limit > 1 {
            aggregate_params.push(Expression::constant(Value::bigint(params.limit)));
        }
        let aggregate_expression = self.create_aggregate_expression(aggregate_params, use_arg, params)?;

        let group_index = self.optimizer.generate_table_index();
        let aggregate_index = self.optimizer.generate_table_index();
        let groupings_index = self.optimizer.generate_table_index();
        let mut groups = partitions;
        if groups.is_empty() && params.limit == 1 {
            // A constant group keeps the window's result for an empty input.
            groups.push(Expression::constant(Value::boolean(true)));
        }
        // The group statistics allow a perfect hash aggregate.
        let group_stats = groups
            .iter()
            .map(|group| match &group.kind {
                ExpressionKind::ColumnRef(column) => self.stats.get(&column.binding).cloned(),
                _ => None,
            })
            .collect();
        let child = window
            .children
            .pop()
            .ok_or_else(|| Error::internal("TopNWindowElimination: window without input"))?;
        let mut aggregate = LogicalOperator::new(LogicalKind::Aggregate(Box::new(LogicalAggregate {
            group_index,
            aggregate_index,
            groupings_index,
            groups,
            grouping_sets: Vec::new(),
            grouping_functions: Vec::new(),
            group_stats,
        })))
        .with_expressions(vec![aggregate_expression])
        .with_children(vec![child]);
        aggregate.resolve_operator_types()?;
        Ok(aggregate)
    }

    /// DuckDB's `CreateRowNumberGenerator`: `unnest(generate_series(1, array_length(list, 1)))`.
    fn create_row_number_generator(&self, aggregate_column_ref: Expression) -> Result<Expression> {
        let array_length = self.optimizer.bind_scalar_function(
            "array_length",
            vec![aggregate_column_ref, Expression::constant(Value::integer(1))],
        )?;
        let generate_series = self.optimizer.bind_scalar_function(
            "generate_series",
            vec![Expression::constant(Value::integer(1)), array_length],
        )?;
        let mut unnest = Expression::new(
            ExpressionType::BoundUnnest,
            LogicalType::BIGINT,
            ExpressionKind::Unnest(Box::new(BoundUnnestExpression {
                child: generate_series,
            })),
        );
        "row_number".clone_into(&mut unnest.alias);
        Ok(unnest)
    }

    /// DuckDB's `TryCreateUnnestOperator`: with `n > 1` the aggregate returns lists.
    fn try_create_unnest_operator(
        &self,
        op: LogicalOperator,
        params: &Parameters,
    ) -> Result<LogicalOperator> {
        let LogicalKind::Aggregate(aggregate) = &op.kind else {
            return Err(Error::internal("TopNWindowElimination: expected an aggregate"));
        };
        if params.limit <= 1 {
            // A single row per group needs no unnest.
            return Ok(op);
        }
        let aggregate_column = aggregate.groups.len();
        let aggregate_type = op
            .types
            .get(aggregate_column)
            .cloned()
            .ok_or_else(|| Error::internal("TopNWindowElimination: aggregate column out of range"))?;
        let binding = op
            .column_bindings()
            .get(aggregate_column)
            .copied()
            .ok_or_else(|| Error::internal("TopNWindowElimination: aggregate column out of range"))?;
        let aggregate_column_ref = Expression::column_ref(String::new(), aggregate_type.clone(), binding, 0);

        let mut unnest_expressions = vec![Expression::new(
            ExpressionType::BoundUnnest,
            aggregate_type.child_type().cloned().unwrap_or_default(),
            ExpressionKind::Unnest(Box::new(BoundUnnestExpression {
                child: aggregate_column_ref.clone(),
            })),
        )];
        if params.include_row_number {
            unnest_expressions.push(self.create_row_number_generator(aggregate_column_ref)?);
        }
        let mut unnest = LogicalOperator::new(LogicalKind::Unnest {
            unnest_index: self.optimizer.generate_table_index(),
        })
        .with_expressions(unnest_expressions)
        .with_children(vec![op]);
        unnest.resolve_operator_types()?;
        Ok(unnest)
    }

    /// DuckDB's `AddStructExtractExprs`: the struct's fields as columns.
    fn add_struct_extract_expressions(
        &self,
        expressions: &mut Vec<Expression>,
        struct_type: &LogicalType,
        aggregate_column_ref: &Expression,
    ) -> Result<()> {
        for (alias, _) in struct_type.struct_children().unwrap_or_default() {
            let mut extract = self.optimizer.bind_scalar_function(
                "struct_extract",
                vec![
                    aggregate_column_ref.clone(),
                    Expression::constant(Value::varchar(alias)),
                ],
            )?;
            extract.alias.clone_from(alias);
            expressions.push(extract);
        }
        Ok(())
    }

    /// DuckDB's `CreateProjectionOperator`: the used groups, the payload columns and the
    /// row number.
    fn create_projection_operator(
        &self,
        op: LogicalOperator,
        params: &Parameters,
        group_indexes: &BTreeMap<usize, usize>,
    ) -> Result<LogicalOperator> {
        let aggregate_type = aggregate_type(&op)?;
        let aggregate_table_index = aggregate_table_index(&op)?;
        let op_bindings = op.column_bindings();

        // The used groups, in the aggregate's order.
        let ordered_group_indexes: BTreeSet<usize> = group_indexes.values().copied().collect();
        let mut expressions = Vec::new();
        for group_index in ordered_group_indexes {
            let (Some(ty), Some(binding)) = (op.types.get(group_index), op_bindings.get(group_index)) else {
                return Err(Error::internal(
                    "TopNWindowElimination: group column out of range",
                ));
            };
            expressions.push(Expression::column_ref(String::new(), ty.clone(), *binding, 0));
        }
        let aggregate_column_ref = Expression::column_ref(
            String::new(),
            aggregate_type.clone(),
            ColumnBinding::new(aggregate_table_index, 0),
            0,
        );
        if params.payload_type == PayloadType::StructPack {
            self.add_struct_extract_expressions(&mut expressions, &aggregate_type, &aggregate_column_ref)?;
        } else {
            expressions.push(aggregate_column_ref);
        }
        if params.include_row_number {
            // The unnest numbers the rows; a single row per group is row 1.
            if op.operator_type() == LogicalOperatorType::LogicalUnnest {
                expressions.push(Expression::column_ref(
                    "row_number",
                    LogicalType::BIGINT,
                    row_number_binding(&op)?,
                    0,
                ));
            } else {
                expressions.push(Expression::constant(Value::bigint(1)));
            }
        }
        let mut projection =
            LogicalOperator::projection(self.optimizer.generate_table_index(), expressions, op);
        projection.resolve_operator_types()?;
        Ok(projection)
    }

    /// DuckDB's `UpdateTopmostBindings`: a projection restoring the filter's columns, and
    /// the replacements of the filter's bindings by its columns.
    #[allow(clippy::too_many_arguments)]
    fn update_topmost_bindings(
        &self,
        window_index: u64,
        op: LogicalOperator,
        types: &[LogicalType],
        group_indexes: &BTreeMap<usize, usize>,
        topmost_bindings: &[ColumnBinding],
        new_bindings: &mut [ColumnBinding],
        replacer: &mut Replacer,
        params: &Parameters,
    ) -> Result<LogicalOperator> {
        // The new operator's columns: [projected groups][payload][row number].
        let group_table_index = group_table_index(&op)?;
        let aggregate_table_index = aggregate_table_index(&op)?;
        let is_join = op.operator_type() == LogicalOperatorType::LogicalComparisonJoin;

        let compact_group_columns = group_table_index == aggregate_table_index;
        let mut compact_group_indexes = BTreeMap::new();
        if compact_group_columns {
            let ordered: BTreeSet<usize> = group_indexes.values().copied().collect();
            for (compact, group_index) in ordered.into_iter().enumerate() {
                compact_group_indexes.insert(group_index, compact);
            }
        }
        let mut current_column = 0_u64;
        for (&referencing, &group_index) in group_indexes {
            let column = if compact_group_columns && !is_join {
                compact_group_indexes
                    .get(&group_index)
                    .copied()
                    .unwrap_or_default()
            } else {
                group_index
            };
            if let Some(binding) = new_bindings.get_mut(referencing) {
                *binding = ColumnBinding::new(group_table_index, column as u64);
            }
            current_column += 1;
        }
        if !compact_group_columns {
            // An aggregate numbers its aggregates from 0.
            current_column = 0;
        }
        if is_join {
            // The late-materialized scan holds the partitions, not the aggregate's groups.
            current_column = params.partition_count as u64;
        }

        // The payload.
        let mut row_number_indexes = Vec::new();
        for (index, binding) in new_bindings.iter_mut().enumerate() {
            if group_indexes.contains_key(&index) {
                continue;
            }
            if binding.table_index == window_index {
                row_number_indexes.push(index);
                continue;
            }
            *binding = ColumnBinding::new(aggregate_table_index, current_column);
            current_column += 1;
        }
        // The row number is the last column.
        for index in row_number_indexes {
            new_bindings[index] = row_number_binding(&op)?;
        }

        // A set operation above needs the same columns, so a projection restores them.
        let mut original_types = vec![None; new_bindings.len()];
        binding_types(&op, new_bindings, &mut original_types);
        let projection_table = self.optimizer.generate_table_index();
        let mut expressions = Vec::with_capacity(topmost_bindings.len());
        for (index, old_binding) in topmost_bindings.iter().enumerate() {
            let original_type = original_types
                .get(index)
                .cloned()
                .flatten()
                .unwrap_or(LogicalType::INVALID);
            let mut expression =
                Expression::column_ref(String::new(), original_type.clone(), new_bindings[index], 0);
            if let Some(target) = types.get(index)
                && *target != original_type
            {
                expression = rustdb_planner::function_binder::add_cast_to_type(
                    self.optimizer.context(),
                    expression,
                    target,
                    false,
                )?;
            }
            expressions.push(expression);
            new_bindings[index] = ColumnBinding::new(projection_table, index as u64);
            replacer
                .replacer
                .replacement_bindings
                .push(ReplacementBinding::new(*old_binding, new_bindings[index]));
        }
        let mut projection = LogicalOperator::projection(projection_table, expressions, op);
        projection.resolve_operator_types()?;
        Ok(projection)
    }

    /// DuckDB's `ExtractOptimizerParameters`.
    fn extract_parameters(
        window: &LogicalOperator,
        window_index: u64,
        filter_expression: &Expression,
        bindings: &[ColumnBinding],
        aggregate_payload: &[Expression],
    ) -> Result<Parameters> {
        let ExpressionKind::Comparison(comparison) = &filter_expression.kind else {
            return Err(Error::internal("TopNWindowElimination: expected a comparison"));
        };
        let mut limit = comparison
            .right
            .as_constant()
            .and_then(Value::as_i128)
            .and_then(|limit| i64::try_from(limit).ok())
            .ok_or_else(|| Error::internal("TopNWindowElimination: expected a constant limit"))?;
        if filter_expression.expression_type == ExpressionType::CompareLessthan {
            limit -= 1;
        }
        let call = window
            .expressions
            .first()
            .and_then(window_call)
            .ok_or_else(|| Error::internal("TopNWindowElimination: expected a window function"))?;
        let order = call
            .orders
            .first()
            .ok_or_else(|| Error::internal("TopNWindowElimination: expected one ORDER BY"))?;
        let child = window
            .children
            .first()
            .ok_or_else(|| Error::internal("TopNWindowElimination: window without input"))?;
        let payload_type = if aggregate_payload.len() > 1 {
            PayloadType::StructPack
        } else {
            PayloadType::SingleColumn
        };
        let nullability = NotNullExpressionAnalyzer::new(None);
        let mut can_be_null = !nullability.is_not_null(child, &order.expression);
        if !can_be_null
            && payload_type == PayloadType::SingleColumn
            && let Some(payload) = aggregate_payload.first()
        {
            can_be_null = !nullability.is_not_null(child, payload);
        }
        Ok(Parameters {
            order_type: order.order_type,
            payload_type,
            limit,
            include_row_number: bindings.iter().any(|binding| binding.table_index == window_index),
            can_be_null,
            partition_count: call.partitions.len(),
            row_ids_may_have_duplicates: false,
        })
    }

    /// DuckDB's `TryPrepareLateMaterialization`: replaces the payload by the row id of
    /// the scan below, and returns the second scan that fetches the columns back.
    fn try_prepare_late_materialization(
        &self,
        window: &mut LogicalOperator,
        args: &mut Vec<Expression>,
        params: &mut Parameters,
    ) -> Result<Option<LogicalOperator>> {
        let Some((mut lhs_projections, path)) = can_use_late_materialization(window, args, params)? else {
            return Ok(None);
        };
        let window_child = window
            .children
            .first_mut()
            .ok_or_else(|| Error::internal("TopNWindowElimination: window without input"))?;
        let lhs = {
            let LogicalKind::Get(rhs_get) = &descendant(window_child, &path)?.kind else {
                return Err(Error::internal("TopNWindowElimination: expected a scan"));
            };
            self.construct_lhs(rhs_get, &mut lhs_projections)?
        };
        // Late materialization only pays if it saves columns; table scans have one row id.
        if args.len() <= 1 {
            return Ok(None);
        }

        let (mut row_id_index, rhs_table_index, rhs_projection_ids) = {
            let LogicalKind::Get(rhs_get) = &mut descendant_mut(window_child, &path)?.kind else {
                return Err(Error::internal("TopNWindowElimination: expected a scan"));
            };
            (
                get_or_insert_row_id(rhs_get),
                rhs_get.table_index,
                rhs_get.projection_ids.clone(),
            )
        };
        // Project the row id up through the operators above the scan.
        let mut last_table_index = rhs_table_index;
        for depth in (0..path.len()).rev() {
            let node = descendant_mut(window_child, &path[..depth])?;
            match &mut node.kind {
                LogicalKind::Projection { table_index } => {
                    let table_index = *table_index;
                    node.expressions.push(Expression::column_ref(
                        ROW_ID_NAME,
                        LogicalType::BIGINT,
                        ColumnBinding::new(last_table_index, row_id_index),
                        0,
                    ));
                    row_id_index = node.expressions.len() as u64 - 1;
                    last_table_index = table_index;
                }
                LogicalKind::Filter { projection_map } => {
                    if !projection_map.is_empty() {
                        // The row id's position in the scan's output.
                        let mut schema_index = row_id_index;
                        if last_table_index == rhs_table_index && !rhs_projection_ids.is_empty() {
                            schema_index = rhs_projection_ids
                                .iter()
                                .position(|&id| id == row_id_index)
                                .unwrap_or(rhs_projection_ids.len())
                                as u64;
                        }
                        projection_map.push(schema_index);
                    }
                }
                LogicalKind::ComparisonJoin(join) => {
                    let projection_map = if path[depth] == 0 {
                        &mut join.left_projection_map
                    } else {
                        &mut join.right_projection_map
                    };
                    // An empty map already projects every column, the row id included.
                    if !projection_map.is_empty() {
                        projection_map.push(row_id_index);
                    }
                }
                _ => {
                    return Err(Error::internal(
                        "Unsupported operator in late materialization right-hand side.",
                    ));
                }
            }
        }
        // The payload is now the row id.
        *args = vec![Expression::column_ref(
            ROW_ID_NAME,
            LogicalType::BIGINT,
            ColumnBinding::new(last_table_index, row_id_index),
            0,
        )];
        Ok(Some(lhs))
    }

    /// DuckDB's `ConstructLHS`: the second scan of the table, with its row id, under a
    /// projection restoring the payload order if needed.
    fn construct_lhs(
        &self,
        rhs: &rustdb_planner::operator::LogicalGet,
        projections: &mut Vec<u64>,
    ) -> Result<LogicalOperator> {
        // DuckDB's `LateMaterializationHelper::CreateLHSGet`.
        let mut lhs_get = rhs.clone();
        lhs_get.table_index = self.optimizer.generate_table_index();
        lhs_get.table_filters = rustdb_storage::TableFilterSet::default();
        lhs_get.dynamic_filters = None;
        let lhs_row_id = get_or_insert_row_id(&mut lhs_get);
        if projections.is_empty() {
            return Ok(LogicalOperator::new(LogicalKind::Get(Box::new(lhs_get))));
        }
        projections.push(lhs_row_id);
        let lhs_table_index = lhs_get.table_index;
        let expressions = projections
            .iter()
            .map(|&column| {
                let ty = lhs_get
                    .column_ids
                    .get(column as usize)
                    .map(|column_index| lhs_get.column_type(column_index))
                    .ok_or_else(|| Error::internal("TopNWindowElimination: scan column out of range"))?;
                Ok(Expression::column_ref(
                    String::new(),
                    ty,
                    ColumnBinding::new(lhs_table_index, column),
                    0,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut get = LogicalOperator::new(LogicalKind::Get(Box::new(lhs_get)));
        get.resolve_operator_types()?;
        Ok(LogicalOperator::projection(
            self.optimizer.generate_table_index(),
            expressions,
            get,
        ))
    }

    /// DuckDB's `ConstructJoin`: joins the second scan with the selected row ids.
    fn construct_join(
        &self,
        mut lhs: LogicalOperator,
        mut rhs: LogicalOperator,
        aggregate_offset: usize,
        params: &Parameters,
    ) -> Result<LogicalOperator> {
        lhs.resolve_operator_types()?;
        rhs.resolve_operator_types()?;
        if !has_external_cte_references(&rhs) {
            remove_unused_columns(self.optimizer, &mut rhs)?;
            rhs.resolve_operator_types()?;
        }
        let row_number_columns = usize::from(params.include_row_number);
        let row_id_count = rhs
            .types
            .len()
            .checked_sub(aggregate_offset + row_number_columns)
            .ok_or_else(|| Error::internal("TopNWindowElimination: missing row id columns"))?;
        let rhs_bindings = rhs.column_bindings();
        let use_inner_reconstruction = params.row_ids_may_have_duplicates || params.include_row_number;

        let mut join = LogicalComparisonJoin::new(
            if use_inner_reconstruction {
                JoinType::Inner
            } else {
                JoinType::Semi
            },
            LogicalOperatorType::LogicalComparisonJoin,
        );
        let lhs_column_count = lhs.types.len();
        for i in 0..row_id_count {
            let lhs_column = lhs_column_info(&lhs, lhs_column_count - (row_id_count - i))?;
            let rhs_column = aggregate_offset + i;
            let (Some(rhs_type), Some(rhs_binding)) =
                (rhs.types.get(rhs_column), rhs_bindings.get(rhs_column))
            else {
                return Err(Error::internal(
                    "TopNWindowElimination: row id column out of range",
                ));
            };
            join.conditions.push(JoinCondition {
                left: Expression::column_ref(lhs_column.name.clone(), lhs_column.ty, lhs_column.binding, 0),
                right: Expression::column_ref(lhs_column.name, rhs_type.clone(), *rhs_binding, 0),
                comparison: ExpressionType::CompareEqual,
            });
        }
        if params.include_row_number {
            join.right_projection_map.push(rhs.types.len() as u64 - 1);
        } else if use_inner_reconstruction {
            // An empty map would expose every column; a narrow key is removed later.
            join.right_projection_map.push(aggregate_offset as u64);
        }
        // The columns of the second scan, without the row ids.
        join.left_projection_map = (0..(lhs_column_count - row_id_count) as u64).collect();
        Ok(LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join))).with_children(vec![lhs, rhs]))
    }
}

/// DuckDB's `CanOptimize`: a filter `rn <= n` / `rn < n` / `rn = 1` over projections over
/// a `row_number()` window with column partitions and one NULLS LAST ordering. Returns
/// the number of projections in between.
fn can_optimize(op: &LogicalOperator) -> Option<usize> {
    if !matches!(op.kind, LogicalKind::Filter { .. }) {
        return None;
    }
    let [filter] = op.expressions.as_slice() else {
        return None;
    };
    let ExpressionKind::Comparison(comparison) = &filter.kind else {
        return None;
    };
    if comparison.right.expression_type != ExpressionType::ValueConstant {
        return None;
    }
    let value = comparison.right.as_constant()?;
    if value.logical_type().id() != LogicalTypeId::BigInt || value.is_null() {
        return None;
    }
    let bigint = i64::try_from(value.as_i128()?).ok()?;
    match filter.expression_type {
        ExpressionType::CompareLessthanorequalto => {
            // The limit becomes the `n` of the top-N aggregates.
            if !(1..MIN_MAX_N_MAX_VALUE).contains(&bigint) {
                return None;
            }
        }
        ExpressionType::CompareLessthan => {
            if !(2..=MIN_MAX_N_MAX_VALUE).contains(&bigint) {
                return None;
            }
        }
        ExpressionType::CompareEqual => {
            if bigint != 1 {
                return None;
            }
        }
        _ => return None,
    }
    if comparison.left.expression_type != ExpressionType::BoundColumnRef {
        return None;
    }
    let mut references = referenced_bindings(&comparison.left);
    let mut child = op.children.first()?;
    let mut depth = 0;
    while let LogicalKind::Projection { .. } = child.kind {
        let [reference] = references.as_slice() else {
            return None;
        };
        references = referenced_bindings(child.expressions.get(reference.column_index as usize)?);
        child = child.children.first()?;
        depth += 1;
    }
    let [reference] = references.as_slice() else {
        return None;
    };
    let LogicalKind::Window { window_index } = child.kind else {
        return None;
    };
    if window_index != reference.table_index {
        return None;
    }
    let first_expression = child.expressions.first()?;
    let first = window_call(first_expression)?;
    if first
        .partitions
        .iter()
        .any(|partition| partition.expression_class() != ExpressionClass::BoundColumnRef)
    {
        return None;
    }
    if child
        .expressions
        .iter()
        .skip(1)
        .any(|other| !other.equals(first_expression))
    {
        return None;
    }
    if first_expression.expression_type != ExpressionType::WindowRowNumber {
        return None;
    }
    let [order] = first.orders.as_slice() else {
        return None;
    };
    if !matches!(order.order_type, OrderType::Ascending | OrderType::Descending)
        || order.null_order != OrderByNullType::NullsLast
    {
        return None;
    }
    // A grouped Top-N window.
    Some(depth)
}

/// DuckDB's `TraverseProjectionBindings`: the filter's columns as bindings of the
/// window's output, through the projections in between.
fn traverse_projection_bindings(
    op: &LogicalOperator,
    old_bindings: &[ColumnBinding],
    projection_depth: usize,
) -> Option<Vec<ColumnBinding>> {
    let mut new_bindings = old_bindings.to_vec();
    let mut node = op.children.first()?;
    for _ in 0..projection_depth {
        for binding in &mut new_bindings {
            let [reference] = referenced_bindings(node.expressions.get(binding.column_index as usize)?)[..]
            else {
                return None;
            };
            *binding = reference;
        }
        node = node.children.first()?;
    }
    Some(new_bindings)
}

/// DuckDB's `GenerateAggregatePayload`: the columns the aggregate must carry (none if
/// only the ordering value is selected), recording which selected columns are groups.
fn generate_aggregate_payload(
    bindings: &[ColumnBinding],
    window: &mut LogicalOperator,
    window_index: u64,
    group_indexes: &mut BTreeMap<usize, usize>,
) -> Result<Vec<Expression>> {
    let child = window
        .children
        .first_mut()
        .ok_or_else(|| Error::internal("TopNWindowElimination: window without input"))?;
    child.resolve_operator_types()?;
    let child_types = child.types.clone();
    let child_bindings = child.column_bindings();
    let child_is_projection = child.operator_type() == LogicalOperatorType::LogicalProjection;
    let call = window
        .expressions
        .first()
        .and_then(window_call)
        .ok_or_else(|| Error::internal("TopNWindowElimination: expected a window function"))?;

    // The partitions' positions, to restore their order later.
    let mut group_bindings = HashMap::new();
    for (index, partition) in call.partitions.iter().enumerate() {
        if let Some(&binding) = referenced_bindings(partition).first() {
            group_bindings.insert(binding, index);
        }
    }

    let mut aggregate_args = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        if let Some(&group) = group_bindings.get(binding) {
            group_indexes.insert(index, group);
            continue;
        }
        if binding.table_index == window_index {
            continue;
        }
        // A projection's column index is the position; other operators may have several
        // or no table indexes.
        let position = if child_is_projection {
            binding.column_index as usize
        } else {
            child_bindings
                .iter()
                .position(|candidate| candidate == binding)
                .unwrap_or(child_bindings.len())
        };
        let ty = child_types
            .get(position)
            .cloned()
            .ok_or_else(|| Error::internal("TopNWindowElimination: payload column out of range"))?;
        aggregate_args.push(Expression::column_ref(binding.to_string(), ty, *binding, 0));
    }

    if let [arg] = aggregate_args.as_slice() {
        // Selecting only the ordering value needs no payload.
        let order = &call.orders[0].expression;
        let [value_binding] = referenced_bindings(order)[..] else {
            return Ok(aggregate_args);
        };
        if order.expression_type == ExpressionType::BoundColumnRef
            && matches!(&arg.kind, ExpressionKind::ColumnRef(column) if column.binding == value_binding)
        {
            return Ok(Vec::new());
        }
    }
    Ok(aggregate_args)
}

/// DuckDB's `CanUseLateMaterialization`: whether the payload and partitions all come
/// from one table scan below projections, filters and equi-joins. Returns the positions
/// of the payload in the second scan (when out of order) and the path to the scan.
fn can_use_late_materialization(
    window: &LogicalOperator,
    args: &[Expression],
    params: &mut Parameters,
) -> Result<Option<(Vec<u64>, Vec<usize>)>> {
    let call = window
        .expressions
        .first()
        .and_then(window_call)
        .ok_or_else(|| Error::internal("TopNWindowElimination: expected a window function"))?;
    // The scan columns of the partitions and the payload, in the aggregate's order.
    let mut projections = Vec::with_capacity(call.partitions.len() + args.len());
    for expression in call.partitions.iter().chain(args) {
        let Some(binding) = single_binding(expression, false) else {
            return Ok(None);
        };
        projections.push(binding);
    }

    let mut op = window
        .children
        .first()
        .ok_or_else(|| Error::internal("TopNWindowElimination: window without input"))?;
    let mut path = Vec::new();
    // Walk down to a single table scan.
    while !op.children.is_empty() {
        match &op.kind {
            LogicalKind::Projection { table_index } => {
                for projection in &mut projections {
                    if projection.table_index != *table_index {
                        return Ok(None);
                    }
                    let Some(expression) = op.expressions.get(projection.column_index as usize) else {
                        return Ok(None);
                    };
                    let Some(binding) = single_binding(expression, true) else {
                        return Ok(None);
                    };
                    *projection = binding;
                }
                path.push(0);
                op = &op.children[0];
            }
            LogicalKind::Filter { .. } => {
                path.push(0);
                op = &op.children[0];
            }
            LogicalKind::ComparisonJoin(join)
                if op.operator_type() == LogicalOperatorType::LogicalComparisonJoin =>
            {
                if !matches!(join.join_type, JoinType::Inner | JoinType::Semi | JoinType::Anti) {
                    return Ok(None);
                }
                if join.join_type == JoinType::Inner {
                    // An inner join can repeat a row, so the reconstruction must be an
                    // inner join too.
                    params.row_ids_may_have_duplicates = true;
                }
                // Columns equal by a join condition can be read from either side.
                let mut replaceable = HashMap::new();
                for condition in &join.conditions {
                    if condition.comparison != ExpressionType::CompareEqual {
                        return Ok(None);
                    }
                    let (Some(left), Some(right)) = (
                        single_binding(&condition.left, true),
                        single_binding(&condition.right, true),
                    ) else {
                        return Ok(None);
                    };
                    replaceable.insert(left, right);
                    replaceable.insert(right, left);
                }
                let [left_child, right_child] = op.children.as_slice() else {
                    return Ok(None);
                };
                let left_bindings = left_child.column_bindings();
                let right_bindings = right_child.column_bindings();
                let (&[left_index], &[right_index]) = (
                    left_child.table_indexes().as_slice(),
                    right_child.table_indexes().as_slice(),
                ) else {
                    return Ok(None);
                };
                let child_binding = |projection: &ColumnBinding| -> Option<ColumnBinding> {
                    let column = projection.column_index as usize;
                    if projection.table_index == left_index {
                        left_bindings.get(column).copied()
                    } else if projection.table_index == right_index {
                        right_bindings.get(column).copied()
                    } else {
                        None
                    }
                };
                let mut all_left_replaceable = true;
                let mut all_right_replaceable = true;
                for projection in &projections {
                    let Some(column_binding) = child_binding(projection) else {
                        return Ok(None);
                    };
                    if !replaceable.contains_key(&column_binding) {
                        if column_binding.table_index == left_index {
                            all_left_replaceable = false;
                        } else {
                            all_right_replaceable = false;
                        }
                    }
                }
                if !all_left_replaceable && !all_right_replaceable {
                    // The columns come from both sides.
                    return Ok(None);
                }
                let replace_table_index = if all_right_replaceable {
                    right_index
                } else {
                    left_index
                };
                for projection in &mut projections {
                    let Some(column_binding) = child_binding(projection) else {
                        return Ok(None);
                    };
                    if column_binding.table_index == replace_table_index {
                        *projection = replaceable
                            .get(&column_binding)
                            .copied()
                            .unwrap_or_else(|| ColumnBinding::new(u64::MAX, u64::MAX));
                    }
                }
                let side = usize::from(!all_right_replaceable);
                path.push(side);
                op = &op.children[side];
            }
            _ => return Ok(None),
        }
    }
    // Only table scans fetch rows by row id.
    let LogicalKind::Get(get) = &op.kind else {
        return Ok(None);
    };
    if get.table.is_none() {
        return Ok(None);
    }
    // A projection restores the payload order if the scan's differs.
    let mut lhs_projections = Vec::new();
    if projections
        .iter()
        .enumerate()
        .any(|(index, projection)| projection.column_index != index as u64)
    {
        lhs_projections = projections
            .iter()
            .map(|projection| projection.column_index)
            .collect();
    }
    Ok(Some((lhs_projections, path)))
}
