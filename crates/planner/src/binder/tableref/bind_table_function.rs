//! Table function calls in FROM (DuckDB's `bind_table_function.cpp` and
//! `bind_named_parameters.cpp`): constant arguments are bound and evaluated, the
//! overload is chosen, and the function's bind callback decides the output columns.

use std::fmt::Write as _;

use rustdb_ast::{
    ExpressionType, FunctionExpression, OrdinalityType, ParsedExpression, ParsedExpressionKind,
    TableFunctionRef,
};
use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::{CastParameters, ValueCast};
use rustdb_function::{TableFunction, TableFunctionBindInput};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::binder::{BindScope, Binding, BindingAlias, BindingKind, BoundStatement};
use crate::expression_binder::{
    ExpressionBinder, TableFunctionBinder, parameter_not_resolved, parsed_is_scalar,
};
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

/// Binds the arguments: positional argument types and values, and named parameters
/// (DuckDB's `BindTableFunctionParameters` for standard table functions).
fn bind_parameters(
    scope: &mut BindScope<'_>,
    function_name: &str,
    children: &mut [ParsedExpression],
    location: Option<u64>,
) -> Result<(Vec<LogicalType>, Vec<Value>, NamedParameters)> {
    if !children.iter_mut().all(parsed_is_scalar) {
        return Err(Error::not_implemented(
            "table in-out functions and table parameters",
        ));
    }
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    let mut named = NamedParameters::default();
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
    Ok((arguments, parameters, named))
}

/// Checks and casts named parameters (DuckDB's `Binder::BindNamedParameters`).
fn bind_named_parameters(
    declared: &[(String, LogicalType)],
    values: &mut NamedParameters,
    location: Option<u64>,
    function_name: &str,
) -> Result<()> {
    for (name, value) in &mut values.0 {
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
    function_name: String,
    column_name_alias: &[String],
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
    let output = bind(&TableFunctionBindInput {
        inputs: &parameters,
        named_parameters: &named_parameters,
        input_table_types: &input_table_types,
        input_table_names: &input_table_names,
    })?;
    let return_types = output.return_types;
    let mut names = output.names;
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
        ordinality_index: None,
        shared_column_ids: Some(std::sync::Arc::clone(&shared_column_ids)),
    };
    // DuckDB's `AddTableFunction`: a table binding tracking the columns used.
    scope.binder.bind_context.add_binding(Binding::new(
        BindingKind::Table {
            bound_column_ids: shared_column_ids,
            entry: None,
            virtual_columns: Vec::new(),
        },
        BindingAlias::new(function_name),
        return_types.clone(),
        names.clone(),
        bind_index,
    )?);
    Ok(BoundStatement::new(
        LogicalOperator::new(LogicalKind::Get(Box::new(get))),
        names,
        return_types,
    ))
}

/// `FROM f(args)` (DuckDB's `Binder::Bind(TableFunctionRef &)`).
pub fn bind_table_function(
    scope: &mut BindScope<'_>,
    function_ref: &mut TableFunctionRef,
    alias: &str,
    location: Option<u64>,
) -> Result<BoundStatement> {
    if function_ref.with_ordinality == OrdinalityType::WithOrdinality {
        return Err(Error::not_implemented("WITH ORDINALITY"));
    }
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
    let EntryKind::TableFunction(functions) = &entry.entry.kind else {
        return Err(Error::not_implemented(format!(
            "{} as a table function",
            entry.entry.catalog_type().display_name()
        )));
    };
    let function_name = functions.name.to_string();
    let (arguments, mut parameters, mut named) =
        bind_parameters(scope, &function_name, &mut function.children, location)
            .map_err(|error| error.with_position(location))?;
    let function_binder = FunctionBinder::new(&context);
    let table_function = function_binder
        .bind_table_function_overload(functions, &arguments)?
        .map_err(|error| error.with_position(location))?;
    bind_named_parameters(
        &table_function.named_parameters,
        &mut named,
        location,
        &table_function.name,
    )?;
    let input_table = if table_function.in_out {
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
    let alias = function_alias(alias, function);
    bind_table_function_internal(
        scope,
        table_function,
        alias,
        &function_ref.column_name_alias,
        parameters,
        named.0,
        input_table,
    )
    .map_err(|error| error.with_position(location))
}
