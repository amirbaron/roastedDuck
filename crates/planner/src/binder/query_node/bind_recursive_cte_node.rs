//! Recursive CTEs (DuckDB's `bind_recursive_cte_node.cpp`): the anchor (left) is bound
//! first; the recursive part (right) sees the CTE, and `recurring.<name>`, as CTE
//! bindings producing the anchor's columns.

use rustdb_ast::{QueryNode, QueryNodeKind};
use rustdb_common::{Error, Result};

use super::bind_node;
use super::bind_setop_node::cast_operator_to_types;
use crate::LogicalOperatorType;
use crate::binder::{BindScope, Binding, BindingAlias, BindingKind, BoundStatement};
use crate::expression_binder::{BaseBinder, ExpressionBinder};
use crate::operator::{LogicalKind, LogicalOperator};

/// A CTE binding of the recursive CTE for its right side.
fn recursive_binding(
    alias: BindingAlias,
    index: u64,
    names: &[String],
    types: &[rustdb_types::LogicalType],
) -> Result<Binding> {
    Binding::new(
        BindingKind::Cte {
            can_be_referenced: true,
            reference_count: 0,
            bind_state: None,
        },
        alias,
        types.to_vec(),
        names.to_vec(),
        index,
    )
}

/// `left UNION [ALL] right` of `WITH RECURSIVE` (DuckDB's `Binder::BindNode(RecursiveCTENode &)`).
/// `USING KEY` with `UNION` (which acts as `UNION ALL`) is deprecated: unless
/// `deprecated_using_key_syntax` accepts it, a warning is logged.
fn warn_deprecated_using_key_union(scope: &BindScope<'_>, union_all: bool) -> Result<()> {
    const MESSAGE: &str = "Deprecated UNION in USING KEY CTE detected.Please transition to using UNION ALL, before \
                           DuckDB's next release. \nUse SET deprecated_using_key_syntax='UNION_AS_UNION_ALL' to \
                           enable the deprecated behavior. \nFor more information, see \
                           https://duckdb.org/docs/stable/sql/query_syntax/with#recursive-ctes-with-using-key.";
    if union_all {
        return Ok(());
    }
    let client = &scope.context.function_client;
    let setting = match client.settings() {
        Some(settings) => settings
            .current_setting("deprecated_using_key_syntax")?
            .as_str()
            .map(str::to_uppercase)
            .unwrap_or_default(),
        None => "DEFAULT".to_owned(),
    };
    if setting != "DEFAULT" {
        return Ok(());
    }
    // `DUCKDB_LOG_WARNING(context, msg)`.
    if let Some(logger) = client.logger() {
        use rustdb_function::logging::{LogLevel, LogScope};
        if logger.should_log(LogScope::Connection, "", LogLevel::Warning) {
            logger.write_log(LogScope::Connection, "", LogLevel::Warning, MESSAGE)?;
        }
    }
    Ok(())
}

pub fn bind_recursive_cte(scope: &mut BindScope<'_>, node: &mut QueryNode) -> Result<BoundStatement> {
    if !node.modifiers.is_empty() {
        return Err(Error::not_implemented("FIXME: bind modifiers in recursive CTE"));
    }
    let QueryNodeKind::RecursiveCte(statement) = &mut node.kind else {
        return Err(Error::internal("expected a recursive CTE"));
    };
    if !statement.key_targets.is_empty() {
        warn_deprecated_using_key_union(scope, statement.union_all)?;
    }
    let ctename = statement.cte_name.clone();
    let union_all = statement.union_all;
    let setop_index = scope.binder.generate_table_index();

    let mut left_binder = scope.binder.child()?;
    let left = {
        let mut left_scope = BindScope {
            binder: &mut left_binder,
            outer: Some(&mut *scope),
        };
        bind_node(&mut left_scope, &mut statement.left)?
    };
    // The CTE's columns are the anchor's, renamed by the aliases.
    let types = left.types.clone();
    let mut names = left.names.clone();
    for (name, alias) in names.iter_mut().zip(&statement.aliases) {
        name.clone_from(alias);
    }
    scope
        .binder
        .bind_context
        .add_generic_binding(setop_index, &ctename, names.clone(), types.clone())?;

    let mut right_binder = scope.binder.child()?;
    let cte_alias = BindingAlias::new(&ctename);
    let recurring_alias = BindingAlias::qualified("", "recurring", &ctename);
    right_binder.bind_context.add_cte_binding(recursive_binding(
        cte_alias.clone(),
        setop_index,
        &names,
        &types,
    )?)?;
    right_binder.bind_context.add_cte_binding(recursive_binding(
        recurring_alias.clone(),
        setop_index,
        &names,
        &types,
    )?)?;
    let right = {
        let mut right_scope = BindScope {
            binder: &mut right_binder,
            outer: Some(&mut *scope),
        };
        bind_node(&mut right_scope, &mut statement.right)?
    };
    for column in &left_binder.correlated_columns.clone() {
        right_binder.add_correlated_column(column.clone());
    }
    scope.binder.move_correlated_expressions(&mut left_binder);
    scope.binder.move_correlated_expressions(&mut right_binder);

    let mut key_targets = Vec::with_capacity(statement.key_targets.len());
    {
        let (inner, outer) = scope.split();
        let mut binder = BaseBinder::new(inner, outer);
        for target in &mut statement.key_targets {
            key_targets.push(binder.bind(target, true)?);
        }
    }
    if left.types.len() != right.types.len() {
        return Err(Error::binder(
            "Set operations can only apply to expressions with the same number of result columns",
        ));
    }
    left_binder.is_outside_flattened = scope.binder.is_outside_flattened;
    right_binder.is_outside_flattened = scope.binder.is_outside_flattened;
    scope.binder.has_unplanned_dependent_joins = scope.binder.has_unplanned_dependent_joins
        || left_binder.has_unplanned_dependent_joins
        || right_binder.has_unplanned_dependent_joins;
    let left_node = cast_operator_to_types(scope.binder, &left.types, &types, left.plan)?;
    let right_node = cast_operator_to_types(scope.binder, &right.types, &types, right.plan)?;

    let referenced = |alias: &BindingAlias| {
        right_binder
            .bind_context
            .cte_binding(alias)
            .is_some_and(crate::binder::Binding::is_referenced)
    };
    let ref_recurring = referenced(&recurring_alias);
    let ref_cte = referenced(&cte_alias);
    let plan = if !ref_cte && !ref_recurring {
        // Not actually recursive: a plain UNION.
        LogicalOperator::new(LogicalKind::SetOperation {
            operator_type: LogicalOperatorType::LogicalUnion,
            table_index: setop_index,
            column_count: types.len() as u64,
            setop_all: union_all,
            allow_out_of_order: true,
        })
        .with_children(vec![left_node, right_node])
    } else {
        LogicalOperator::new(LogicalKind::RecursiveCte {
            ctename,
            table_index: setop_index,
            column_count: types.len() as u64,
            union_all,
            key_targets,
            correlated_columns: crate::binder::CorrelatedColumns::default(),
            ref_recurring,
        })
        .with_children(vec![left_node, right_node])
    };
    Ok(BoundStatement::new(plan, names, types))
}
