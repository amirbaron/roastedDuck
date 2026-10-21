//! Removing columns nothing reads (DuckDB's `RemoveUnusedColumns` with its
//! `BaseColumnPruner`): unreferenced projection and aggregate expressions are dropped,
//! scans read only the referenced columns (and exclude filter-only columns from their
//! output through `projection_ids`), UNION ALL inputs are narrowed and materialized CTEs
//! only keep the columns their readers use.
//!
//! Struct field projection pushdown: a column only read through `struct_extract` (or
//! `variant_extract`) calls is scanned with child indexes naming the fields. A table scan
//! of a STRUCT or VARIANT column goes further (`PushdownExtract`): it emits each extracted
//! field (cast to the type of a cast on top of the extract) as a column of its own, and
//! the extracts become references to those columns. A projection of such a column pulls
//! the extracts up into itself, so the scan below it can do the same.
//!
//! DuckDB keeps pointers to every column reference (and struct extract) it registered and
//! rewrites them in place when a column below moves. Here the plan is owned, so the
//! rewrites are recorded instead and applied to an operator's expressions once its
//! subtree has been processed:
//! - a producer that drops columns renumbers the remaining ones, which is a plan-wide
//!   binding map (table indexes are unique);
//! - a producer that pushes extracts down maps each extract path of a column to the new
//!   column that produces it; the extracts on that column are found again when the
//!   expressions are rewritten (the same way they were registered) and replaced;
//! - an inner join on `l = r` makes the references to `r` *above* the join refer to `l`
//!   ([`JoinReplacement`]); those apply to the expressions of the join's ancestors.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rustdb_ast::JoinType;
use rustdb_catalog::EntryKind;
use rustdb_common::{Error, Result};
use rustdb_planner::operator::{
    COLUMN_IDENTIFIER_EMPTY, COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, ExpressionTableFilter, LogicalGet,
};
use rustdb_planner::{
    ColumnBinding, DistinctType, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_storage::table_filter::TableFilter;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::optimizer::{Optimizer, bind_aggregate_function};

/// References to `from` above an inner join that now read `to` (the other side of an
/// equality condition).
#[derive(Clone, Copy, Debug)]
struct JoinReplacement {
    from: ColumnBinding,
    to: ColumnBinding,
}

/// Whether the extracts on a column can be pushed into the operator producing it
/// (DuckDB's `PushdownExtractSupport`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PushdownExtractSupport {
    #[default]
    Unchecked,
    Disabled,
    Enabled,
}

/// DuckDB's `BaseColumnPrunerMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrunerMode {
    #[default]
    Default,
    /// Any reference to a child of a column disables the pushdown of its extracts.
    DisablePushdownExtract,
}

/// A chain of extracts on a referenced column (DuckDB's `ReferencedStructExtract`).
#[derive(Clone, Debug)]
struct ReferencedStructExtract {
    /// The path the chain reads below the column (`extract_path`); its leaf has the type
    /// of a cast on top of the chain.
    extract_path: ColumnIndex,
    /// The number of extracts in the chain (`components.size()`).
    component_count: usize,
    /// The type of the cast on top of the chain (the top component's `cast`).
    cast_type: Option<LogicalType>,
}

/// How a column is referenced (DuckDB's `ReferencedColumn`).
#[derive(Clone, Debug, Default)]
struct ReferencedColumn {
    /// The number of column references to it (DuckDB's `bindings`).
    bindings: usize,
    struct_extracts: Vec<ReferencedStructExtract>,
    /// The children read, if the column is only read through extracts (empty: all of
    /// it).
    child_columns: Vec<ColumnIndex>,
    supports_pushdown_extract: PushdownExtractSupport,
    /// The distinct extract paths, none a prefix of another (`unique_paths`).
    unique_paths: Vec<ColumnIndex>,
}

impl ReferencedColumn {
    /// Whether the extracts on the column are pushed into its producer: one column per
    /// unique path replaces it.
    fn pushes_down_extract(&self) -> bool {
        !self.child_columns.is_empty() && self.supports_pushdown_extract == PushdownExtractSupport::Enabled
    }

    /// Registers an extract path, unless a prefix of it is registered; registered paths
    /// it is a prefix of are dropped (`ReferencedColumn::AddPath`).
    fn add_path(&mut self, path: &ColumnIndex) {
        if self.child_columns.is_empty() {
            // The whole column is referenced: no pushdown.
            return;
        }
        if find_shortest_matching_path(&self.unique_paths, path).is_some() {
            return;
        }
        self.unique_paths
            .retain(|unique_path| !unique_path.is_child_path_of(path));
        self.unique_paths.push(path.clone());
    }
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

/// The columns a pushed-down column became: per unique extract path, the new column
/// index (in the producer's table) that produces it.
type PushdownColumns = Vec<(ColumnIndex, u64)>;

/// State shared by all the pruners of one run.
#[derive(Default)]
struct SharedState {
    /// The new binding of every column a producer renumbered.
    replacements: HashMap<ColumnBinding, ColumnBinding>,
    /// The columns whose extracts were pushed into their producer.
    pushdown: HashMap<ColumnBinding, PushdownColumns>,
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
    /// How each binding is referenced (DuckDB's `column_references`).
    column_references: HashMap<ColumnBinding, ReferencedColumn>,
    mode: PrunerMode,
}

impl<'a> RemoveUnusedColumns<'a> {
    fn new(optimizer: &'a Optimizer, shared: &'a RefCell<SharedState>, everything_referenced: bool) -> Self {
        Self {
            optimizer,
            shared,
            everything_referenced,
            column_references: HashMap::new(),
            mode: PrunerMode::Default,
        }
    }

    /// A fresh scope below this one.
    fn child(&self, everything_referenced: bool) -> Self {
        Self::new(self.optimizer, self.shared, everything_referenced)
    }

    /// Adds a reference to a whole column (`AddBinding(col)`).
    fn add_binding(&mut self, binding: ColumnBinding) {
        let column = self.column_references.entry(binding).or_default();
        column.bindings += 1;
        // A reference to the whole column: no field projection.
        column.child_columns.clear();
    }

    /// Adds a reference to a part of a column (`AddBinding(col, child_column)`).
    fn add_child_binding(&mut self, binding: ColumnBinding, child_column: ColumnIndex) {
        let mode = self.mode;
        let column = match self.column_references.entry(binding) {
            std::collections::hash_map::Entry::Vacant(vacant) => vacant.insert(ReferencedColumn {
                bindings: 1,
                child_columns: vec![child_column],
                ..ReferencedColumn::default()
            }),
            std::collections::hash_map::Entry::Occupied(occupied) => {
                let column = occupied.into_mut();
                column.bindings += 1;
                merge_child_columns(&mut column.child_columns, child_column);
                column
            }
        };
        if mode == PrunerMode::DisablePushdownExtract {
            column.supports_pushdown_extract = PushdownExtractSupport::Disabled;
        }
    }

    /// Adds a reference to a part of a column through a chain of extracts
    /// (`AddBinding(col, child_column, parent)`).
    fn add_extract_binding(
        &mut self,
        binding: ColumnBinding,
        child_column: ColumnIndex,
        component_count: usize,
        cast_type: Option<LogicalType>,
    ) {
        self.add_child_binding(binding, child_column.clone());
        let column = self.column_references.entry(binding).or_default();
        column.add_path(&child_column);
        column.struct_extracts.push(ReferencedStructExtract {
            extract_path: child_column,
            component_count,
            cast_type,
        });
    }

    /// Registers a chain of extracts on a column reference, optionally under a cast
    /// (`HandleExtractExpression`); `false` if `expression` is no such chain.
    fn handle_extract_expression(
        &mut self,
        expression: &Expression,
        cast_type: Option<&LogicalType>,
    ) -> bool {
        let Some((binding, mut path)) = extract_chain(expression) else {
            return false;
        };
        let component_count = path.len();
        if let (Some(cast_type), Some(leaf)) = (cast_type, path.last_mut()) {
            leaf.ty = cast_type.clone();
        }
        self.add_extract_binding(binding, nest_path(path), component_count, cast_type.cloned());
        true
    }

    /// Registers the column references of an expression (DuckDB's
    /// `BaseColumnPruner::VisitExpression` / `VisitReplace`).
    fn visit_expression(&mut self, expression: &Expression) {
        // An extract chain under a cast.
        if let Some(child) = cast_child(expression)
            && self.handle_extract_expression(child, Some(&expression.return_type))
        {
            return;
        }
        if self.handle_extract_expression(expression, None) {
            return;
        }
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
    /// referenced; with `replace`, records the new positions of the remaining ones, a
    /// column whose extracts are pushed down taking one position per extract path
    /// (DuckDB's `ClearUnusedExpressions`).
    fn clear_unused_expressions<T>(&self, list: &mut Vec<T>, table_index: u64, replace: bool) {
        let mut kept = Vec::with_capacity(list.len());
        let mut replacements = Vec::new();
        let mut new_position = 0u64;
        for (position, item) in std::mem::take(list).into_iter().enumerate() {
            let binding = ColumnBinding::new(table_index, position as u64);
            let Some(column) = self.column_references.get(&binding) else {
                continue;
            };
            kept.push(item);
            if !replace {
                continue;
            }
            if column.pushes_down_extract() {
                // The extracts are rewritten to the new columns instead (`ReplaceBinding`).
                new_position += column.unique_paths.len() as u64;
                continue;
            }
            if new_position != position as u64 {
                replacements.push((binding, ColumnBinding::new(table_index, new_position)));
            }
            new_position += 1;
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
    /// made below `op` (top-down), then the pushed-down extracts and the renumbering of
    /// the producers.
    fn apply_replacements(&self, op: &mut LogicalOperator, joins: &[JoinReplacement]) {
        let shared = self.shared.borrow();
        if joins.is_empty() && shared.replacements.is_empty() && shared.pushdown.is_empty() {
            return;
        }
        let rewriter = Rewriter {
            joins,
            replacements: &shared.replacements,
            pushdown: &shared.pushdown,
        };
        op.for_each_expression_mut(|expression| rewriter.rewrite(expression));
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
                        if condition.comparison != ExpressionType::CompareEqual {
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
                        let Some(moved) = self.column_references.remove(&right.binding) else {
                            continue;
                        };
                        // Each moved reference is a reference to the whole left column.
                        let column = self.column_references.entry(left.binding).or_default();
                        column.bindings += moved.bindings;
                        column.child_columns.clear();
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
                    self.check_projection_pushdown_extract(op, table_index);
                    let expression_count = op.expressions.len() as u64;
                    self.clear_unused_expressions(&mut op.expressions, table_index, true);
                    self.rewrite_projection_expressions(op, table_index, expression_count)?;
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

    /// The projection part of DuckDB's `CheckPushdownExtract`: a STRUCT column reference
    /// of which only fields are read can have the extracts pulled up into the projection.
    fn check_projection_pushdown_extract(&mut self, op: &LogicalOperator, table_index: u64) {
        for (position, expression) in op.expressions.iter().enumerate() {
            let Some(column) = self
                .column_references
                .get_mut(&ColumnBinding::new(table_index, position as u64))
            else {
                continue;
            };
            if column.child_columns.is_empty()
                || !matches!(expression.kind, ExpressionKind::ColumnRef(_))
                || expression.return_type.id() != LogicalTypeId::Struct
                || column.supports_pushdown_extract == PushdownExtractSupport::Disabled
            {
                continue;
            }
            column.supports_pushdown_extract = PushdownExtractSupport::Enabled;
        }
    }

    /// Replaces each projected STRUCT column whose extracts are pulled up by one
    /// expression per extract path (DuckDB's `RewriteExpressions`); `expression_count` is
    /// the number of expressions before the unused ones were removed.
    fn rewrite_projection_expressions(
        &mut self,
        op: &mut LogicalOperator,
        table_index: u64,
        expression_count: u64,
    ) -> Result<()> {
        let mut remaining = std::mem::take(&mut op.expressions).into_iter();
        let mut expressions: Vec<Expression> = Vec::new();
        // The expression of each full path (`new_bindings`).
        let mut new_bindings: Vec<(ColumnIndex, u64)> = Vec::new();
        for position in 0..expression_count {
            let binding = ColumnBinding::new(table_index, position);
            let Some(column) = self.column_references.get(&binding) else {
                continue;
            };
            let expression = remaining
                .next()
                .ok_or_else(|| Error::internal("RemoveUnusedColumns: projection expression missing"))?;
            if !column.pushes_down_extract() {
                expressions.push(expression);
                continue;
            }
            let column = column.clone();
            let ExpressionKind::ColumnRef(column_ref) = &expression.kind else {
                return Err(Error::internal(
                    "RemoveUnusedColumns: pulled-up extract on a non column reference",
                ));
            };
            let original_binding = column_ref.binding;
            let column_type = expression.return_type.clone();
            let start = expressions.len();
            let created = write_pushdown_extract_columns(&column, |extract_path, cast_type| {
                let target = Expression::column_ref(
                    expression.alias.clone(),
                    column_type.clone(),
                    original_binding,
                    0,
                );
                let mut new_extract = self.construct_struct_extract_from_path(target, extract_path)?;
                if let Some(cast_type) = cast_type {
                    new_extract = rustdb_planner::function_binder::add_cast_to_type(
                        self.optimizer.context(),
                        new_extract,
                        cast_type,
                        false,
                    )?;
                }
                let full_path = ColumnIndex::with_children(position, vec![extract_path.clone()]);
                if let Some((_, index)) = new_bindings.iter().find(|(path, _)| *path == full_path) {
                    return Ok(*index);
                }
                let index = expressions.len() as u64;
                new_bindings.push((full_path, index));
                expressions.push(new_extract);
                Ok(index)
            })?;
            self.shared.borrow_mut().pushdown.insert(binding, created);
            for new_expression in &expressions[start..] {
                self.visit_expression(new_expression);
            }
        }
        op.expressions = expressions;
        Ok(())
    }

    /// `struct_extract` calls reading `path` out of `target` (DuckDB's
    /// `ConstructStructExtractFromPath`).
    fn construct_struct_extract_from_path(
        &self,
        mut target: Expression,
        path: &ColumnIndex,
    ) -> Result<Expression> {
        let mut path = path;
        loop {
            let struct_type = target.return_type.clone();
            let child_types = struct_type
                .struct_children()
                .ok_or_else(|| Error::internal("ConstructStructExtractFromPath on a non-STRUCT"))?;
            let child_index = path.index as usize;
            let (name, _) = child_types
                .get(child_index)
                .ok_or_else(|| Error::internal("ConstructStructExtractFromPath: field out of range"))?;
            let key = if struct_type.is_unnamed_struct() {
                Value::bigint(child_index as i64 + 1)
            } else {
                Value::varchar(name)
            };
            target = self
                .optimizer
                .bind_scalar_function("struct_extract", vec![target, Expression::constant(key)])?;
            let Some(child) = path.child_indexes.first() else {
                return Ok(target);
            };
            path = child;
        }
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
    /// Columns read only through extracts are scanned with the fields they need, or, when
    /// the scan supports it, replaced by one column per extracted field.
    fn remove_columns_from_get(&mut self, get: &mut LogicalGet) -> Result<()> {
        if self.everything_referenced || !supports_projection_pushdown(get) {
            return Ok(());
        }
        let old_column_ids = std::mem::take(&mut get.column_ids);
        let column_count = old_column_ids.len() as u64;
        // The columns the operators above use...
        let mut projection_selection: Vec<u64> = (0..column_count).collect();
        self.clear_unused_expressions(&mut projection_selection, get.table_index, false);

        // FIXME (DuckDB's): a field read by a filter disables the pushdown of extracts,
        // as it would mean rewriting the table filters.
        self.mode = PrunerMode::DisablePushdownExtract;
        // ...and those the pushed-down filters need as well.
        for (&column, filter) in &get.table_filters.filters {
            let index = old_column_ids
                .iter()
                .position(|id| id.index == column as u64)
                .ok_or_else(|| Error::internal("Could not find column index for table filter"))?;
            let column_type = get.column_type(&ColumnIndex::new(column as u64));
            let column_ref = Expression::column_ref(
                String::new(),
                column_type,
                ColumnBinding::new(get.table_index, index as u64),
                0,
            );
            let filter_expression = match self.table_filter_expression(filter, &column_ref)? {
                Some(expression) if !expression.is_scalar() => expression,
                _ => column_ref,
            };
            self.visit_expression(&filter_expression);
        }
        // Whether the scan supports pushing down the extracts.
        self.check_get_pushdown_extract(get, &old_column_ids);

        let mut column_selection: Vec<u64> = (0..column_count).collect();
        self.clear_unused_expressions(&mut column_selection, get.table_index, true);

        // The old position each new column comes from.
        let mut original_ids: Vec<u64> = Vec::with_capacity(column_selection.len());
        let mut new_column_ids: Vec<ColumnIndex> = Vec::with_capacity(column_selection.len());
        for &selected in &column_selection {
            let binding = ColumnBinding::new(get.table_index, selected);
            let old_column_id = &old_column_ids[selected as usize];
            let column = self
                .column_references
                .get(&binding)
                .ok_or_else(|| Error::internal("RemoveUnusedColumns - could not find referenced column"))?;
            if !column.pushes_down_extract() {
                original_ids.push(selected);
                if old_column_id.is_pushdown_extract() {
                    // An earlier run (the optimizers rerun this one) pushed it down already.
                    new_column_ids.push(old_column_id.clone());
                } else {
                    new_column_ids.push(ColumnIndex::with_children(
                        old_column_id.index,
                        column.child_columns.clone(),
                    ));
                }
                continue;
            }
            let column_type = get.column_type(old_column_id);
            let struct_column_index = old_column_id.index;
            let created = write_pushdown_extract_columns(column, |extract_path, cast_type| {
                let mut new_index =
                    ColumnIndex::with_children(struct_column_index, vec![extract_path.clone()]);
                new_index.set_pushdown_extract_type(&column_type, cast_type);
                if let Some(position) = new_column_ids.iter().position(|existing| *existing == new_index) {
                    return Ok(position as u64);
                }
                new_column_ids.push(new_index);
                original_ids.push(selected);
                Ok(new_column_ids.len() as u64 - 1)
            })?;
            self.shared.borrow_mut().pushdown.insert(binding, created);
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

    /// The scan part of DuckDB's `CheckPushdownExtract`: whether the scan can emit the
    /// fields its columns are read through.
    fn check_get_pushdown_extract(&mut self, get: &LogicalGet, column_ids: &[ColumnIndex]) {
        for (position, column_id) in column_ids.iter().enumerate() {
            let Some(column) = self
                .column_references
                .get_mut(&ColumnBinding::new(get.table_index, position as u64))
            else {
                continue;
            };
            if column.struct_extracts.is_empty()
                || column.supports_pushdown_extract == PushdownExtractSupport::Disabled
            {
                continue;
            }
            column.supports_pushdown_extract = if supports_pushdown_extract(get, column_id.index) {
                PushdownExtractSupport::Enabled
            } else {
                PushdownExtractSupport::Disabled
            };
        }
    }

    /// The expression of a table filter over `column` (DuckDB's `TableFilter::ToExpression`),
    /// as far as the column references go: the operands other than the column are left
    /// out. `None` for a filter that is constant.
    fn table_filter_expression(
        &self,
        filter: &TableFilter,
        column: &Expression,
    ) -> Result<Option<Expression>> {
        Ok(match filter {
            TableFilter::ConstantComparison { .. }
            | TableFilter::IsNull
            | TableFilter::IsNotNull
            | TableFilter::In(_) => Some(column.clone()),
            TableFilter::Or(children) | TableFilter::And(children) => {
                let mut expressions = Vec::with_capacity(children.len());
                for child in children {
                    if let Some(expression) = self.table_filter_expression(child, column)? {
                        expressions.push(expression);
                    }
                }
                let expression_type = if matches!(filter, TableFilter::Or(_)) {
                    ExpressionType::ConjunctionOr
                } else {
                    ExpressionType::ConjunctionAnd
                };
                (!expressions.is_empty()).then(|| Expression::conjunction(expression_type, expressions))
            }
            TableFilter::StructExtract {
                child_index,
                child_filter,
                ..
            } => {
                let extract = self.optimizer.bind_scalar_function(
                    "struct_extract_at",
                    vec![
                        column.clone(),
                        Expression::constant(Value::bigint(*child_index as i64 + 1)),
                    ],
                )?;
                self.table_filter_expression(child_filter, &extract)?
            }
            TableFilter::Optional(child) => match child {
                Some(child) => self.table_filter_expression(child, column)?,
                None => None,
            },
            TableFilter::SelectivityOptional(optional) => {
                self.table_filter_expression(&optional.child, column)?
            }
            TableFilter::Dynamic(data) => data.get().map(|_| column.clone()),
            TableFilter::Expression(expression_filter) => {
                if expression_filter.is_bloom_filter() {
                    None
                } else if let Some(expression_filter) =
                    expression_filter.as_any().downcast_ref::<ExpressionTableFilter>()
                {
                    let mut expression = expression_filter.expression.clone();
                    replace_references(&mut expression, column);
                    Some(expression)
                } else {
                    Some(column.clone())
                }
            }
        })
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

/// Whether a scan can emit the fields of column `column` itself (DuckDB's
/// `TableFunction::supports_pushdown_extract`, which only the table scan has:
/// `TableSupportsPushdownExtract`, for STRUCT and VARIANT columns that are not
/// generated).
fn supports_pushdown_extract(get: &LogicalGet, column: u64) -> bool {
    let Some(entry) = &get.table else {
        return false;
    };
    let EntryKind::Table(table) = &entry.kind else {
        return false;
    };
    let Some(column) = usize::try_from(column)
        .ok()
        .and_then(|column| table.columns.get(column))
    else {
        return false;
    };
    column.generated.is_none()
        && matches!(
            column.logical_type.id(),
            LogicalTypeId::Struct | LogicalTypeId::Variant
        )
}

/// Calls `callback` with the path each extract on `column` reads from its producer (the
/// shortest registered prefix of its path) and the type of the cast replaced with it, if
/// any; returns the column index `callback` gave each path (DuckDB's
/// `WritePushdownExtractColumns`, which also rewrites the extracts: see [`Rewriter`]).
fn write_pushdown_extract_columns(
    column: &ReferencedColumn,
    mut callback: impl FnMut(&ColumnIndex, Option<&LogicalType>) -> Result<u64>,
) -> Result<PushdownColumns> {
    let mut created: PushdownColumns = Vec::new();
    for extract in &column.struct_extracts {
        let (entry, depth) = find_shortest_matching_path(&column.unique_paths, &extract.extract_path)
            .ok_or_else(|| {
                Error::internal("This path wasn't found in the registered paths for this expression at all!?")
            })?;
        // Only the top extract of a chain has the cast.
        let cast_type = if depth + 1 == extract.component_count {
            extract.cast_type.as_ref()
        } else {
            None
        };
        let path = &column.unique_paths[entry];
        let index = callback(path, cast_type)?;
        if !created.iter().any(|(existing, _)| existing == path) {
            created.push((path.clone(), index));
        }
    }
    Ok(created)
}

/// The position in `paths` of the shortest prefix of `full_path` (types included) and
/// its depth (DuckDB's `FindShortestMatchingPath`).
fn find_shortest_matching_path(paths: &[ColumnIndex], full_path: &ColumnIndex) -> Option<(usize, usize)> {
    let mut levels: Vec<ColumnIndex> = Vec::new();
    let mut current = full_path;
    let mut depth = 0;
    loop {
        let mut element = current.same_element();
        if current.has_type() {
            element.ty = current.ty.clone();
        }
        levels.push(element);
        let candidate = nest_path(levels.clone());
        if let Some(position) = paths.iter().position(|path| *path == candidate) {
            return Some((position, depth));
        }
        current = current.child_indexes.first()?;
        depth += 1;
    }
}

/// The single path `levels[0] -> levels[1] -> ...` as a nested [`ColumnIndex`].
fn nest_path(mut levels: Vec<ColumnIndex>) -> ColumnIndex {
    let mut node = levels.pop().unwrap_or_default();
    while let Some(mut parent) = levels.pop() {
        parent.child_indexes = vec![node];
        node = parent;
    }
    node
}

/// Adds `new_child` to the children read of a column (DuckDB's `MergeChildColumns`).
fn merge_child_columns(current: &mut Vec<ColumnIndex>, new_child: ColumnIndex) {
    if current.is_empty() {
        // The whole column is referenced already: no field projection.
        return;
    }
    for existing in current.iter_mut() {
        if !existing.same_element_as(&new_child) {
            continue;
        }
        // The field is read already.
        match new_child.child_indexes.into_iter().next() {
            // Now all of it is.
            None => existing.child_indexes.clear(),
            Some(grandchild) => merge_child_columns(&mut existing.child_indexes, grandchild),
        }
        return;
    }
    current.push(new_child);
}

/// The child of a cast (not a `TRY_CAST`) (DuckDB's `TryGetCastChild`).
fn cast_child(expression: &Expression) -> Option<&Expression> {
    match &expression.kind {
        ExpressionKind::Cast(cast)
            if expression.expression_type == ExpressionType::OperatorCast && !cast.try_cast =>
        {
            Some(&cast.child)
        }
        _ => None,
    }
}

/// The column reference a chain of `struct_extract` / `variant_extract` calls reads and
/// the path it reads, root first (DuckDB's `HandleExtractRecursive`); `None` if
/// `expression` is not such a chain over a column reference.
fn extract_chain(expression: &Expression) -> Option<(ColumnBinding, Vec<ColumnIndex>)> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    if !matches!(
        &*function.function.name,
        "struct_extract_at" | "struct_extract" | "array_extract" | "variant_extract"
    ) || function.bind_info.is_none()
    {
        return None;
    }
    let child = function.children.first()?;
    let element = match child.return_type.id() {
        // `HandleStructExtract`: the field index of the bind data.
        LogicalTypeId::Struct => ColumnIndex::new(struct_extract_index(
            &function.function.name,
            child,
            function.children.get(1)?,
        )?),
        // `HandleVariantExtract`: only extracts by key are pushed down.
        LogicalTypeId::Variant => {
            let key = function.children.get(1)?.as_constant()?;
            if key.logical_type().id() != LogicalTypeId::Varchar {
                return None;
            }
            ColumnIndex::field(key.as_str()?)
        }
        _ => return None,
    };
    if let ExpressionKind::ColumnRef(column) = &child.kind {
        return Some((column.binding, vec![element]));
    }
    let (binding, mut path) = extract_chain(child)?;
    path.push(element);
    Some((binding, path))
}

/// The field a struct extract reads (its `StructExtractBindData::index`): by name
/// (case-insensitive) or by 1-based position.
fn struct_extract_index(name: &str, input: &Expression, key: &Expression) -> Option<u64> {
    let key = key.as_constant()?;
    if name != "struct_extract_at" && key.logical_type().id() == LogicalTypeId::Varchar {
        let key = key.as_str()?.to_lowercase();
        let position = input
            .return_type
            .struct_children()?
            .iter()
            .position(|(field, _)| field.to_lowercase() == key)?;
        return Some(position as u64);
    }
    let position = key.as_i128()?;
    u64::try_from(position - 1).ok()
}

/// Replaces the `BOUND_REF` placeholders of an expression filter's predicate by `column`
/// (DuckDB's `ExpressionFilter::ReplaceExpressionRecursive`).
fn replace_references(expression: &mut Expression, column: &Expression) {
    if matches!(expression.kind, ExpressionKind::Reference(_)) {
        *expression = column.clone();
        return;
    }
    expression.for_each_child_mut(|child| replace_references(child, column));
}

/// Rewrites the column references of an expression for the columns that moved below its
/// operator: join replacements, pushed-down extracts and renumbered columns.
struct Rewriter<'a> {
    joins: &'a [JoinReplacement],
    replacements: &'a HashMap<ColumnBinding, ColumnBinding>,
    pushdown: &'a HashMap<ColumnBinding, PushdownColumns>,
}

impl Rewriter<'_> {
    /// `binding` after the join replacements (applied top-down).
    fn joined(&self, mut binding: ColumnBinding) -> ColumnBinding {
        for join in self.joins {
            if binding == join.from {
                binding = join.to;
            }
        }
        binding
    }

    fn rewrite(&self, expression: &mut Expression) {
        if !self.pushdown.is_empty() && self.rewrite_pushed_down_extract(expression) {
            return;
        }
        if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
            let binding = self.joined(column.binding);
            column.binding = self.replacements.get(&binding).copied().unwrap_or(binding);
            return;
        }
        expression.for_each_child_mut(|child| self.rewrite(child));
    }

    /// Replaces an extract chain (possibly under a cast) on a column whose extracts were
    /// pushed down by a reference to the column producing its path: the extract at the
    /// depth of the path, or the cast on top of a chain read entirely (DuckDB's
    /// `WritePushdownExtractColumns`). The chains are found the way they were registered.
    fn rewrite_pushed_down_extract(&self, expression: &mut Expression) -> bool {
        let cast_type = expression.return_type.clone();
        if let Some(child) = cast_child(expression)
            && let Some((binding, path)) = extract_chain(child)
        {
            let binding = self.joined(binding);
            if let Some(columns) = self.pushdown.get(&binding) {
                return Self::replace_extract(expression, binding, path, Some(&cast_type), columns);
            }
        }
        if let Some((binding, path)) = extract_chain(expression) {
            let binding = self.joined(binding);
            if let Some(columns) = self.pushdown.get(&binding) {
                return Self::replace_extract(expression, binding, path, None, columns);
            }
        }
        false
    }

    fn replace_extract(
        expression: &mut Expression,
        binding: ColumnBinding,
        mut path: Vec<ColumnIndex>,
        cast_type: Option<&LogicalType>,
        columns: &PushdownColumns,
    ) -> bool {
        let component_count = path.len();
        if let (Some(cast_type), Some(leaf)) = (cast_type, path.last_mut()) {
            leaf.ty = cast_type.clone();
        }
        let full_path = nest_path(path);
        let paths: Vec<ColumnIndex> = columns.iter().map(|(path, _)| path.clone()).collect();
        let Some((entry, depth)) = find_shortest_matching_path(&paths, &full_path) else {
            return false;
        };
        let column_index = columns[entry].1;
        // The node to replace: the cast over a chain read entirely, else the extract at
        // `depth` (the chain's top extract is at depth `component_count - 1`).
        let mut target: &mut Expression = expression;
        if cast_type.is_some() && depth + 1 != component_count {
            let ExpressionKind::Cast(cast) = &mut target.kind else {
                return false;
            };
            target = &mut cast.child;
        }
        let steps = if cast_type.is_some() && depth + 1 == component_count {
            0
        } else {
            component_count - 1 - depth
        };
        for _ in 0..steps {
            let ExpressionKind::Function(function) = &mut target.kind else {
                return false;
            };
            let Some(child) = function.children.first_mut() else {
                return false;
            };
            target = child;
        }
        // A copy of the column reference at the root of the chain, reading the new column.
        let Some(mut column_ref) = base_column_ref(target).cloned() else {
            return false;
        };
        column_ref.return_type = target.return_type.clone();
        if let ExpressionKind::ColumnRef(column) = &mut column_ref.kind {
            column.binding = ColumnBinding::new(binding.table_index, column_index);
        }
        *target = column_ref;
        true
    }
}

/// The column reference at the root of an extract chain (or its cast).
fn base_column_ref(expression: &Expression) -> Option<&Expression> {
    match &expression.kind {
        ExpressionKind::ColumnRef(_) => Some(expression),
        ExpressionKind::Cast(cast) => base_column_ref(&cast.child),
        ExpressionKind::Function(function) => base_column_ref(function.children.first()?),
        _ => None,
    }
}
