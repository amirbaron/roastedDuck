//! The Rust engine (`rustdb`).
//!
//! Where `rustdb` does not expose an equivalent of a DuckDB runner hook yet, this backend does the
//! closest thing it can and the gap is noted on the method.

use rustdb::{
    AccessMode, Config, Connection, Database, LogicalType, LogicalTypeId, MaterializedResult, Value,
};

use super::{ColumnClass, Engine, ExtensionLoad, QueryResult, RequireSetting};
use crate::compare::numeric;

/// Default `default_block_size` (DuckDB's `DEFAULT_BLOCK_ALLOC_SIZE`).
const DEFAULT_BLOCK_SIZE: u64 = 262_144;

/// The `rustdb` engine.
#[derive(Debug)]
pub struct RustEngine {
    build_directory: String,
}

impl RustEngine {
    /// `build_directory` is what `__BUILD_DIRECTORY__` expands to in test files.
    pub fn new(build_directory: impl Into<String>) -> Self {
        Self {
            build_directory: build_directory.into(),
        }
    }
}

/// A `rustdb` query outcome.
#[derive(Debug)]
pub struct RustResult {
    outcome: Result<MaterializedResult, String>,
}

impl RustResult {
    fn result(&self) -> Option<&MaterializedResult> {
        self.outcome.as_ref().ok()
    }

    fn logical_type(&self, column: usize) -> Option<&LogicalType> {
        self.result().and_then(|result| result.types().get(column))
    }
}

impl QueryResult for RustResult {
    fn error(&self) -> Option<&str> {
        self.outcome.as_ref().err().map(String::as_str)
    }

    fn column_count(&self) -> usize {
        self.result().map_or(0, MaterializedResult::column_count)
    }

    fn row_count(&self) -> usize {
        self.result().map_or(0, MaterializedResult::row_count)
    }

    fn column_name(&self, column: usize) -> &str {
        self.result()
            .and_then(|result| result.names().get(column))
            .map_or("", String::as_str)
    }

    fn column_type(&self, column: usize) -> String {
        self.logical_type(column)
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    fn column_class(&self, column: usize) -> ColumnClass {
        self.logical_type(column)
            .map_or(ColumnClass::Other, |ty| match ty.id() {
                LogicalTypeId::Boolean => ColumnClass::Boolean,
                _ if is_numeric(ty.id()) => ColumnClass::Numeric,
                _ => ColumnClass::Other,
            })
    }
}

/// `LogicalType::IsNumeric`.
fn is_numeric(id: LogicalTypeId) -> bool {
    use LogicalTypeId as L;
    matches!(
        id,
        L::TinyInt
            | L::SmallInt
            | L::Integer
            | L::BigInt
            | L::HugeInt
            | L::Float
            | L::Double
            | L::Decimal
            | L::UTinyInt
            | L::USmallInt
            | L::UInteger
            | L::UBigInt
            | L::UHugeInt
    )
}

/// Whether `ty` is or contains the JSON type (the only type with extension casts).
fn contains_json(ty: &LogicalType) -> bool {
    match ty.id() {
        LogicalTypeId::Varchar => ty.alias() == Some("JSON"),
        LogicalTypeId::List => ty.child_type().is_some_and(contains_json),
        LogicalTypeId::Array => ty.array_child().is_some_and(|(child, _)| contains_json(child)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| contains_json(key) || contains_json(value)),
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| contains_json(child))),
        LogicalTypeId::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| contains_json(member))),
        _ => false,
    }
}

/// `TestResultHelper::SQLLogicTestConvertValue`.
fn convert_value(value: &Value, original_sqlite_test: bool) -> Result<String, String> {
    if value.is_null() {
        return Ok("NULL".to_owned());
    }
    let ty = value.logical_type();
    if original_sqlite_test {
        match ty.id() {
            LogicalTypeId::Float | LogicalTypeId::Double => {
                let float = value.as_f64().ok_or("FLOAT value without payload")?;
                return numeric::float_to_bigint(float).map(|v| v.to_string());
            }
            LogicalTypeId::Decimal => {
                let (_, scale) = ty.decimal_properties().ok_or("DECIMAL type without properties")?;
                let unscaled = value.as_i128().ok_or("DECIMAL value without payload")?;
                return numeric::decimal_to_bigint(unscaled, scale).map(|v| v.to_string());
            }
            _ => {}
        }
    }
    if ty.id() == LogicalTypeId::Boolean {
        return Ok(if value.as_bool() == Some(true) { "1" } else { "0" }.to_owned());
    }
    let text = value.to_varchar().map_err(|error| error.to_string())?;
    Ok(if text.is_empty() {
        "(empty)".to_owned()
    } else {
        text.replace('\0', "\\0")
    })
}

fn option<'a>(config: &'a Config, name: &str) -> Option<&'a Value> {
    config
        .options
        .iter()
        .rev()
        .find(|(option, _)| option.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

fn option_u64(config: &Config, name: &str) -> Option<u64> {
    let value = option(config, name)?;
    if let Some(flag) = value.as_bool() {
        return Some(u64::from(flag));
    }
    if let Some(number) = value.as_i128() {
        return u64::try_from(number).ok();
    }
    let text = value.to_varchar().ok()?;
    match text.to_ascii_lowercase().as_str() {
        "true" => Some(1),
        "false" => Some(0),
        other => other.parse().ok(),
    }
}

impl Engine for RustEngine {
    type Config = Config;
    type Database = Database;
    type Connection = Connection;
    type Result = RustResult;
    type Session = ();

    fn name(&self) -> &'static str {
        "rust"
    }

    fn build_directory(&self) -> &str {
        &self.build_directory
    }

    /// `GetTestConfig()` expressed as settings. Gap: `rustdb::Config` has no fields yet for the
    /// non-setting options `checkpoint_on_shutdown`, `abort_on_wal_failure` and
    /// `debug_initialize`.
    ///
    /// `SLT_THREADS=<n>` runs every database with `threads = n` (to check that results do
    /// not depend on the number of threads).
    fn test_config(&self) -> Result<Config, String> {
        let mut config = Config {
            load_extensions: false,
            ..Config::default()
        }
        .with_option("checkpoint_threshold", Value::varchar("0b"))
        .with_option("allow_unsigned_extensions", Value::boolean(true))
        .with_option("default_block_size", Value::ubigint(DEFAULT_BLOCK_SIZE))
        .with_option("debug_verify_vector", Value::varchar("NONE"));
        if let Ok(threads) = std::env::var("SLT_THREADS") {
            let threads: u64 = threads
                .parse()
                .map_err(|_| format!("SLT_THREADS must be a number, got {threads:?}"))?;
            config = config.with_option("threads", Value::ubigint(threads));
        }
        Ok(config)
    }

    fn set_option(&self, config: &mut Config, name: &str, value: &str) -> Result<(), String> {
        config.options.push((name.to_owned(), Value::varchar(value)));
        Ok(())
    }

    /// Gap: `use_temporary_directory` has no `rustdb::Config` equivalent yet.
    fn set_read_only(&self, config: &mut Config, read_only: bool) {
        config.access_mode = if read_only {
            AccessMode::ReadOnly
        } else {
            AccessMode::Automatic
        };
    }

    fn set_storage_version(&self, config: &mut Config, version: &str) -> Result<(), String> {
        config.options.push((
            "storage_compatibility_version".to_owned(),
            Value::varchar(version),
        ));
        Ok(())
    }

    /// The storage version the database uses now, set at open or by a global `SET`
    /// (`runner.db->instance->config.options.serialization_compatibility`).
    fn inherit_storage_version(&self, config: &mut Config, database: &Database) {
        let current = database
            .global_setting("storage_compatibility_version")
            .or_else(|| option(database.config(), "storage_compatibility_version").cloned());
        if let Some(version) = current {
            config
                .options
                .push(("storage_compatibility_version".to_owned(), version));
        }
    }

    fn setting(&self, config: &Config, setting: RequireSetting) -> u64 {
        let (name, default) = match setting {
            RequireSetting::DefaultBlockSize => ("default_block_size", DEFAULT_BLOCK_SIZE),
            RequireSetting::AutoloadKnownExtensions => ("autoload_known_extensions", 0),
            RequireSetting::AllowUnsignedExtensions => ("allow_unsigned_extensions", 0),
            RequireSetting::VacuumRebuildIndexes => ("vacuum_rebuild_indexes", 0),
        };
        option_u64(config, name).unwrap_or(default)
    }

    fn open(&self, path: &str, config: &Config) -> Result<Database, String> {
        Database::open(path, config).map_err(|error| error.to_string())
    }

    fn load_extension(&self, database: &Database, name: &str) -> Result<ExtensionLoad, String> {
        Ok(
            match database.load_extension(name).map_err(|error| error.to_string())? {
                rustdb::ExtensionLoad::Loaded => ExtensionLoad::Loaded,
                rustdb::ExtensionLoad::NotAvailable => ExtensionLoad::NotLoaded,
                rustdb::ExtensionLoad::Unknown => ExtensionLoad::Unknown,
            },
        )
    }

    fn connect(&self, database: &Database) -> Result<Connection, String> {
        database.connect().map_err(|error| error.to_string())
    }

    /// Gap: `rustdb` has no client configuration (progress bar) yet.
    fn configure_test_client(&self, _connection: &mut Connection) {}

    fn query(&self, connection: &mut Connection, sql: &str) -> RustResult {
        RustResult {
            outcome: connection.query(sql).map_err(|error| error.to_string()),
        }
    }

    fn render(
        &self,
        context: &Connection,
        result: &mut RustResult,
        original_sqlite_test: bool,
    ) -> Result<Vec<String>, String> {
        let Some(result) = result.result() else {
            return Ok(Vec::new());
        };
        let mut values = Vec::with_capacity(result.row_count() * result.column_count());
        for row in 0..result.row_count() {
            for column in 0..result.column_count() {
                let value = result.value(column, row);
                // Like DuckDB's runner (`Value::CastAs(context, VARCHAR)`): types with
                // extension casts (JSON, and TIMESTAMPTZ with ICU) are rendered through
                // the connection's casts.
                let value = if (contains_json(value.logical_type())
                    || context.has_extension_cast(value.logical_type()))
                    && !value.is_null()
                {
                    context
                        .cast_value(&value, &LogicalType::VARCHAR)
                        .map_err(|error| error.to_string())?
                } else {
                    value
                };
                values.push(convert_value(&value, original_sqlite_test)?);
            }
        }
        Ok(values)
    }

    fn values_equal(
        &self,
        _context: &Connection,
        result: &RustResult,
        column: usize,
        actual: &str,
        expected: &str,
    ) -> bool {
        result
            .logical_type(column)
            .is_some_and(|ty| numeric::values_equal(ty, actual, expected))
    }

    fn load_result_file(
        &self,
        _context: &Connection,
        path: &str,
        names: &[String],
    ) -> Result<(Vec<String>, usize), String> {
        crate::result_file::load(path, names.len()).map(|values| (values, names.len()))
    }

    /// Carries the options set on the main connection over to the restarted database.
    /// Gap: the search path is not carried over yet.
    fn save_session(&self, main: &Connection, config: &mut Config) {
        for (name, value) in main.assigned_settings() {
            config
                .options
                .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
            config.options.push((name, value));
        }
    }

    fn restore_session(&self, _main: &mut Connection, (): ()) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_values_like_the_runner() {
        assert_eq!(convert_value(&Value::boolean(true), false).unwrap(), "1");
        assert_eq!(convert_value(&Value::varchar(""), false).unwrap(), "(empty)");
        assert_eq!(convert_value(&Value::varchar("a\0b"), false).unwrap(), "a\\0b");
        assert_eq!(
            convert_value(&Value::null(LogicalType::INTEGER), false).unwrap(),
            "NULL"
        );
        assert_eq!(convert_value(&Value::double(2.5), true).unwrap(), "2");
        assert_eq!(convert_value(&Value::decimal(250, 18, 2), true).unwrap(), "3");
        assert_eq!(convert_value(&Value::integer(7), true).unwrap(), "7");
    }

    #[test]
    fn query_errors_are_reported_in_the_result() {
        let engine = RustEngine::new("build");
        let config = engine.test_config().unwrap();
        let db = engine.open("", &config).unwrap();
        let mut con = engine.connect(&db).unwrap();
        assert!(engine.query(&mut con, "SELECT 42").error().is_none());
        let result = engine.query(&mut con, "SELEC 42");
        assert!(
            result
                .error()
                .is_some_and(|error| error.starts_with("Parser Error: syntax error at or near \"SELEC\""))
        );
        assert_eq!(
            engine.setting(&config, RequireSetting::DefaultBlockSize),
            DEFAULT_BLOCK_SIZE
        );
        assert_eq!(
            engine.setting(&config, RequireSetting::AllowUnsignedExtensions),
            1
        );
    }
}
