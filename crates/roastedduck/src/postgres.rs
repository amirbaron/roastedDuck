//! Runtime integration for roastedDuck's compatible PostgreSQL implementation.
pub(crate) mod auth;
pub(crate) mod binary_file;
pub(crate) mod catalog;
pub(crate) mod hstore;
mod legacy;
pub(crate) mod logging;
mod mutation;
pub(crate) mod pool;
mod query;
pub(crate) mod secret_storage;
mod text;
mod transaction;
pub(crate) use query::query_functions;
use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{
    ExtensionOption, FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_postgres::{BinaryParameter, Driver, codec::Options, config::Parameters};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

pub(crate) struct AttachConfig {
    pub isolation: String,
    pub schemas: Vec<String>,
    pub pool: roastedduck_postgres::pool::Config,
    pub secret: String,
    pub secret_table: String,
    pub explicit_secret_table: bool,
    pub secret_entry: Option<crate::secrets::KeyValueSecret>,
}
impl AttachConfig {
    pub fn parse(info: &roastedduck_ast::AttachInfo) -> Result<Self> {
        let mut result = Self {
            isolation: "REPEATABLE READ".into(),
            schemas: Vec::new(),
            pool: roastedduck_postgres::pool::Config::default(),
            secret: String::new(),
            secret_table: "duckdb_secrets".into(),
            explicit_secret_table: false,
            secret_entry: None,
        };
        for (name, value) in info.options.iter() {
            match name.to_ascii_lowercase().as_bytes() {
                b"type" | b"readonly" | b"read_only" | b"readwrite" | b"read_write" | b"hidden"
                | b"default_table" => {}
                b"isolation_level" => {
                    let value = value.to_varchar()?;
                    result.isolation = match value.to_ascii_lowercase().as_str() {
                        "read committed" => "READ COMMITTED", "repeatable read" => "REPEATABLE READ", "serializable" => "SERIALIZABLE",
                        _ => return Err(Error::binder(format!("Invalid value \"{value}\" for isolation_level, expected READ COMMITTED, REPEATABLE READ or SERIALIZABLE"))),
                    }.into();
                }
                b"schema" => {
                    if value.is_null() {
                        return Err(Error::binder("Value for \"SCHEMA\" option must not be null"));
                    }
                    let values = if value.logical_type() == &LogicalType::VARCHAR {
                        vec![value.clone()]
                    } else if value.logical_type().child_type() == Some(&LogicalType::VARCHAR) {
                        value.children().unwrap_or_default().to_vec()
                    } else {
                        return Err(Error::binder(format!(
                            "Value for \"SCHEMA\" option must be either \"VARCHAR\" or \"VARCHAR[]\", was: \"{}\"",
                            value.logical_type()
                        )));
                    };
                    for value in values {
                        if value.is_null() {
                            return Err(Error::binder("Values for \"SCHEMA\" option must not be null"));
                        }
                        let name = value.to_varchar()?;
                        if name.is_empty() {
                            return Err(Error::binder("Value \"SCHEMA\" option must be not empty"));
                        }
                        result.schemas.push(name);
                    }
                }
                b"secret_storage_table" => {
                    result.secret_table = value.to_varchar()?;
                    result.explicit_secret_table = true;
                }
                b"secret" => {
                    result.secret = value.to_varchar()?;
                }
                _ => {
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "Unrecognized option for Postgres attach: {name}"
                    )));
                }
            }
        }
        Ok(result)
    }
}

fn identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

struct AttachData {
    instance: std::sync::Weak<crate::database::DatabaseInstance>,
    client: Arc<FunctionClient>,
    dsn: String,
    source_schema: String,
    sink_schema: String,
    overwrite: bool,
    filter_pushdown: bool,
}
impl std::fmt::Debug for AttachData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresAttach")
            .field("source_schema", &self.source_schema)
            .field("sink_schema", &self.sink_schema)
            .finish_non_exhaustive()
    }
}
impl FunctionData for AttachData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The legacy function deliberately creates views through a separate connection,
/// as upstream does. This is distinct from ATTACH's remote catalog implementation.
pub(crate) fn attach_function() -> TableFunctionSet {
    let mut function = TableFunction::new("postgres_attach", vec![LogicalType::VARCHAR])
        .with_bind(|input| {
            let context = input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
                .ok_or_else(|| Error::internal("postgres_attach requires a client context"))?;
            if input.inputs[0].is_null() { return Err(Error::invalid_input("Parameters to postgres_attach cannot be NULL")); }
            let named = |name: &str| input.named_parameters.iter().find(|(n, _)| n.as_bytes().eq_ignore_ascii_case(name.as_bytes())).map(|(_, v)| v);
            let text = |name: &str, default: &str| named(name).map_or_else(|| Ok(default.to_owned()), Value::to_varchar);
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN], names: vec!["Success".into()],
                bind_data: Some(Arc::new(AttachData {
                    instance: crate::database::instance_state(context)?.instance(),
                    client: Arc::clone(&context.function_client), dsn: input.inputs[0].to_varchar()?,
                    source_schema: text("source_schema", "public")?, sink_schema: text("sink_schema", "main")?,
                    overwrite: named("overwrite").and_then(Value::as_bool).unwrap_or(false),
                    filter_pushdown: named("filter_pushdown").and_then(Value::as_bool).unwrap_or(false),
                })),
            })
        })
        .with_scan(|bind_data, _, state, output| {
            *output = DataChunk::new(vec![Vector::flat(LogicalType::BOOLEAN, 0)], 0);
            if state.position != 0 { return Ok(false); }
            let data = bind_data.and_then(|d| d.as_any().downcast_ref::<AttachData>()).ok_or_else(|| Error::internal("postgres_attach without bind data"))?;
            let driver = Driver::new()?;
            let session = driver.connect(&Parameters::parse(&data.dsn)?)?;
            let mut tables = session.stream_interruptible(
                "SELECT relname::text FROM pg_catalog.pg_class JOIN pg_catalog.pg_namespace ON pg_class.relnamespace=pg_namespace.oid JOIN pg_catalog.pg_attribute ON pg_class.oid=pg_attribute.attrelid WHERE relkind='r' AND attnum>0 AND nspname=$1::text GROUP BY relname ORDER BY relname",
                &[BinaryParameter(Some(data.source_schema.as_bytes().to_vec()))], data.client.interrupt_flag().as_deref())?;
            let database = crate::Database::from_instance(&data.instance).ok_or_else(|| Error::internal("postgres_attach after database shutdown"))?;
            let mut connection = database.connect()?;
            loop {
                let rows = tables.next_batch_interruptible(STANDARD_VECTOR_SIZE, data.client.interrupt_flag().as_deref())?;
                if rows.is_empty() { break; }
                for row in rows {
                    let table = row[0].as_deref().and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(|| Error::internal("Invalid Postgres table name"))?;
                    let sink = if data.sink_schema.is_empty() { identifier(table) } else { format!("{}.{}", identifier(&data.sink_schema), identifier(table)) };
                    let create = if data.overwrite { "CREATE OR REPLACE VIEW" } else { "CREATE VIEW IF NOT EXISTS" };
                    let scan = if data.filter_pushdown { "postgres_scan_pushdown" } else { "postgres_scan" };
                    let sql = format!("{create} {sink} AS SELECT * FROM {scan}({},{},{})", literal(&data.dsn), literal(&data.source_schema), literal(table));
                    // The pinned AttachFunction discards each dconn.Query result,
                    // including creation errors; preserve that legacy behavior.
                    let _ = connection.query(&sql);
                }
            }
            state.position = 1;
            Ok(false)
        });
    function.named_parameters = vec![
        ("overwrite".into(), LogicalType::BOOLEAN),
        ("filter_pushdown".into(), LogicalType::BOOLEAN),
        ("source_schema".into(), LogicalType::VARCHAR),
        ("sink_schema".into(), LogicalType::VARCHAR),
        ("suffix".into(), LogicalType::VARCHAR),
    ];
    // `suffix` is accepted but unused by the pinned implementation.
    let mut set = TableFunctionSet::new("postgres_attach");
    set.add(function);
    set
}

pub(crate) fn options() -> Vec<ExtensionOption> {
    let mut options: Vec<ExtensionOption> = [
        ("pg_experimental_filter_pushdown", "Whether or not to use filter pushdown", true),
        ("pg_use_text_protocol", "Use text protocol instead of binary protocol", false),
        ("pg_use_binary_copy", "Whether or not to use BINARY copy to read data", true),
        ("pg_array_as_varchar", "Read Postgres arrays as varchar - enables reading mixed dimensional arrays", false),
        ("pg_numeric_as_varchar", "Read Postgres numerics without precision and scale or precision > 38 as varchar instead of double", false),
        ("pg_numeric_nan_as_null", "Read Postgres numeric NaN value as NULL instead of throwing an error", true),
    ].into_iter().map(|(name, description, default)| ExtensionOption {
        name: name.into(), description: description.into(), input_type: LogicalType::BOOLEAN,
        default_value: Value::boolean(default), set_function: None, global_by_default: false,
    }).collect();
    options.push(ExtensionOption {
        name: "pg_null_byte_replacement".into(),
        description: "When writing NULL bytes to Postgres, replace them with the given character".into(),
        input_type: LogicalType::VARCHAR,
        default_value: Value::null(LogicalType::SQLNULL),
        global_by_default: false,
        set_function: Some(Arc::new(|value| {
            if value.as_bytes().is_some_and(|bytes| bytes.contains(&0)) {
                return Err(Error::binder(
                    "NULL byte replacement string cannot contain NULL values",
                ));
            }
            Ok(())
        })),
    });
    for (name, ty, value) in [
        ("pg_pages_per_task", LogicalType::UBIGINT, Value::ubigint(1000)),
        ("pg_use_ctid_scan", LogicalType::BOOLEAN, Value::boolean(true)),
        (
            "pg_statement_timeout_millis",
            LogicalType::UINTEGER,
            Value::null(LogicalType::SQLNULL),
        ),
        (
            "pg_idle_in_transaction_timeout_millis",
            LogicalType::UINTEGER,
            Value::null(LogicalType::SQLNULL),
        ),
    ] {
        options.push(ExtensionOption {
            name: name.into(),
            description: format!("PostgreSQL scan option {name}"),
            input_type: ty,
            default_value: value,
            set_function: None,
            global_by_default: false,
        });
    }
    for (name, description, ty, value, global) in [
        (
            "pg_use_information_schema_introspection",
            "Use SQL-standard information_schema views for ATTACH-time schema discovery instead of pg_catalog.For compatibility with pg protocol compatible databases like spanner, cockroach, redshift, ...",
            LogicalType::BOOLEAN,
            Value::boolean(false),
            false,
        ),
        (
            "pg_staleness_query_enabled",
            "Whether or not the table catalog cache checks Postgres for external DDL changes before serving a cache hit. Defaults to the opposite of pg_use_information_schema_introspection when not explicitly set (off for pg protocol compatible databases that may not support the underlying query, on for Postgres).",
            LogicalType::BOOLEAN,
            Value::boolean(false),
            true,
        ),
        (
            "pg_staleness_query",
            "Custom query used in place of the default table staleness query when pg_staleness_query_enabled resolves to true. Must contain a ${SCHEMA} placeholder and return at least 3 columns (identity, name, revision marker). Empty (default) uses the built-in pg_class/xmin query.",
            LogicalType::VARCHAR,
            Value::null(LogicalType::SQLNULL),
            true,
        ),
    ] {
        options.push(ExtensionOption {
            name: name.into(),
            description: description.into(),
            input_type: ty,
            default_value: value,
            set_function: None,
            global_by_default: global,
        });
    }
    options.extend(pool::options());
    options
}

fn type_options(client: &FunctionClient) -> Options {
    let setting = |name: &str, default: bool| {
        client
            .settings()
            .and_then(|s| s.current_setting(name).ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };
    Options {
        array_as_varchar: setting("pg_array_as_varchar", false),
        numeric_as_varchar: setting("pg_numeric_as_varchar", false),
        // Legacy PostgresBind does not assign bind_data.type_config; the parser
        // therefore retains numeric_nan_as_null=true even after SET. Attached
        // tables and postgres_query use their explicit codec options instead.
        numeric_nan_as_null: true,
        resolve_custom_types: false,
    }
}

pub(crate) fn scan_functions() -> Vec<TableFunctionSet> {
    legacy::functions()
}

fn use_text_protocol(client: &FunctionClient) -> bool {
    client
        .settings()
        .and_then(|s| s.current_setting("pg_use_text_protocol").ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
