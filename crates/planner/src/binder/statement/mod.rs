//! Statements (DuckDB's `planner/binder/statement`): SELECT, EXPLAIN, SET/RESET, PRAGMA,
//! transaction statements, PREPARE/EXECUTE/DEALLOCATE, CALL, CREATE/DROP and
//! INSERT/UPDATE/DELETE. Other statements report `Not implemented`.

use std::sync::Arc;

use rustdb_ast::{
    CatalogType as AstCatalogType, DropInfo, ExecuteStatement, ExplainStatement, ExpressionType, PragmaInfo,
    PrepareStatement, SetScope, SetStatement, SqlStatement, SqlStatementKind, TransactionInfo,
    TransactionType,
};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId};

mod create;
mod modify;
mod table;

use super::query_node::bind_node;
use super::{BindScope, Binder, BoundStatement};
use crate::expression::BoundParameterData;
use crate::expression_binder::{ConstantBinder, ExpressionBinder};
use crate::operator::{LogicalKind, LogicalOperator};
use crate::planner::Planner;
use crate::prepared::StatementReturnType;

fn success(plan: LogicalOperator) -> BoundStatement {
    BoundStatement::new(plan, vec!["Success".to_owned()], vec![LogicalType::BOOLEAN])
}

/// Binds any statement (DuckDB's `Binder::Bind(SQLStatement &)`).
pub fn bind_statement(scope: &mut BindScope<'_>, statement: &mut SqlStatement) -> Result<BoundStatement> {
    match &mut statement.kind {
        SqlStatementKind::Select(select) => {
            scope.with_properties(|properties| properties.return_type = StatementReturnType::QueryResult);
            bind_node(scope, &mut select.node)
        }
        SqlStatementKind::Explain(explain) => bind_explain(scope, explain),
        SqlStatementKind::Set(set) => bind_set(scope, set),
        SqlStatementKind::Transaction(info) => Ok(bind_transaction(scope, info)),
        SqlStatementKind::Prepare(prepare) => bind_prepare(scope, prepare),
        SqlStatementKind::Execute(execute) => bind_execute(scope, execute),
        SqlStatementKind::Drop(drop) if drop.catalog_type == AstCatalogType::PreparedStatement => {
            Ok(bind_deallocate(scope, drop))
        }
        SqlStatementKind::Pragma(pragma) => bind_pragma(scope, pragma),
        SqlStatementKind::Create(info) => create::bind_create(scope, info),
        SqlStatementKind::Drop(drop) => create::bind_drop(scope, drop),
        SqlStatementKind::Insert(insert) => modify::bind_insert(scope, insert),
        SqlStatementKind::Update(update) => modify::bind_update(scope, update),
        SqlStatementKind::Delete(delete) => modify::bind_delete(scope, delete),
        SqlStatementKind::Call(call) => bind_call(scope, call),
        other => Err(Error::not_implemented(format!(
            "Unimplemented statement type \"{}\" for Bind",
            other.statement_type().name()
        ))),
    }
}

/// `CALL function(...)` (DuckDB's `Binder::Bind(CallStatement &)`): bound as
/// `SELECT * FROM function(...)`.
fn bind_call(scope: &mut BindScope<'_>, call: &mut rustdb_ast::CallStatement) -> Result<BoundStatement> {
    use rustdb_ast::{
        AggregateHandling, CommonTableExpressionMap, OrdinalityType, ParsedExpression, ParsedExpressionKind,
        QueryNode, QueryNodeKind, SelectNode, StarExpression, TableFunctionRef, TableRef, TableRefKind,
    };
    let star = ParsedExpression::new(
        ExpressionType::Star,
        ParsedExpressionKind::Star(Box::new(StarExpression {
            relation_name: String::new(),
            exclude_list: Vec::new(),
            replace_list: CaseInsensitiveMap::default(),
            rename_list: rustdb_common::OrderedMap::default(),
            columns: false,
            expr: None,
        })),
    );
    let from_table = TableRef {
        alias: String::new(),
        sample: None,
        query_location: None,
        kind: TableRefKind::TableFunction(TableFunctionRef {
            function: call.function.clone(),
            column_name_alias: Vec::new(),
            with_ordinality: OrdinalityType::WithoutOrdinality,
        }),
    };
    let mut node = QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list: vec![star],
            from_table: Box::new(from_table),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    };
    scope.with_properties(|properties| properties.return_type = StatementReturnType::QueryResult);
    bind_node(scope, &mut node)
}

/// `EXPLAIN statement` (DuckDB's `Binder::Bind(ExplainStatement &)`).
fn bind_explain(scope: &mut BindScope<'_>, explain: &mut ExplainStatement) -> Result<BoundStatement> {
    let bound = bind_statement(scope, &mut explain.statement)?;
    let plan = LogicalOperator::new(LogicalKind::Explain {
        explain_type: explain.explain_type,
        explain_format: explain.explain_format,
    })
    .with_children(vec![bound.plan]);
    scope.with_properties(|properties| properties.return_type = StatementReturnType::QueryResult);
    Ok(BoundStatement::new(
        plan,
        vec!["explain_key".to_owned(), "explain_value".to_owned()],
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
    ))
}

/// `SET name = value` / `RESET name` (DuckDB's `Bind(SetVariableStatement &)` and
/// `Bind(ResetVariableStatement &)`).
fn bind_set(scope: &mut BindScope<'_>, set: &mut SetStatement) -> Result<BoundStatement> {
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    let Some(value) = &mut set.value else {
        return Ok(success(LogicalOperator::new(LogicalKind::Reset {
            name: set.name.clone(),
            scope: set.scope,
        })));
    };
    if set.scope == SetScope::Variable {
        return Err(Error::not_implemented("SET VARIABLE"));
    }
    let bound = {
        let (inner, outer) = scope.split();
        let mut binder = ConstantBinder::new(inner, outer, "SET value");
        binder.bind(value, true)?
    };
    if bound.has_parameter() {
        return Err(Error::not_implemented("SET statements cannot have parameters"));
    }
    let value = scope.context.evaluator.evaluate_scalar(&bound)?;
    Ok(success(LogicalOperator::new(LogicalKind::Set {
        name: set.name.clone(),
        value,
        scope: set.scope,
    })))
}

/// `BEGIN` / `COMMIT` / `ROLLBACK` (DuckDB's `Bind(TransactionStatement &)`).
fn bind_transaction(scope: &BindScope<'_>, info: &TransactionInfo) -> BoundStatement {
    scope.with_properties(|properties| {
        properties.requires_valid_transaction = info.transaction_type == TransactionType::BeginTransaction;
        properties.return_type = StatementReturnType::Nothing;
    });
    success(LogicalOperator::new(LogicalKind::Transaction {
        info: info.clone(),
    }))
}

/// `DEALLOCATE name` (DuckDB's `Bind(DropStatement &)` for prepared statements).
fn bind_deallocate(scope: &BindScope<'_>, drop: &DropInfo) -> BoundStatement {
    scope.with_properties(|properties| {
        properties.requires_valid_transaction = false;
        properties.return_type = StatementReturnType::Nothing;
    });
    success(LogicalOperator::new(LogicalKind::Deallocate {
        name: drop.name.clone(),
    }))
}

/// `PRAGMA name(args)` without a result (DuckDB's `Bind(PragmaStatement &)`): the
/// arguments must be constants.
fn bind_pragma(scope: &mut BindScope<'_>, pragma: &mut PragmaInfo) -> Result<BoundStatement> {
    let mut parameters = Vec::with_capacity(pragma.parameters.len());
    for parameter in &mut pragma.parameters {
        let bound = {
            let (inner, outer) = scope.split();
            let mut binder = ConstantBinder::new(inner, outer, "PRAGMA value");
            binder.bind(parameter, true)?
        };
        parameters.push(scope.context.evaluator.evaluate_scalar(&bound)?);
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    Ok(success(LogicalOperator::new(LogicalKind::Pragma {
        name: pragma.name.clone(),
        parameters,
    })))
}

/// `PREPARE name AS statement` (DuckDB's `Bind(PrepareStatement &)`).
fn bind_prepare(scope: &mut BindScope<'_>, prepare: &mut PrepareStatement) -> Result<BoundStatement> {
    let planner = Planner::new(Arc::clone(&scope.context));
    let prepared = planner.prepare((*prepare.statement).clone())?;
    scope.with_properties(|properties| {
        properties.requires_valid_transaction = false;
        properties.bound_all_parameters = true;
        properties.parameter_count = 0;
        properties.return_type = StatementReturnType::Nothing;
    });
    Ok(success(LogicalOperator::new(LogicalKind::Prepare {
        name: prepare.name.clone(),
        prepared: Arc::new(prepared),
    })))
}

/// `EXECUTE name(values)` (DuckDB's `Bind(ExecuteStatement &)`). The statement is bound
/// again with the values, so the plan uses them as constants.
fn bind_execute(scope: &mut BindScope<'_>, execute: &mut ExecuteStatement) -> Result<BoundStatement> {
    let Some(prepared) = scope.context.prepared_statements.get(&execute.name).cloned() else {
        return Err(Error::binder(format!(
            "Prepared statement \"{}\" does not exist",
            execute.name
        )));
    };
    prepared.check_parameter_count(execute.named_values.len())?;
    // DuckDB's `PreparedStatementData::Bind`: every parameter needs a value.
    for (identifier, _) in prepared.value_map.iter() {
        if execute.named_values.get(identifier).is_none() {
            return Err(Error::binder(format!(
                "Could not find parameter with identifier {identifier}"
            )));
        }
    }
    let mut values = CaseInsensitiveMap::default();
    let mut constant_binder = Binder::new(Arc::clone(&scope.context))?;
    constant_binder.can_contain_nulls = true;
    for (name, expr) in execute.named_values.iter_mut() {
        let is_literal = expr.expression_type == ExpressionType::ValueConstant;
        let bound = {
            let mut binder = ConstantBinder::new(&mut constant_binder, None, "EXECUTE statement");
            binder.bind(expr, true)?
        };
        let data = if is_literal && let Some(value) = bound.as_constant() {
            let return_type = if bound.return_type.id() == LogicalTypeId::Varchar
                && crate::expression_binder::collation(&bound.return_type).is_none()
            {
                LogicalType::STRING_LITERAL
            } else if bound.return_type.is_integral() {
                LogicalType::integer_literal(value.clone())
            } else {
                value.logical_type().clone()
            };
            BoundParameterData {
                value: value.clone(),
                return_type,
            }
        } else {
            let value = scope.context.evaluator.evaluate_scalar(&bound)?;
            BoundParameterData {
                return_type: value.logical_type().clone(),
                value,
            }
        };
        values.insert(name.to_owned(), data);
    }
    let planner = Planner::new(Arc::clone(&scope.context));
    let (rebound, plan) = planner.prepare_with_values(prepared.unbound_statement.clone(), values)?;
    let parameter_count = execute.named_values.len();
    scope.with_properties(|properties| {
        *properties = rebound.properties.clone();
        properties.parameter_count = parameter_count;
    });
    let names = rebound.names.clone();
    let types = rebound.types.clone();
    let plan = LogicalOperator::new(LogicalKind::Execute {
        prepared: Arc::new(rebound),
    })
    .with_children(vec![plan]);
    Ok(BoundStatement::new(plan, names, types))
}
