//! Table function calls in FROM (DuckDB's `bind_table_function.cpp` and
//! `bind_named_parameters.cpp`): constant arguments are bound and evaluated, the
//! overload is chosen, and the function's bind callback decides the output columns.

use std::fmt::Write as _;

use rustdb_ast::{
    ExpressionType, FunctionExpression, MacroFunction, MacroFunctionKind, OrdinalityType, ParsedExpression,
    ParsedExpressionKind, QueryNode, TableFunctionRef,
};
use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::{CastParameters, ValueCast};
use rustdb_function::{TableFunction, TableFunctionBindInput};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::binder::expression::bind_macro;
use crate::binder::query_node::bind_node;
use crate::binder::{BindScope, Binding, BindingAlias, BindingKind, BoundStatement};
use crate::expression_binder::{ExpressionBinder, TableFunctionBinder, parameter_not_resolved};
use crate::function_binder::FunctionBinder;
use crate::operator::{LogicalGet, LogicalKind, LogicalOperator};

/// `name := value` arguments in the order they were written; a repeated name replaces
/// the earlier value (DuckDB's `named_parameter_map_t`).
#[derive(Debug, Default)]
struct NamedParameters(Vec<(String, Value)>);

impl NamedParameters {
    fn insert(&mut self, name: String, value: Value) {
        match self
            .0
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            Some(entry) => entry.1 = value,
            None => self.0.push((name, value)),
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The name a table function's columns are bound under (DuckDB's `GetAlias`).
fn function_alias(alias: &str, function: &FunctionExpression) -> String {
    if alias.is_empty() {
        function.function_name.clone()
    } else {
        alias.to_owned()
    }
}

/// Whether a table function argument binds to a constant: no subqueries (table
/// parameters); column references are names.
fn table_argument_is_scalar(expr: &mut ParsedExpression) -> bool {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(_) => true,
        ParsedExpressionKind::Subquery(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::PositionalReference(_) => false,
        _ => {
            let mut scalar = true;
            let _ = crate::expression_binder::for_each_child(expr, &mut |child| {
                scalar &= table_argument_is_scalar(child);
                Ok(())
            });
            scalar
        }
    }
}

/// DuckDB's `ParsedExpression::IsScalar`: no column, positional or DEFAULT references
/// and no subqueries.
fn is_scalar_expression(expr: &mut ParsedExpression) -> bool {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::Subquery(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::PositionalReference(_) => false,
        ParsedExpressionKind::Parameter(_) => true,
        _ => {
            let mut scalar = true;
            let _ = crate::expression_binder::for_each_child(expr, &mut |child| {
                scalar &= is_scalar_expression(child);
                Ok(())
            });
            scalar
        }
    }
}

/// `Binder::BindTableInTableOutFunction`: the arguments as the select list of a subquery
/// without FROM, whose rows feed the function (`UNNEST([1, 2])` reads
/// `UNNEST((SELECT [1, 2]))`); its correlated columns move to this binder.
fn bind_in_out_arguments(scope: &mut BindScope<'_>, children: &[ParsedExpression]) -> Result<BoundStatement> {
    let mut node = rustdb_ast::QueryNode {
        modifiers: Vec::new(),
        cte_map: rustdb_ast::CommonTableExpressionMap::default(),
        kind: rustdb_ast::QueryNodeKind::Select(Box::new(rustdb_ast::SelectNode {
            select_list: children.to_vec(),
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
        })),
    };
    let mut binder = scope.binder.child()?;
    binder.can_contain_nulls = true;
    let bound = {
        let mut child_scope = BindScope {
            binder: &mut binder,
            outer: Some(&mut *scope),
        };
        crate::binder::query_node::bind_node(&mut child_scope, &mut node)?
    };
    if binder.has_unplanned_dependent_joins {
        scope.binder.has_unplanned_dependent_joins = true;
    }
    scope.binder.move_correlated_expressions(&mut binder);
    Ok(bound)
}

/// Binds the arguments: positional argument types and values, and named parameters
/// (DuckDB's `BindTableFunctionParameters` for standard table functions).
fn bind_parameters(
    scope: &mut BindScope<'_>,
    function_name: &str,
    children: &mut [ParsedExpression],
    location: Option<u64>,
    table_parameters: bool,
    table_parameter_function: Option<&rustdb_function::TableFunctionSet>,
) -> Result<BoundParameters> {
    // While a macro definition is verified its parameters stand for constants; binding
    // them reports that their types are unknown.
    // `name = value` names a parameter: only its value has to be a constant. Column
    // references bind as their names (`TableFunctionBinder`).
    let argument_is_scalar = |child: &mut ParsedExpression| {
        if child.expression_type == ExpressionType::CompareEqual
            && let ParsedExpressionKind::Comparison(comparison) = &mut child.kind
            && let ParsedExpressionKind::ColumnRef(column) = &comparison.left.kind
            && column.column_names.len() == 1
        {
            return table_argument_is_scalar(&mut comparison.right);
        }
        table_argument_is_scalar(child)
    };
    if table_parameters
        && table_parameter_function.is_none()
        && scope.binder.macro_binding.is_none()
        && !children.iter_mut().all(argument_is_scalar)
    {
        return Err(Error::not_implemented(
            "table in-out functions and table parameters",
        ));
    }
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    let mut named = NamedParameters::default();
    let mut subquery: Option<BoundStatement> = None;
    for child in children.iter_mut() {
        let mut parameter_name = String::new();
        // `name = value` and `name := value` both name a parameter.
        if child.expression_type == ExpressionType::CompareEqual {
            if let ParsedExpressionKind::Comparison(comparison) = &mut child.kind
                && let ParsedExpressionKind::ColumnRef(column) = &comparison.left.kind
                && column.column_names.len() == 1
            {
                parameter_name.clone_from(&column.column_names[0]);
                let right = std::mem::replace(
                    &mut *comparison.right,
                    ParsedExpression::constant(Value::default()),
                );
                *child = right;
            }
        } else if !child.alias.is_empty() {
            parameter_name.clone_from(&child.alias);
        }
        // `TABLE_PARAMETER_FUNCTION`: a subquery argument is the function's input table.
        if let Some(functions) = table_parameter_function
            && let ParsedExpressionKind::Subquery(subquery_expression) = &mut child.kind
        {
            let only = functions.functions.first();
            if functions.functions.len() != 1 || only.is_none_or(|function| function.arguments.is_empty()) {
                return Err(Error::binder(format!(
                    "Only table-in-out functions can have subquery parameters - {} only accepts constant parameters",
                    only.map_or(function_name, |function| &*function.name)
                )));
            }
            if subquery.is_some() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "Table function can have at most one subquery parameter",
                )
                .with_position(location));
            }
            let mut binder = scope.binder.child()?;
            binder.can_contain_nulls = true;
            let bound = {
                let mut child_scope = BindScope {
                    binder: &mut binder,
                    outer: Some(&mut *scope),
                };
                bind_node(&mut child_scope, &mut subquery_expression.subquery.node)?
            };
            if binder.has_unplanned_dependent_joins {
                scope.binder.has_unplanned_dependent_joins = true;
            }
            scope.binder.move_correlated_expressions(&mut binder);
            subquery = Some(bound);
            arguments.push(LogicalType::simple(LogicalTypeId::Table));
            parameters.push(Value::default());
            continue;
        }
        let bound = {
            let (inner, outer) = scope.split();
            let mut binder = TableFunctionBinder::new(inner, outer, function_name, "Table function");
            binder.bind(child, true)?
        };
        if bound.has_parameter() {
            return Err(parameter_not_resolved());
        }
        if !bound.is_scalar() {
            return Err(Error::internal("Table function requires a constant parameter"));
        }
        let sql_type = bound.return_type.clone();
        let constant = scope.context.evaluator.evaluate_scalar(&bound)?;
        if parameter_name.is_empty() {
            if !named.is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "Unnamed parameters cannot come after named parameters",
                )
                .with_position(location));
            }
            arguments.push(if constant.is_null() {
                LogicalType::SQLNULL
            } else {
                sql_type
            });
            parameters.push(constant);
        } else {
            named.insert(parameter_name, constant);
        }
    }
    Ok(BoundParameters {
        arguments,
        parameters,
        named,
        subquery,
    })
}

/// The bound arguments of a table function call (`BindTableFunctionParameters`); a
/// table parameter function also has the subquery of its `TABLE` argument.
struct BoundParameters {
    arguments: Vec<LogicalType>,
    parameters: Vec<Value>,
    named: NamedParameters,
    subquery: Option<BoundStatement>,
}

/// Checks and casts named parameters (DuckDB's `Binder::BindNamedParameters`).
pub(crate) fn bind_named_parameters(
    declared: &[(String, LogicalType)],
    values: &mut [(String, Value)],
    location: Option<u64>,
    function_name: &str,
) -> Result<()> {
    for (name, value) in values {
        let Some((_, ty)) = declared
            .iter()
            .find(|(declared, _)| declared.eq_ignore_ascii_case(name))
        else {
            let mut ordered: Vec<&(String, LogicalType)> = declared.iter().collect();
            ordered.sort_by(|a, b| a.0.cmp(&b.0));
            let named = ordered.iter().fold(String::new(), |mut named, (name, ty)| {
                let _ = writeln!(named, "    {name} {ty}");
                named
            });
            let message = if named.is_empty() {
                "Function does not accept any named parameters.".to_owned()
            } else {
                format!("Candidates:\n{named}")
            };
            return Err(Error::binder(format!(
                "Invalid named parameter \"{name}\" for function {function_name}\n{message}"
            ))
            .with_position(location));
        };
        if ty.id() != LogicalTypeId::Any {
            *value = value.default_cast_as(ty)?;
        }
    }
    Ok(())
}

/// The input row of an in-out table function (DuckDB's `input_table_types` and
/// `input_table_names`); empty for other table functions.
#[derive(Default)]
struct InputTable {
    types: Vec<LogicalType>,
    names: Vec<String>,
}

/// Calls the bind callback and builds the scan (DuckDB's `BindTableFunctionInternal`).
fn bind_table_function_internal(
    scope: &mut BindScope<'_>,
    function: TableFunction,
    function_name: &str,
    reference: &TableFunctionRef,
    parameters: Vec<Value>,
    named_parameters: Vec<(String, Value)>,
    input_table: InputTable,
) -> Result<BoundStatement> {
    let InputTable {
        types: input_table_types,
        names: input_table_names,
    } = input_table;
    let bind_index = scope.binder.generate_table_index();
    let Some(bind) = function.bind.clone() else {
        return Err(Error::invalid_input(format!(
            "Cannot call function \"{}\" directly - it has no bind function",
            function.name
        )));
    };
    let context = std::sync::Arc::clone(&scope.context);
    let output = bind(&TableFunctionBindInput {
        inputs: &parameters,
        named_parameters: &named_parameters,
        input_table_types: &input_table_types,
        input_table_names: &input_table_names,
        binder: Some(context.as_ref() as &dyn std::any::Any),
    })?;
    let column_name_alias = &reference.column_name_alias;
    let mut return_types = output.return_types;
    let mut names = output.names;
    let with_ordinality = reference.with_ordinality == OrdinalityType::WithOrdinality;
    let correlated = !scope.binder.correlated_columns.is_empty();
    let mut ordinality_name = "ordinality".to_owned();
    let mut ordinality_index = None;
    if with_ordinality {
        // A name not taken by the function's columns or the aliases.
        let mut taken = rustdb_common::CaseInsensitiveSet::default();
        for name in names.iter().chain(column_name_alias) {
            taken.insert(name.clone());
        }
        let mut suffix = 0;
        while taken.contains(&ordinality_name) {
            ordinality_name = format!("ordinality{suffix}");
            suffix += 1;
        }
        if correlated {
            return_types.push(LogicalType::BIGINT);
            names.push(ordinality_name.clone());
            ordinality_index = Some(return_types.len() as u64 - 1);
        }
    }
    if return_types.len() != names.len() {
        return Err(Error::internal(format!(
            "Failed to bind \"{}\": return_types/names must have same size",
            function.name
        )));
    }
    if return_types.is_empty() {
        return Err(Error::internal(format!(
            "Failed to bind \"{}\": Table function must return at least one column",
            function.name
        )));
    }
    for (name, alias) in names.iter_mut().zip(column_name_alias) {
        name.clone_from(alias);
    }
    for (i, name) in names.iter_mut().enumerate() {
        if name.is_empty() {
            *name = format!("C{i}");
        }
    }
    let shared_column_ids = crate::operator::SharedColumnIds::default();
    if function.in_out {
        // In-out functions produce all their columns.
        shared_column_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend((0..return_types.len() as u64).map(crate::operator::ColumnIndex::new));
    }
    // DuckDB's `get_virtual_columns`: `filename` and similar columns of the call.
    let virtual_columns = function
        .get_virtual_columns
        .as_ref()
        .map(|virtual_columns| virtual_columns(output.bind_data.as_deref()))
        .unwrap_or_default();
    let get = LogicalGet {
        table_index: bind_index,
        function,
        bind_data: output.bind_data,
        returned_types: return_types.clone(),
        names: names.clone(),
        column_ids: Vec::new(),
        projection_ids: Vec::new(),
        parameters,
        named_parameters,
        input_table_types,
        input_table_names,
        projected_input: Vec::new(),
        ordinality_index,
        shared_column_ids: Some(std::sync::Arc::clone(&shared_column_ids)),
        table: None,
        table_schema: None,
        table_filters: rustdb_storage::TableFilterSet::default(),
        dynamic_filters: None,
        sample_options: None,
        file_filters: crate::operator::FileFilterInfo::default(),
        row_group_order: None,
        is_create_index: false,
    };
    // DuckDB's `AddTableFunction`: a table binding tracking the columns used.
    scope.binder.bind_context.add_binding(Binding::new(
        BindingKind::Table {
            bound_column_ids: shared_column_ids,
            entry: None,
            virtual_columns,
        },
        BindingAlias::new(function_name),
        return_types.clone(),
        names.clone(),
        bind_index,
    )?);
    let get = LogicalOperator::new(LogicalKind::Get(Box::new(get)));
    if !with_ordinality || correlated {
        return Ok(BoundStatement::new(get, names, return_types));
    }
    // WITH ORDINALITY: a row number over the scan.
    let window_index = scope.binder.generate_table_index();
    let ordinality_alias = column_name_alias
        .get(names.len())
        .cloned()
        .unwrap_or(ordinality_name);
    let row_number = crate::subquery::row_number(Vec::new(), Vec::new(), &ordinality_alias);
    names.push(ordinality_alias.clone());
    return_types.push(LogicalType::BIGINT);
    scope.binder.bind_context.add_generic_binding(
        window_index,
        function_name,
        vec![ordinality_alias],
        vec![LogicalType::BIGINT],
    )?;
    let window = LogicalOperator::new(LogicalKind::Window { window_index })
        .with_expressions(vec![row_number])
        .with_children(vec![get]);
    Ok(BoundStatement::new(window, names, return_types))
}

/// `FROM f(args)` (DuckDB's `Binder::Bind(TableFunctionRef &)`).
pub fn bind_table_function(
    scope: &mut BindScope<'_>,
    function_ref: &mut TableFunctionRef,
    alias: &str,
    location: Option<u64>,
) -> Result<BoundStatement> {
    let ParsedExpressionKind::Function(function) = &mut function_ref.function.kind else {
        return Err(Error::internal(
            "table function reference without a function call",
        ));
    };
    if !function.schema.is_empty()
        && function.catalog.is_empty()
        && scope.context.catalog(&function.schema).is_some()
    {
        function.catalog = std::mem::take(&mut function.schema);
    }
    let context = scope.context.clone();
    let entry = context
        .get_entry(
            &function.catalog,
            &function.schema,
            CatalogType::TableFunction,
            &function.function_name,
            false,
        )
        .map_err(|error| crate::context::position_missing_entry(error, location))?
        .ok_or_else(|| Error::internal("table function lookup returned nothing"))?;
    if let EntryKind::TableMacro(macro_entry) = &entry.entry.kind {
        let macros = macro_entry.macros.clone();
        let location = function_ref.function.query_location;
        return bind_table_macro_ref(scope, function_ref, entry.entry.name(), &macros, alias, location);
    }
    let EntryKind::TableFunction(functions) = &entry.entry.kind else {
        return Err(Error::not_implemented(format!(
            "{} as a table function",
            entry.entry.catalog_type().display_name()
        )));
    };
    let function_name = functions.name.to_string();
    // `GetTableFunctionBindType`: non-constant arguments of a function with an in-out form
    // are bound as a subquery feeding it (`TABLE_IN_OUT_FUNCTION`).
    let has_table_parameter = functions.functions.iter().any(|candidate| {
        candidate
            .arguments
            .iter()
            .any(|argument| argument.id() == LogicalTypeId::Table)
    });
    if !has_table_parameter
        && scope.binder.macro_binding.is_none()
        && functions
            .functions
            .iter()
            .any(|candidate| candidate.in_out_function.is_some())
        && !function.children.iter_mut().all(is_scalar_expression)
    {
        return bind_in_out_function(
            scope,
            functions,
            &function.children.clone(),
            function_ref,
            alias,
            location,
        );
    }
    // DuckDB's `TableFunctionBindType`: in-out and table-parameter functions take
    // subqueries; other functions bind their arguments as constants.
    let table_parameters = functions.functions.iter().any(|candidate| {
        candidate.in_out
            || candidate
                .arguments
                .iter()
                .any(|argument| argument.id() == LogicalTypeId::Table)
    });
    let BoundParameters {
        arguments,
        mut parameters,
        mut named,
        subquery,
    } = bind_parameters(
        scope,
        &function_name,
        &mut function.children,
        location,
        table_parameters,
        has_table_parameter.then_some(functions),
    )
    .map_err(|error| error.with_position(location))?;
    let function_binder = FunctionBinder::new(&context);
    let table_function = function_binder
        .bind_table_function_overload(functions, &arguments)?
        .map_err(|error| error.with_position(location))?;
    bind_named_parameters(
        &table_function.named_parameters,
        &mut named.0,
        location,
        &table_function.name,
    )?;
    let input_table = if let Some(subquery) = &subquery {
        InputTable {
            types: subquery.types.clone(),
            names: subquery.names.clone(),
        }
    } else if table_function.in_out {
        // The constant arguments are the input row (typed before the casts below).
        let types: Vec<LogicalType> = parameters
            .iter()
            .map(|value| value.logical_type().clone())
            .collect();
        let names = vec![String::new(); types.len()];
        InputTable { types, names }
    } else {
        InputTable::default()
    };
    // Cast the arguments to the overload's types.
    for (i, parameter) in parameters.iter_mut().enumerate() {
        let target = table_function
            .arguments
            .get(i)
            .or(table_function.varargs.as_ref())
            .cloned()
            .unwrap_or(LogicalType::ANY);
        if !matches!(
            target.id(),
            LogicalTypeId::Any | LogicalTypeId::Pointer | LogicalTypeId::List | LogicalTypeId::Table
        ) {
            let mut cast_parameters = CastParameters::new();
            *parameter = parameter
                .cast_with(&context.casts, &target, &mut cast_parameters)
                .map_err(|error| {
                    if error.kind() == ErrorKind::Conversion {
                        Error::invalid_input(format!("Failed to cast value: {}", error.raw_message()))
                    } else {
                        error
                    }
                })?;
        }
    }
    if let Some(bind_replace) = table_function.bind_replace.clone() {
        let replacement = bind_replace(&TableFunctionBindInput {
            inputs: &parameters,
            named_parameters: &named.0,
            input_table_types: &input_table.types,
            input_table_names: &input_table.names,
            binder: Some(context.as_ref() as &dyn std::any::Any),
        })
        .map_err(|error| error.caught().with_position(location))?;
        if let Some(replacement) = replacement {
            // The query replaces the call, keeping the reference's aliases.
            let mut statements = rustdb_parser::parse(&replacement.query)?;
            let (Some(statement), true) = (statements.pop(), statements.is_empty()) else {
                return Err(Error::parser(replacement.error));
            };
            let select = match statement.kind {
                rustdb_ast::SqlStatementKind::Select(select) => select,
                // A PIVOT that creates enum types (`ParseSubquery`).
                rustdb_ast::SqlStatementKind::Multi(_) => {
                    return Err(Error::parser(
                        "PIVOT statements without explicit IN clauses are not supported in query() function. \
                         Please specify the pivot values explicitly, e.g.: PIVOT ... ON col IN (val1, val2, ...)",
                    ));
                }
                _ => return Err(Error::parser(replacement.error)),
            };
            let mut subquery = rustdb_ast::SubqueryRef {
                subquery: Box::new(select),
                column_name_alias: function_ref.column_name_alias.clone(),
            };
            // Errors in the replacement point at the call (`error.AddQueryLocation(ref)`).
            return super::bind_subquery_ref(scope, &mut subquery, alias)
                .map_err(|error| error.caught().with_position_override(location));
        }
    }
    let alias = function_alias(alias, function);
    let mut get = bind_table_function_internal(
        scope,
        table_function,
        &alias,
        function_ref,
        parameters,
        named.0,
        input_table,
    )
    .map_err(|error| error.caught().with_position(location))?;
    if let Some(subquery) = subquery {
        attach_input(&mut get, subquery)?;
    }
    Ok(get)
}

/// Makes `input` the child of the scan at the bottom of the (linear) plan of `get`
/// (`Binder::CreatePlan(BoundTableFunction &)`).
fn attach_input(get: &mut BoundStatement, input: BoundStatement) -> Result<()> {
    let mut node = &mut get.plan;
    while !node.children.is_empty() {
        if node.children.len() != 1 {
            return Err(Error::internal(
                "Binder::CreatePlan<BoundTableFunction>: linear path expected",
            ));
        }
        node = &mut node.children[0];
    }
    node.children.push(input.plan);
    Ok(())
}

/// The `TABLE_IN_OUT_FUNCTION` binding of `Binder::Bind(TableFunctionRef &)`: the
/// arguments' subquery becomes the input of the function's scan.
fn bind_in_out_function(
    scope: &mut BindScope<'_>,
    functions: &rustdb_function::TableFunctionSet,
    children: &[ParsedExpression],
    function_ref: &TableFunctionRef,
    alias: &str,
    location: Option<u64>,
) -> Result<BoundStatement> {
    let mut subquery =
        bind_in_out_arguments(scope, children).map_err(|error| error.with_position(location))?;
    let context = std::sync::Arc::clone(&scope.context);
    let table_function = FunctionBinder::new(&context)
        .bind_table_function_overload(functions, &subquery.types)?
        .map_err(|error| error.with_position(location))?;
    // The input columns take the overload's types (DuckDB casts them in a projection when
    // planning; here the arguments' projection casts them).
    let types: Vec<LogicalType> = subquery
        .types
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let target = table_function
                .arguments
                .get(i)
                .or(table_function.varargs.as_ref())
                .cloned()
                .unwrap_or(LogicalType::ANY);
            if matches!(
                target.id(),
                LogicalTypeId::Any | LogicalTypeId::Pointer | LogicalTypeId::List
            ) {
                ty.clone()
            } else {
                target
            }
        })
        .collect();
    if subquery.plan.operator_type() == crate::LogicalOperatorType::LogicalProjection {
        for (index, target) in types.iter().enumerate() {
            if let Some(expression) = subquery.plan.expressions.get_mut(index)
                && &expression.return_type != target
            {
                let original = std::mem::replace(
                    expression,
                    crate::expression::Expression::constant(Value::null(LogicalType::SQLNULL)),
                );
                *expression = crate::function_binder::add_cast_to_type(&context, original, target, false)?;
            }
        }
        subquery.plan.types.clone_from(&types);
        subquery.types.clone_from(&types);
    }
    let input_table = InputTable {
        types,
        names: subquery.names.clone(),
    };
    let ParsedExpressionKind::Function(function) = &function_ref.function.kind else {
        return Err(Error::internal(
            "table function reference without a function call",
        ));
    };
    let alias = function_alias(alias, function);
    let mut get = bind_table_function_internal(
        scope,
        table_function,
        &alias,
        function_ref,
        Vec::new(),
        Vec::new(),
        input_table,
    )
    .map_err(|error| error.caught().with_position(location))?;
    // The subquery is the input of the scan at the bottom of the (linear) plan.
    attach_input(&mut get, subquery)?;
    Ok(get)
}

/// The query of a table macro call with the parameters replaced by the arguments
/// (DuckDB's `Binder::BindTableMacro`).
fn bind_table_macro(
    binder: &mut crate::binder::Binder,
    function: &mut FunctionExpression,
    name: &str,
    macros: &[MacroFunction],
    location: Option<u64>,
) -> Result<QueryNode> {
    let arguments = match bind_macro::bind_macro_function(binder, macros, name, function, 0)? {
        Ok(arguments) => arguments,
        Err(message) => return Err(Error::binder(message).with_position(location)),
    };
    let definition = &macros[arguments.index];
    let MacroFunctionKind::Table(table) = &definition.kind else {
        return Err(Error::internal("expected a table macro"));
    };
    let binding = bind_macro::create_dummy_binding(definition, name, arguments)?;
    let mut node = (*table.query_node).clone();
    bind_macro::replace_macro_parameters_in_node(&mut node, &binding)?;
    Ok(node)
}

/// `FROM m(args)` of a table macro (the `TABLE_MACRO_ENTRY` case of DuckDB's
/// `Binder::Bind(TableFunctionRef &)`): the macro's query is bound like a subquery.
fn bind_table_macro_ref(
    scope: &mut BindScope<'_>,
    function_ref: &mut TableFunctionRef,
    name: &str,
    macros: &[MacroFunction],
    alias: &str,
    location: Option<u64>,
) -> Result<BoundStatement> {
    let ParsedExpressionKind::Function(function) = &mut function_ref.function.kind else {
        return Err(Error::internal(
            "table function reference without a function call",
        ));
    };
    let mut node = bind_table_macro(scope.binder, function, name, macros, location)?;
    let mut binder = scope.binder.child()?;
    binder.can_contain_nulls = true;
    binder.alias = if alias.is_empty() {
        "unnamed_query".to_owned()
    } else {
        alias.to_owned()
    };
    let bound = {
        let mut child_scope = BindScope {
            binder: &mut binder,
            outer: Some(&mut *scope),
        };
        bind_node(&mut child_scope, &mut node).map_err(|error| error.caught().with_position(location))?
    };
    let bind_index = bound.plan.root_index()?;
    let subquery_alias = if alias.is_empty() {
        format!("unnamed_query{bind_index}")
    } else {
        alias.to_owned()
    };
    scope.binder.bind_context.add_subquery(
        bind_index,
        &subquery_alias,
        &function_ref.column_name_alias,
        &bound.names,
        bound.types.clone(),
    )?;
    scope.binder.move_correlated_expressions(&mut binder);
    Ok(bound)
}
