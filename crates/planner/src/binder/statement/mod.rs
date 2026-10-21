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

mod alter;
mod alter_struct;
mod copy;
mod create;
mod create_index;
mod create_macro;
mod export;
mod merge_into;
mod modify;
mod table;

pub use create_index::bind_index_expressions;

use super::query_node::{bind_node, bind_statement_with_ctes};
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
            scope.with_properties(|properties| {
                properties.allow_stream_result = true;
                properties.return_type = StatementReturnType::QueryResult;
            });
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
        SqlStatementKind::Insert(insert) => {
            let cte_map = std::mem::take(&mut insert.cte_map);
            bind_statement_with_ctes(scope, &cte_map, &mut |scope| modify::bind_insert(scope, insert))
        }
        SqlStatementKind::Update(update) => {
            let cte_map = std::mem::take(&mut update.cte_map);
            bind_statement_with_ctes(scope, &cte_map, &mut |scope| modify::bind_update(scope, update))
        }
        SqlStatementKind::Delete(delete) => {
            let cte_map = std::mem::take(&mut delete.cte_map);
            bind_statement_with_ctes(scope, &cte_map, &mut |scope| modify::bind_delete(scope, delete))
        }
        SqlStatementKind::MergeInto(merge_into) => {
            let cte_map = std::mem::take(&mut merge_into.cte_map);
            bind_statement_with_ctes(scope, &cte_map, &mut |scope| {
                merge_into::bind_merge_into(scope, merge_into)
            })
        }
        SqlStatementKind::Call(call) => bind_call(scope, call),
        SqlStatementKind::Alter(info) => alter::bind_alter(scope, info),
        SqlStatementKind::Attach(info) => bind_attach(scope, info),
        SqlStatementKind::Detach(info) => Ok(bind_detach(scope, info)),
        SqlStatementKind::Copy(info) => copy::bind_copy(scope, info),
        SqlStatementKind::Export(export) => export::bind_export(scope, export),
        SqlStatementKind::Vacuum(info) => bind_vacuum(scope, info),
        other => Err(Error::not_implemented(format!(
            "Unimplemented statement type \"{}\" for Bind",
            other.statement_type().name()
        ))),
    }
}

/// `VACUUM` / `ANALYZE` (DuckDB's `Binder::Bind(VacuumStatement &)`): the table and
/// columns are checked; the statistics DuckDB recomputes are not kept here.
fn bind_vacuum(scope: &mut BindScope<'_>, info: &mut rustdb_ast::VacuumInfo) -> Result<BoundStatement> {
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    let names = vec!["Success".to_owned()];
    let types = vec![LogicalType::BOOLEAN];
    let Some(table_ref) = info.table_ref.as_mut().filter(|_| info.has_table) else {
        let plan = LogicalOperator::new(LogicalKind::EmptyResult {
            return_types: types.clone(),
            bindings: vec![crate::ColumnBinding::new(0, 0)],
        });
        return Ok(BoundStatement::new(plan, names, types));
    };
    // `Binder::BindVacuumTable`: the vacuum sinks a scan of the analyzed columns.
    let mut bound = super::tableref::bind_table_ref(scope, table_ref)?;
    let base_tables = || Error::binder("Can only vacuum or analyze base tables");
    let LogicalKind::Get(get) = &mut bound.plan.kind else {
        return Err(base_tables());
    };
    let Some(entry) = get.table.clone() else {
        return Err(base_tables());
    };
    let rustdb_catalog::EntryKind::Table(table) = &entry.kind else {
        return Err(base_tables());
    };
    if info.columns.is_empty() {
        info.columns = table
            .physical_columns()
            .map(|(_, _, column)| column.name.clone())
            .collect();
    }
    let mut column_ids = Vec::with_capacity(info.columns.len());
    let mut column_id_map = Vec::with_capacity(info.columns.len());
    let mut select_list = Vec::with_capacity(info.columns.len());
    for name in &info.columns {
        let Some((logical, column)) = table.column(name) else {
            return Err(Error::binder(format!(
                "Column with name \"{name}\" does not exist"
            )));
        };
        if column.generated.is_some() {
            return Err(Error::binder(format!(
                "cannot vacuum or analyze generated column \"{}\" - specify non-generated columns to vacuum or analyze",
                column.name
            )));
        }
        if column_ids
            .iter()
            .any(|existing: &crate::operator::ColumnIndex| existing.index as usize == logical)
        {
            return Err(Error::binder(
                "cannot vacuum or analyze the same column twice, i.e., there is a duplicate entry in the list of column names",
            ));
        }
        let binding = crate::ColumnBinding::new(get.table_index, column_ids.len() as u64);
        select_list.push(crate::expression::Expression::column_ref(
            column.name.clone(),
            column.logical_type.clone(),
            binding,
            0,
        ));
        column_ids.push(crate::operator::ColumnIndex::new(logical as u64));
        column_id_map.push(table.logical_to_physical(logical).unwrap_or(logical));
    }
    get.shared_column_ids = None;
    get.column_ids = column_ids;
    let projection = LogicalOperator::projection(scope.generate_table_index(), select_list, bound.plan);
    let mut plan = LogicalOperator::new(LogicalKind::Vacuum(Box::new(crate::operator::LogicalVacuum {
        table: entry,
        column_id_map,
    })));
    plan.add_child(projection);
    Ok(BoundStatement::new(plan, names, types))
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
    let logical_plan_unopt = bound.plan.to_explain_string(explain.explain_format);
    let plan = LogicalOperator::new(LogicalKind::Explain {
        explain_type: explain.explain_type,
        explain_format: explain.explain_format,
        logical_plan_unopt,
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
        // The value is computed by running `SELECT value` when the statement executes.
        let select = rustdb_ast::SelectNode {
            select_list: vec![(**value).clone()],
            from_table: Box::new(rustdb_ast::TableRef {
                alias: String::new(),
                sample: None,
                query_location: None,
                kind: rustdb_ast::TableRefKind::Empty,
            }),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: rustdb_ast::AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        };
        let mut node = rustdb_ast::QueryNode {
            modifiers: Vec::new(),
            cte_map: rustdb_ast::CommonTableExpressionMap::default(),
            kind: rustdb_ast::QueryNodeKind::Select(Box::new(select)),
        };
        let bound = bind_node(scope, &mut node)?;
        if bound.types.len() > 1 {
            return Err(Error::binder("SET variable expected a single input"));
        }
        let plan = LogicalOperator::new(LogicalKind::Set {
            name: set.name.clone(),
            value: rustdb_types::Value::null(LogicalType::SQLNULL),
            scope: set.scope,
        })
        .with_children(vec![bound.plan]);
        return Ok(success(plan));
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

/// `ATTACH path AS name (options)` (DuckDB's `Binder::Bind(AttachStatement &)`): the
/// options are evaluated; a bare identifier is its own name as a string.
fn bind_attach(scope: &mut BindScope<'_>, info: &mut rustdb_ast::AttachInfo) -> Result<BoundStatement> {
    scope.with_properties(|properties| {
        properties.return_type = StatementReturnType::Nothing;
        properties.requires_valid_transaction = false;
    });
    let mut bound_info = info.clone();
    let parsed = std::mem::take(&mut bound_info.parsed_options);
    for (name, expression) in parsed.iter() {
        let value = if let rustdb_ast::ParsedExpressionKind::ColumnRef(column) = &expression.kind {
            if column.column_names.len() != 1 {
                return Err(Error::binder(format!(
                    "ATTACH option \"{name}\": qualified column names are not supported"
                )));
            }
            rustdb_types::Value::varchar(column.column_names[0].clone())
        } else {
            let mut expression = expression.clone();
            let bound = {
                let (inner, outer) = scope.split();
                let mut binder = ConstantBinder::new(inner, outer, "ATTACH option");
                binder.bind(&mut expression, true)?
            };
            scope.context.evaluator.evaluate_scalar(&bound)?
        };
        if value.is_null() {
            return Err(Error::binder(format!(
                "NULL is not supported as a valid option for ATTACH option \"{name}\""
            )));
        }
        bound_info.options.insert(name.to_lowercase(), value);
    }
    Ok(success(LogicalOperator::new(LogicalKind::Attach(Box::new(
        bound_info,
    )))))
}

/// `DETACH name` (DuckDB's `Binder::Bind(DetachStatement &)`).
fn bind_detach(scope: &BindScope<'_>, info: &rustdb_ast::DetachInfo) -> BoundStatement {
    scope.with_properties(|properties| {
        properties.return_type = StatementReturnType::Nothing;
        properties.requires_valid_transaction = false;
    });
    success(LogicalOperator::new(LogicalKind::Detach(info.clone())))
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

/// `PRAGMA name(args)` without a result (DuckDB's `Bind(PragmaStatement &)` over
/// `Binder::BindPragma`): the arguments are constants, and the catalog must hold the
/// pragma with an overload for them.
fn bind_pragma(scope: &mut BindScope<'_>, pragma: &mut PragmaInfo) -> Result<BoundStatement> {
    let constant = |scope: &mut BindScope<'_>, parameter: &mut rustdb_ast::ParsedExpression| {
        let bound = {
            let (inner, outer) = scope.split();
            let mut binder = ConstantBinder::new(inner, outer, "PRAGMA value");
            binder.bind(parameter, true)?
        };
        scope.context.evaluator.evaluate_scalar(&bound)
    };
    let mut parameters = Vec::with_capacity(pragma.parameters.len());
    for parameter in &mut pragma.parameters {
        parameters.push(constant(scope, parameter)?);
    }
    let mut named_parameters = Vec::with_capacity(pragma.named_parameters.len());
    for (name, parameter) in pragma.named_parameters.iter_mut() {
        named_parameters.push((name.to_owned(), constant(scope, parameter)?));
    }
    let context = Arc::clone(&scope.context);
    let lookup = |if_exists| {
        context.get_entry(
            "",
            rustdb_catalog::DEFAULT_SCHEMA,
            rustdb_catalog::CatalogType::PragmaFunction,
            &pragma.name,
            if_exists,
        )
    };
    let entry = if let Some(entry) = lookup(true)? {
        entry
    } else {
        let table_function = context.get_entry(
            "",
            rustdb_catalog::DEFAULT_SCHEMA,
            rustdb_catalog::CatalogType::TableFunction,
            &pragma.name,
            true,
        )?;
        if table_function.is_some() {
            return Err(Error::catalog(format!(
                "Pragma Function with name {0} does not exist, but a table function with the same name \
                 exists, try `CALL {0}(...)`",
                pragma.name
            )));
        }
        // The lookup again, for its error.
        lookup(false)?.ok_or_else(|| Error::internal("pragma lookup"))?
    };
    let rustdb_catalog::EntryKind::PragmaFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("\"{}\" is not a pragma", pragma.name)));
    };
    let types: Vec<LogicalType> = parameters
        .iter()
        .map(|value| value.logical_type().clone())
        .collect();
    let index = crate::function_binder::FunctionBinder::new(&context).bind_function(
        entry.entry.name(),
        &functions.functions,
        &types,
    )??;
    let function = &functions.functions[index];
    super::tableref::bind_named_parameters(
        &function.named_parameters,
        &mut named_parameters,
        None,
        &function.name,
    )?;
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

/// `PreparedStatement::VerifyParameters`: the supplied values name exactly the
/// statement's parameters (`MissingValuesException` / `ExcessValuesException`).
pub fn verify_parameters<V, E>(
    provided: &rustdb_common::CaseInsensitiveMap<V>,
    expected: &rustdb_common::CaseInsensitiveMap<E>,
) -> Result<()> {
    let names = |from: &dyn Fn(&str) -> bool, of: Vec<&str>| {
        let mut names: Vec<&str> = of.into_iter().filter(|name| !from(name)).collect();
        names.sort_unstable();
        names.dedup();
        names.join(", ")
    };
    let missing = || {
        Error::invalid_input(format!(
            "Values were not provided for the following prepared statement parameters: {}",
            names(
                &|name| provided.contains_key(name),
                expected.iter().map(|(name, _)| name).collect()
            )
        ))
    };
    if expected.len() >= provided.len() {
        if expected.iter().any(|(name, _)| !provided.contains_key(name)) {
            return Err(missing());
        }
        return Ok(());
    }
    Err(Error::invalid_input(format!(
        "Parameter argument/count mismatch, identifiers of the excess parameters: {}",
        names(
            &|name| expected.contains_key(name),
            provided.iter().map(|(name, _)| name).collect()
        )
    )))
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
    verify_parameters(&execute.named_values, &prepared.unbound_statement.named_param_map)?;
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
