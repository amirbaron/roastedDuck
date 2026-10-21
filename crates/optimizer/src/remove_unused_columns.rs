//! Removing columns nothing reads (DuckDB's `RemoveUnusedColumns`): unreferenced
//! projection and aggregate expressions are dropped, scans read only the referenced
//! columns (and exclude filter-only columns from their output through `projection_ids`),
//! UNION ALL inputs are narrowed and materialized CTEs only keep the columns their
//! readers use.
//!
//! DuckDB keeps pointers to every column reference it registered and rewrites them in
//! place when a column below moves. Here the plan is owned, so the rewrites are recorded
//! instead and applied to an operator's expressions once its subtree has been processed:
//! - a producer that drops columns renumbers the remaining ones, which is a plan-wide
//!   binding map (table indexes are unique);
//! - an inner join on `l = r` makes the references to `r` *above* the join refer to `l`
//!   ([`JoinReplacement`]); those apply to the expressions of the join's ancestors.
//!
//! Not ported: struct field projection pushdown (`PushdownExtract`), which needs storage
//! support for scanning single struct fields.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_planner::operator::{COLUMN_IDENTIFIER_EMPTY, COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, LogicalGet};
use rustdb_planner::{
    ColumnBinding, DistinctType, Expression, ExpressionKind, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_types::{LogicalTypeId, Value};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::optimizer::{Optimizer, bind_aggregate_function};

/// References to `from` above an inner join that now read `to` (the other side of an
/// equality condition).
#[derive(Clone, Copy, Debug)]
struct JoinReplacement {
    from: ColumnBinding,
    to: ColumnBinding,
}

/// What the readers of a CTE use (DuckDB's `MaterializedCTEInfo`).
#[derive(Debug)]
struct MaterializedCteInfo {
    column_references: HashSet<ColumnBinding>,
    expected_readers: HashSet<u64>,
    seen_readers: HashSet<u64>,
    everything_referenced: bool,
}

impl Default for MaterializedCteInfo {
    fn default() -> Self {
        Self {
            column_references: HashSet::new(),
            expected_readers: HashSet::new(),
            seen_readers: HashSet::new(),
            everything_referenced: true,
        }
    }
}

/// State shared by all the pruners of one run.
#[derive(Default)]
struct SharedState {
    /// The new binding of every column a producer renumbered.
    replacements: HashMap<ColumnBinding, ColumnBinding>,
    /// The CTEs seen so far (DuckDB's `root_cte_map`), created on first use.
    cte_map: Option<HashMap<u64, MaterializedCteInfo>>,
}

/// Removes unreferenced columns from `plan` (DuckDB's `RemoveUnusedColumns` run from the
/// root, where everything the plan outputs is referenced).
pub fn remove_unused_columns(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let shared = RefCell::new(SharedState::default());
    let mut root = RemoveUnusedColumns::new(optimizer, &shared, true);
    root.visit_operator(plan)?;
    Ok(())
}

/// One pruning scope (DuckDB's `RemoveUnusedColumns` instance): the column references
/// collected from the operators above, up to the closest operator that defines new
/// bindings.
struct RemoveUnusedColumns<'a> {
    optimizer: &'a Optimizer,
    shared: &'a RefCell<SharedState>,
    everything_referenced: bool,
    /// How often each binding is referenced (DuckDB's `column_references`).
    column_references: HashMap<ColumnBinding, usize>,
}

impl<'a> RemoveUnusedColumns<'a> {
    fn new(optimizer: &'a Optimizer, shared: &'a RefCell<SharedState>, everything_referenced: bool) -> Self {
        Self {
            optimizer,
            shared,
            everything_referenced,
            column_references: HashMap::new(),
        }
    }

    /// A fresh scope below this one.
    fn child(&self, everything_referenced: bool) -> Self {
        Self::new(self.optimizer, self.shared, everything_referenced)
    }

    fn add_binding(&mut self, binding: ColumnBinding) {
        *self.column_references.entry(binding).or_default() += 1;
    }

    /// Registers the column references of an expression (DuckDB's `VisitExpression` /
    /// `VisitReplace`).
    fn visit_expression(&mut self, expression: &Expression) {
        if let ExpressionKind::ColumnRef(column) = &expression.kind {
            self.add_binding(column.binding);
            return;
        }
        for child in expression.children() {
            self.visit_expression(child);
        }
    }

    /// Registers the column references of an operator's expressions.
    fn visit_operator_expressions(&mut self, op: &mut LogicalOperator) {
        op.for_each_expression_mut(|expression| self.visit_expression(expression));
    }

    /// Visits the children of `op` in this scope.
    fn visit_operator_children(&mut self, op: &mut LogicalOperator) -> Result<Vec<JoinReplacement>> {
        let mut replacements = Vec::new();
        for child in &mut op.children {
            replacements.extend(self.visit_operator(child)?);
        }
        Ok(replacements)
    }

    /// Visits `op` in a fresh scope.
    fn visit_in_child_scope(
        &self,
        op: &mut LogicalOperator,
        everything_referenced: bool,
    ) -> Result<Vec<JoinReplacement>> {
        self.child(everything_referenced).visit_operator(op)
    }

    /// Drops the entries of `list` whose binding `(table_index, position)` is not
    /// referenced; with `replace`, records the new positions of the remaining ones
    /// (DuckDB's `ClearUnusedExpressions`).
    fn clear_unused_expressions<T>(&self, list: &mut Vec<T>, table_index: u64, replace: bool) {
        let mut kept = Vec::with_capacity(list.len());
        let mut replacements = Vec::new();
        for (position, item) in std::mem::take(list).into_iter().enumerate() {
            let binding = ColumnBinding::new(table_index, position as u64);
            if !self.column_references.contains_key(&binding) {
                continue;
            }
            let new_position = kept.len() as u64;
            if replace && new_position != position as u64 {
                replacements.push((binding, ColumnBinding::new(table_index, new_position)));
            }
            kept.push(item);
        }
        *list = kept;
        if !replacements.is_empty() {
            self.shared.borrow_mut().replacements.extend(replacements);
        }
    }

    /// Visits `op` and its subtree, then rewrites the column references of `op`'s own
    /// expressions for the columns that moved below it. Returns the join replacements
    /// made in the subtree, in visiting order.
    fn visit_operator(&mut self, op: &mut LogicalOperator) -> Result<Vec<JoinReplacement>> {
        let (own, below) = self.visit_operator_inner(op)?;
        self.apply_replacements(op, &below);
        if let LogicalKind::ComparisonJoin(join) = &mut op.kind {
            remove_duplicate_conditions(join);
        }
        let mut replacements = own;
        replacements.extend(below);
        Ok(replacements)
    }

    /// Rewrites the column references in `op`'s expressions: first the join replacements
    /// made below `op` (top-down), then the renumbering of the producers.
    fn apply_replacements(&self, op: &mut LogicalOperator, joins: &[JoinReplacement]) {
        let shared = self.shared.borrow();
        if joins.is_empty() && shared.replacements.is_empty() {
            return;
        }
        op.for_each_expression_mut(|expression| {
            rewrite_column_refs(expression, &mut |binding| {
                let mut binding = binding;
                for join in joins {
                    if binding == join.from {
                        binding = join.to;
                    }
                }
                shared.replacements.get(&binding).copied().unwrap_or(binding)
            });
        });
    }

    /// DuckDB's `RemoveUnusedColumns::VisitOperator`; returns the join replacements made
    /// at `op` itself and those made below it.
    #[allow(clippy::too_many_lines)]
    fn visit_operator_inner(
        &mut self,
        op: &mut LogicalOperator,
    ) -> Result<(Vec<JoinReplacement>, Vec<JoinReplacement>)> {
        let mut own = Vec::new();
        match op.operator_type() {
            LogicalOperatorType::LogicalAggregateAndGroupBy => {
                if !self.everything_referenced
                    && let LogicalKind::Aggregate(aggregate) = &op.kind
                {
                    let aggregate_index = aggregate.aggregate_index;
                    let no_groups = aggregate.groups.is_empty();
                    self.clear_unused_expressions(&mut op.expressions, aggregate_index, true);
                    if op.expressions.is_empty() && no_groups {
                        // Everything was removed: keep a COUNT(*) so the aggregate still
                        // produces its single row.
                        op.expressions.push(bind_aggregate_function(
                            self.optimizer.context(),
                            "count_star",
                            Vec::new(),
                        )?);
                    }
                }
                let mut scope = self.child(self.everything_referenced);
                scope.visit_operator_expressions(op);
                let below = scope.visit_operator_children(op)?;
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalAsofJoin
            | LogicalOperatorType::LogicalDelimJoin
            | LogicalOperatorType::LogicalComparisonJoin => {
                if !self.everything_referenced
                    && let LogicalKind::ComparisonJoin(join) = &op.kind
                    && join.join_type == JoinType::Inner
                {
                    // For inner joins on `X = Y`, references to the right side `Y` can
                    // read the left side `X` instead, so `Y` need not be extracted from
                    // the hash table (not for floats: +0 and -0 are equal but differ).
                    for condition in &join.conditions {
                        if condition.comparison != rustdb_planner::ExpressionType::CompareEqual {
                            continue;
                        }
                        let (ExpressionKind::ColumnRef(left), ExpressionKind::ColumnRef(right)) =
                            (&condition.left.kind, &condition.right.kind)
                        else {
                            continue;
                        };
                        if is_floating(&condition.left) || is_floating(&condition.right) {
                            continue;
                        }
                        let Some(count) = self.column_references.remove(&right.binding) else {
                            continue;
                        };
                        *self.column_references.entry(left.binding).or_default() += count;
                        own.push(JoinReplacement {
                            from: right.binding,
                            to: left.binding,
                        });
                    }
                }
            }
            LogicalOperatorType::LogicalUnion => {
                let LogicalKind::SetOperation {
                    table_index,
                    column_count,
                    setop_all,
                    ..
                } = op.kind
                else {
                    return Err(Error::internal("UNION is not a set operation"));
                };
                let mut below = Vec::new();
                if setop_all && !self.everything_referenced {
                    // For UNION ALL the unreferenced columns can be removed (for UNION they
                    // affect the result).
                    let mut entries: Vec<u64> = (0..column_count).collect();
                    self.clear_unused_expressions(&mut entries, table_index, true);
                    if entries.len() as u64 >= column_count {
                        for child in &mut op.children {
                            below.extend(self.visit_in_child_scope(child, true)?);
                        }
                        return Ok((own, below));
                    }
                    if entries.is_empty() {
                        // Nothing is referenced (e.g. a COUNT(*)): keep the first column.
                        entries.push(0);
                    }
                    if let LogicalKind::SetOperation { column_count, .. } = &mut op.kind {
                        *column_count = entries.len() as u64;
                    }
                    for child in &mut op.children {
                        // Narrow each input to the referenced columns with a projection.
                        child.resolve_operator_types()?;
                        let bindings = child.column_bindings();
                        let expressions = entries
                            .iter()
                            .map(|&column| {
                                let column = column as usize;
                                Expression::column_ref(
                                    String::new(),
                                    child.types[column].clone(),
                                    bindings[column],
                                    0,
                                )
                            })
                            .collect();
                        let input = std::mem::replace(child, LogicalOperator::new(LogicalKind::CrossProduct));
                        let estimated_cardinality = input.estimated_cardinality;
                        let mut projection = LogicalOperator::projection(
                            self.optimizer.generate_table_index(),
                            expressions,
                            input,
                        );
                        projection.estimated_cardinality = estimated_cardinality;
                        *child = projection;
                        below.extend(self.visit_in_child_scope(child, true)?);
                    }
                    return Ok((own, below));
                }
                for child in &mut op.children {
                    below.extend(self.visit_in_child_scope(child, true)?);
                }
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalExcept | LogicalOperatorType::LogicalIntersect => {
                // Nothing can be removed from EXCEPT/INTERSECT.
                let mut below = Vec::new();
                for child in &mut op.children {
                    below.extend(self.visit_in_child_scope(child, true)?);
                }
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalProjection => {
                if !self.everything_referenced
                    && let LogicalKind::Projection { table_index } = op.kind
                {
                    self.clear_unused_expressions(&mut op.expressions, table_index, true);
                    if op.expressions.is_empty() {
                        // Nothing references the projection (e.g. EXISTS(SELECT * ...)):
                        // project a single constant.
                        op.expressions.push(Expression::constant(Value::integer(42)));
                    }
                }
                let mut scope = self.child(false);
                scope.visit_operator_expressions(op);
                let below = scope.visit_operator_children(op)?;
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalInsert
            | LogicalOperatorType::LogicalUpdate
            | LogicalOperatorType::LogicalDelete
            | LogicalOperatorType::LogicalMergeInto => {
                // A RETURNING projection reads the modified rows: keep everything.
                let mut scope = self.child(true);
                scope.visit_operator_expressions(op);
                let below = scope.visit_operator_children(op)?;
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalGet => {
                self.visit_operator_expressions(op);
                if let LogicalKind::Get(get) = &mut op.kind {
                    self.remove_columns_from_get(get)?;
                }
                let mut below = Vec::new();
                for child in &mut op.children {
                    // Table in-out functions read their input entirely.
                    below.extend(self.visit_in_child_scope(child, true)?);
                }
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalDistinct => {
                if let LogicalKind::Distinct { distinct_type, .. } = &op.kind
                    && *distinct_type != DistinctType::DistinctOn
                {
                    // All columns take part in the DISTINCT.
                    self.everything_referenced = true;
                }
            }
            LogicalOperatorType::LogicalRecursiveCte => {
                // Columns of recursive CTEs are not pruned, but their readers must find
                // the CTE.
                if let LogicalKind::RecursiveCte { table_index, .. } = op.kind {
                    self.shared
                        .borrow_mut()
                        .cte_map
                        .get_or_insert_with(HashMap::new)
                        .insert(table_index, MaterializedCteInfo::default());
                }
                self.everything_referenced = true;
            }
            LogicalOperatorType::LogicalMaterializedCte => {
                let below = self.visit_materialized_cte(op)?;
                return Ok((own, below));
            }
            LogicalOperatorType::LogicalCteRef => {
                if let LogicalKind::CteRef {
                    table_index,
                    cte_index,
                    chunk_types,
                    ..
                } = &op.kind
                {
                    self.visit_cte_ref(*table_index, *cte_index, chunk_types.len())?;
                }
            }
            LogicalOperatorType::LogicalCopyToFile
            | LogicalOperatorType::LogicalPivot
            | LogicalOperatorType::LogicalVacuum => {
                self.everything_referenced = true;
            }
            _ => {}
        }
        self.visit_operator_expressions(op);
        let below = self.visit_operator_children(op)?;
        Ok((own, below))
    }

    /// A CTE reader: the columns referenced above it are the CTE's referenced columns.
    fn visit_cte_ref(&mut self, table_index: u64, cte_index: u64, column_count: usize) -> Result<()> {
        let mut shared = self.shared.borrow_mut();
        let Some(cte_map) = shared.cte_map.as_mut() else {
            self.everything_referenced = true;
            return Ok(());
        };
        let entry = cte_map
            .get_mut(&cte_index)
            .ok_or_else(|| Error::internal("Could not find CTE definition for CTE reference"))?;
        entry.seen_readers.insert(table_index);
        entry.column_references.extend(
            self.column_references
                .keys()
                .filter(|binding| binding.table_index == table_index)
                .copied(),
        );
        // A reader whose columns are all needed by its parent (e.g. directly below a set
        // operation) references the whole CTE.
        entry.everything_referenced |=
            self.everything_referenced || column_count == entry.column_references.len();
        Ok(())
    }

    /// DuckDB's `LOGICAL_MATERIALIZED_CTE` case: visits the readers first, then narrows
    /// the CTE definition to the columns they use.
    fn visit_materialized_cte(&mut self, op: &mut LogicalOperator) -> Result<Vec<JoinReplacement>> {
        let LogicalKind::MaterializedCte { table_index, .. } = op.kind else {
            return Err(Error::internal("not a materialized CTE"));
        };
        if op.children.len() != 2 {
            return Err(Error::internal("materialized CTE without two children"));
        }
        {
            let mut expected_readers = HashSet::new();
            gather_cte_scans(table_index, &op.children[1], &mut expected_readers);
            let mut shared = self.shared.borrow_mut();
            let entry = shared
                .cte_map
                .get_or_insert_with(HashMap::new)
                .entry(table_index)
                .or_default();
            entry.expected_readers.extend(expected_readers);
            entry.everything_referenced = false;
        }
        let mut below = self.visit_in_child_scope(&mut op.children[1], true)?;
        let (referenced_columns, everything_referenced) = {
            let shared = self.shared.borrow();
            let entry = shared
                .cte_map
                .as_ref()
                .and_then(|map| map.get(&table_index))
                .ok_or_else(|| Error::internal("CTE pruning lost its CTE"))?;
            if entry.expected_readers != entry.seen_readers {
                return Err(Error::internal(
                    "CTE pruning did not traverse the entire plan. This is a bug in the optimizer.",
                ));
            }
            let referenced: HashSet<u64> = entry
                .column_references
                .iter()
                .map(|binding| binding.column_index)
                .collect();
            (referenced, entry.everything_referenced)
        };
        if everything_referenced {
            self.everything_referenced = true;
            below.extend(self.visit_in_child_scope(&mut op.children[0], true)?);
            return Ok(below);
        }
        let mut referenced_columns = referenced_columns;
        if referenced_columns.is_empty() {
            referenced_columns.insert(0);
        }
        // Project only the referenced columns of the CTE definition.
        let definition = &mut op.children[0];
        definition.resolve_operator_types()?;
        let bindings = definition.column_bindings();
        let expressions = bindings
            .iter()
            .enumerate()
            .filter(|(column, _)| referenced_columns.contains(&(*column as u64)))
            .map(|(column, binding)| {
                Expression::column_ref(String::new(), definition.types[column].clone(), *binding, 0)
            })
            .collect();
        let input = std::mem::replace(definition, LogicalOperator::new(LogicalKind::CrossProduct));
        *definition = LogicalOperator::projection(self.optimizer.generate_table_index(), expressions, input);
        below.extend(self.visit_in_child_scope(definition, true)?);
        // The readers now produce fewer columns: renumber their references.
        let mut replacer = ColumnBindingReplacer::default();
        prune_cte_refs(
            table_index,
            &referenced_columns,
            &mut op.children[1],
            &mut replacer.replacement_bindings,
        );
        replacer.visit_operator(&mut op.children[1]);
        Ok(below)
    }

    /// Narrows a scan to the referenced columns (DuckDB's `RemoveColumnsFromLogicalGet`).
    fn remove_columns_from_get(&mut self, get: &mut LogicalGet) -> Result<()> {
        if self.everything_referenced || !supports_projection_pushdown(get) {
            return Ok(());
        }
        let old_column_ids = std::mem::take(&mut get.column_ids);
        let column_count = old_column_ids.len() as u64;
        // The columns the operators above use...
        let mut projection_selection: Vec<u64> = (0..column_count).collect();
        self.clear_unused_expressions(&mut projection_selection, get.table_index, false);
        // ...and those the pushed-down filters need as well.
        for &column in get.table_filters.filters.keys() {
            let index = old_column_ids
                .iter()
                .position(|id| id.index == column as u64)
                .ok_or_else(|| Error::internal("Could not find column index for table filter"))?;
            self.add_binding(ColumnBinding::new(get.table_index, index as u64));
        }
        let mut column_selection: Vec<u64> = (0..column_count).collect();
        self.clear_unused_expressions(&mut column_selection, get.table_index, true);

        let mut original_ids: Vec<u64> = Vec::with_capacity(column_selection.len());
        let mut new_column_ids: Vec<ColumnIndex> = Vec::with_capacity(column_selection.len());
        for &selected in &column_selection {
            original_ids.push(selected);
            new_column_ids.push(old_column_ids[selected as usize].clone());
        }
        if new_column_ids.is_empty() {
            // Only the existence of rows matters (e.g. EXISTS(SELECT * FROM tbl)): scan
            // the cheapest column.
            let any_column = any_column(get);
            original_ids.push(any_column);
            new_column_ids.push(ColumnIndex::new(any_column));
        }
        get.column_ids = new_column_ids;
        if !supports_filter_prune(get) {
            return Ok(());
        }
        // Output only the columns used above, not the filter-only ones.
        get.projection_ids.clear();
        let mut filtered_original_ids = Vec::new();
        let mut column_index = 0;
        for &to_keep in &projection_selection {
            while column_index < column_selection.len() {
                if column_selection[column_index] == to_keep {
                    filtered_original_ids.push(to_keep);
                    break;
                }
                column_index += 1;
            }
        }
        let mut column_index = 0;
        for &column in &filtered_original_ids {
            while column_index < original_ids.len() {
                if original_ids[column_index] == column {
                    get.projection_ids.push(column_index as u64);
                } else if original_ids[column_index] > column {
                    break;
                }
                column_index += 1;
            }
        }
        Ok(())
    }
}

/// Whether a scan can produce only the requested columns (DuckDB's
/// `TableFunction::projection_pushdown`; base table scans always can).
pub(crate) fn supports_projection_pushdown(get: &LogicalGet) -> bool {
    get.table.is_some() || get.function.projection_pushdown
}

/// Whether a scan can drop filter-only columns from its output (DuckDB's
/// `TableFunction::filter_prune`, set for base table scans).
pub(crate) fn supports_filter_prune(get: &LogicalGet) -> bool {
    get.table.is_some() || get.function.filter_prune
}

/// The column to scan when no column is needed (DuckDB's `LogicalGet::GetAnyColumn`):
/// the empty column of a multi-file reader, the row id of a base table, the first
/// column otherwise.
pub(crate) fn any_column(get: &LogicalGet) -> u64 {
    if get.function.empty_column {
        COLUMN_IDENTIFIER_EMPTY
    } else if get.table.is_some() {
        COLUMN_IDENTIFIER_ROW_ID
    } else {
        0
    }
}

/// Whether an expression is a FLOAT or DOUBLE (DuckDB's `LogicalType::IsFloating`).
fn is_floating(expression: &Expression) -> bool {
    matches!(
        expression.return_type.id(),
        LogicalTypeId::Float | LogicalTypeId::Double
    )
}

/// Rewrites the binding of every column reference in `expression`.
fn rewrite_column_refs(
    expression: &mut Expression,
    rewrite: &mut impl FnMut(ColumnBinding) -> ColumnBinding,
) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        column.binding = rewrite(column.binding);
        return;
    }
    expression.for_each_child_mut(|child| rewrite_column_refs(child, rewrite));
}

/// Drops join conditions that became duplicates once references were redirected (when
/// the join graph is cyclic).
fn remove_duplicate_conditions(join: &mut rustdb_planner::operator::LogicalComparisonJoin) {
    let mut unique: Vec<rustdb_planner::operator::JoinCondition> = Vec::with_capacity(join.conditions.len());
    for condition in std::mem::take(&mut join.conditions) {
        let duplicate = unique.iter().any(|existing| {
            existing.comparison == condition.comparison
                && existing.left.equals(&condition.left)
                && existing.right.equals(&condition.right)
        });
        if !duplicate {
            unique.push(condition);
        }
    }
    join.conditions = unique;
}

/// The table indexes of the readers of CTE `cte_index` below `op` (DuckDB's
/// `GatherCTEScans`).
fn gather_cte_scans(cte_index: u64, op: &LogicalOperator, readers: &mut HashSet<u64>) {
    if let LogicalKind::CteRef {
        table_index,
        cte_index: reference,
        ..
    } = &op.kind
        && *reference == cte_index
    {
        readers.insert(*table_index);
    }
    for child in &op.children {
        gather_cte_scans(cte_index, child, readers);
    }
}

/// Narrows the readers of CTE `cte_index` to the referenced columns and collects the
/// renumbering of their bindings (DuckDB's `CTERefPruner`).
fn prune_cte_refs(
    cte_index: u64,
    referenced_columns: &HashSet<u64>,
    op: &mut LogicalOperator,
    replacements: &mut Vec<ReplacementBinding>,
) {
    if let LogicalKind::CteRef {
        table_index,
        cte_index: reference,
        chunk_types,
        bound_columns,
        ..
    } = &mut op.kind
        && *reference == cte_index
    {
        let mut types = Vec::new();
        let mut names = Vec::new();
        let mut skipped = 0;
        for (column, ty) in chunk_types.iter().enumerate() {
            let column = column as u64;
            if referenced_columns.contains(&column) {
                types.push(ty.clone());
                if let Some(name) = bound_columns.get(column as usize) {
                    names.push(name.clone());
                }
                if skipped > 0 {
                    replacements.push(ReplacementBinding::new(
                        ColumnBinding::new(*table_index, column),
                        ColumnBinding::new(*table_index, column - skipped),
                    ));
                }
            } else {
                skipped += 1;
            }
        }
        if bound_columns.len() == chunk_types.len() {
            *bound_columns = names;
        }
        chunk_types.clone_from(&types);
        op.types = types;
    }
    for child in &mut op.children {
        prune_cte_refs(cte_index, referenced_columns, child, replacements);
    }
}
