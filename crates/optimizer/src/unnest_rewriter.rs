//! Removing the delim join of a lateral UNNEST (DuckDB's `UnnestRewriter`): a correlated
//! `UNNEST` of the outer rows' lists is decorrelated into a delim join numbering the outer
//! rows; the UNNEST can instead run directly on the outer rows, carrying their columns.

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_planner::expression::BoundUnnestExpression;
use rustdb_planner::expression_binder::is_unnest_function;
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_types::LogicalType;

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};

/// A column of the delim join's input (DuckDB's `LHSBinding`).
struct LhsBinding {
    binding: ColumnBinding,
    ty: LogicalType,
    alias: String,
}

/// Rewrites the bindings of column references, each by the first matching replacement
/// (DuckDB's `UnnestRewriterPlanUpdater`).
#[derive(Default)]
struct PlanUpdater {
    replace_bindings: Vec<(ColumnBinding, ColumnBinding)>,
}

impl PlanUpdater {
    fn visit_operator(&self, op: &mut LogicalOperator) {
        for child in &mut op.children {
            self.visit_operator(child);
        }
        op.for_each_expression_mut(|expression| self.visit_expression(expression));
    }

    fn visit_expression(&self, expression: &mut Expression) {
        if let ExpressionKind::ColumnRef(column) = &mut expression.kind
            && let Some((_, new)) = self
                .replace_bindings
                .iter()
                .find(|(old, _)| *old == column.binding)
        {
            column.binding = *new;
        }
        expression.for_each_child_mut(|child| self.visit_expression(child));
    }
}

/// The state of one rewrite (DuckDB's `UnnestRewriter` members).
#[derive(Default)]
struct UnnestRewriter {
    delim_columns: Vec<ColumnBinding>,
    lhs_bindings: Vec<LhsBinding>,
    overwritten_table_index: u64,
    distinct_unnest_count: usize,
}

/// Rewrites the lateral UNNESTs of `plan` (DuckDB's `UnnestRewriter::Optimize`).
pub fn rewrite_unnests(plan: &mut LogicalOperator) -> Result<()> {
    let mut candidates = Vec::new();
    find_candidates(plan, &mut Vec::new(), &mut candidates)?;
    for candidate in candidates {
        let Some(path) = find_path(plan, candidate) else {
            continue;
        };
        let mut rewriter = UnnestRewriter::default();
        let topmost = node_at_mut(plan, &path)?;
        if rewriter.rewrite_candidate(topmost)? {
            rewriter.update_bound_unnest_bindings(topmost)?;
            rewriter.update_rhs_bindings(plan, &path)?;
        }
    }
    Ok(())
}

/// The INNER delim join of a single condition below `op` (DuckDB's candidate checks).
fn candidate_delim_join(op: &LogicalOperator) -> Option<&LogicalOperator> {
    let [child] = op.children.as_slice() else {
        return None;
    };
    let LogicalKind::ComparisonJoin(join) = &child.kind else {
        return None;
    };
    if child.operator_type() != LogicalOperatorType::LogicalDelimJoin
        || join.join_type != JoinType::Inner
        || join.conditions.len() != 1
    {
        return None;
    }
    Some(child)
}

/// The positions of the delim side and the other side of a delim join.
fn delim_sides(delim_join: &LogicalOperator) -> (usize, usize) {
    let flipped = matches!(&delim_join.kind, LogicalKind::ComparisonJoin(join) if join.delim_flipped);
    let delim_index = usize::from(flipped);
    (delim_index, 1 - delim_index)
}

/// The operators whose child is a delim join of a numbered input (a window) with an
/// UNNEST of the delim scan under projections, bottom-up (DuckDB's `FindCandidates`).
/// The `unnest` table function over a projection of the delim scan is turned into that
/// shape first.
fn find_candidates(
    root: &mut LogicalOperator,
    path: &mut Vec<usize>,
    candidates: &mut Vec<u64>,
) -> Result<()> {
    let child_count = node_at_mut(root, path)?.children.len();
    for index in 0..child_count {
        path.push(index);
        find_candidates(root, path, candidates)?;
        path.pop();
    }
    let op = node_at_mut(root, path)?;
    let node_id = op.node_id();
    let Some(delim_join) = candidate_delim_join(op) else {
        return Ok(());
    };
    let (delim_index, other_index) = delim_sides(delim_join);
    if delim_join
        .children
        .get(delim_index)
        .map(LogicalOperator::operator_type)
        != Some(LogicalOperatorType::LogicalWindow)
    {
        return Ok(());
    }
    let Some(other) = delim_join.children.get(other_index) else {
        return Ok(());
    };
    let mut current = other;
    while current.operator_type() == LogicalOperatorType::LogicalProjection {
        let [child] = current.children.as_slice() else {
            break;
        };
        current = child;
    }
    // Pattern 1: delim scan -> UNNEST -> projections.
    if current.operator_type() == LogicalOperatorType::LogicalUnnest
        && current.children.first().map(LogicalOperator::operator_type)
            == Some(LogicalOperatorType::LogicalDelimGet)
    {
        candidates.push(node_id);
        return Ok(());
    }
    // Pattern 2: delim scan -> projection -> `unnest` table function.
    let LogicalKind::Get(get) = &other.kind else {
        return Ok(());
    };
    // With ORDINALITY the delim index would be unnested too.
    if !is_unnest_function(&get.function.name) || get.ordinality_index.is_some() {
        return Ok(());
    }
    let Some(projection) = other.children.first() else {
        return Ok(());
    };
    if projection.operator_type() != LogicalOperatorType::LogicalProjection
        || projection.children.first().map(LogicalOperator::operator_type)
            != Some(LogicalOperatorType::LogicalDelimGet)
    {
        return Ok(());
    }
    let Some(replacer) = unnest_function_to_operator(op, other_index)? else {
        return Ok(());
    };
    let stop_operator = node_at_mut(root, path)?.children[0].children[other_index].node_id();
    replacer.visit_operator_until(root, Some(stop_operator));
    candidates.push(node_id);
    Ok(())
}

/// Turns the `unnest` table function on the `other_index` side of `op`'s delim join into
/// an UNNEST operator below its argument projection (DuckDB's pattern 2 conversion in
/// `FindCandidates`). Returns the rewrite of the references to the function's columns,
/// or `None` when an argument is not a plain column (deeply nested struct paths project
/// struct extractions instead).
fn unnest_function_to_operator(
    op: &mut LogicalOperator,
    other_index: usize,
) -> Result<Option<ColumnBindingReplacer>> {
    let missing = || Error::internal("UnnestRewriter: unexpected plan shape");
    let delim_join = op.children.first_mut().ok_or_else(missing)?;
    let slot = delim_join.children.get_mut(other_index).ok_or_else(missing)?;
    slot.resolve_operator_types()?;
    let get_columns = slot.column_bindings();
    let LogicalKind::Get(get) = &slot.kind else {
        return Err(missing());
    };
    let get_table_index = get.table_index;
    let projection = slot.children.first().ok_or_else(missing)?;
    let is_column_ref = |column: u64| {
        projection
            .expressions
            .get(column as usize)
            .is_some_and(|expression| matches!(expression.kind, ExpressionKind::ColumnRef(_)))
    };
    if get_columns
        .iter()
        .any(|binding| binding.table_index == get_table_index && !is_column_ref(binding.column_index))
    {
        return Ok(None);
    }

    let mut unnest_get = std::mem::replace(slot, LogicalOperator::new(LogicalKind::CrossProduct));
    let mut projection = unnest_get.children.swap_remove(0);
    let LogicalKind::Projection {
        table_index: projection_index,
    } = projection.kind
    else {
        return Err(missing());
    };
    let delim_get = std::mem::replace(
        projection.children.first_mut().ok_or_else(missing)?,
        LogicalOperator::new(LogicalKind::CrossProduct),
    );
    let mut unnest = LogicalOperator::new(LogicalKind::Unnest {
        unnest_index: get_table_index,
    });
    unnest.children.push(delim_get);
    let mut replacer = ColumnBindingReplacer::default();
    for (binding, ty) in get_columns.iter().zip(&unnest_get.types) {
        if binding.table_index != get_table_index {
            continue;
        }
        let column = binding.column_index as usize;
        let argument = &projection.expressions[column];
        let ExpressionKind::ColumnRef(argument_ref) = &argument.kind else {
            return Err(missing());
        };
        let alias = argument.alias.clone();
        let depth = argument_ref.depth;
        unnest.expressions.push(Expression::new(
            ExpressionType::BoundUnnest,
            ty.clone(),
            ExpressionKind::Unnest(Box::new(BoundUnnestExpression {
                child: argument.clone(),
            })),
        ));
        let unnest_column = ColumnBinding::new(get_table_index, unnest.expressions.len() as u64 - 1);
        projection.expressions[column] = Expression::column_ref(alias, ty.clone(), unnest_column, depth);
        if let Some(projection_type) = projection.types.get_mut(column) {
            *projection_type = ty.clone();
        }
        replacer.replacement_bindings.push(ReplacementBinding::with_type(
            *binding,
            ColumnBinding::new(projection_index, binding.column_index),
            ty.clone(),
        ));
    }
    projection.children[0] = unnest;
    *slot = projection;
    Ok(Some(replacer))
}

impl UnnestRewriter {
    /// DuckDB's `RewriteCandidate`: the UNNEST reads the numbered input directly and the
    /// delim join is replaced by its other side.
    fn rewrite_candidate(&mut self, topmost: &mut LogicalOperator) -> Result<bool> {
        if !matches!(
            topmost.operator_type(),
            LogicalOperatorType::LogicalProjection
                | LogicalOperatorType::LogicalWindow
                | LogicalOperatorType::LogicalFilter
                | LogicalOperatorType::LogicalAggregateAndGroupBy
                | LogicalOperatorType::LogicalUnnest
        ) {
            return Ok(false);
        }
        let missing = || Error::internal("UnnestRewriter: unexpected plan shape");
        let delim_join = topmost.children.first_mut().ok_or_else(missing)?;
        let (delim_index, other_index) = delim_sides(delim_join);
        let LogicalKind::ComparisonJoin(join) = &delim_join.kind else {
            return Ok(false);
        };
        // DuckDB's `GetDelimColumns`.
        for column in &join.duplicate_eliminated_columns {
            let ExpressionKind::ColumnRef(column) = &column.kind else {
                return Err(Error::internal(
                    "UnnestRewriter: delim column is not a column reference",
                ));
            };
            self.delim_columns.push(column.binding);
        }

        // The window's input becomes the UNNEST's input.
        let window = delim_join.children.get_mut(delim_index).ok_or_else(missing)?;
        let lhs = window.children.first_mut().ok_or_else(missing)?;
        self.get_lhs_expressions(lhs)?;

        let mut unnest = delim_join.children.get_mut(other_index).ok_or_else(missing)?;
        let mut projections = 0;
        while unnest.operator_type() == LogicalOperatorType::LogicalProjection {
            unnest = unnest.children.first_mut().ok_or_else(missing)?;
            projections += 1;
        }
        if projections == 0 {
            return Ok(false);
        }
        let delim_get = unnest.children.first().ok_or_else(missing)?;
        let LogicalKind::DelimGet {
            table_index,
            chunk_types,
        } = &delim_get.kind
        else {
            return Err(missing());
        };
        self.overwritten_table_index = *table_index;
        self.distinct_unnest_count = chunk_types.len();

        let window = delim_join.children.get_mut(delim_index).ok_or_else(missing)?;
        let lhs = std::mem::replace(
            window.children.first_mut().ok_or_else(missing)?,
            LogicalOperator::new(LogicalKind::CrossProduct),
        );
        let mut unnest = delim_join.children.get_mut(other_index).ok_or_else(missing)?;
        while unnest.operator_type() == LogicalOperatorType::LogicalProjection {
            unnest = unnest.children.first_mut().ok_or_else(missing)?;
        }
        unnest.children[0] = lhs;

        // The delim join gives way to its other side.
        let other = std::mem::replace(
            delim_join.children.get_mut(other_index).ok_or_else(missing)?,
            LogicalOperator::new(LogicalKind::CrossProduct),
        );
        topmost.children[0] = other;
        Ok(true)
    }

    /// DuckDB's `GetLHSExpressions`: the numbered input's columns, named after a
    /// projection's expressions.
    fn get_lhs_expressions(&mut self, op: &mut LogicalOperator) -> Result<()> {
        op.resolve_operator_types()?;
        let bindings = op.column_bindings();
        let named = op.operator_type() == LogicalOperatorType::LogicalProjection
            && op.expressions.len() == op.types.len();
        for (index, (binding, ty)) in bindings.iter().zip(&op.types).enumerate() {
            let alias = if named {
                op.expressions
                    .get(index)
                    .map(|expression| expression.alias.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            self.lhs_bindings.push(LhsBinding {
                binding: *binding,
                ty: ty.clone(),
                alias,
            });
        }
        Ok(())
    }

    /// DuckDB's `UpdateBoundUnnestBindings`: the UNNEST's references to the delim scan
    /// now read the delim columns of its new input.
    fn update_bound_unnest_bindings(&self, topmost: &mut LogicalOperator) -> Result<()> {
        let missing = || Error::internal("UnnestRewriter: unexpected plan shape");
        let mut current = topmost.children.first_mut().ok_or_else(missing)?;
        while current.operator_type() == LogicalOperatorType::LogicalProjection {
            current = current.children.first_mut().ok_or_else(missing)?;
        }
        let mut unnest_columns = current.children.first().ok_or_else(missing)?.column_bindings();
        let mut updater = PlanUpdater::default();
        for (index, delim_binding) in self.delim_columns.iter().enumerate() {
            if let Some(position) = unnest_columns
                .iter()
                .position(|binding| binding.table_index == delim_binding.table_index)
            {
                updater.replace_bindings.push((
                    ColumnBinding::new(self.overwritten_table_index, index as u64),
                    *delim_binding,
                ));
                unnest_columns.remove(position);
            }
        }
        for expression in &mut current.expressions {
            updater.visit_expression(expression);
        }
        Ok(())
    }

    /// DuckDB's `UpdateRHSBindings`: the projections above the UNNEST drop the delim
    /// columns and carry the input's columns in front instead.
    fn update_rhs_bindings(&mut self, plan: &mut LogicalOperator, path: &[usize]) -> Result<()> {
        let mut updater = PlanUpdater::default();
        let missing = || Error::internal("UnnestRewriter: unexpected plan shape");
        let shift = self.lhs_bindings.len() as u64;
        let mut projection_depth = 0;
        let mut top_projection_index = None;
        {
            let topmost = node_at_mut(plan, path)?;
            let mut current = topmost.children.first_mut().ok_or_else(missing)?;
            while let LogicalKind::Projection { table_index } = current.kind {
                top_projection_index.get_or_insert(table_index);
                // Drop the delim columns, shift the rest behind the input's columns.
                if current.expressions.len() <= self.distinct_unnest_count {
                    return Err(missing());
                }
                let keep = current.expressions.len() - self.distinct_unnest_count;
                current.expressions.truncate(keep);
                for column in 0..keep as u64 {
                    updater.replace_bindings.push((
                        ColumnBinding::new(table_index, column),
                        ColumnBinding::new(table_index, column + shift),
                    ));
                }
                current = current.children.first_mut().ok_or_else(missing)?;
                projection_depth += 1;
            }
        }
        updater.visit_operator(plan);
        updater.replace_bindings.clear();

        // References to the input's columns now read the top projection's.
        let top_projection_index = top_projection_index.ok_or_else(missing)?;
        for (index, lhs) in self.lhs_bindings.iter().enumerate() {
            updater.replace_bindings.push((
                lhs.binding,
                ColumnBinding::new(top_projection_index, index as u64),
            ));
        }
        // ... except in the UNNEST and below it.
        let mut unnest_path = path.to_vec();
        unnest_path.extend(std::iter::repeat_n(0, projection_depth + 1));
        let unnest = node_at_mut(plan, &unnest_path)?;
        let unnest_expressions = std::mem::take(&mut unnest.expressions);
        let unnest_children = std::mem::take(&mut unnest.children);
        updater.visit_operator(plan);
        let unnest = node_at_mut(plan, &unnest_path)?;
        unnest.expressions = unnest_expressions;
        unnest.children = unnest_children;

        // Each projection passes the input's columns through, bottom-up.
        for depth in (1..=projection_depth).rev() {
            let mut projection_path = path.to_vec();
            projection_path.extend(std::iter::repeat_n(0, depth));
            let projection = node_at_mut(plan, &projection_path)?;
            let LogicalKind::Projection { table_index } = projection.kind else {
                return Err(missing());
            };
            let existing = std::mem::take(&mut projection.expressions);
            for (index, lhs) in self.lhs_bindings.iter_mut().enumerate() {
                projection.expressions.push(Expression::column_ref(
                    lhs.alias.clone(),
                    lhs.ty.clone(),
                    lhs.binding,
                    0,
                ));
                lhs.binding = ColumnBinding::new(table_index, index as u64);
            }
            projection.expressions.extend(existing);
        }
        Ok(())
    }
}

/// The child positions leading from `op` to the operator with node id `id`.
fn find_path(op: &LogicalOperator, id: u64) -> Option<Vec<usize>> {
    if op.node_id() == id {
        return Some(Vec::new());
    }
    op.children.iter().enumerate().find_map(|(index, child)| {
        find_path(child, id).map(|mut path| {
            path.insert(0, index);
            path
        })
    })
}

fn node_at_mut<'a>(op: &'a mut LogicalOperator, path: &[usize]) -> Result<&'a mut LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node
            .children
            .get_mut(index)
            .ok_or_else(|| Error::internal("UnnestRewriter: broken path"))?;
    }
    Ok(node)
}
