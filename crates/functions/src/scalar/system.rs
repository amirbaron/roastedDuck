//! Connection and catalog information functions (DuckDB's
//! `core_functions/scalar/generic/system_functions.cpp` and
//! `function/scalar/system`): `current_query`, `current_schema`, `current_schemas`,
//! `current_database`, `in_search_path`, `txid_current`, `current_connection_id`,
//! `current_transaction_id` and `current_query_id`.
//!
//! The search path is DuckDB's default one (`temp.main`, `<database>.main`,
//! `system.main`, `system.pg_catalog`); `SET search_path` is not supported yet.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::{
    BindInput, FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

/// The calling connection, kept from bind time for the kernel.
#[derive(Debug)]
struct Client(Arc<FunctionClient>);

impl FunctionData for Client {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind_client(_: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    Ok(Some(Arc::new(Client(Arc::clone(input.client)))))
}

fn client(state: &FunctionState) -> Result<Arc<FunctionClient>> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<Client>())
        .map(|client| Arc::clone(&client.0))
        .ok_or_else(|| rustdb_common::Error::internal("system function without its connection"))
}

/// The `(database, schema)` entries of the default search path (DuckDB's
/// `CatalogSearchPath::Get`).
fn search_path(database: &str) -> Vec<(String, String)> {
    vec![
        ("temp".to_owned(), "main".to_owned()),
        (database.to_owned(), "main".to_owned()),
        ("system".to_owned(), "main".to_owned()),
        ("system".to_owned(), "pg_catalog".to_owned()),
    ]
}

/// A function of the connection without arguments whose result is a constant.
fn connection_function(
    name: &str,
    return_type: LogicalType,
    stability: Stability,
    value: fn(&FunctionClient) -> Value,
) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        Vec::new(),
        return_type,
        move |_: &DataChunk, state: &mut FunctionState, result: &mut Vector| {
            let client = client(state)?;
            *result = Vector::constant(&value(&client))?;
            Ok(())
        },
    )
    .with_bind(bind_client);
    function.stability = stability;
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `current_schemas(include_implicit)`: the schemas of the search path.
/// The schemas of the connection's set search path (`CatalogSearchPath::GetSetPaths`),
/// parsed from the `search_path` setting (`CatalogSearchEntry::ParseList`: `"`-quoted
/// parts, `.` between catalog and schema, `,` between entries).
fn set_path_schemas(client: &FunctionClient) -> Vec<String> {
    let Some(text) = client
        .settings()
        .and_then(|settings| settings.current_setting("search_path").ok())
        .and_then(|value| value.as_str().map(str::to_owned))
    else {
        return Vec::new();
    };
    let mut schemas = Vec::new();
    let mut part = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                part.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            '.' if !quoted => part.clear(),
            ',' if !quoted => schemas.push(std::mem::take(&mut part)),
            _ => part.push(c),
        }
    }
    if !part.is_empty() {
        schemas.push(part);
    }
    schemas
}

fn current_schemas_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "current_schemas",
        vec![LogicalType::BOOLEAN],
        LogicalType::list(LogicalType::VARCHAR),
        |args, state, result| {
            let client = client(state)?;
            let include_implicit = args.column(0).get_value(0);
            let value = match include_implicit.as_bool() {
                None => Value::null(LogicalType::list(LogicalType::VARCHAR)),
                Some(false) => Value::list(
                    LogicalType::VARCHAR,
                    set_path_schemas(&client)
                        .into_iter()
                        .map(Value::varchar)
                        .collect(),
                ),
                Some(true) => {
                    // `CatalogSearchPath::Get`: temp.main, the set paths, the default
                    // schema and system.main / system.pg_catalog.
                    let mut paths = search_path(&client.database());
                    let set = set_path_schemas(&client);
                    let tail = paths.split_off(1);
                    paths.extend(set.into_iter().map(|schema| (String::new(), schema)));
                    paths.extend(tail);
                    Value::list(
                        LogicalType::VARCHAR,
                        paths
                            .into_iter()
                            .map(|(_, schema)| Value::varchar(schema))
                            .collect(),
                    )
                }
            };
            *result = Vector::constant(&value)?;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        if input.argument_types[0] != LogicalType::BOOLEAN {
            return Err(rustdb_common::Error::binder(
                "current_schemas requires a boolean input",
            ));
        }
        if input.constants[0].is_none() {
            return Err(rustdb_common::Error::not_implemented(
                "current_schemas requires a constant input",
            ));
        }
        bind_client(function, input)
    });
    function.stability = Stability::ConsistentWithinQuery;
    let mut set = ScalarFunctionSet::new("current_schemas");
    set.add(function);
    set
}

/// The system information functions. `current_database`, `current_schema` and
/// `in_search_path` need the catalog and are registered by the database crate.
pub fn system_functions() -> Vec<ScalarFunctionSet> {
    vec![
        connection_function(
            "current_query",
            LogicalType::VARCHAR,
            Stability::Volatile,
            |client| Value::varchar(client.query()),
        ),
        current_schemas_functions(),
        connection_function(
            "txid_current",
            LogicalType::UBIGINT,
            Stability::ConsistentWithinQuery,
            |client| Value::ubigint(client.transaction_id()),
        ),
        connection_function(
            "current_connection_id",
            LogicalType::UBIGINT,
            Stability::Volatile,
            |client| Value::ubigint(client.connection_id()),
        ),
        connection_function(
            "current_transaction_id",
            LogicalType::UBIGINT,
            Stability::Volatile,
            |client| Value::ubigint(client.transaction_id()),
        ),
        connection_function(
            "current_query_id",
            LogicalType::UBIGINT,
            Stability::Volatile,
            |client| Value::ubigint(client.query_id()),
        ),
    ]
}
