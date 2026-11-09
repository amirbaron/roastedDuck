//! `store_rejects`: the rows the reader rejected, in temporary tables (DuckDB's
//! `CSVRejectsTable`, `CSVGlobalState::FillRejectsTable` and
//! `CSVErrorHandler::FillRejectsTable`).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::{Error, Result};
use roastedduck_function::{FunctionClient, TempCatalog};
use roastedduck_types::{LogicalType, Value};

use crate::error::CsvErrorType;
use crate::file_scan::CsvFileScan;
use crate::options::FormatType;

/// The rejects tables of one pair of names, kept in the object cache
/// (`CSVRejectsTable`).
#[derive(Debug)]
pub(crate) struct CsvRejectsTable {
    scan_table: String,
    errors_table: String,
    state: Mutex<RejectsState>,
}

#[derive(Debug, Default)]
struct RejectsState {
    /// Rejected rows written for the current file.
    count: u64,
    current_query_id: u64,
    current_file_idx: u64,
}

/// The temporary catalog and object cache of the connection reading.
fn temp_catalog(client: &FunctionClient) -> Result<Arc<dyn TempCatalog>> {
    client
        .temp_catalog()
        .ok_or_else(|| Error::internal("store_rejects without a temporary catalog"))
}

impl CsvRejectsTable {
    /// `CSVRejectsTable::GetOrCreate`.
    pub(crate) fn get_or_create(
        client: &FunctionClient,
        rejects_scan: &str,
        rejects_error: &str,
    ) -> Result<Arc<Self>> {
        if rejects_scan == rejects_error {
            return Err(Error::binder(
                "The names of the rejects scan and rejects error tables can't be the same. Use different names \
                 for these tables.",
            ));
        }
        let key = format!(
            "CSV_REJECTS_TABLE_CACHE_ENTRY_{}_{}",
            rejects_scan.to_uppercase(),
            rejects_error.to_uppercase()
        );
        let cache = client
            .object_cache()
            .ok_or_else(|| Error::internal("store_rejects without an object cache"))?;
        let catalog = temp_catalog(client)?;
        let cached = cache.get::<Self>(&key);
        let scan_exists = catalog.table_exists(rejects_scan)?;
        let error_exists = catalog.table_exists(rejects_error)?;
        if (scan_exists || error_exists) && cached.is_none() {
            let mut message = String::new();
            if scan_exists {
                let _ = write!(
                    message,
                    "Reject Scan Table name \"{rejects_scan}\" is already in use. "
                );
            }
            if error_exists {
                let _ = write!(
                    message,
                    "Reject Error Table name \"{rejects_error}\" is already in use. "
                );
            }
            message.push_str(
                "Either drop the used name(s), or give other name options in the CSV Reader function.\n",
            );
            return Err(Error::binder(message));
        }
        if let Some(cached) = cached {
            return Ok(cached);
        }
        let table = Arc::new(Self {
            scan_table: rejects_scan.to_owned(),
            errors_table: rejects_error.to_owned(),
            state: Mutex::default(),
        });
        cache.put(key, Arc::clone(&table));
        Ok(table)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RejectsState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `CSVRejectsTable::GetCurrentFileIndex`: file indexes restart with every query.
    pub(crate) fn current_file_index(&self, query_id: u64) -> u64 {
        let mut state = self.lock();
        if state.current_query_id != query_id {
            state.current_query_id = query_id;
            state.current_file_idx = 0;
        }
        let index = state.current_file_idx;
        state.current_file_idx += 1;
        index
    }

    /// `CSVRejectsTable::InitializeTable`: creates the `CSV_ERROR_TYPE` enum and both
    /// tables unless they exist.
    pub(crate) fn initialize_table(&self, client: &FunctionClient) -> Result<()> {
        let catalog = temp_catalog(client)?;
        let error_type = LogicalType::enum_type(
            [
                CsvErrorType::CastError,
                CsvErrorType::TooFewColumns,
                CsvErrorType::TooManyColumns,
                CsvErrorType::UnterminatedQuotes,
                CsvErrorType::MaximumLineSize,
                CsvErrorType::InvalidEncoding,
                CsvErrorType::InvalidState,
            ]
            .iter()
            .map(|error| error.reject_name().to_owned())
            .collect(),
        );
        catalog.create_type("CSV_ERROR_TYPE", error_type.clone())?;
        let column = |name: &str, ty: LogicalType| (name.to_owned(), ty);
        catalog.create_table(
            &self.scan_table,
            &[
                column("scan_id", LogicalType::UBIGINT),
                column("file_id", LogicalType::UBIGINT),
                column("file_path", LogicalType::VARCHAR),
                column("delimiter", LogicalType::VARCHAR),
                column("quote", LogicalType::VARCHAR),
                column("escape", LogicalType::VARCHAR),
                column("newline_delimiter", LogicalType::VARCHAR),
                column("skip_rows", LogicalType::UINTEGER),
                column("has_header", LogicalType::BOOLEAN),
                column("columns", LogicalType::VARCHAR),
                column("date_format", LogicalType::VARCHAR),
                column("timestamp_format", LogicalType::VARCHAR),
                column("user_arguments", LogicalType::VARCHAR),
            ],
        )?;
        catalog.create_table(
            &self.errors_table,
            &[
                column("scan_id", LogicalType::UBIGINT),
                column("file_id", LogicalType::UBIGINT),
                column("line", LogicalType::UBIGINT),
                column("line_byte_position", LogicalType::UBIGINT),
                column("byte_position", LogicalType::UBIGINT),
                column("column_idx", LogicalType::UBIGINT),
                column("column_name", LogicalType::VARCHAR),
                column("error_type", error_type),
                column("csv_line", LogicalType::VARCHAR),
                column("error_message", LogicalType::VARCHAR),
            ],
        )?;
        self.lock().count = 0;
        Ok(())
    }

    /// `CSVGlobalState::FillRejectsTable` for a finished file: its accepted errors, up to
    /// `limit` (0: no limit), and a row for the file if it had any.
    pub(crate) fn fill(
        &self,
        client: &FunctionClient,
        file: &CsvFileScan,
        (scan_idx, file_idx): (u64, u64),
        names: &[roastedduck_common::ByteString],
        limit: u64,
    ) -> Result<()> {
        let catalog = temp_catalog(client)?;
        let mut error_rows = Vec::new();
        let count = {
            let mut state = self.lock();
            for (error, line) in file.error_handler.errors_with_lines() {
                if !error.error_type.accepted_reject() {
                    continue;
                }
                if limit != 0 && state.count >= limit {
                    break;
                }
                state.count += 1;
                error_rows.push(error_row(&error, line, (scan_idx, file_idx), names));
            }
            std::mem::take(&mut state.count)
        };
        catalog.append(&self.errors_table, &error_rows)?;
        if count != 0 {
            catalog.append(&self.scan_table, &[scan_row(file, scan_idx, file_idx)?])?;
        }
        Ok(())
    }
}

/// A row of the errors table (`CSVErrorHandler::FillRejectsTable`).
fn error_row(
    error: &crate::error::CsvError,
    line: u64,
    (scan_idx, file_idx): (u64, u64),
    names: &[roastedduck_common::ByteString],
) -> Vec<Value> {
    let column_idx = error.column_idx as usize;
    let name = |index: usize| {
        names.get(index).map_or_else(
            || Value::null(LogicalType::VARCHAR),
            |name| Value::bytes(LogicalType::VARCHAR, name),
        )
    };
    let column_name = match error.error_type {
        CsvErrorType::TooManyColumns | CsvErrorType::MaximumLineSize => Value::null(LogicalType::VARCHAR),
        CsvErrorType::TooFewColumns => name(column_idx + 1),
        _ => name(column_idx),
    };
    vec![
        Value::ubigint(scan_idx),
        Value::ubigint(file_idx),
        Value::ubigint(line),
        Value::ubigint(error.row_byte_position + 1),
        // Errors found while flushing have no position.
        error.byte_position.map_or_else(
            || Value::null(LogicalType::UBIGINT),
            |position| Value::ubigint(position + 1),
        ),
        if error.error_type == CsvErrorType::MaximumLineSize {
            Value::null(LogicalType::UBIGINT)
        } else {
            Value::ubigint(error.column_idx + 1)
        },
        column_name,
        Value::varchar(error.error_type.reject_name()),
        Value::varchar(&error.csv_row),
        Value::bytes(LogicalType::VARCHAR, &error.error_message),
    ]
}

/// The row of the scans table of a file (`FillScanErrorTable`).
fn scan_row(file: &CsvFileScan, scan_idx: u64, file_idx: u64) -> Result<Vec<Value>> {
    let options = &file.options;
    let sm = &options.dialect_options.state_machine_options;
    let mut columns = roastedduck_common::ByteString::from("{");
    for (i, (name, ty)) in file.names.iter().zip(&file.types).enumerate() {
        let _ = roastedduck_common::byte_write!(columns, "'{name}': '{ty}'");
        if i + 1 != file.types.len() {
            columns.push(b',');
        }
    }
    columns.push(b'}');
    let format = |ty: FormatType| {
        let format = options.dialect_options.date_format(ty);
        if format.get().is_empty() {
            Value::null(LogicalType::VARCHAR)
        } else {
            Value::varchar(format.get().format_specifier())
        }
    };
    Ok(vec![
        Value::ubigint(scan_idx),
        Value::ubigint(file_idx),
        Value::try_varchar(&file.file_path)?,
        Value::varchar(sm.delimiter.format_value()),
        Value::varchar(sm.quote.format_value()),
        Value::varchar(sm.escape.format_value()),
        Value::varchar(options.newline()),
        Value::uinteger(u32::try_from(options.dialect_options.skip_rows.value()).unwrap_or(u32::MAX)),
        Value::boolean(options.dialect_options.header.value()),
        Value::bytes(LogicalType::VARCHAR, &columns),
        format(FormatType::Date),
        format(FormatType::Timestamp),
        if options.user_defined_parameters.is_empty() {
            Value::null(LogicalType::VARCHAR)
        } else {
            Value::bytes(LogicalType::VARCHAR, &options.user_defined_parameters_string())
        },
    ])
}
