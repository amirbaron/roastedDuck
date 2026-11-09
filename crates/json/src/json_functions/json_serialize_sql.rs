//! `json_serialize_sql`, `json_deserialize_sql` and `json_execute_serialized_sql` (DuckDB's
//! `json_serialize_sql.cpp`): the parse trees of SELECT statements as JSON, and back.

use std::any::Any;
use std::sync::Arc;

use roastedduck_ast::json::{JsonSerializeOptions, deserialize_statements};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::{
    BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, TableFunction,
    TableFunctionBindInput, TableFunctionReplacement, TableFunctionSet,
};
use roastedduck_serde::SerializationCompatibility;
use roastedduck_serde::json::JsonOptions;
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use crate::json_common::read_document;
use crate::json_executors::{ColumnBuilder, StrBuilder, StrInput, addressable, all_constant};
use crate::json_type;
use crate::yyjson::JsonArena;

/// The options of a `json_serialize_sql` call (`JsonSerializeBindData`).
#[derive(Debug)]
struct SerializeBindData {
    options: JsonSerializeOptions,
}

impl FunctionData for SerializeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The database's `storage_compatibility_version` (`serialization_compatibility`).
fn compatibility(input: &BindInput<'_>) -> SerializationCompatibility {
    input
        .client
        .settings()
        .and_then(|settings| settings.current_setting("storage_compatibility_version").ok())
        .and_then(|value| value.to_varchar().ok())
        .and_then(|name| SerializationCompatibility::from_version_name(&name).ok())
        .unwrap_or_default()
}

/// `JsonSerializeBind`.
fn serialize_bind(_: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(first) = input.argument_types.first() else {
        return Err(Error::binder("json_serialize_sql takes at least one argument"));
    };
    if first.id() != LogicalTypeId::Varchar && first.id() != LogicalTypeId::StringLiteral {
        return Err(Error::new(
            ErrorKind::InvalidType,
            "json_serialize_sql first argument must be a VARCHAR",
        ));
    }
    let mut json = JsonOptions::default();
    let mut format = false;
    for i in 1..input.argument_types.len() {
        if input.argument_types[i].id() == LogicalTypeId::Unknown {
            return Err(roastedduck_planner::expression_binder::parameter_not_resolved());
        }
        let Some(Some(value)) = input.constants.get(i) else {
            return Err(Error::binder("json_serialize_sql: arguments must be constant"));
        };
        let alias = input
            .aliases
            .get(i)
            .map_or(b"".as_slice(), roastedduck_common::ByteString::as_bytes);
        let flag = |name: &[u8]| -> Result<bool> {
            if input.argument_types[i].id() != LogicalTypeId::Boolean {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "json_serialize_sql: '{name}' argument must be a boolean"
                )));
            }
            Ok(value.as_bool().unwrap_or(false))
        };
        match alias {
            b"skip_null" => json.skip_null = flag(alias)?,
            b"skip_empty" => json.skip_empty = flag(alias)?,
            b"format" => format = flag(alias)?,
            b"skip_default" => json.skip_default = flag(alias)?,
            _ => {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "json_serialize_sql: Unknown argument '{alias}'"
                )));
            }
        }
    }
    Ok(Some(Arc::new(SerializeBindData {
        options: JsonSerializeOptions {
            json,
            format,
            compatibility: compatibility(input),
        },
    })))
}

/// `JsonSerializeFunction`.
fn serialize_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let options = state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<SerializeBindData>())
        .map(|data| data.options.clone())
        .unwrap_or_default();
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let mut builder = StrBuilder::default();
    for row in 0..rows {
        match input.get(row) {
            Some(sql) => {
                let sql = String::from_utf8_lossy(sql);
                builder.push(roastedduck_parser::json::serialize_sql(&sql, &options).as_bytes());
            }
            None => builder.push_null(),
        }
    }
    *result = builder.finish(&json_type())?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetSerializeSqlFunction`: the SQL and up to four BOOLEAN options.
pub(crate) fn serialize_sql_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_serialize_sql");
    for options in 0..=4 {
        let mut arguments = vec![LogicalType::VARCHAR];
        arguments.extend(std::iter::repeat_n(LogicalType::BOOLEAN, options));
        set.add(
            ScalarFunction::new("json_serialize_sql", arguments, json_type(), serialize_rows)
                .with_bind(serialize_bind),
        );
    }
    set
}

/// `DeserializeSelectStatement`: the SQL of each serialized statement.
fn deserialize_select_statements(json: &[u8]) -> Result<Vec<String>> {
    // DuckDB reads the document with its JSON reader first (malformed input errors).
    let mut arena = JsonArena::default();
    read_document(&mut arena, json)?;
    let text = String::from_utf8_lossy(json);
    let statements = deserialize_statements(&text).map_err(|error| {
        // The checks DuckDB makes after deserializing a statement.
        let message = error.raw_message();
        if message.contains("unexpected NULL ParsedExpression") {
            Error::parser("Error parsing json: null expression found in json")
        } else if message.contains("\"node\" of SelectStatement") {
            Error::parser("Error parsing json: no select node found in json")
        } else {
            error
        }
    })?;
    Ok(statements.iter().map(ToString::to_string).collect())
}

/// `JsonDeserializeFunction`: the statements as SQL, separated by `; `.
fn deserialize_rows(args: &DataChunk, _: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let mut builder = StrBuilder::default();
    for row in 0..rows {
        match input.get(row) {
            Some(json) => builder.push(deserialize_select_statements(json)?.join("; ").as_bytes()),
            None => builder.push_null(),
        }
    }
    *result = builder.finish(&LogicalType::VARCHAR)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetDeserializeSqlFunction`.
pub(crate) fn deserialize_sql_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_deserialize_sql");
    set.add(ScalarFunction::new(
        "json_deserialize_sql",
        vec![json_type()],
        LogicalType::VARCHAR,
        deserialize_rows,
    ));
    set
}

/// `ExecuteJsonSerializedSqlPragmaFunction`: the SQL `PRAGMA json_execute_serialized_sql`
/// runs.
pub fn execute_serialized_sql_pragma(json: &str) -> Result<String> {
    let statements = deserialize_select_statements(json.as_bytes())?;
    let [statement] = statements.as_slice() else {
        return Err(Error::binder(
            "json_execute_serialized_sql pragma expects exactly one statement",
        ));
    };
    Ok(statement.clone())
}

/// `ExecuteSqlTableFunction::Bind`: the call is replaced by the deserialized query.
fn execute_bind_replace(input: &TableFunctionBindInput<'_>) -> Result<Option<TableFunctionReplacement>> {
    let Some(serialized) = input.inputs.first() else {
        return Err(Error::internal("json_execute_serialized_sql without an argument"));
    };
    if serialized.is_null() {
        return Err(Error::binder(
            "json_execute_serialized_sql cannot execute NULL plan",
        ));
    }
    let statements = deserialize_select_statements(serialized.to_varchar()?.as_bytes())?;
    let [statement] = statements.as_slice() else {
        return Err(Error::binder(
            "json_execute_serialized_sql expects exactly one statement",
        ));
    };
    Ok(Some(TableFunctionReplacement {
        query: statement.clone(),
        error: "json_execute_serialized_sql expects exactly one statement".to_owned(),
    }))
}

/// `JSONFunctions::GetExecuteJsonSerializedSqlFunction`.
pub(crate) fn execute_serialized_sql_function() -> TableFunctionSet {
    let mut function = TableFunction::new("json_execute_serialized_sql", vec![LogicalType::VARCHAR]);
    function.bind_replace = Some(Arc::new(execute_bind_replace));
    let mut set = TableFunctionSet::new("json_execute_serialized_sql");
    set.add(function);
    set
}
