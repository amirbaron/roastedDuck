//! The shell's extension and log storage (DuckDB's `tools/shell/shell_extension.cpp` and
//! `ShellLogStorage` of `shell_renderer.cpp`): `getenv`, the `_` replacement scan of the
//! last result, `duckdb_api = 'cli'`, and warnings printed as they are logged.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb::{Connection, Database, LogLevel, LogStorage, LogicalType, Result};
use rustdb_ast::{ColumnDataRef, TableRef, TableRefKind};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::executor::{Str, StrOut, unary};
use rustdb_planner::{ReplacementScan, ReplacementScanInput};
use rustdb_types::ColumnDataCollection;

use crate::highlight::{HighlightElementType, ShellHighlight};
use crate::output::{Output, PrintOutput};
use crate::state::LastResult;

/// `DUCKDB_API_CLI`, the `duckdb_api` of databases the shell opens (set by
/// `ShellExtension::Load`).
pub const DUCKDB_API_CLI: &str = "cli";

/// `ShellExtension::Load`: registers `getenv` and adds the replacement scan of `_`; the
/// extension then shows as loaded.
pub fn load_shell_extension(db: &Database, conn: &mut Connection, last_result: &LastResult) -> Result<()> {
    let getenv = ScalarFunction::new(
        "getenv",
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        |args, _state, result| {
            // `GetEnvFunction`: an unset variable is the empty string.
            unary::<Str, StrOut>(args, result, |name, out| {
                if let Some(value) = std::env::var_os(String::from_utf8_lossy(name).as_ref()) {
                    out.extend_from_slice(value.to_string_lossy().as_bytes());
                }
                Ok(true)
            })
        },
    )
    .with_bind(|_function, input| {
        // `GetEnvBind`.
        let external_access = input
            .client
            .settings()
            .and_then(|settings| settings.current_setting("enable_external_access").ok())
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        if !external_access {
            return Err(rustdb::Error::new(
                rustdb::ErrorKind::Permission,
                "getenv is disabled through configuration",
            ));
        }
        Ok(None)
    });
    let mut getenv_set = ScalarFunctionSet::new("getenv");
    getenv_set.add(getenv);
    conn.create_scalar_function_set(getenv_set)?;
    let last_result = Arc::clone(last_result);
    db.add_replacement_scan(ReplacementScan(Arc::new(
        move |input: &ReplacementScanInput<'_>| shell_scan_last_result(input, &last_result),
    )));
    db.set_extension_loaded("shell", "Adds CLI-specific support and functionalities");
    Ok(())
}

/// `ShellScanLastResult`: `FROM _` reads the result of the last query.
fn shell_scan_last_result(
    input: &ReplacementScanInput<'_>,
    last_result: &LastResult,
) -> Result<Option<TableRef>> {
    if input.table_name != "_" {
        return Ok(None);
    }
    let Some(scanned) = last_result.lock().unwrap_or_else(PoisonError::into_inner).clone() else {
        return Err(rustdb::Error::binder(
            "Failed to query last result \"_\": no result available",
        ));
    };
    let result = &scanned.result;
    let collection = ColumnDataCollection::new(result.types().to_vec(), result.collect_chunks()?);
    Ok(Some(rustdb_parser::build::table_ref(TableRefKind::ColumnData(
        ColumnDataRef {
            expected_names: result.names().to_vec(),
            collection: Some(Box::new(collection)),
        },
    ))))
}

/// `ShellLogStorage`: prints each log entry once, to the shell's output.
#[derive(Debug)]
struct ShellLogStorage {
    out: Output,
    /// The messages printed so far (`printed_logs`, compared without case).
    printed_logs: Mutex<HashSet<String>>,
}

impl LogStorage for ShellLogStorage {
    fn write_log_entry(&self, _timestamp: i64, level: LogLevel, _log_type: &str, message: &str) {
        let element_type = match level {
            LogLevel::Trace => HighlightElementType::LogTrace,
            LogLevel::Debug => HighlightElementType::LogDebug,
            LogLevel::Info => HighlightElementType::LogInfo,
            LogLevel::Warning => HighlightElementType::LogWarning,
            LogLevel::Error | LogLevel::Fatal => HighlightElementType::ErrorToken,
        };
        let mut printed_logs = self.printed_logs.lock().unwrap_or_else(PoisonError::into_inner);
        if !printed_logs.insert(message.to_lowercase()) {
            return;
        }
        let highlight = ShellHighlight::new(&self.out);
        highlight.print_text(&format!("{}:\n", level.name()), PrintOutput::Stdout, element_type);
        highlight.print_text(&format!("{message}\n\n"), PrintOutput::Stdout, element_type);
    }
}

/// `RegisterShellLogger`: warnings and above go to the shell's output.
pub fn register_shell_logger(db: &Database, out: &Output) -> Result<()> {
    let storage = Arc::new(ShellLogStorage {
        out: out.clone(),
        printed_logs: Mutex::default(),
    });
    db.register_log_storage("shell_log_storage", storage);
    db.set_log_storage("shell_log_storage")?;
    db.set_enable_logging(true);
    db.set_log_level(LogLevel::Warning);
    Ok(())
}
