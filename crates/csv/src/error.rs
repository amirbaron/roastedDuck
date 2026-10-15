//! CSV errors (DuckDB's `csv_error.hpp/.cpp`): what went wrong on which line, the message
//! with fixes and options, and the per-scan handler that throws, delays or collects them
//! (for `ignore_errors` and rejects tables).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::LogicalTypeId;

use crate::options::CsvReaderOptions;

/// How many lines a scanner boundary read (DuckDB's `LinesPerBoundary`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinesPerBoundary {
    pub boundary_idx: u64,
    pub lines_in_batch: u64,
}

impl LinesPerBoundary {
    pub fn new(boundary_idx: u64, lines_in_batch: u64) -> Self {
        Self {
            boundary_idx,
            lines_in_batch,
        }
    }
}

/// DuckDB's `CSVErrorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CsvErrorType {
    #[default]
    CastError = 0,
    ColumnNameTypeMismatch = 1,
    TooFewColumns = 2,
    TooManyColumns = 3,
    UnterminatedQuotes = 4,
    Sniffing = 5,
    MaximumLineSize = 6,
    NullpaddedQuotedNewValue = 7,
    InvalidEncoding = 8,
    InvalidState = 9,
}

impl CsvErrorType {
    /// `IsCSVErrorAcceptedReject`.
    pub fn accepted_reject(self) -> bool {
        matches!(
            self,
            Self::InvalidState
                | Self::CastError
                | Self::TooManyColumns
                | Self::TooFewColumns
                | Self::MaximumLineSize
                | Self::UnterminatedQuotes
                | Self::InvalidEncoding
        )
    }

    /// `CSVErrorTypeToEnum`.
    pub fn reject_name(self) -> &'static str {
        match self {
            Self::CastError => "CAST",
            Self::TooFewColumns => "MISSING COLUMNS",
            Self::TooManyColumns => "TOO MANY COLUMNS",
            Self::MaximumLineSize => "LINE SIZE OVER MAXIMUM",
            Self::UnterminatedQuotes => "UNQUOTED VALUE",
            Self::InvalidEncoding => "INVALID ENCODING",
            Self::InvalidState => "INVALID STATE",
            _ => "",
        }
    }
}

/// One CSV error (DuckDB's `CSVError`).
#[derive(Clone, Debug, Default)]
pub struct CsvError {
    pub error_message: String,
    /// The message with fixes and options, used when throwing.
    pub full_error_message: String,
    pub error_type: CsvErrorType,
    pub column_idx: u64,
    pub csv_row: String,
    pub error_info: LinesPerBoundary,
    pub row_byte_position: u64,
    pub byte_position: Option<u64>,
}

impl CsvError {
    /// `CSVError(error_message, type, error_info)`.
    pub fn simple(error_message: String, error_type: CsvErrorType, error_info: LinesPerBoundary) -> Self {
        Self {
            error_message,
            error_type,
            error_info,
            ..Self::default()
        }
    }

    /// The full constructor: the message, the fixes and the options.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut error_message: String,
        error_type: CsvErrorType,
        column_idx: u64,
        mut csv_row: String,
        error_info: LinesPerBoundary,
        row_byte_position: u64,
        byte_position: Option<u64>,
        reader_options: &CsvReaderOptions,
        fixes: &str,
        current_path: &str,
    ) -> Self {
        if reader_options.ignore_errors.value() {
            remove_new_line(&mut error_message);
        }
        if csv_row.len() > 10000 {
            let mut end = 10000;
            while !csv_row.is_char_boundary(end) {
                end -= 1;
            }
            csv_row.truncate(end);
        }
        let full_error_message = format!(
            "{error_message}\n{fixes}\n{}\n",
            reader_options.to_display_string(current_path)
        );
        Self {
            error_message,
            full_error_message,
            error_type,
            column_idx,
            csv_row,
            error_info,
            row_byte_position,
            byte_position,
        }
    }

    pub fn boundary_index(&self) -> u64 {
        self.error_info.boundary_idx
    }

    /// `ColumnTypesError`.
    pub fn column_types_error(mut sql_types_per_column: Vec<String>, names: &[String]) -> Self {
        sql_types_per_column.retain(|name| !names.iter().any(|n| n.eq_ignore_ascii_case(name)));
        if sql_types_per_column.is_empty() {
            return Self::simple(
                String::new(),
                CsvErrorType::ColumnNameTypeMismatch,
                LinesPerBoundary::default(),
            );
        }
        let quoted: Vec<String> = sql_types_per_column
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect();
        Self::simple(
            format!(
                "COLUMN_TYPES error: Columns with names: {} do not exist in the CSV File",
                quoted.join(",")
            ),
            CsvErrorType::ColumnNameTypeMismatch,
            LinesPerBoundary::default(),
        )
    }

    /// `CastError`.
    #[allow(clippy::too_many_arguments)]
    pub fn cast_error(
        options: &CsvReaderOptions,
        column_name: &str,
        cast_error: &str,
        column_idx: u64,
        csv_row: String,
        error_info: LinesPerBoundary,
        row_byte_position: u64,
        byte_position: Option<u64>,
        type_id: LogicalTypeId,
        current_path: &str,
    ) -> Self {
        let error = format!("Error when converting column \"{column_name}\". {cast_error}\n");
        let mut fix = format!(
            "Column {column_name} is being converted as type {}\n",
            type_id.name()
        );
        if options.was_type_manually_set(column_idx as usize) {
            fix += "This type was either manually set or derived from an existing table. Select a different type to correctly parse this column.\n";
        } else {
            fix += "This type was auto-detected from the CSV file.\n";
            fix += "Possible solutions:\n";
            let _ = writeln!(
                fix,
                "* Override the type for this column manually by setting the type explicitly, e.g., types={{'{column_name}': 'VARCHAR'}}"
            );
            fix += "* Set the sample size to a larger value to enable the auto-detection to scan more values, e.g., sample_size=-1\n";
            fix += "* Use a COPY statement to automatically derive types from an existing table.\n";
        }
        fix += "* Check whether the null string value is set correctly (e.g., nullstr = 'N/A')\n";
        Self::new(
            error,
            CsvErrorType::CastError,
            column_idx,
            csv_row,
            error_info,
            row_byte_position,
            byte_position,
            options,
            &fix,
            current_path,
        )
    }

    /// `LineSizeError`.
    pub fn line_size_error(
        options: &CsvReaderOptions,
        error_info: LinesPerBoundary,
        csv_row: String,
        byte_position: u64,
        current_path: &str,
    ) -> Self {
        let error = format!(
            "Maximum line size of {} bytes exceeded. Actual Size:{} bytes.\n",
            options.maximum_line_size.value(),
            csv_row.len()
        );
        let fix = format!(
            "Possible Solution: Change the maximum length size, e.g., max_line_size={}\n",
            csv_row.len() + 2
        );
        Self::new(
            error,
            CsvErrorType::MaximumLineSize,
            0,
            csv_row,
            error_info,
            byte_position,
            Some(byte_position),
            options,
            &fix,
            current_path,
        )
    }

    /// `InvalidState`.
    pub fn invalid_state(
        options: &CsvReaderOptions,
        current_column: u64,
        error_info: LinesPerBoundary,
        csv_row: String,
        row_byte_position: u64,
        byte_position: Option<u64>,
        current_path: &str,
    ) -> Self {
        let error = "The CSV Parser state machine reached an invalid state.\nThis can happen when is not possible to parse your CSV File with the given options, or the CSV File is not RFC 4180 compliant ".to_owned();
        let mut fix = String::new();
        if options.dialect_options.state_machine_options.strict_mode.value() {
            fix += "Possible fixes:\n";
            fix += "* Disable the parser's strict mode (strict_mode=false) to allow reading rows that do not comply with the CSV standard.\n";
        }
        Self::new(
            error,
            CsvErrorType::InvalidState,
            current_column,
            csv_row,
            error_info,
            row_byte_position,
            byte_position,
            options,
            &fix,
            current_path,
        )
    }

    /// `HeaderSniffingError`.
    pub fn header_sniffing_error(
        options: &CsvReaderOptions,
        best_header_row: &[Option<String>],
        column_count: u64,
        delimiter: &str,
    ) -> Self {
        let mut error = format!("Error when sniffing file \"{}\".\n", options.file_path);
        error += "It was not possible to detect the CSV Header, due to the header having less columns than expected\n";
        let _ = writeln!(
            error,
            "Number of expected columns: {column_count}. Actual number of columns {}",
            best_header_row.len()
        );
        error += "Detected row as Header:\n";
        for (index, value) in best_header_row.iter().enumerate() {
            error += value.as_deref().unwrap_or("NULL");
            if index + 1 < best_header_row.len() {
                error += delimiter;
                error += " ";
            }
        }
        error += "\n";
        error += "Possible fixes:\n";
        if options.dialect_options.state_machine_options.strict_mode.value() {
            error += "* Disable the parser's strict mode (strict_mode=false) to allow reading rows that do not comply with the CSV standard.\n";
        }
        if options.dialect_options.header.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Header is set to '{}'. Consider unsetting it.",
                u8::from(options.dialect_options.header.value())
            );
        } else {
            error +=
                "* Set header (header = true) if your CSV has a header, or (header = false) if it doesn't\n";
        }
        if options.dialect_options.skip_rows.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Skip is set to '{}'. Consider unsetting it.",
                options.dialect_options.skip_rows.value()
            );
        } else {
            error += "* Set skip (skip=${n}) to skip ${n} lines at the top of the file\n";
        }
        if !options.ignore_errors.value() {
            error += "* Enable ignore errors (ignore_errors=true) to ignore potential errors\n";
        }
        if !options.null_padding {
            error += "* Enable null padding (null_padding=true) to pad missing columns with NULL values\n";
        }
        Self::simple(error, CsvErrorType::Sniffing, LinesPerBoundary::default())
    }

    /// `SniffingError`.
    pub fn sniffing_error(
        options: &CsvReaderOptions,
        search_space: &str,
        max_columns_found: u64,
        set_columns: Option<(&str, usize)>,
        type_detection: bool,
    ) -> Self {
        let sm = &options.dialect_options.state_machine_options;
        let mut error = format!("Error when sniffing file \"{}\".\n", options.file_path);
        error += "It was not possible to automatically detect the CSV parsing ";
        error += if type_detection { "types\n" } else { "dialect\n" };
        error += "The search space used was:\n";
        error += search_space;
        let _ = writeln!(error, "Encoding: {}", options.encoding);
        error += "Possible fixes:\n";
        if sm.strict_mode.value() {
            error += "* Disable the parser's strict mode (strict_mode=false) to allow reading rows that do not comply with the CSV standard.\n";
        }
        if options.columns_set {
            let (text, size) = set_columns.unwrap_or(("", 0));
            let _ = writeln!(
                error,
                "* Columns are set as: \"{text}\", and they contain: {size} columns. It does not match the number of columns found by the sniffer: {max_columns_found}. Verify the columns parameter is correctly set."
            );
        }
        error += "* Make sure you are using the correct file encoding. If not, set it (e.g., encoding = 'utf-16').\n";
        if sm.delimiter.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Delimiter is set to '{}'. Consider unsetting it.",
                sm.delimiter.get()
            );
        } else {
            error += "* Set delimiter (e.g., delim=',')\n";
        }
        if sm.quote.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Quote is set to '{}'. Consider unsetting it.",
                crate::options::byte_string(sm.quote.value())
            );
        } else {
            error += "* Set quote (e.g., quote='\"')\n";
        }
        if sm.escape.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Escape is set to '{}'. Consider unsetting it.",
                crate::options::byte_string(sm.escape.value())
            );
        } else {
            error += "* Set escape (e.g., escape='\"')\n";
        }
        if sm.comment.is_set_by_user() {
            let _ = writeln!(
                error,
                "* Comment is set to '{}'. Consider unsetting it.",
                crate::options::byte_string(sm.comment.value())
            );
        } else {
            error += "* Set comment (e.g., comment='#')\n";
        }
        if !options.dialect_options.skip_rows.is_set_by_user() {
            error += "* Set skip (skip=${n}) to skip ${n} lines at the top of the file\n";
        }
        if !options.ignore_errors.value() {
            error += "* Enable ignore errors (ignore_errors=true) to ignore potential errors\n";
        }
        if !options.null_padding {
            error += "* Enable null padding (null_padding=true) to pad missing columns with NULL values\n";
        }
        error += "* Check you are using the correct file compression, otherwise set it (e.g., compression = 'zstd')\n";
        error += "* Be sure that the maximum line size is set to an appropriate value, otherwise set it (e.g., max_line_size=10000000)\n";
        Self::simple(error, CsvErrorType::Sniffing, LinesPerBoundary::default())
    }

    /// `NullPaddingFail`.
    pub fn null_padding_fail(
        options: &CsvReaderOptions,
        error_info: LinesPerBoundary,
        current_path: &str,
    ) -> Self {
        let error = format!(
            " The parallel scanner does not support null_padding in conjunction with quoted new lines. Please disable the parallel csv reader with parallel=false\n{}",
            options.to_display_string(current_path)
        );
        Self::simple(error, CsvErrorType::NullpaddedQuotedNewValue, error_info)
    }

    /// `UnterminatedQuotesError`.
    pub fn unterminated_quotes_error(
        options: &CsvReaderOptions,
        current_column: u64,
        error_info: LinesPerBoundary,
        csv_row: String,
        row_byte_position: u64,
        byte_position: Option<u64>,
        current_path: &str,
    ) -> Self {
        let error = "Value with unterminated quote found.\n".to_owned();
        let mut fix = "Possible fixes:\n".to_owned();
        if options.dialect_options.state_machine_options.strict_mode.value() {
            fix += "* Disable the parser's strict mode (strict_mode=false) to allow reading rows that do not comply with the CSV standard.\n";
        }
        fix += "* Enable ignore errors (ignore_errors=true) to skip this row\n";
        fix += "* Set quote to empty or to a different value (e.g., quote='')\n";
        Self::new(
            error,
            CsvErrorType::UnterminatedQuotes,
            current_column,
            csv_row,
            error_info,
            row_byte_position,
            byte_position,
            options,
            &fix,
            current_path,
        )
    }

    /// `IncorrectColumnAmountError`.
    pub fn incorrect_column_amount_error(
        options: &CsvReaderOptions,
        actual_columns: u64,
        error_info: LinesPerBoundary,
        csv_row: String,
        row_byte_position: u64,
        byte_position: Option<u64>,
        current_path: &str,
    ) -> Self {
        let mut fix = "Possible fixes:\n".to_owned();
        if options.dialect_options.state_machine_options.strict_mode.value() {
            fix += "* Disable the parser's strict mode (strict_mode=false) to allow reading rows that do not comply with the CSV standard.\n";
        }
        if !options.null_padding {
            fix += "* Enable null padding (null_padding=true) to replace missing values with NULL\n";
        }
        if !options.ignore_errors.value() {
            fix += "* Enable ignore errors (ignore_errors=true) to skip this row\n";
        }
        let error = format!(
            "Expected Number of Columns: {} Found: {}",
            options.dialect_options.num_cols,
            actual_columns.wrapping_add(1)
        );
        let position = byte_position.unwrap_or(0);
        let byte_pos = if position == 0 { 0 } else { position - 1 };
        let error_type = if actual_columns >= options.dialect_options.num_cols {
            CsvErrorType::TooManyColumns
        } else {
            CsvErrorType::TooFewColumns
        };
        Self::new(
            error,
            error_type,
            actual_columns,
            csv_row,
            error_info,
            row_byte_position,
            Some(byte_pos),
            options,
            &fix,
            current_path,
        )
    }

    /// `InvalidUTF8`.
    pub fn invalid_utf8(
        options: &CsvReaderOptions,
        current_column: u64,
        error_info: LinesPerBoundary,
        csv_row: String,
        row_byte_position: u64,
        byte_position: Option<u64>,
        current_path: &str,
    ) -> Self {
        let error = format!(
            "Invalid unicode (byte sequence mismatch) detected. This file is not {} encoded.\n",
            options.encoding
        );
        let fix = "Possible Solution: Set the correct encoding, if available, to read this CSV File (e.g., encoding='UTF-16')\nPossible Solution: Enable ignore errors (ignore_errors=true) to skip this row\n";
        Self::new(
            error,
            CsvErrorType::InvalidEncoding,
            current_column,
            csv_row,
            error_info,
            row_byte_position,
            byte_position,
            options,
            fix,
            current_path,
        )
    }
}

/// `RemoveNewLine`: the first line of a message.
fn remove_new_line(error: &mut String) {
    if let Some(position) = error.find('\n') {
        error.truncate(position);
    }
}

#[derive(Debug, Default)]
struct HandlerState {
    lines_per_batch_map: HashMap<u64, LinesPerBoundary>,
    max_line_length: u64,
    ignore_errors: bool,
    print_line: bool,
    errors: Vec<CsvError>,
}

/// Collects, delays and throws the errors of a scan (DuckDB's `CSVErrorHandler`).
#[derive(Debug)]
pub struct CsvErrorHandler {
    state: Mutex<HandlerState>,
}

impl Default for CsvErrorHandler {
    fn default() -> Self {
        Self::new(false)
    }
}

impl CsvErrorHandler {
    pub fn new(ignore_errors: bool) -> Self {
        Self {
            state: Mutex::new(HandlerState {
                ignore_errors,
                print_line: true,
                ..HandlerState::default()
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HandlerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `PrintLineNumber`.
    fn print_line_number(state: &HandlerState, error: &CsvError) -> bool {
        state.print_line
            && matches!(
                error.error_type,
                CsvErrorType::CastError
                    | CsvErrorType::UnterminatedQuotes
                    | CsvErrorType::TooFewColumns
                    | CsvErrorType::TooManyColumns
                    | CsvErrorType::MaximumLineSize
                    | CsvErrorType::NullpaddedQuotedNewValue
                    | CsvErrorType::InvalidEncoding
            )
    }

    /// `CanGetLine`.
    fn can_get_line(state: &HandlerState, boundary_index: u64) -> bool {
        (0..boundary_index).all(|index| state.lines_per_batch_map.contains_key(&index))
    }

    /// `GetLineInternal`: the 1-indexed line of an error.
    fn line_internal(state: &HandlerState, error_info: &LinesPerBoundary) -> u64 {
        let mut current_line = 1 + error_info.lines_in_batch;
        for boundary in 0..error_info.boundary_idx {
            current_line += state
                .lines_per_batch_map
                .get(&boundary)
                .map_or(0, |lines| lines.lines_in_batch);
        }
        current_line
    }

    /// `ThrowError`.
    fn throw_error(state: &HandlerState, csv_error: &CsvError) -> Error {
        let mut error_to_throw = csv_error;
        let mut error_to_throw_row = Self::line_internal(state, &error_to_throw.error_info);
        if Self::print_line_number(state, error_to_throw) && !state.errors.is_empty() {
            for error in &state.errors {
                if Self::can_get_line(state, error.boundary_index()) {
                    let row = Self::line_internal(state, &error.error_info);
                    if row < error_to_throw_row {
                        error_to_throw = error;
                        error_to_throw_row = row;
                    }
                }
            }
        }
        let mut message = String::new();
        if Self::print_line_number(state, error_to_throw) {
            let _ = writeln!(message, "CSV Error on Line: {error_to_throw_row}");
            if !error_to_throw.csv_row.is_empty() {
                let _ = writeln!(message, "Original Line: {}", error_to_throw.csv_row);
            }
        }
        if error_to_throw.full_error_message.is_empty() {
            message += &error_to_throw.error_message;
        } else {
            message += &error_to_throw.full_error_message;
        }
        let kind = match csv_error.error_type {
            CsvErrorType::CastError => ErrorKind::Conversion,
            CsvErrorType::ColumnNameTypeMismatch => ErrorKind::Binder,
            CsvErrorType::NullpaddedQuotedNewValue => ErrorKind::ParameterNotAllowed,
            _ => ErrorKind::InvalidInput,
        };
        Error::new(kind, message)
    }

    /// `Error`: throws now, or keeps the error for later (ignored errors, or errors whose
    /// line is not known yet).
    pub fn error(&self, csv_error: CsvError, force_error: bool) -> Result<()> {
        let mut state = self.lock();
        if !force_error
            && (state.ignore_errors
                || (Self::print_line_number(&state, &csv_error)
                    && !Self::can_get_line(&state, csv_error.boundary_index())))
        {
            state.errors.push(csv_error);
            return Ok(());
        }
        Err(Self::throw_error(&state, &csv_error))
    }

    /// `ErrorIfNeeded`.
    pub fn error_if_needed(&self) -> Result<()> {
        let state = self.lock();
        if state.ignore_errors || state.errors.is_empty() {
            return Ok(());
        }
        if Self::can_get_line(&state, state.errors[0].error_info.boundary_idx) {
            return Err(Self::throw_error(&state, &state.errors[0]));
        }
        Ok(())
    }

    /// `ErrorIfAny`.
    pub fn error_if_any(&self) -> Result<()> {
        let state = self.lock();
        if state.ignore_errors || state.errors.is_empty() {
            return Ok(());
        }
        if !Self::can_get_line(&state, state.errors[0].error_info.boundary_idx) {
            return Err(Error::internal(
                "Failed to get error information for boundary index",
            ));
        }
        Err(Self::throw_error(&state, &state.errors[0]))
    }

    /// `ErrorIfTypeExists`.
    pub fn error_if_type_exists(&self, error_type: CsvErrorType) -> Result<()> {
        let state = self.lock();
        if let Some(error) = state.errors.iter().find(|error| error.error_type == error_type) {
            return Err(Self::throw_error(&state, error));
        }
        Ok(())
    }

    /// `Insert`: the lines a boundary read.
    pub fn insert(&self, boundary_idx: u64, rows: u64) {
        let mut state = self.lock();
        state
            .lines_per_batch_map
            .entry(boundary_idx)
            .and_modify(|lines| lines.lines_in_batch += rows)
            .or_insert(LinesPerBoundary::new(boundary_idx, rows));
    }

    pub fn new_max_line_size(&self, size: u64) {
        let mut state = self.lock();
        state.max_line_length = state.max_line_length.max(size);
    }

    pub fn any_errors(&self) -> bool {
        !self.lock().errors.is_empty()
    }

    pub fn has_error(&self, error_type: CsvErrorType) -> bool {
        self.lock()
            .errors
            .iter()
            .any(|error| error.error_type == error_type)
    }

    /// `GetFirstError`.
    pub fn first_error(&self, error_type: CsvErrorType) -> Result<CsvError> {
        self.lock()
            .errors
            .iter()
            .find(|error| error.error_type == error_type)
            .cloned()
            .ok_or_else(|| {
                Error::internal(
                    "CSVErrorHandler::GetFirstError was called without having an appropriate error type",
                )
            })
    }

    pub fn size(&self) -> usize {
        self.lock().errors.len()
    }

    pub fn max_line_length(&self) -> u64 {
        self.lock().max_line_length
    }

    /// `DontPrintErrorLine`.
    pub fn dont_print_error_line(&self) {
        self.lock().print_line = false;
    }

    pub fn set_ignore_errors(&self, ignore_errors: bool) {
        self.lock().ignore_errors = ignore_errors;
    }

    /// `GetLine`.
    pub fn line(&self, error_info: &LinesPerBoundary) -> u64 {
        let state = self.lock();
        Self::line_internal(&state, error_info)
    }

    /// The stored errors with their line numbers, for a rejects table
    /// (`FillRejectsTable`).
    pub fn errors_with_lines(&self) -> Vec<(CsvError, u64)> {
        let state = self.lock();
        state
            .errors
            .iter()
            .map(|error| (error.clone(), Self::line_internal(&state, &error.error_info)))
            .collect()
    }
}
