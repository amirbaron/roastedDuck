//! The logging table functions (DuckDB's `duckdb_logs`, `duckdb_log_contexts`,
//! `enable_logging`, `disable_logging` and `truncate_duckdb_logs`).

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::logging::{ClientLogger, LogLevel, LogScope};
use roastedduck_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionReplacement,
    TableFunctionSet,
};
use roastedduck_planner::ClientContext;
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

use crate::logging::{ConnectionLogger, FILE_STORAGE, LogConfig, LogManager, LogMode, LogTable};

/// The log manager of the binding connection.
fn log_manager(input: &TableFunctionBindInput<'_>) -> Result<(Arc<LogManager>, Arc<dyn ClientLogger>)> {
    let context = input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("logging function without a client context"))?;
    let logger = context
        .function_client
        .logger()
        .ok_or_else(|| Error::internal("connection without loggers"))?;
    let manager = logger
        .as_any()
        .downcast_ref::<ConnectionLogger>()
        .map(|logger| Arc::clone(&logger.manager))
        .ok_or_else(|| Error::internal("loggers of another database"))?;
    Ok((manager, logger))
}

/// Rows computed when bound.
#[derive(Debug)]
struct LogRows {
    types: Vec<LogicalType>,
    rows: Vec<Vec<Value>>,
}

impl FunctionData for LogRows {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn rows_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut roastedduck_function::TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<LogRows>())
        .ok_or_else(|| Error::internal("log function without rows"))?;
    let start = usize::try_from(state.position)
        .unwrap_or(usize::MAX)
        .min(data.rows.len());
    let end = (start + STANDARD_VECTOR_SIZE).min(data.rows.len());
    let mut columns: Vec<Vector> = data
        .types
        .iter()
        .map(|ty| Vector::flat(ty.clone(), (end - start).max(1)))
        .collect();
    for (row_index, row) in data.rows[start..end].iter().enumerate() {
        for (column, value) in columns.iter_mut().zip(row) {
            column.set_value(row_index, value)?;
        }
    }
    *chunk = DataChunk::new(columns, end - start);
    state.position = end as u64;
    Ok(end < data.rows.len())
}

/// `duckdb_logs([denormalized_table := ...])`: the log entries, or joined with their
/// contexts (`DuckDBLogBindReplace`).
fn duckdb_logs() -> TableFunctionSet {
    let mut function = TableFunction::new("duckdb_logs", Vec::new())
        .with_bind(|input| {
            let (manager, _) = log_manager(input)?;
            let types = vec![
                LogicalType::UBIGINT,
                LogicalType::TIMESTAMP_TZ,
                LogicalType::VARCHAR,
                LogicalType::VARCHAR,
                LogicalType::VARCHAR,
            ];
            let rows = manager
                .entries()
                .iter()
                .map(crate::logging::LogEntry::row)
                .collect();
            Ok(TableFunctionBindOutput {
                return_types: types.clone(),
                names: ["context_id", "timestamp", "type", "log_level", "message"]
                    .map(roastedduck_common::ByteString::from)
                    .to_vec(),
                bind_data: Some(Arc::new(LogRows { types, rows })),
            })
        })
        .with_scan(rows_scan);
    function.named_parameters = vec![("denormalized_table".to_owned(), LogicalType::BOOLEAN)];
    function.bind_replace = Some(Arc::new(|input: &TableFunctionBindInput<'_>| {
        let (manager, _) = log_manager(input)?;
        let replacement = |query: String| TableFunctionReplacement {
            query,
            error: String::new(),
        };
        let denormalized = match input
            .named_parameters
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("denormalized_table"))
        {
            Some((_, value)) if value.is_null() => {
                return Err(Error::invalid_input("denormalized_table cannot be NULL"));
            }
            Some((_, value)) => value.as_bool() == Some(true),
            None => false,
        };
        // Without join contexts the entries are scanned (from the storage's files, if any).
        if !denormalized {
            return Ok(manager.bind_replace(LogTable::LogEntries)?.map(replacement));
        }
        if let Some(query) = manager.bind_replace(LogTable::AllLogs)? {
            return Ok(Some(replacement(query)));
        }
        Ok(Some(TableFunctionReplacement {
            query: "SELECT l.context_id, scope, connection_id, transaction_id, query_id, thread_id, timestamp, type, \
                    log_level, message FROM (SELECT row_number() OVER () AS rowid, * FROM duckdb_logs()) as l JOIN \
                    duckdb_log_contexts() as c ON l.context_id=c.context_id order by timestamp, l.rowid;"
                .to_owned(),
            error: String::new(),
        }))
    }));
    let mut set = TableFunctionSet::new("duckdb_logs");
    set.add(function);
    set
}

/// `duckdb_log_contexts()`.
fn duckdb_log_contexts() -> TableFunctionSet {
    let mut function = TableFunction::new("duckdb_log_contexts", Vec::new())
        .with_bind(|input| {
            let (manager, _) = log_manager(input)?;
            let types = vec![
                LogicalType::UBIGINT,
                LogicalType::VARCHAR,
                LogicalType::UBIGINT,
                LogicalType::UBIGINT,
                LogicalType::UBIGINT,
                LogicalType::UBIGINT,
            ];
            let rows = manager
                .contexts()
                .iter()
                .map(crate::logging::RegisteredContext::row)
                .collect();
            Ok(TableFunctionBindOutput {
                return_types: types.clone(),
                names: [
                    "context_id",
                    "scope",
                    "connection_id",
                    "transaction_id",
                    "query_id",
                    "thread_id",
                ]
                .map(roastedduck_common::ByteString::from)
                .to_vec(),
                bind_data: Some(Arc::new(LogRows { types, rows })),
            })
        })
        .with_scan(rows_scan);
    function.bind_replace = Some(Arc::new(|input: &TableFunctionBindInput<'_>| {
        let (manager, _) = log_manager(input)?;
        Ok(manager
            .bind_replace(LogTable::LogContexts)?
            .map(|query| TableFunctionReplacement {
                query,
                error: String::new(),
            }))
    }));
    let mut set = TableFunctionSet::new("duckdb_log_contexts");
    set.add(function);
    set
}

/// What `enable_logging`, `disable_logging` or `truncate_duckdb_logs` do when run.
#[derive(Debug)]
enum LogAction {
    Enable {
        config: LogConfig,
        log_types: Vec<String>,
        storage_config: Vec<(roastedduck_common::ByteString, Value)>,
    },
    Disable,
    Truncate,
}

#[derive(Debug)]
struct LogActionData {
    manager: Arc<LogManager>,
    logger: Arc<dyn ClientLogger>,
    action: LogAction,
}

impl FunctionData for LogActionData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `EnableLogging` / `DisableLogging` / `TruncateLogs`: the action runs once per execution
/// and returns no rows.
fn run_action(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut roastedduck_function::TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<LogActionData>())
        .ok_or_else(|| Error::internal("logging function without bind data"))?;
    *chunk = DataChunk::new(vec![Vector::flat(LogicalType::BOOLEAN, 1)], 0);
    if state.position > 0 {
        return Ok(false);
    }
    state.position = 1;
    match &data.action {
        LogAction::Enable {
            config,
            log_types,
            storage_config,
        } => {
            let warning = "The logging settings have been changed so you may lose warnings printed in the CLI.\nTo \
                           continue printing warnings to the console, set storage='shell_log_storage'.\nFor more \
                           info see https://duckdb.org/docs/stable/operations_manual/logging/overview.";
            if data
                .logger
                .should_log(LogScope::Connection, "", LogLevel::Warning)
            {
                data.logger
                    .write_log(LogScope::Connection, "", LogLevel::Warning, warning)?;
            }
            data.manager.set_config(config.clone())?;
            if log_types.is_empty() {
                data.manager.set_enabled(true);
                data.manager.set_mode(LogMode::LevelOnly);
            } else {
                data.manager.set_enable_structured_loggers(log_types)?;
            }
            if !storage_config.is_empty() {
                data.manager.update_storage_config(storage_config)?;
            }
        }
        LogAction::Disable => data.manager.set_enabled(false),
        LogAction::Truncate => data.manager.truncate()?,
    }
    Ok(false)
}

fn success_output(bind_data: LogActionData) -> TableFunctionBindOutput {
    TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned().into()],
        bind_data: Some(Arc::new(bind_data)),
    }
}

/// `BindEnableLogging`.
fn bind_enable_logging(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    if input.inputs.len() > 1 {
        return Err(Error::invalid_input("EnableLogging: expected 0 or 1 parameter"));
    }
    let (manager, logger) = log_manager(input)?;
    let mut config = LogConfig::default();
    let mut storage_config: Vec<(roastedduck_common::ByteString, Value)> = Vec::new();
    let mut set_config = |key: &[u8], value: &Value| match storage_config
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
    {
        Some(entry) => entry.1 = value.clone(),
        None => storage_config.push((key.into(), value.clone())),
    };
    let mut storage_set = false;
    for (name, value) in input.named_parameters {
        match name.to_ascii_lowercase().as_bytes() {
            b"level" => config.level = LogLevel::parse(&value.to_varchar()?)?,
            b"storage" => {
                storage_set = true;
                config.storage = value.to_varchar()?;
            }
            b"storage_config" => {
                if value.logical_type().id() != roastedduck_types::LogicalTypeId::Struct {
                    return Err(Error::invalid_input(
                        "EnableLogging: storage_config must be a struct",
                    ));
                }
                let names = value.logical_type().struct_children().unwrap_or_default();
                for ((field, _), child) in names.iter().zip(value.children().unwrap_or_default()) {
                    set_config(field, child);
                }
            }
            b"storage_path" => set_config(b"path", value),
            b"storage_normalize" => set_config(b"normalize", value),
            b"storage_buffer_size" => set_config(b"buffer_size", value),
            _ => {
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "EnableLogging: unknown named parameter: {name}"
                )));
            }
        }
    }
    let has_path = storage_config
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("path"));
    if !storage_set {
        config.storage = if has_path {
            FILE_STORAGE.to_owned()
        } else {
            manager.config().storage
        };
    }
    if config.storage.eq_ignore_ascii_case(FILE_STORAGE)
        && !manager.config().storage.eq_ignore_ascii_case(FILE_STORAGE)
    {
        let usable = storage_config
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("path"))
            .is_some_and(|(_, path)| !path.is_null() && !path.to_varchar().unwrap_or_default().is_empty());
        if !usable {
            return Err(Error::invalid_input(
                "Cannot enable 'file' log storage without a valid path. Provide one via storage_path, e.g. CALL \
                 enable_logging(storage='file', storage_path='mylog.csv');",
            ));
        }
    }
    let mut log_types = Vec::new();
    if let Some(types) = input.inputs.first() {
        match types.logical_type().id() {
            roastedduck_types::LogicalTypeId::Varchar => log_types.push(types.to_varchar()?),
            roastedduck_types::LogicalTypeId::List
                if types.logical_type().child_type() == Some(&LogicalType::VARCHAR) =>
            {
                for child in types.children().unwrap_or_default() {
                    log_types.push(child.to_varchar()?);
                }
            }
            _ => {
                return Err(Error::binder(
                    "Unexpected type positional parameter to enable_logging",
                ));
            }
        }
    }
    Ok(success_output(LogActionData {
        manager,
        logger,
        action: LogAction::Enable {
            config,
            log_types,
            storage_config,
        },
    }))
}

fn simple_action(name: &'static str, action: fn() -> LogAction) -> TableFunctionSet {
    let function = TableFunction::new(name, Vec::new())
        .with_bind(move |input| {
            let (manager, logger) = log_manager(input)?;
            Ok(success_output(LogActionData {
                manager,
                logger,
                action: action(),
            }))
        })
        .with_scan(run_action);
    let mut set = TableFunctionSet::new(name);
    set.add(function);
    set
}

/// `enable_logging`, `disable_logging` and `truncate_duckdb_logs`
/// (`EnableLoggingFun::RegisterFunction`).
fn logging_actions() -> Vec<TableFunctionSet> {
    let mut enable = TableFunction::new("enable_logging", Vec::new())
        .with_bind(bind_enable_logging)
        .with_scan(run_action);
    enable.named_parameters = vec![
        ("level".to_owned(), LogicalType::VARCHAR),
        ("storage".to_owned(), LogicalType::VARCHAR),
        ("storage_config".to_owned(), LogicalType::ANY),
        ("storage_path".to_owned(), LogicalType::VARCHAR),
        ("storage_normalize".to_owned(), LogicalType::BOOLEAN),
        ("storage_buffer_size".to_owned(), LogicalType::UBIGINT),
    ];
    enable.varargs = Some(LogicalType::ANY);
    let mut enable_set = TableFunctionSet::new("enable_logging");
    enable_set.add(enable);
    vec![
        enable_set,
        simple_action("disable_logging", || LogAction::Disable),
        simple_action("truncate_duckdb_logs", || LogAction::Truncate),
    ]
}

/// Every logging table function.
pub(crate) fn log_table_functions() -> Vec<TableFunctionSet> {
    let mut sets = vec![duckdb_logs(), duckdb_log_contexts()];
    sets.extend(logging_actions());
    sets
}
