//! Joins in FROM (DuckDB's `bind_joinref.cpp`): both sides are bound in child binders,
//! the right one with a [`LateralBinder`] over the left side active, so that LATERAL
//! references are found; USING and NATURAL joins become equality conditions.

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{
    ComparisonExpression, ConjunctionExpression, ExpressionType, JoinRef, JoinRefType, JoinType,
    ParsedExpression, ParsedExpressionKind,
};
use rustdb_common::{CaseInsensitiveSet, Error, Result};

use super::bind_table_ref;
use super::plan_joinref::{BoundJoinRef, create_join_plan};
use crate::binder::BindingAlias;
use crate::binder::{BindScope, Binder, BoundStatement, Scope};
use crate::binder::{UsingColumnSet, UsingSetRef};
use crate::expression_binder::{BaseBinder, ExpressionBinder, LateralBinder, WhereBinder, set_bound};

/// A column reference qualified by `alias` (DuckDB's `ColumnRefExpression(name, alias)`).
fn column_ref(column_name: &str, alias: &BindingAlias) -> ParsedExpression {
    let mut names = Vec::new();
    if alias.is_set() {
        if !alias.catalog.is_empty() {
            names.push(alias.catalog.clone());
        }
        if !alias.schema.is_empty() {
            names.push(alias.schema.clone());
        }
        names.push(alias.alias.clone());
    }
    names.push(column_name.to_owned());
    ParsedExpression::column_ref(names)
}

/// `alias.column` bound in `binder`, wrapped as a parsed node (DuckDB's `BindColumn`).
fn bind_column(binder: &mut Binder, alias: &BindingAlias, column_name: &str) -> Result<ParsedExpression> {
    let mut expr = column_ref(column_name, alias);
    let bound = BaseBinder::new(binder, None).bind(&mut expr, true)?;
    set_bound(&mut expr, bound);
    Ok(expr)
}

/// The condition `left.column <op> right.column` of a USING column (DuckDB's `AddCondition`).
fn add_condition(
    left_binder: &mut Binder,
    right_binder: &mut Binder,
    left_alias: &BindingAlias,
    right_alias: &BindingAlias,
    column_name: &str,
    expression_type: ExpressionType,
) -> Result<ParsedExpression> {
    let left = bind_column(left_binder, left_alias, column_name)?;
    let right = bind_column(right_binder, right_alias, column_name)?;
    Ok(ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Comparison(ComparisonExpression {
            left: Box::new(left),
            right: Box::new(right),
        }),
    ))
}

/// The single relation of `binder` with a column `using_column` (DuckDB's
/// `TryFindBinding`): `Ok(None)` if there is none, an error if there are several.
fn try_find_binding(binder: &Binder, using_column: &str, join_side: &str) -> Result<Option<BindingAlias>> {
    let bindings = binder.bind_context.matching_bindings(using_column);
    let mut result: Option<BindingAlias> = None;
    for binding in &bindings {
        if result.is_some() {
            let mut error = format!(
                "Column name \"{using_column}\" is ambiguous: it exists more than once on {join_side} side of \
                 join.\nCandidates:"
            );
            for other in &bindings {
                error.push_str("\n\t");
                error.push_str(other.alias_name());
                error.push('.');
                error.push_str(
                    &binder
                        .bind_context
                        .actual_column_name(&other.alias, using_column)?,
                );
            }
            return Err(Error::binder(error));
        }
        result = Some(binding.alias.clone());
    }
    Ok(result)
}

/// DuckDB's `FindBinding`.
fn find_binding(binder: &Binder, using_column: &str, join_side: &str) -> Result<BindingAlias> {
    try_find_binding(binder, using_column, join_side)?.ok_or_else(|| {
        Error::binder(format!(
            "Column \"{using_column}\" does not exist on {join_side} side of join!"
        ))
    })
}

fn lock(set: &UsingSetRef) -> std::sync::MutexGuard<'_, UsingColumnSet> {
    set.lock().unwrap_or_else(PoisonError::into_inner)
}

/// DuckDB's `AddUsingBindings`.
fn add_using_bindings(
    set: &mut UsingColumnSet,
    input_set: Option<&UsingSetRef>,
    input_binding: &BindingAlias,
) {
    match input_set {
        Some(input_set) => set.bindings.extend(lock(input_set).bindings.iter().cloned()),
        None => set.bindings.push(input_binding.clone()),
    }
}

/// DuckDB's `SetPrimaryBinding`: outer joins have no primary binding.
fn set_primary_binding(
    set: &mut UsingColumnSet,
    join_type: JoinType,
    left: &BindingAlias,
    right: &BindingAlias,
) {
    match join_type {
        JoinType::Left | JoinType::Inner | JoinType::Semi | JoinType::Anti => {
            set.primary_binding = left.clone();
        }
        JoinType::Right | JoinType::RightSemi | JoinType::RightAnti => set.primary_binding = right.clone(),
        _ => {}
    }
}

/// DuckDB's `RetrieveUsingBinding`.
fn retrieve_using_binding(
    binder: &Binder,
    current_set: Option<&UsingSetRef>,
    using_column: &str,
    join_side: &str,
) -> Result<BindingAlias> {
    match current_set {
        None => find_binding(binder, using_column, join_side),
        Some(set) => Ok(lock(set).primary_binding.clone()),
    }
}

/// DuckDB's `RemoveDuplicateUsingColumns`.
fn remove_duplicate_using_columns(using_columns: Vec<String>) -> Vec<String> {
    let mut handled = CaseInsensitiveSet::default();
    using_columns
        .into_iter()
        .filter(|column| handled.insert(column.clone()))
        .collect()
}

/// The `NATURAL JOIN` columns: names of the left side also present on the right.
fn natural_join_columns(left: &Binder, right: &Binder, location: Option<u64>) -> Result<Vec<String>> {
    let mut lhs_columns = Vec::new();
    let mut seen = CaseInsensitiveSet::default();
    for binding in left.bind_context.bindings() {
        for name in &binding.names {
            if seen.insert(name.clone()) {
                lhs_columns.push(name.clone());
            }
        }
    }
    let mut result = Vec::new();
    for column_name in lhs_columns {
        if right.bind_context.using_binding(&column_name)?.is_none()
            && try_find_binding(right, &column_name, "right")?.is_none()
        {
            continue;
        }
        result.push(column_name);
    }
    if result.is_empty() {
        let candidates = |binder: &Binder| {
            let mut list = Vec::new();
            for binding in binder.bind_context.bindings() {
                for name in &binding.names {
                    list.push(format!("{}.{name}", binding.alias_name()));
                }
            }
            list.join(", ")
        };
        return Err(Error::binder(format!(
            "No columns found to join on in NATURAL JOIN.\nUse CROSS JOIN if you intended for this to be a \
             cross-product.\n   Left candidates: {}\n   Right candidates: {}",
            candidates(left),
            candidates(right)
        ))
        .with_position(location));
    }
    Ok(result)
}

/// Binds one side of a join, sharing the unnamed-subquery counter with the parent
/// (DuckDB's `Binder::BindJoin`).
fn bind_join_side(
    binder: &mut Binder,
    parent_index: &mut u64,
    outer: &mut dyn Scope,
    table_ref: &mut rustdb_ast::TableRef,
) -> Result<BoundStatement> {
    binder.unnamed_subquery_index = *parent_index;
    let result = {
        let mut scope = BindScope {
            binder: &mut *binder,
            outer: Some(outer),
        };
        bind_table_ref(&mut scope, table_ref)
    };
    *parent_index = binder.unnamed_subquery_index;
    result
}

/// `left JOIN right ...` (DuckDB's `Binder::Bind(JoinRef &)`).
pub fn bind_join(
    scope: &mut BindScope<'_>,
    join: &mut JoinRef,
    location: Option<u64>,
) -> Result<BoundStatement> {
    let mut left_binder = scope.binder.child()?;
    let mut right_binder = scope.binder.child()?;
    let mut unnamed_index = scope.binder.unnamed_subquery_index;
    let left = bind_join_side(&mut left_binder, &mut unnamed_index, &mut *scope, &mut join.left)?;
    if !join.duplicate_eliminated_columns.is_empty() {
        return Err(Error::not_implemented("joins with duplicate-eliminated columns"));
    }
    let right = {
        let mut lateral = LateralBinder::new(&mut left_binder, Some(&mut *scope as &mut dyn Scope));
        bind_join_side(
            &mut right_binder,
            &mut unnamed_index,
            &mut lateral,
            &mut join.right,
        )?
    };
    scope.binder.unnamed_subquery_index = unnamed_index;
    let spec = JoinSpec {
        join_type: join.join_type,
        ref_type: join.ref_type,
        using_columns: &join.using_columns,
        condition: &mut join.condition,
        location,
    };
    bind_bound_join(scope, spec, (left_binder, left), (right_binder, right))
}

/// What a join is bound with besides its sides.
#[derive(Debug)]
pub struct JoinSpec<'a> {
    pub join_type: JoinType,
    pub ref_type: JoinRefType,
    pub using_columns: &'a [String],
    /// The `ON` condition; USING columns are added to it.
    pub condition: &'a mut Option<Box<ParsedExpression>>,
    pub location: Option<u64>,
}

/// Finishes binding a join whose sides are bound in `left` and `right` (the rest of
/// DuckDB's `Binder::Bind(JoinRef &)`, also used with the pre-bound sides of
/// `BoundRefWrapper`).
pub fn bind_bound_join(
    scope: &mut BindScope<'_>,
    spec: JoinSpec<'_>,
    (mut left_binder, left): (Binder, BoundStatement),
    (mut right_binder, right): (Binder, BoundStatement),
) -> Result<BoundStatement> {
    let JoinSpec {
        join_type,
        ref_type,
        using_columns,
        condition: join_condition,
        location,
    } = spec;
    // Correlations of the right side with the left (depth 1) make this a LATERAL join;
    // those of the left side are handled when flattening.
    let correlated_columns = right_binder.correlated_columns.clone();
    let lateral = correlated_columns.iter().any(|column| column.depth == 1);
    if lateral && !matches!(join_type, JoinType::Inner | JoinType::Left) {
        return Err(Error::binder(
            "The combining JOIN type must be INNER or LEFT for a LATERAL reference",
        ));
    }

    let mut extra_using_columns = match ref_type {
        JoinRefType::Natural => natural_join_columns(&left_binder, &right_binder, location)?,
        JoinRefType::Regular | JoinRefType::Asof => using_columns.to_vec(),
        _ => Vec::new(),
    };
    extra_using_columns = remove_duplicate_using_columns(extra_using_columns);

    let mut extra_conditions = Vec::new();
    if !extra_using_columns.is_empty() {
        let mut left_sets = Vec::with_capacity(extra_using_columns.len());
        let mut right_sets = Vec::with_capacity(extra_using_columns.len());
        for using_column in &extra_using_columns {
            // Chained USING clauses (`x JOIN y USING (c) JOIN z USING (c)`) reuse the set.
            let left_set = left_binder.bind_context.using_binding(using_column)?;
            let right_set = right_binder.bind_context.using_binding(using_column)?;
            if left_set.is_none() {
                left_binder.bind_context.matching_binding(using_column, None)?;
            }
            if right_set.is_none() {
                right_binder.bind_context.matching_binding(using_column, None)?;
            }
            left_sets.push(left_set);
            right_sets.push(right_set);
        }
        let column_count = extra_using_columns.len();
        for (i, using_column) in extra_using_columns.iter().enumerate() {
            let left_set = left_sets[i].as_ref();
            let right_set = right_sets[i].as_ref();
            let left_alias = retrieve_using_binding(&left_binder, left_set, using_column, "left")?;
            let right_alias = retrieve_using_binding(&right_binder, right_set, using_column, "right")?;
            // The last column of ASOF JOIN ... USING is the inequality.
            let comparison = if ref_type == JoinRefType::Asof && i == column_count - 1 {
                ExpressionType::CompareGreaterthanorequalto
            } else {
                ExpressionType::CompareEqual
            };
            extra_conditions.push(add_condition(
                &mut left_binder,
                &mut right_binder,
                &left_alias,
                &right_alias,
                using_column,
                comparison,
            )?);
            let mut set = UsingColumnSet::default();
            add_using_bindings(&mut set, left_set, &left_alias);
            add_using_bindings(&mut set, right_set, &right_alias);
            set_primary_binding(&mut set, join_type, &left_alias, &right_alias);
            let set: UsingSetRef = Arc::new(Mutex::new(set));
            // DuckDB's TransferUsingBinding, for both sides.
            scope
                .binder
                .bind_context
                .add_using_binding(using_column, Arc::clone(&set));
            if let Some(left_set) = left_set {
                left_binder
                    .bind_context
                    .remove_using_binding(using_column, left_set);
            }
            scope
                .binder
                .bind_context
                .add_using_binding(using_column, Arc::clone(&set));
            if let Some(right_set) = right_set {
                right_binder
                    .bind_context
                    .remove_using_binding(using_column, right_set);
            }
        }
    }

    let right_aliases = right_binder.bind_context.binding_aliases();
    let left_aliases = left_binder.bind_context.binding_aliases();
    scope
        .binder
        .bind_context
        .add_context(std::mem::take(&mut left_binder.bind_context));
    scope
        .binder
        .bind_context
        .add_context(std::mem::take(&mut right_binder.bind_context));

    // Correlations with enclosing queries: depth >= 1 on the left; depth > 1 on the
    // right, which is one level deeper because of the lateral binder.
    for column in &left_binder.correlated_columns {
        if column.depth >= 1 {
            scope.binder.add_correlated_column(column.clone());
        }
    }
    for column in &right_binder.correlated_columns {
        if column.depth > 1 {
            let mut column = column.clone();
            column.depth -= 1;
            scope.binder.add_correlated_column(column);
        }
    }

    for condition in extra_conditions {
        *join_condition = Some(Box::new(match join_condition.take() {
            Some(existing) => ParsedExpression::new(
                ExpressionType::ConjunctionAnd,
                ParsedExpressionKind::Conjunction(ConjunctionExpression {
                    children: vec![*existing, condition],
                }),
            ),
            None => condition,
        }));
    }
    let condition = match join_condition {
        Some(condition) => {
            let (inner, outer) = scope.split();
            Some(WhereBinder::new(inner, outer, None).bind(condition, true)?)
        }
        None => None,
    };

    let mut mark_index = 0;
    if matches!(join_type, JoinType::Semi | JoinType::Anti | JoinType::Mark) {
        scope.binder.bind_context.remove_context(&right_aliases);
        if join_type == JoinType::Mark {
            mark_index = scope.binder.generate_table_index();
            let alias = format!("__internal_mark_join_ref{mark_index}");
            scope.binder.bind_context.add_generic_binding(
                mark_index,
                &alias,
                vec!["__mark_index_column".to_owned()],
                vec![rustdb_types::LogicalType::BOOLEAN],
            )?;
        }
    }
    if matches!(join_type, JoinType::RightSemi | JoinType::RightAnti) {
        scope.binder.bind_context.remove_context(&left_aliases);
    }

    let mut types = left.types.clone();
    types.extend(right.types.iter().cloned());
    let mut names = left.names.clone();
    names.extend(right.names.iter().cloned());
    let bound = BoundJoinRef {
        left,
        right,
        join_type,
        ref_type,
        condition,
        lateral,
        correlated_columns,
        mark_index,
    };
    let plan = create_join_plan(scope, bound)?;
    Ok(BoundStatement::new(plan, names, types))
}
