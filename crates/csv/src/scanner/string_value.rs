//! The scanner that parses values into vectors (DuckDB's `string_value_scanner.hpp/.cpp`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::cast::text_parse::{TextFloat, TextInteger, parse_bool, parse_decimal};
use rustdb_function::cast::{CastParameters, cast_vector};
use rustdb_functions::strtime::StrpTimeFormat;
use rustdb_types::{
    DataChunk, Date, LogicalType, LogicalTypeId, PhysicalType, SelectionVector, StringT, Time, Timestamp,
    Vector,
};

use super::base::{BaseScanner, CsvProcess};
use super::boundary::{CsvIterator, CsvPosition, FullLinePosition, LinePosition, sanitize_error};
use crate::buffer::{CsvBufferHandle, CsvBufferManager};
use crate::error::{CsvError, CsvErrorHandler, CsvErrorType, LinesPerBoundary};
use crate::file_scan::CsvFileScan;
use crate::options::{FormatType, NewLineIdentifier};
use crate::state_machine::{CsvState, CsvStateMachine, CsvStates, cached_state_machine};

/// Releases a buffer once the scanners of a boundary are done with it (DuckDB's
/// `CSVBufferUsage`).
#[derive(Debug)]
pub struct CsvBufferUsage {
    pub buffer_manager: Arc<CsvBufferManager>,
    pub buffer_idx: u64,
}

impl Drop for CsvBufferUsage {
    fn drop(&mut self) {
        self.buffer_manager.reset_buffer(self.buffer_idx);
    }
}

/// An error of the current line (DuckDB's `CurrentError`).
#[derive(Clone, Debug)]
struct CurrentError {
    error_type: CsvErrorType,
    col_idx: u64,
    chunk_idx: u64,
    error_message: String,
    error_position: LinePosition,
}

/// The errors of the current line (DuckDB's `LineError`).
#[derive(Debug)]
pub struct LineError {
    current_errors: Vec<CurrentError>,
    is_error_in_line: bool,
    ignore_errors: bool,
    scan_id: u64,
}

impl LineError {
    fn new(scan_id: u64, ignore_errors: bool) -> Self {
        Self {
            current_errors: Vec::new(),
            is_error_in_line: false,
            ignore_errors,
            scan_id,
        }
    }

    fn reset(&mut self) {
        self.current_errors.clear();
        self.is_error_in_line = false;
    }

    fn insert(
        &mut self,
        error_type: CsvErrorType,
        col_idx: u64,
        chunk_idx: u64,
        error_position: LinePosition,
    ) {
        self.is_error_in_line = true;
        if !self.ignore_errors {
            self.current_errors.push(CurrentError {
                error_type,
                col_idx,
                chunk_idx,
                error_message: String::new(),
                error_position,
            });
        }
    }

    fn modify_error_message_of_last_error(&mut self, message: String) {
        if let Some(last) = self.current_errors.last_mut() {
            last.error_message = message;
        }
    }

    pub fn has_error_type(&self, error_type: CsvErrorType) -> bool {
        self.current_errors
            .iter()
            .any(|error| error.error_type == error_type)
    }

    pub fn has_error(&self) -> bool {
        !self.current_errors.is_empty()
    }

    pub fn size(&self) -> usize {
        self.current_errors.len()
    }
}

/// How a column is parsed (DuckDB's `ParseTypeInfo`).
#[derive(Clone, Debug)]
struct ParseTypeInfo {
    validate_utf8: bool,
    type_id: LogicalTypeId,
    physical: PhysicalType,
    numeric: bool,
    scale: u8,
    width: u8,
}

impl ParseTypeInfo {
    fn new(ty: &LogicalType, validate_utf8: bool) -> Self {
        let (width, scale) = if ty.id() == LogicalTypeId::Decimal {
            ty.decimal_properties().unwrap_or((18, 3))
        } else {
            (0, 0)
        };
        Self {
            validate_utf8,
            type_id: ty.id(),
            physical: ty.physical_type(),
            numeric: ty.is_numeric(),
            scale,
            width,
        }
    }
}

/// The parse state of a [`StringValueScanner`] (DuckDB's `StringValueResult`).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct StringValueResult {
    pub buffer: Option<CsvBufferHandle>,
    pub buffer_size: u64,
    position_before_comment: u64,
    pub number_of_columns: u32,
    null_padding: bool,
    extra_delimiter_bytes: u64,
    null_str: Vec<Vec<u8>>,
    pub parse_chunk: Vec<Vector>,
    parse_types_logical: Vec<LogicalType>,
    pub number_of_rows: i64,
    pub cur_col_id: u64,
    pub figure_out_new_line: bool,
    pub current_line_position: FullLinePosition,
    pub line_positions_per_row: HashMap<u64, FullLinePosition>,
    store_line_size: bool,
    pub added_last_line: bool,
    pub quoted_new_line: bool,
    pub try_row: bool,
    parse_types: Vec<ParseTypeInfo>,
    pub names: Vec<String>,
    projected_columns: Vec<bool>,
    projecting_columns: bool,
    pub chunk_col_id: u64,
    icu_loaded: bool,
    pub buffer_handles: HashMap<u64, CsvBufferHandle>,
    pub requested_size: u64,
    pub current_errors: LineError,
    date_format: StrpTimeFormat,
    timestamp_format: StrpTimeFormat,
    sniffing: bool,
    pub borked_rows: HashSet<u64>,
    pub path: String,
    pub first_line_is_comment: bool,
    ignore_empty_values: bool,
    // ScannerResult
    pub quoted: bool,
    pub unquoted: bool,
    pub escaped: bool,
    pub comment: bool,
    pub quoted_position: u64,
    pub last_position: LinePosition,
    pub result_size: u64,
}

impl StringValueResult {
    /// The value bytes of the current buffer.
    fn buffer_bytes(&self) -> &[u8] {
        self.buffer.as_ref().map_or(&[], |buffer| buffer.data())
    }

    fn validity_set_invalid(&mut self, column: usize, row: usize) {
        let capacity = self.result_size as usize;
        self.parse_chunk[column].validity_mut().set_invalid(row, capacity);
    }

    /// `PrintErrorLine`.
    fn print_error_line(&self, state_machine: &CsvStateMachine) -> bool {
        self.result_size != 1
            && (state_machine.options.store_rejects.value() || !state_machine.options.ignore_errors.value())
    }

    fn reset_vectors(&mut self) {
        let capacity = self.result_size as usize;
        self.parse_chunk = self
            .parse_types_logical
            .iter()
            .map(|ty| Vector::flat(ty.clone(), capacity))
            .collect();
    }
}

/// `IsValueNull`.
fn is_value_null(null_str: &[u8], value: &[u8]) -> bool {
    value.len() >= null_str.len() && null_str.iter().zip(value).all(|(a, b)| a == b)
}

/// Reads one CSV file part into chunks of values (DuckDB's `StringValueScanner`).
#[derive(Debug)]
pub struct StringValueScanner {
    pub base: BaseScanner,
    pub scanner_idx: u64,
    pub csv_file_scan: Option<Arc<CsvFileScan>>,
    pub buffer_tracker: Option<Arc<CsvBufferUsage>>,
    pub result: StringValueResult,
    start_pos: u64,
    previous_buffer_handle: Option<CsvBufferHandle>,
    state_machine_strict: Option<Arc<CsvStateMachine>>,
}

/// `StringValueScanner::LINE_FINDER_ID`.
pub const LINE_FINDER_ID: u64 = u64::MAX;

impl Drop for StringValueScanner {
    /// `~StringValueResult`: records the lines read for error line numbers.
    fn drop(&mut self) {
        self.base
            .error_handler
            .insert(self.base.iterator.boundary_idx(), self.base.lines_read);
        if !self.base.iterator.done {
            self.base.error_handler.dont_print_error_line();
        }
    }
}

/// `StringValueScanner::CanDirectlyCast`.
pub fn can_directly_cast(ty: &LogicalType, icu_loaded: bool) -> bool {
    match ty.id() {
        LogicalTypeId::TinyInt
        | LogicalTypeId::SmallInt
        | LogicalTypeId::Integer
        | LogicalTypeId::BigInt
        | LogicalTypeId::UTinyInt
        | LogicalTypeId::USmallInt
        | LogicalTypeId::UInteger
        | LogicalTypeId::UBigInt
        | LogicalTypeId::Double
        | LogicalTypeId::Float
        | LogicalTypeId::Date
        | LogicalTypeId::Timestamp
        | LogicalTypeId::Time
        | LogicalTypeId::Decimal
        | LogicalTypeId::Boolean => true,
        LogicalTypeId::TimestampTz => !icu_loaded,
        LogicalTypeId::Varchar => !is_json(ty),
        _ => false,
    }
}

/// Whether a type is the JSON alias of VARCHAR (`LogicalType::IsJSONType`).
pub fn is_json(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar && ty.alias().is_some_and(|alias| alias.eq_ignore_ascii_case("json"))
}

/// `StringValueScanner::RemoveEscape`.
pub fn remove_escape(value: &[u8], escape: u8, quote: u8, strict_mode: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(value.len());
    let mut just_escaped = false;
    for &c in value {
        if c == escape && !just_escaped {
            just_escaped = true;
        } else if c == quote {
            if just_escaped || !strict_mode {
                result.push(c);
            }
            just_escaped = false;
        } else {
            just_escaped = false;
            result.push(c);
        }
    }
    result
}

/// `StringUtil::SkipBOM`.
fn skip_bom(buffer: &[u8], buffer_pos: &mut u64) {
    if buffer.len() >= 3 && buffer[..3] == [0xEF, 0xBB, 0xBF] && *buffer_pos == 0 {
        *buffer_pos = 3;
    }
}

impl StringValueScanner {
    /// `StringValueScanner(scanner_idx, buffer_manager, state_machine, error_handler,
    /// csv_file_scan, sniffing, boundary, result_size)`.
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    pub fn new(
        scanner_idx: u64,
        buffer_manager: Arc<CsvBufferManager>,
        state_machine: Arc<CsvStateMachine>,
        error_handler: Arc<CsvErrorHandler>,
        csv_file_scan: Option<Arc<CsvFileScan>>,
        sniffing: bool,
        boundary: CsvIterator,
        result_size: u64,
    ) -> Result<Self> {
        let mut base = BaseScanner::new(buffer_manager, state_machine, error_handler, sniffing, boundary)?;
        let sm = Arc::clone(&base.state_machine);
        let number_of_columns = sm.dialect_options.num_cols as u32;
        let delimiter = sm.dialect_options.state_machine_options.delimiter.get();
        let extra_delimiter_bytes = if delimiter.is_empty() {
            0
        } else {
            delimiter.len() as u64 - 1
        };
        let mut result = StringValueResult {
            buffer: None,
            buffer_size: 0,
            position_before_comment: 0,
            number_of_columns,
            null_padding: sm.options.null_padding,
            extra_delimiter_bytes,
            null_str: sm
                .options
                .null_str
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
            parse_chunk: Vec::new(),
            parse_types_logical: Vec::new(),
            number_of_rows: 0,
            cur_col_id: 0,
            figure_out_new_line: false,
            current_line_position: FullLinePosition::default(),
            line_positions_per_row: HashMap::new(),
            store_line_size: false,
            added_last_line: false,
            quoted_new_line: false,
            try_row: scanner_idx == LINE_FINDER_ID,
            parse_types: Vec::new(),
            names: Vec::new(),
            projected_columns: Vec::new(),
            projecting_columns: false,
            chunk_col_id: 0,
            icu_loaded: true,
            buffer_handles: HashMap::new(),
            requested_size: 0,
            current_errors: LineError::new(scanner_idx, sm.options.ignore_errors_only()),
            date_format: sm
                .options
                .dialect_options
                .date_format(FormatType::Date)
                .get()
                .clone(),
            timestamp_format: sm
                .options
                .dialect_options
                .date_format(FormatType::Timestamp)
                .get()
                .clone(),
            sniffing,
            borked_rows: HashSet::new(),
            path: base.buffer_manager.file_path().to_owned(),
            first_line_is_comment: false,
            ignore_empty_values: true,
            quoted: false,
            unquoted: false,
            escaped: false,
            comment: false,
            quoted_position: 0,
            last_position: LinePosition::default(),
            result_size,
        };
        if let Some(handle) = base.cur_buffer_handle.clone() {
            result
                .buffer_handles
                .insert(handle.buffer_idx(), Arc::clone(&handle));
            result.buffer_size = handle.actual_size();
            result.last_position = LinePosition::new(
                handle.buffer_idx(),
                base.iterator.pos.buffer_pos,
                result.buffer_size,
            );
            result.requested_size = handle.requested_size();
            result.current_line_position.begin = LinePosition::new(
                base.iterator.pos.buffer_idx,
                base.iterator.pos.buffer_pos,
                handle.actual_size(),
            );
            result.current_line_position.end = result.current_line_position.begin;
            result.buffer = Some(handle);
            let mut logical_types = Vec::new();
            match &csv_file_scan {
                None => {
                    for i in 0..number_of_columns {
                        result
                            .parse_types
                            .push(ParseTypeInfo::new(&LogicalType::VARCHAR, true));
                        logical_types.push(LogicalType::VARCHAR);
                        result.names.push(format!("Column_{i}"));
                    }
                }
                Some(file_scan) => {
                    if file_scan.file_types.len() > number_of_columns as usize {
                        return Err(Error::invalid_input(format!(
                            "Mismatch between the number of columns ({number_of_columns}) in the CSV file and what is expected in the scanner ({}).",
                            file_scan.file_types.len()
                        )));
                    }
                    for ty in &file_scan.file_types {
                        let ty = if is_json(ty) {
                            LogicalType::VARCHAR
                        } else {
                            ty.clone()
                        };
                        if can_directly_cast(&ty, result.icu_loaded) {
                            result.parse_types.push(ParseTypeInfo::new(&ty, true));
                            logical_types.push(ty);
                        } else {
                            let validate = ty.id() == LogicalTypeId::Varchar || ty.is_nested();
                            result
                                .parse_types
                                .push(ParseTypeInfo::new(&LogicalType::VARCHAR, validate));
                            logical_types.push(LogicalType::VARCHAR);
                        }
                    }
                    result.names.clone_from(&file_scan.names);
                    if !file_scan.projected_columns.is_empty() {
                        result.projecting_columns = false;
                        result.projected_columns = (0..u64::from(number_of_columns))
                            .map(|column| {
                                let projected = file_scan.projected_columns.contains(&column);
                                if !projected {
                                    result.projecting_columns = true;
                                }
                                projected
                            })
                            .collect();
                    }
                    if !result.projecting_columns {
                        for _ in logical_types.len()..number_of_columns as usize {
                            result
                                .parse_types
                                .push(ParseTypeInfo::new(&LogicalType::VARCHAR, true));
                            logical_types.push(LogicalType::VARCHAR);
                        }
                    }
                }
            }
            result.parse_types_logical = logical_types;
            result.reset_vectors();
            if base.iterator.first_one {
                base.lines_read +=
                    sm.dialect_options.skip_rows.value() + u64::from(sm.dialect_options.header.value());
                if base.lines_read == 0 {
                    let bytes = result.buffer_bytes().to_vec();
                    skip_bom(&bytes, &mut base.iterator.pos.buffer_pos);
                }
            }
            let sm_options = &sm.dialect_options.state_machine_options;
            result.ignore_empty_values = sm_options.delimiter.get().as_bytes().first() != Some(&b' ')
                && sm_options.quote.value() != b' '
                && sm_options.escape.value() != b' '
                && sm_options.comment.value() != b' ';
        }
        if scanner_idx == 0
            && let Some(file_scan) = &csv_file_scan
        {
            base.lines_read += file_scan.skipped_rows;
        }
        base.iterator.buffer_size = sm.options.buffer_size_option.value();
        Ok(Self {
            base,
            scanner_idx,
            csv_file_scan,
            buffer_tracker: None,
            result,
            start_pos: 0,
            previous_buffer_handle: None,
            state_machine_strict: None,
        })
    }

    /// `StringValueScanner(buffer_manager, state_machine, error_handler, result_size,
    /// boundary)`: a scanner without a file scan.
    pub fn without_file(
        buffer_manager: Arc<CsvBufferManager>,
        state_machine: Arc<CsvStateMachine>,
        error_handler: Arc<CsvErrorHandler>,
        result_size: u64,
        boundary: CsvIterator,
    ) -> Result<Self> {
        Self::new(
            0,
            buffer_manager,
            state_machine,
            error_handler,
            None,
            false,
            boundary,
            result_size,
        )
    }

    pub fn state_machine(&self) -> &Arc<CsvStateMachine> {
        &self.base.state_machine
    }

    pub fn finished_iterator(&self) -> bool {
        self.base.iterator.done
    }

    pub fn iterator_position(&self) -> CsvPosition {
        self.base.iterator.pos
    }

    /// `GetValidationLine`.
    pub fn validation_line(&self) -> (u64, u64) {
        (self.start_pos, self.base.iterator.global_current_pos())
    }

    // ------------------------------------------------------------------------------------
    // StringValueResult handlers
    // ------------------------------------------------------------------------------------

    fn strict(&self) -> bool {
        self.base.state_machine.state_machine_options.strict_mode.value()
    }

    /// `HandleTooManyColumnsError`.
    fn handle_too_many_columns_error(&mut self, value: &[u8]) -> bool {
        let r = &mut self.result;
        if r.cur_col_id >= u64::from(r.number_of_columns) {
            if self.base.state_machine.state_machine_options.strict_mode.value() {
                let mut error = true;
                if r.cur_col_id == u64::from(r.number_of_columns)
                    && (!r.quoted || self.base.state_machine.options.allow_quoted_nulls)
                {
                    let is_value_null = r.null_str.iter().any(|null| is_value_null(null, value));
                    error = !is_value_null;
                }
                if error {
                    r.current_errors.insert(
                        CsvErrorType::TooManyColumns,
                        r.cur_col_id,
                        r.chunk_col_id,
                        r.last_position,
                    );
                    r.cur_col_id += 1;
                }
                return true;
            }
            self.base.used_unstrictness = true;
        }
        false
    }

    /// The `Could not convert string ...` message of a cast error.
    fn cast_message(value: &[u8], type_id: LogicalTypeId) -> String {
        sanitize_error(
            format!(
                "Could not convert string \"{}\" to '{}'",
                String::from_utf8_lossy(value),
                type_id.name()
            )
            .as_bytes(),
        )
    }

    /// `AddValueToVector`.
    #[allow(clippy::too_many_lines)]
    fn add_value_to_vector(&mut self, value: &[u8], allocate: bool) -> Result<()> {
        let _ = allocate;
        if self.handle_too_many_columns_error(value) {
            return Ok(());
        }
        let number_of_columns = u64::from(self.result.number_of_columns);
        if self.result.cur_col_id >= number_of_columns {
            if !self.strict() {
                self.base.used_unstrictness = true;
                return Ok(());
            }
            let r = &mut self.result;
            let mut error = true;
            if r.cur_col_id == number_of_columns
                && (!r.quoted || self.base.state_machine.options.allow_quoted_nulls)
            {
                error = !r.null_str.iter().any(|null| is_value_null(null, value));
            }
            if error {
                r.current_errors.insert(
                    CsvErrorType::TooManyColumns,
                    r.cur_col_id,
                    r.chunk_col_id,
                    r.last_position,
                );
                r.cur_col_id += 1;
            }
            return Ok(());
        }
        if self.result.projecting_columns && !self.result.projected_columns[self.result.cur_col_id as usize] {
            self.result.cur_col_id += 1;
            return Ok(());
        }
        let options = &self.base.state_machine.options;
        let escape_char = self.base.state_machine.state_machine_options.escape.value();
        let strict = self.base.state_machine.state_machine_options.strict_mode.value();
        let row = self.result.number_of_rows as usize;
        let chunk_col = self.result.chunk_col_id as usize;
        if !self.result.quoted || options.allow_quoted_nulls {
            let check_unquoted_escaped_null =
                !strict && self.result.escaped && !self.result.quoted && value.len() == 1;
            for null in &self.result.null_str {
                let is_null = if null.len() == 2 && null[0] == escape_char {
                    check_unquoted_escaped_null && null[1] == value[0]
                } else if value.len() == null.len() && !check_unquoted_escaped_null {
                    is_value_null(null, value)
                } else {
                    false
                };
                if is_null {
                    let empty = options.force_not_null.get(chunk_col).copied().unwrap_or(false);
                    if empty {
                        if self.result.parse_types[chunk_col].type_id == LogicalTypeId::Varchar {
                            self.result.parse_chunk[chunk_col].values_mut::<StringT>()[row] =
                                StringT::inline(b"");
                        } else {
                            let r = &mut self.result;
                            r.current_errors.insert(
                                CsvErrorType::CastError,
                                r.cur_col_id,
                                r.chunk_col_id,
                                r.last_position,
                            );
                        }
                    } else {
                        if chunk_col == number_of_columns as usize {
                            return Ok(());
                        }
                        self.result.validity_set_invalid(chunk_col, row);
                    }
                    self.result.cur_col_id += 1;
                    self.result.chunk_col_id += 1;
                    return Ok(());
                }
            }
        }
        let parse_type = self.result.parse_types[chunk_col].clone();
        let stripped;
        let value = if parse_type.numeric && options.thousands_separator != 0 {
            stripped = BaseScanner::remove_separator(value, options.thousands_separator);
            &stripped[..]
        } else {
            value
        };
        let decimal_separator = options
            .decimal_separator
            .as_bytes()
            .first()
            .copied()
            .unwrap_or(b'.');
        let vector = &mut self.result.parse_chunk[chunk_col];
        macro_rules! store {
            ($ty:ty, $parsed:expr) => {{
                match $parsed {
                    Some(parsed) => {
                        vector.values_mut::<$ty>()[row] = parsed;
                        true
                    }
                    None => false,
                }
            }};
        }
        let success = match parse_type.type_id {
            LogicalTypeId::Boolean => store!(bool, parse_bool(value)),
            LogicalTypeId::TinyInt => store!(i8, i8::parse_integer(value)),
            LogicalTypeId::SmallInt => store!(i16, i16::parse_integer(value)),
            LogicalTypeId::Integer => store!(i32, i32::parse_integer(value)),
            LogicalTypeId::BigInt => store!(i64, i64::parse_integer(value)),
            LogicalTypeId::UTinyInt => store!(u8, u8::parse_integer(value)),
            LogicalTypeId::USmallInt => store!(u16, u16::parse_integer(value)),
            LogicalTypeId::UInteger => store!(u32, u32::parse_integer(value)),
            LogicalTypeId::UBigInt => store!(u64, u64::parse_integer(value)),
            LogicalTypeId::Double => store!(f64, f64::parse_float(value, decimal_separator)),
            LogicalTypeId::Float => store!(f32, f32::parse_float(value, decimal_separator)),
            LogicalTypeId::Date => {
                let parsed = if self.result.date_format.is_empty() {
                    Date::parse(value, false).ok()
                } else {
                    self.result.date_format.try_parse_date(value)
                };
                store!(Date, parsed)
            }
            LogicalTypeId::Time => store!(Time, Time::parse(value, false)),
            LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz => {
                let parsed = if self.result.timestamp_format.is_empty() {
                    let use_offset = parse_type.type_id == LogicalTypeId::TimestampTz;
                    Timestamp::parse(value, use_offset)
                        .ok()
                        .map(|(timestamp, _)| timestamp)
                } else {
                    self.result.timestamp_format.try_parse_timestamp(value)
                };
                store!(Timestamp, parsed)
            }
            LogicalTypeId::Decimal => {
                if decimal_separator != b',' && decimal_separator != b'.' {
                    return Err(Error::invalid_input(
                        "Decimals can only have ',' and '.' as decimal separators",
                    ));
                }
                let parsed = parse_decimal(value, parse_type.width, parse_type.scale, decimal_separator);
                match (parse_type.physical, parsed) {
                    (PhysicalType::Int16, Some(v)) => store!(i16, i16::try_from(v).ok()),
                    (PhysicalType::Int32, Some(v)) => store!(i32, i32::try_from(v).ok()),
                    (PhysicalType::Int64, Some(v)) => store!(i64, i64::try_from(v).ok()),
                    (PhysicalType::Int128, Some(v)) => store!(i128, Some(v)),
                    _ => false,
                }
            }
            _ => {
                if parse_type.validate_utf8 && std::str::from_utf8(value).is_err() {
                    let force_error = !options.ignore_errors.value() && self.result.sniffing;
                    if force_error {
                        let position = self.result.last_position;
                        self.handle_unicode_error(self.result.cur_col_id, position)?;
                    }
                    let r = &mut self.result;
                    r.current_errors.insert(
                        CsvErrorType::InvalidEncoding,
                        r.cur_col_id,
                        r.chunk_col_id,
                        r.last_position,
                    );
                    let vector = &mut self.result.parse_chunk[chunk_col];
                    vector.values_mut::<StringT>()[row] = StringT::inline(b"");
                    self.result.cur_col_id += 1;
                    self.result.chunk_col_id += 1;
                    return Ok(());
                }
                let string = vector.add_string(value);
                vector.values_mut::<StringT>()[row] = string;
                true
            }
        };
        if !success {
            let ignore = self.base.state_machine.options.ignore_errors_only();
            let r = &mut self.result;
            r.current_errors.insert(
                CsvErrorType::CastError,
                r.cur_col_id,
                r.chunk_col_id,
                r.last_position,
            );
            if !ignore {
                r.current_errors
                    .modify_error_message_of_last_error(Self::cast_message(value, parse_type.type_id));
            }
        }
        self.result.cur_col_id += 1;
        self.result.chunk_col_id += 1;
        Ok(())
    }

    /// The type-mismatch handling of escaped values in non-VARCHAR columns.
    fn escaped_cast_error(&mut self, value: &[u8]) {
        let ignore = self.base.state_machine.options.ignore_errors_only();
        let r = &mut self.result;
        r.current_errors.insert(
            CsvErrorType::CastError,
            r.cur_col_id,
            r.chunk_col_id,
            r.last_position,
        );
        if !ignore {
            let type_id = r.parse_types[r.chunk_col_id as usize].type_id;
            r.current_errors
                .modify_error_message_of_last_error(Self::cast_message(value, type_id));
        }
    }

    /// `AddPossiblyEscapedValue`.
    fn add_possibly_escaped_value(&mut self, value: &[u8], empty: bool) -> Result<()> {
        if self.result.escaped {
            if self.result.projecting_columns
                && !self.result.projected_columns[self.result.cur_col_id as usize]
            {
                self.result.cur_col_id += 1;
                self.result.escaped = false;
                return Ok(());
            }
            if self.result.cur_col_id >= u64::from(self.result.number_of_columns) && !self.strict() {
                self.base.used_unstrictness = true;
                return Ok(());
            }
            if !self.handle_too_many_columns_error(value) {
                let chunk_col = self.result.chunk_col_id as usize;
                if self.result.parse_chunk[chunk_col].logical_type().id() == LogicalTypeId::Varchar {
                    let sm = &self.base.state_machine.dialect_options.state_machine_options;
                    let unescaped =
                        remove_escape(value, sm.escape.value(), sm.quote.value(), sm.strict_mode.value());
                    self.add_value_to_vector(&unescaped, false)?;
                } else {
                    self.escaped_cast_error(value);
                    self.result.cur_col_id += 1;
                    self.result.chunk_col_id += 1;
                }
            }
        } else if empty {
            self.add_value_to_vector(b"", false)?;
        } else {
            self.add_value_to_vector(value, false)?;
        }
        self.result.escaped = false;
        Ok(())
    }

    /// `AddQuotedValue`.
    fn add_quoted_value(&mut self, buffer_pos: u64) -> Result<()> {
        if !self.result.unquoted {
            let r = &mut self.result;
            r.current_errors.insert(
                CsvErrorType::UnterminatedQuotes,
                r.cur_col_id,
                r.chunk_col_id,
                r.last_position,
            );
        }
        let buffer = self.result.buffer.clone();
        let data = buffer.as_ref().map_or(&[][..], |buffer| buffer.data());
        let start = (self.result.quoted_position + 1) as usize;
        if buffer_pos <= self.result.quoted_position + 1 {
            let empty = buffer_pos < self.result.last_position.buffer_pos + 2;
            self.add_possibly_escaped_value(&[], empty)?;
            self.result.quoted = false;
            return Ok(());
        }
        let mut length = buffer_pos - self.result.quoted_position - 1;
        while length > 0 && self.result.ignore_empty_values && data[start + length as usize - 1] == b' ' {
            length -= 1;
        }
        if length == 0 {
            self.add_possibly_escaped_value(&[], true)?;
            self.result.quoted = false;
            return Ok(());
        }
        length -= 1;
        let empty = buffer_pos < self.result.last_position.buffer_pos + 2;
        let value = data[start..start + length as usize].to_vec();
        self.add_possibly_escaped_value(&value, empty)?;
        self.result.quoted = false;
        Ok(())
    }

    /// `HandleMultiDelimiter`.
    fn handle_multi_delimiter(&self, buffer_pos: u64) -> Result<u64> {
        let r = &self.result;
        if buffer_pos < r.last_position.buffer_pos + r.extra_delimiter_bytes {
            if buffer_pos == r.last_position.buffer_pos {
                return Ok(0);
            }
            return Err(Error::internal(format!(
                "Value size is lower than the number of extra delimiter bytes in the HandleMultiDelimiter(). buffer_pos = {buffer_pos}, last_position.buffer_pos = {}, extra_delimiter_bytes = {}",
                r.last_position.buffer_pos, r.extra_delimiter_bytes
            )));
        }
        Ok(buffer_pos - r.last_position.buffer_pos - r.extra_delimiter_bytes)
    }

    /// `StringValueResult::AddValue`.
    fn add_value_impl(&mut self, buffer_pos: u64) -> Result<()> {
        if self.result.last_position.buffer_pos > buffer_pos {
            return Ok(());
        }
        if self.result.quoted {
            self.add_quoted_value(buffer_pos - self.result.extra_delimiter_bytes)?;
        } else if self.result.escaped {
            let start = self.result.last_position.buffer_pos as usize;
            let value = self.result.buffer_bytes()[start..buffer_pos as usize].to_vec();
            self.add_possibly_escaped_value(&value, false)?;
        } else {
            let size = self.handle_multi_delimiter(buffer_pos)?;
            let start = self.result.last_position.buffer_pos as usize;
            let buffer = self.result.buffer.clone();
            let data = buffer.as_ref().map_or(&[][..], |buffer| buffer.data());
            self.add_value_to_vector(&data[start..start + size as usize], false)?;
        }
        self.result.last_position.buffer_pos = buffer_pos + 1;
        Ok(())
    }

    /// `HandleUnicodeError`.
    fn handle_unicode_error(&mut self, col_idx: u64, error_position: LinePosition) -> Result<()> {
        let mut first_nl = false;
        let print = self.result.print_error_line(&self.base.state_machine);
        let borked_line = self.result.current_line_position.reconstruct_current_line(
            &mut first_nl,
            &self.result.buffer_handles,
            print,
        );
        let lines_per_batch = LinesPerBoundary::new(self.base.iterator.boundary_idx(), self.base.lines_read);
        let requested = self.result.requested_size;
        let begin = self.result.current_line_position.begin;
        let byte_position = if begin == error_position {
            error_position.global_position(requested, first_nl)
        } else {
            error_position.global_position(requested, false)
        };
        let error = CsvError::invalid_utf8(
            &self.base.state_machine.options,
            col_idx,
            lines_per_batch,
            borked_line,
            begin.global_position(requested, first_nl),
            Some(byte_position),
            &self.result.path,
        );
        self.base.error_handler.error(error, true)
    }

    /// `LineError::HandleErrors`: reports the errors of the current line; returns whether
    /// the line had errors.
    #[allow(clippy::too_many_lines)]
    fn handle_errors(&mut self) -> Result<bool> {
        let mut skip_sniffing = self
            .result
            .current_errors
            .current_errors
            .iter()
            .any(|error| error.error_type == CsvErrorType::InvalidEncoding);
        skip_sniffing = self.result.sniffing && skip_sniffing;
        let errors = &self.result.current_errors;
        if (errors.ignore_errors || skip_sniffing)
            && errors.is_error_in_line
            && !self.result.figure_out_new_line
        {
            self.remove_last_line();
            self.result.current_errors.reset();
            return Ok(true);
        }
        let current_errors = self.result.current_errors.current_errors.clone();
        for cur_error in &current_errors {
            let lines_per_batch =
                LinesPerBoundary::new(self.base.iterator.boundary_idx(), self.base.lines_read);
            let mut first_nl = false;
            let print = self.result.print_error_line(&self.base.state_machine);
            let borked_line = self.result.current_line_position.reconstruct_current_line(
                &mut first_nl,
                &self.result.buffer_handles,
                print,
            );
            let col_idx = cur_error.col_idx;
            let line_pos = cur_error.error_position;
            let requested = self.result.requested_size;
            let begin = self.result.current_line_position.begin;
            let row_byte_position = begin.global_position(requested, first_nl);
            let byte_position = if begin == line_pos {
                line_pos.global_position(requested, first_nl)
            } else {
                line_pos.global_position(requested, false)
            };
            let options = &self.base.state_machine.options;
            let path = &self.result.path;
            let csv_error = match cur_error.error_type {
                CsvErrorType::TooManyColumns | CsvErrorType::TooFewColumns => {
                    CsvError::incorrect_column_amount_error(
                        options,
                        col_idx,
                        lines_per_batch,
                        borked_line,
                        row_byte_position,
                        Some(byte_position),
                        path,
                    )
                }
                CsvErrorType::InvalidEncoding => {
                    let error = CsvError::invalid_utf8(
                        options,
                        col_idx,
                        lines_per_batch,
                        borked_line,
                        row_byte_position,
                        Some(byte_position),
                        path,
                    );
                    if let Some(file_scan) = &self.csv_file_scan
                        && let Some(ty) = file_scan.file_types.get(col_idx as usize)
                        && !can_directly_cast(ty, self.result.icu_loaded)
                    {
                        self.result.number_of_rows -= 1;
                    }
                    error
                }
                CsvErrorType::UnterminatedQuotes => CsvError::unterminated_quotes_error(
                    options,
                    col_idx,
                    lines_per_batch,
                    borked_line,
                    row_byte_position,
                    Some(byte_position),
                    path,
                ),
                CsvErrorType::CastError => {
                    let column_name = self
                        .result
                        .names
                        .get(cur_error.col_idx as usize)
                        .cloned()
                        .unwrap_or_default();
                    let type_id = if cur_error.col_idx < u64::from(self.result.number_of_columns) {
                        self.result
                            .parse_types
                            .get(cur_error.chunk_idx as usize)
                            .map_or(LogicalTypeId::Invalid, |info| info.type_id)
                    } else {
                        LogicalTypeId::Invalid
                    };
                    CsvError::cast_error(
                        options,
                        &column_name,
                        &cur_error.error_message,
                        cur_error.col_idx,
                        borked_line,
                        lines_per_batch,
                        row_byte_position,
                        Some(byte_position),
                        type_id,
                        path,
                    )
                }
                CsvErrorType::MaximumLineSize => {
                    CsvError::line_size_error(options, lines_per_batch, borked_line, row_byte_position, path)
                }
                CsvErrorType::InvalidState => CsvError::invalid_state(
                    options,
                    col_idx,
                    lines_per_batch,
                    borked_line,
                    row_byte_position,
                    Some(byte_position),
                    path,
                ),
                _ => return Err(Error::invalid_input("CSV Error not allowed when inserting row")),
            };
            self.base.error_handler.error(csv_error, self.result.try_row)?;
        }
        if self.result.current_errors.is_error_in_line && self.result.current_errors.scan_id != LINE_FINDER_ID
        {
            if self.result.sniffing {
                self.remove_last_line();
            } else {
                let row = self.result.number_of_rows as u64;
                self.result.borked_rows.insert(row);
                self.result.cur_col_id = 0;
                self.result.chunk_col_id = 0;
            }
            self.result.current_errors.reset();
            return Ok(true);
        }
        Ok(false)
    }

    /// `NullPaddingQuotedNewlineCheck`.
    fn null_padding_quoted_newline_check(&self) -> Result<()> {
        if self.base.state_machine.options.null_padding
            && self.base.iterator.is_boundary_set()
            && self.result.quoted_new_line
        {
            let lines_per_batch =
                LinesPerBoundary::new(self.base.iterator.boundary_idx(), self.base.lines_read);
            let error = CsvError::null_padding_fail(
                &self.base.state_machine.options,
                lines_per_batch,
                &self.result.path,
            );
            self.base.error_handler.error(error, true)?;
        }
        Ok(())
    }

    /// `AddRowInternal`.
    #[allow(clippy::too_many_lines)]
    fn add_row_internal(&mut self) -> Result<bool> {
        let current_line_start = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos,
            self.result.buffer_size,
        );
        let current_line_size = current_line_start.distance_from(&self.result.current_line_position.end);
        if self.result.store_line_size {
            self.base.error_handler.new_max_line_size(current_line_size);
        }
        self.result.current_line_position.begin = self.result.current_line_position.end;
        self.result.current_line_position.end = current_line_start;
        if current_line_size > self.base.state_machine.options.maximum_line_size.value() {
            let r = &mut self.result;
            r.current_errors
                .insert(CsvErrorType::MaximumLineSize, 1, r.chunk_col_id, r.last_position);
        }
        if !self.base.state_machine.options.null_padding {
            let r = &mut self.result;
            for col_idx in r.cur_col_id..u64::from(r.number_of_columns) {
                r.current_errors.insert(
                    CsvErrorType::TooFewColumns,
                    col_idx.wrapping_sub(1),
                    r.chunk_col_id,
                    r.last_position,
                );
            }
        }
        let chunk_col_id_before = self.result.chunk_col_id;
        if self.handle_errors()? {
            let row = self.result.number_of_rows as u64;
            if self.result.borked_rows.contains(&row) {
                for column in chunk_col_id_before as usize..self.result.parse_chunk.len() {
                    self.result.validity_set_invalid(column, row as usize);
                }
            }
            let position = self.result.current_line_position;
            self.result.line_positions_per_row.insert(row, position);
            self.result.number_of_rows += 1;
            return Ok(self.result.number_of_rows as u64 >= self.result.result_size);
        }
        self.null_padding_quoted_newline_check()?;
        self.result.quoted_new_line = false;
        let number_of_columns = u64::from(self.result.number_of_columns);
        if self.result.cur_col_id < number_of_columns {
            if self.result.null_padding {
                while self.result.cur_col_id < number_of_columns {
                    let cur = self.result.cur_col_id as usize;
                    let empty = self
                        .base
                        .state_machine
                        .options
                        .force_not_null
                        .get(cur)
                        .copied()
                        .unwrap_or(false);
                    if self.result.projecting_columns && !self.result.projected_columns[cur] {
                        self.result.cur_col_id += 1;
                        continue;
                    }
                    let row = self.result.number_of_rows as usize;
                    let chunk_col = self.result.chunk_col_id as usize;
                    if empty {
                        self.result.parse_chunk[chunk_col].values_mut::<StringT>()[row] =
                            StringT::inline(b"");
                    } else {
                        self.result.validity_set_invalid(chunk_col, row);
                    }
                    self.result.cur_col_id += 1;
                    self.result.chunk_col_id += 1;
                }
            } else {
                if !self.base.state_machine.options.ignore_errors_only() {
                    let mut first_nl = false;
                    let print = self.result.print_error_line(&self.base.state_machine);
                    let borked_line = self.result.current_line_position.reconstruct_current_line(
                        &mut first_nl,
                        &self.result.buffer_handles,
                        print,
                    );
                    let lines_per_batch =
                        LinesPerBoundary::new(self.base.iterator.boundary_idx(), self.base.lines_read);
                    let requested = self.result.requested_size;
                    let begin = self.result.current_line_position.begin;
                    let last = self.result.last_position;
                    let byte_position = if begin == last {
                        last.global_position(requested, first_nl)
                    } else {
                        last.global_position(requested, false)
                    };
                    let error = CsvError::incorrect_column_amount_error(
                        &self.base.state_machine.options,
                        self.result.cur_col_id.wrapping_sub(1),
                        lines_per_batch,
                        borked_line,
                        begin.global_position(requested, first_nl),
                        Some(byte_position),
                        &self.result.path,
                    );
                    self.base.error_handler.error(error, self.result.try_row)?;
                }
                self.remove_last_line();
            }
        }
        let row = self.result.number_of_rows as u64;
        let position = self.result.current_line_position;
        self.result.line_positions_per_row.insert(row, position);
        self.result.cur_col_id = 0;
        self.result.chunk_col_id = 0;
        self.result.number_of_rows += 1;
        Ok(self.result.number_of_rows as u64 >= self.result.result_size)
    }

    /// `RemoveLastLine`.
    fn remove_last_line(&mut self) {
        let row = self.result.number_of_rows;
        if row >= 0 {
            for column in 0..self.result.chunk_col_id as usize {
                self.result.parse_chunk[column]
                    .validity_mut()
                    .set_valid(row as usize);
            }
        }
        self.result.cur_col_id = 0;
        self.result.chunk_col_id = 0;
        self.result.number_of_rows -= 1;
    }

    /// `StringValueResult::AddRow`.
    fn add_row_impl(&mut self, buffer_pos: u64) -> Result<bool> {
        if self.result.last_position.buffer_pos <= buffer_pos {
            if self.result.quoted {
                self.add_quoted_value(buffer_pos)?;
            } else {
                let start = self.result.last_position.buffer_pos as usize;
                let buffer = self.result.buffer.clone();
                let data = buffer.as_ref().map_or(&[][..], |buffer| buffer.data());
                let value = &data[start..buffer_pos as usize];
                if self.result.escaped {
                    let value = value.to_vec();
                    self.add_possibly_escaped_value(&value, value.is_empty())?;
                } else {
                    self.add_value_to_vector(value, false)?;
                }
            }
            if self.base.new_line() == NewLineIdentifier::CarryOn {
                if self.base.states.states[1] == CsvState::RecordSeparator {
                    self.result.last_position.buffer_pos = buffer_pos + 1;
                } else {
                    self.result.last_position.buffer_pos = buffer_pos + 2;
                }
            } else {
                self.result.last_position.buffer_pos = buffer_pos + 1;
            }
        }
        self.add_row_internal()
    }

    /// `StringValueResult::InvalidState`.
    fn invalid_state_impl(&mut self) -> Result<()> {
        if self.result.quoted {
            let r = &mut self.result;
            r.current_errors.insert(
                CsvErrorType::UnterminatedQuotes,
                r.cur_col_id,
                r.chunk_col_id,
                r.last_position,
            );
            return Ok(());
        }
        let lines_per_batch = LinesPerBoundary::new(self.base.iterator.boundary_idx(), self.base.lines_read);
        let mut first_nl = false;
        let print = self.result.print_error_line(&self.base.state_machine);
        let borked_line = self.result.current_line_position.reconstruct_current_line(
            &mut first_nl,
            &self.result.buffer_handles,
            print,
        );
        let requested = self.result.requested_size;
        let error = CsvError::invalid_state(
            &self.base.state_machine.options,
            self.result.cur_col_id,
            lines_per_batch,
            borked_line,
            self.result
                .current_line_position
                .begin
                .global_position(requested, first_nl),
            Some(self.result.last_position.global_position(requested, first_nl)),
            &self.result.path,
        );
        self.base.error_handler.error(error, true)
    }

    /// `StringValueResult::EmptyLine`.
    fn empty_line_impl(&mut self, _buffer_pos: u64) -> bool {
        self.result.last_position = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos + 1,
            self.result.buffer_size,
        );
        if self.base.states.is_carriage_return() && self.base.new_line() == NewLineIdentifier::CarryOn {
            self.result.last_position.buffer_pos += 1;
        }
        if self.result.number_of_columns == 1 {
            let null_count = self.result.null_str.iter().filter(|null| null.is_empty()).count();
            for _ in 0..null_count {
                let empty = self
                    .base
                    .state_machine
                    .options
                    .force_not_null
                    .first()
                    .copied()
                    .unwrap_or(false);
                let row = self.result.number_of_rows as usize;
                if empty {
                    self.result.parse_chunk[0].values_mut::<StringT>()[row] = StringT::inline(b"");
                } else {
                    self.result.validity_set_invalid(0, row);
                }
                self.result.number_of_rows += 1;
            }
            if self.result.number_of_rows as u64 >= self.result.result_size {
                return true;
            }
        }
        false
    }

    /// `StringValueResult::UnsetComment`.
    fn unset_comment_impl(&mut self, buffer_pos: u64) -> Result<bool> {
        let mut done = false;
        if self.result.last_position.buffer_pos < self.result.position_before_comment {
            let start = self.result.last_position.buffer_pos as usize;
            let end = self.result.position_before_comment as usize;
            let all_empty = self.result.buffer_bytes()[start..end]
                .iter()
                .all(|&byte| byte == b' ');
            if !all_empty {
                done = self.add_row_impl(self.result.position_before_comment)?;
            }
        } else if self.result.cur_col_id != 0 {
            done = self.add_row_impl(self.result.position_before_comment)?;
        }
        if self.result.number_of_rows == 0 {
            self.result.first_line_is_comment = true;
        }
        self.result.comment = false;
        if self.base.new_line() == NewLineIdentifier::CarryOn {
            self.result.last_position.buffer_pos = buffer_pos.wrapping_add(2);
        } else {
            self.result.last_position.buffer_pos = buffer_pos.wrapping_add(1);
        }
        let current_line_start = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos,
            self.result.buffer_size,
        );
        self.result.current_line_position.begin = self.result.current_line_position.end;
        self.result.current_line_position.end = current_line_start;
        self.result.cur_col_id = 0;
        self.result.chunk_col_id = 0;
        Ok(done)
    }

    // ------------------------------------------------------------------------------------
    // The scanner
    // ------------------------------------------------------------------------------------

    /// `StringValueResult::Reset`.
    fn reset_result(&mut self) {
        if self.result.number_of_rows == 0 {
            return;
        }
        self.result.number_of_rows = 0;
        self.result.cur_col_id = 0;
        self.result.chunk_col_id = 0;
        self.result.reset_vectors();
        let current = self
            .result
            .buffer_handles
            .get(&self.base.iterator.buffer_idx())
            .cloned();
        self.result.buffer_handles.clear();
        let mut actual_size = 0;
        if let Some(current) = current {
            actual_size = current.actual_size();
            self.result.buffer_handles.insert(current.buffer_idx(), current);
        }
        self.result.current_errors.reset();
        self.result.borked_rows.clear();
        self.result.line_positions_per_row.clear();
        self.result.current_line_position.begin = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos,
            actual_size,
        );
        self.result.current_line_position.end = self.result.current_line_position.begin;
    }

    /// `ParseChunk`: parses up to a chunk of rows.
    pub fn parse_chunk(&mut self) -> Result<()> {
        self.reset_result();
        if self.base.iterator.done {
            return Ok(());
        }
        if !self.base.initialized {
            self.initialize();
            self.base.initialized = true;
        }
        if !self.base.iterator.done && self.base.cur_buffer_handle.is_some() {
            self.process()?;
        }
        self.finalize_chunk_process()
    }

    /// The parsed rows (`ToChunk`).
    pub fn parsed_rows(&self) -> Result<usize> {
        usize::try_from(self.result.number_of_rows).map_err(|_| {
            Error::internal(
                "CSVScanner: ToChunk() function. Has a negative number of rows, this indicates an issue with the error handler.",
            )
        })
    }

    /// `Initialize`.
    fn initialize(&mut self) {
        self.base.states.initialize(CsvState::NotSet);
        let sm_options = &self.base.state_machine.options;
        if self.result.result_size != 1
            && !(self.base.sniffing
                && sm_options.null_padding
                && !sm_options.dialect_options.skip_rows.is_set_by_user())
        {
            self.set_start();
        } else {
            self.start_pos = self.base.iterator.global_current_pos();
        }
        self.result.last_position = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos,
            self.base.current_buffer_size(),
        );
        self.result.current_line_position.begin = self.result.last_position;
        self.result.current_line_position.end = self.result.current_line_position.begin;
    }

    /// `SkipUntilState`.
    fn skip_until_state(
        &self,
        initial_state: CsvState,
        until_state: CsvState,
        current_iterator: &mut CsvIterator,
        quoted: &mut bool,
    ) -> bool {
        let Some(strict) = &self.state_machine_strict else {
            return false;
        };
        let Some(handle) = &self.base.cur_buffer_handle else {
            return false;
        };
        let buffer = handle.data();
        let transitions = &strict.transition_array;
        let quote = strict.dialect_options.state_machine_options.quote.value();
        let mut current_state = CsvStates::default();
        current_state.initialize(initial_state);
        let mut first_column = true;
        let to_pos = current_iterator.end_pos().min(buffer.len() as u64);
        while current_iterator.pos.buffer_pos < to_pos {
            let byte = buffer[current_iterator.pos.buffer_pos as usize];
            current_iterator.pos.buffer_pos += 1;
            strict.transition(&mut current_state, byte);
            if current_state.is_state(CsvState::Standard) || current_state.is_state(CsvState::StandardNewline)
            {
                let mut position = current_iterator.pos.buffer_pos;
                while position + 8 < to_pos {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&buffer[position as usize..position as usize + 8]);
                    let value = u64::from_ne_bytes(bytes);
                    if crate::state_machine::contains_zero_byte(
                        (value ^ transitions.delimiter)
                            & (value ^ transitions.new_line)
                            & (value ^ transitions.carriage_return)
                            & (value ^ transitions.comment),
                    ) {
                        break;
                    }
                    position += 8;
                }
                while position + 1 < to_pos && transitions.skip_standard[buffer[position as usize] as usize] {
                    position += 1;
                }
                current_iterator.pos.buffer_pos = position;
            }
            if current_state.is_state(CsvState::Quoted) {
                let mut position = current_iterator.pos.buffer_pos;
                while position + 8 < to_pos {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&buffer[position as usize..position as usize + 8]);
                    let value = u64::from_ne_bytes(bytes);
                    if crate::state_machine::contains_zero_byte(
                        (value ^ transitions.quote) & (value ^ transitions.escape),
                    ) {
                        break;
                    }
                    position += 8;
                }
                while position + 1 < to_pos && transitions.skip_quoted[buffer[position as usize] as usize] {
                    position += 1;
                }
                current_iterator.pos.buffer_pos = position;
            }
            if (current_state.is_state(CsvState::Delimiter)
                || current_state.is_state(CsvState::CarriageReturn)
                || current_state.is_state(CsvState::RecordSeparator))
                && first_column
                && current_iterator.pos.buffer_pos >= 1
                && buffer[current_iterator.pos.buffer_pos as usize - 1] == quote
            {
                *quoted = true;
            }
            if current_state.was_state(CsvState::Delimiter) {
                first_column = false;
            }
            if current_state.is_state(until_state) {
                return true;
            }
            if current_state.is_state(CsvState::Invalid) {
                return false;
            }
        }
        false
    }

    /// `FirstValueEndsOnQuote`.
    fn first_value_ends_on_quote(&self, mut iterator: CsvIterator) -> bool {
        let Some(handle) = &self.base.cur_buffer_handle else {
            return false;
        };
        let buffer = handle.data();
        let mut current_state = CsvStates::default();
        current_state.initialize(CsvState::Standard);
        let to_pos = iterator.end_pos().min(buffer.len() as u64);
        let quote = self
            .base
            .state_machine
            .dialect_options
            .state_machine_options
            .quote
            .value();
        while iterator.pos.buffer_pos < to_pos {
            let byte = buffer[iterator.pos.buffer_pos as usize];
            iterator.pos.buffer_pos += 1;
            self.base.state_machine.transition(&mut current_state, byte);
            if current_state.is_state(CsvState::Delimiter)
                || current_state.is_state(CsvState::CarriageReturn)
                || current_state.is_state(CsvState::RecordSeparator)
            {
                return iterator.pos.buffer_pos >= 2 && buffer[iterator.pos.buffer_pos as usize - 2] == quote;
            }
        }
        false
    }

    /// `IsRowValid`.
    fn is_row_valid(&self, current_iterator: &mut CsvIterator) -> bool {
        if self.base.iterator.pos.buffer_pos == self.base.current_buffer_size() {
            return false;
        }
        let Some(strict) = &self.state_machine_strict else {
            return false;
        };
        let scanner = Self::new(
            LINE_FINDER_ID,
            Arc::clone(&self.base.buffer_manager),
            Arc::clone(strict),
            Arc::new(CsvErrorHandler::default()),
            self.csv_file_scan.clone(),
            false,
            *current_iterator,
            1,
        );
        let Ok(mut scan_finder) = scanner else {
            return false;
        };
        if scan_finder.parse_chunk().is_err() {
            return false;
        }
        current_iterator.pos = scan_finder.iterator_position();
        let errors = &scan_finder.result.current_errors;
        let has_error = errors.has_error()
            && (errors.size() != 1 || !errors.has_error_type(CsvErrorType::MaximumLineSize));
        (scan_finder.result.number_of_rows == 1 || scan_finder.result.first_line_is_comment)
            && !has_error
            && scan_finder.result.borked_rows.is_empty()
    }

    /// `TryRow`: (`is_valid`, `start_pos`, `end_buffer_idx`, `end_pos`, `last_state_quote`).
    fn try_row(&self, state: CsvState, start_pos: u64, end_pos: u64) -> (bool, u64, u64, u64, bool) {
        let mut current_iterator = self.base.iterator;
        current_iterator.set_start(start_pos);
        current_iterator.set_end(end_pos);
        let mut quoted = false;
        if self.skip_until_state(
            state,
            CsvState::RecordSeparator,
            &mut current_iterator,
            &mut quoted,
        ) {
            let iterator_start = current_iterator;
            let current_pos = current_iterator.pos.buffer_pos;
            current_iterator.set_end(self.base.iterator.end_pos());
            if self.is_row_valid(&mut current_iterator) {
                if !quoted {
                    quoted = self.first_value_ends_on_quote(iterator_start);
                }
                return (
                    true,
                    current_pos,
                    current_iterator.pos.buffer_idx,
                    current_iterator.pos.buffer_pos,
                    quoted,
                );
            }
        }
        (
            false,
            current_iterator.pos.buffer_pos,
            current_iterator.pos.buffer_idx,
            current_iterator.pos.buffer_pos,
            quoted,
        )
    }

    /// `SetStart`: positions a boundary scanner at the start of a line.
    fn set_start(&mut self) {
        self.start_pos = self.base.iterator.global_current_pos();
        if self.base.iterator.first_one {
            if self.result.store_line_size {
                self.base
                    .error_handler
                    .new_max_line_size(self.base.iterator.pos.buffer_pos);
            }
            return;
        }
        let actual_size = self.base.current_buffer_size();
        if self.base.iterator.end_pos() > actual_size {
            self.base.iterator.set_end(actual_size);
        }
        if self.state_machine_strict.is_none() {
            let mut state_options = self.base.state_machine.state_machine_options.clone();
            if !state_options.strict_mode.is_set_by_user() {
                state_options.strict_mode = crate::options::CsvOption::new(true);
            }
            self.state_machine_strict = Some(Arc::new(CsvStateMachine::with_transitions(
                cached_state_machine(&state_options),
                &self.base.state_machine.options,
            )));
        }
        let mut best = self.try_row(
            CsvState::StandardNewline,
            self.base.iterator.pos.buffer_pos,
            self.base.iterator.end_pos(),
        );
        let quote = self
            .base
            .state_machine
            .dialect_options
            .state_machine_options
            .quote
            .value();
        if quote != 0 {
            let mut end_pos = self.base.iterator.end_pos();
            if best.0 && best.2 == self.base.iterator.pos.buffer_idx {
                end_pos = best.3;
            }
            let quoted_row = self.try_row(CsvState::Quoted, self.base.iterator.pos.buffer_pos, end_pos);
            if quoted_row.0 && (!best.0 || best.4) {
                best = quoted_row;
            }
            if !best.0 && !quoted_row.0 && best.1 < quoted_row.1 {
                best = quoted_row;
            }
            if quoted_row.0 && quoted_row.1 < best.1 {
                best = quoted_row;
            }
        }
        if !best.0 && quote != 0 {
            let escape_row = self.try_row(
                CsvState::Escape,
                self.base.iterator.pos.buffer_pos,
                self.base.iterator.end_pos(),
            );
            if escape_row.0 || best.1 < escape_row.1 {
                best = escape_row;
            }
        }
        let is_last = self
            .base
            .cur_buffer_handle
            .as_ref()
            .is_some_and(|handle| handle.is_last_buffer);
        if best.0 {
            self.base.iterator.pos.buffer_pos = best.1;
            if best.1 >= actual_size && is_last {
                self.base.iterator.done = true;
            }
        } else if best.1 >= actual_size && is_last {
            self.base.iterator.pos.buffer_pos = best.1;
            self.base.iterator.done = true;
        } else {
            let mut mock = false;
            let mut iterator = self.base.iterator;
            let found = self.skip_until_state(
                CsvState::StandardNewline,
                CsvState::RecordSeparator,
                &mut iterator,
                &mut mock,
            );
            self.base.iterator = iterator;
            if !found {
                self.base.iterator.check_if_done();
            }
        }
        self.result.last_position = LinePosition::new(
            self.base.iterator.pos.buffer_idx,
            self.base.iterator.pos.buffer_pos,
            self.result.buffer_size,
        );
        self.start_pos = self.base.iterator.global_current_pos();
    }

    /// `ProcessExtraRow`: finishes the row a boundary scanner ends in.
    fn process_extra_row(&mut self) -> Result<()> {
        self.null_padding_quoted_newline_check()?;
        let Some(handle) = self.base.cur_buffer_handle.clone() else {
            return Ok(());
        };
        let buffer = handle.data();
        let to_pos = handle.actual_size();
        let transitions = Arc::clone(&self.base.state_machine.transition_array);
        while self.base.iterator.pos.buffer_pos < to_pos {
            let current = self.base.iterator.pos.buffer_pos;
            self.base
                .state_machine
                .transition(&mut self.base.states, buffer[current as usize]);
            let states = self.base.states;
            match states.states[1] {
                CsvState::Invalid => {
                    self.invalid_state_impl()?;
                    self.base.iterator.pos.buffer_pos += 1;
                    return Ok(());
                }
                CsvState::RecordSeparator => {
                    if states.states[0] == CsvState::RecordSeparator {
                        self.empty_line_impl(current);
                        self.base.iterator.pos.buffer_pos += 1;
                        self.base.lines_read += 1;
                        return Ok(());
                    } else if states.states[0] != CsvState::CarriageReturn {
                        if self.result.comment {
                            self.unset_comment_impl(current)?;
                        } else {
                            self.add_row_impl(current)?;
                        }
                        self.base.iterator.pos.buffer_pos += 1;
                        self.base.lines_read += 1;
                        return Ok(());
                    }
                    self.base.lines_read += 1;
                    self.base.iterator.pos.buffer_pos += 1;
                }
                CsvState::CarriageReturn => {
                    if states.states[0] == CsvState::RecordSeparator {
                        self.empty_line_impl(current);
                    } else if self.result.comment {
                        self.unset_comment_impl(current)?;
                    } else {
                        self.add_row_impl(current)?;
                    }
                    self.base.iterator.pos.buffer_pos += 1;
                    self.base.lines_read += 1;
                    return Ok(());
                }
                CsvState::Delimiter => {
                    self.add_value_impl(current)?;
                    self.base.iterator.pos.buffer_pos += 1;
                }
                CsvState::Quoted => {
                    if states.states[0] == CsvState::Unquoted {
                        self.result.escaped = true;
                    }
                    self.set_quoted(current);
                    let mut position = current + 1;
                    while position + 1 < to_pos && transitions.skip_quoted[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    self.base.iterator.pos.buffer_pos = position;
                }
                CsvState::Escape | CsvState::UnquotedEscape | CsvState::EscapedReturn => {
                    self.result.escaped = true;
                    self.base.iterator.pos.buffer_pos += 1;
                }
                CsvState::Standard => {
                    let mut position = current + 1;
                    while position + 1 < to_pos
                        && transitions.skip_standard[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    self.base.iterator.pos.buffer_pos = position;
                }
                CsvState::Unquoted => {
                    self.set_unquoted();
                    self.base.iterator.pos.buffer_pos += 1;
                }
                CsvState::Comment => {
                    self.set_comment(current);
                    let mut position = current + 1;
                    while position + 1 < to_pos
                        && transitions.skip_comment[buffer[position as usize] as usize]
                    {
                        position += 1;
                    }
                    self.base.iterator.pos.buffer_pos = position;
                }
                CsvState::QuotedNewLine => {
                    self.result.quoted_new_line = true;
                    self.null_padding_quoted_newline_check()?;
                    self.base.iterator.pos.buffer_pos += 1;
                }
                _ => self.base.iterator.pos.buffer_pos += 1,
            }
        }
        Ok(())
    }

    /// `ProcessOverBufferValue`: the value that spans the previous and the current buffer.
    #[allow(clippy::too_many_lines)]
    fn process_over_buffer_value(&mut self) -> Result<()> {
        let Some(previous) = self.previous_buffer_handle.clone() else {
            return Ok(());
        };
        let Some(current) = self.base.cur_buffer_handle.clone() else {
            return Ok(());
        };
        if self.result.last_position.buffer_pos != previous.actual_size() {
            self.base.states.initialize(CsvState::NotSet);
        }
        let sm = Arc::clone(&self.base.state_machine);
        let escape_set = sm.dialect_options.state_machine_options.escape.value() != 0;
        let mut over_buffer: Vec<u8> = Vec::new();
        let previous_data = previous.data();
        let mut j = 0u64;
        self.result.quoted = false;
        let start = self.result.last_position.buffer_pos.min(previous.actual_size());
        for i in start..previous.actual_size() {
            sm.transition(&mut self.base.states, previous_data[i as usize]);
            let states = self.base.states;
            if states.empty_line() || states.is_current_new_row() {
                continue;
            }
            if states.new_row() || states.new_value() {
                break;
            } else if !self.result.comment {
                over_buffer.push(previous_data[i as usize]);
            }
            if states.is_quoted() {
                self.set_quoted(j);
            }
            if states.is_unquoted() {
                self.set_unquoted();
            }
            if states.is_escaped() && escape_set {
                self.result.escaped = true;
            }
            if states.is_comment() {
                self.set_comment(j);
            }
            if states.is_invalid() {
                self.invalid_state_impl()?;
            }
            j += 1;
        }
        let current_data = current.data();
        if over_buffer.is_empty()
            && sm.dialect_options.state_machine_options.new_line.value() == NewLineIdentifier::CarryOn
        {
            if self.base.iterator.is_boundary_set() {
                while self.base.iterator.pos.buffer_pos < current.actual_size()
                    && matches!(
                        current_data[self.base.iterator.pos.buffer_pos as usize],
                        b'\n' | b'\r'
                    )
                {
                    if current_data[self.base.iterator.pos.buffer_pos as usize] == b'\r' {
                        if self.result.last_position.buffer_pos <= previous.actual_size() {
                            self.add_value_impl(previous.actual_size())?;
                            if self.result.comment {
                                self.unset_comment_impl(self.base.iterator.pos.buffer_pos)?;
                            } else {
                                self.add_row_impl(previous.actual_size())?;
                            }
                            let byte = current_data[self.base.iterator.pos.buffer_pos as usize];
                            self.base.iterator.pos.buffer_pos += 1;
                            sm.transition(&mut self.base.states, byte);
                            while self.base.iterator.pos.buffer_pos < current.actual_size()
                                && matches!(
                                    current_data[self.base.iterator.pos.buffer_pos as usize],
                                    b'\r' | b'\n'
                                )
                            {
                                let byte = current_data[self.base.iterator.pos.buffer_pos as usize];
                                self.base.iterator.pos.buffer_pos += 1;
                                sm.transition(&mut self.base.states, byte);
                            }
                            return Ok(());
                        }
                    } else if self.base.iterator.pos.buffer_pos + 1 == current.actual_size() {
                        return Ok(());
                    }
                    let byte = current_data[self.base.iterator.pos.buffer_pos as usize];
                    sm.transition(&mut self.base.states, byte);
                    self.base.iterator.pos.buffer_pos += 1;
                }
            } else if current_data.get(self.base.iterator.pos.buffer_pos as usize) == Some(&b'\n') {
                self.base.iterator.pos.buffer_pos += 1;
            }
        }
        // The second buffer.
        while self.base.iterator.pos.buffer_pos < current.actual_size() {
            let byte = current_data[self.base.iterator.pos.buffer_pos as usize];
            sm.transition(&mut self.base.states, byte);
            let states = self.base.states;
            if states.empty_line() {
                if sm.dialect_options.num_cols == 1 {
                    break;
                }
                self.base.iterator.pos.buffer_pos += 1;
                continue;
            }
            if states.new_row() || states.new_value() {
                break;
            } else if !self.result.comment && !states.is_comment() {
                over_buffer.push(byte);
            }
            if states.is_quoted() {
                self.set_quoted(j);
            }
            if states.is_comment() {
                self.set_comment(j);
            }
            if states.is_escaped() && escape_set {
                self.result.escaped = true;
            }
            if states.is_invalid() {
                self.invalid_state_impl()?;
            }
            j += 1;
            self.base.iterator.pos.buffer_pos += 1;
        }
        let mut skip_value = false;
        if self.result.projecting_columns
            && (self.result.cur_col_id as usize) < self.result.projected_columns.len()
            && !self.result.projected_columns[self.result.cur_col_id as usize]
            && self.result.cur_col_id != u64::from(self.result.number_of_columns)
        {
            self.result.cur_col_id += 1;
            skip_value = true;
        }
        let states = self.base.states;
        if skip_value {
            if states.empty_line() && sm.dialect_options.num_cols == 1 {
                self.empty_line_impl(self.base.iterator.pos.buffer_pos);
            }
        } else {
            let strict = sm.dialect_options.state_machine_options.strict_mode.value();
            let escape = sm.dialect_options.state_machine_options.escape.value();
            let quote = sm.dialect_options.state_machine_options.quote.value();
            let value = if self.result.quoted && !self.result.comment {
                let quoted_position = self.result.quoted_position as usize;
                let mut length = (over_buffer.len() as u64).saturating_sub(1 + self.result.quoted_position);
                while length > 0
                    && self.result.ignore_empty_values
                    && over_buffer[quoted_position + length as usize] == b' '
                {
                    length -= 1;
                }
                let mut quoted_value = over_buffer[quoted_position.min(over_buffer.len())
                    ..(quoted_position + length as usize).min(over_buffer.len())]
                    .to_vec();
                if self.result.escaped && !self.handle_too_many_columns_error(&over_buffer.clone()) {
                    let chunk_col = self.result.chunk_col_id as usize;
                    if self.result.parse_chunk[chunk_col].logical_type().id() != LogicalTypeId::Varchar {
                        self.escaped_cast_error(&over_buffer.clone());
                        return Ok(());
                    }
                    let end = (quoted_position + over_buffer.len().saturating_sub(2)).min(over_buffer.len());
                    quoted_value =
                        remove_escape(&over_buffer[quoted_position.min(end)..end], escape, quote, strict);
                }
                quoted_value
            } else {
                let mut plain = over_buffer.clone();
                if self.result.escaped {
                    if self.result.cur_col_id >= u64::from(self.result.number_of_columns) && !strict {
                        self.base.used_unstrictness = true;
                    } else if !self.handle_too_many_columns_error(&over_buffer.clone()) {
                        let chunk_col = self.result.chunk_col_id as usize;
                        if self.result.parse_chunk[chunk_col].logical_type().id() != LogicalTypeId::Varchar {
                            self.escaped_cast_error(&over_buffer.clone());
                            return Ok(());
                        }
                        plain = remove_escape(&over_buffer, escape, quote, strict);
                    }
                }
                plain
            };
            if states.empty_line() && sm.dialect_options.num_cols == 1 {
                self.empty_line_impl(self.base.iterator.pos.buffer_pos);
            } else if !states.is_not_set() && (!self.result.comment || !value.is_empty()) {
                let mut value_size = value.len();
                let delimiter = sm.dialect_options.state_machine_options.delimiter.get();
                if states.is_delimiter() && !delimiter.is_empty() {
                    let extra = delimiter.len() - 1;
                    if extra > value_size {
                        return Err(Error::internal(
                            "Value size is lower than the number of extra delimiter bytes in the ProcessOverBufferValue()",
                        ));
                    }
                    value_size -= extra;
                }
                self.add_value_to_vector(&value[..value_size], true)?;
            }
        }
        if self.base.states.new_row() && !self.base.states.is_not_set() {
            if self.result.comment {
                self.unset_comment_impl(self.base.iterator.pos.buffer_pos)?;
            } else {
                self.add_row_internal()?;
            }
            self.base.lines_read += 1;
        }
        if self.base.iterator.pos.buffer_pos >= current.actual_size() && current.is_last_buffer {
            self.result.added_last_line = true;
        }
        self.base.iterator.pos.buffer_pos += 1;
        if self.base.states.is_carriage_return()
            && sm.dialect_options.state_machine_options.new_line.value() == NewLineIdentifier::CarryOn
        {
            self.result.last_position = LinePosition::new(
                self.base.iterator.pos.buffer_idx,
                self.base.iterator.pos.buffer_pos + 1,
                self.result.buffer_size,
            );
        } else {
            self.result.last_position = LinePosition::new(
                self.base.iterator.pos.buffer_idx,
                self.base.iterator.pos.buffer_pos,
                self.result.buffer_size,
            );
        }
        self.result.quoted = false;
        self.result.escaped = false;
        Ok(())
    }

    /// `MoveToNextBuffer`.
    #[allow(clippy::too_many_lines)]
    fn move_to_next_buffer(&mut self) -> Result<bool> {
        let Some(current) = self.base.cur_buffer_handle.clone() else {
            return Ok(false);
        };
        if self.base.iterator.pos.buffer_pos < current.actual_size() {
            return Ok(false);
        }
        self.previous_buffer_handle = Some(Arc::clone(&current));
        self.base.iterator.pos.buffer_idx += 1;
        self.base.cur_buffer_handle = self
            .base
            .buffer_manager
            .get_buffer(self.base.iterator.pos.buffer_idx)?;
        let Some(next) = self.base.cur_buffer_handle.clone() else {
            self.base.iterator.pos.buffer_idx -= 1;
            self.result.quoted_new_line = false;
            let states = self.base.states;
            let previous_size = current.actual_size();
            if states.empty_line()
                || states.new_row()
                || self.result.added_last_line
                || states.is_current_new_row()
                || states.is_not_set()
            {
                if self.result.cur_col_id == u64::from(self.result.number_of_columns)
                    && !self.base.states.is_state(CsvState::Invalid)
                {
                    self.result.number_of_rows += 1;
                }
                self.result.cur_col_id = 0;
                self.result.chunk_col_id = 0;
                return Ok(false);
            } else if states.new_value() {
                self.add_value_impl(previous_size)?;
                if self.result.comment {
                    self.unset_comment_impl(self.base.iterator.pos.buffer_pos)?;
                } else {
                    self.add_row_impl(previous_size)?;
                }
                self.base.lines_read += 1;
            } else if states.is_quoted_current()
                && self
                    .base
                    .state_machine
                    .dialect_options
                    .state_machine_options
                    .strict_mode
                    .value()
            {
                let current_line_start = LinePosition::new(
                    self.base.iterator.pos.buffer_idx,
                    self.base.iterator.pos.buffer_pos,
                    self.result.buffer_size,
                );
                self.result.current_line_position.begin = self.result.current_line_position.end;
                self.result.current_line_position.end = current_line_start;
                self.invalid_state_impl()?;
            } else {
                if self.result.comment {
                    self.unset_comment_impl(self.base.iterator.pos.buffer_pos)?;
                } else {
                    if self.result.quoted
                        && states.is_delimiter_bytes()
                        && self
                            .base
                            .state_machine
                            .dialect_options
                            .state_machine_options
                            .strict_mode
                            .value()
                    {
                        let r = &mut self.result;
                        r.current_errors.insert(
                            CsvErrorType::UnterminatedQuotes,
                            r.cur_col_id,
                            r.chunk_col_id,
                            r.last_position,
                        );
                    }
                    self.add_row_impl(previous_size)?;
                }
                self.base.lines_read += 1;
            }
            return Ok(false);
        };
        self.result
            .buffer_handles
            .insert(next.buffer_idx(), Arc::clone(&next));
        self.base.iterator.pos.buffer_pos = 0;
        self.process_over_buffer_value()?;
        self.result.buffer = Some(Arc::clone(&next));
        self.result.buffer_size = next.actual_size();
        Ok(true)
    }

    /// `FinalizeChunkProcess`.
    #[allow(clippy::too_many_lines)]
    fn finalize_chunk_process(&mut self) -> Result<()> {
        if self.result.number_of_rows as u64 >= self.result.result_size || self.base.iterator.done {
            if !self.base.sniffing
                && let Some(file_scan) = &self.csv_file_scan
            {
                file_scan
                    .bytes_read
                    .fetch_add(self.base.bytes_read, Ordering::Relaxed);
                self.base.bytes_read = 0;
            }
            return Ok(());
        }
        if self.base.iterator.is_boundary_set() {
            let mut found_error = false;
            let mut error_type = CsvErrorType::UnterminatedQuotes;
            if !self
                .result
                .current_errors
                .has_error_type(CsvErrorType::UnterminatedQuotes)
                && !self
                    .result
                    .current_errors
                    .has_error_type(CsvErrorType::InvalidState)
            {
                self.base.iterator.done = true;
            } else {
                found_error = true;
                error_type = if self
                    .result
                    .current_errors
                    .has_error_type(CsvErrorType::UnterminatedQuotes)
                {
                    CsvErrorType::UnterminatedQuotes
                } else {
                    CsvErrorType::InvalidState
                };
            }
            if self.base.cur_buffer_handle.is_none() {
                return Ok(());
            }
            let moved = self.move_to_next_buffer()?;
            if let Some(handle) = self.base.cur_buffer_handle.clone() {
                if !moved || self.result.cur_col_id > 0 {
                    self.process_extra_row()?;
                }
                if handle.is_last_buffer && self.base.iterator.pos.buffer_pos >= handle.actual_size() {
                    self.move_to_next_buffer()?;
                }
            }
            if self
                .result
                .current_errors
                .has_error_type(CsvErrorType::UnterminatedQuotes)
            {
                found_error = true;
                error_type = CsvErrorType::UnterminatedQuotes;
            } else if self
                .result
                .current_errors
                .has_error_type(CsvErrorType::InvalidState)
            {
                found_error = true;
                error_type = CsvErrorType::InvalidState;
            }
            if self.handle_errors()? {
                self.result.number_of_rows += 1;
            }
            if self.base.states.is_quoted_current() && !found_error {
                if self
                    .base
                    .state_machine
                    .dialect_options
                    .state_machine_options
                    .strict_mode
                    .value()
                {
                    error_type = CsvErrorType::UnterminatedQuotes;
                    let r = &mut self.result;
                    r.current_errors
                        .insert(error_type, r.cur_col_id, r.chunk_col_id, r.last_position);
                    if self.handle_errors()? {
                        self.result.number_of_rows += 1;
                    }
                } else {
                    self.base.used_unstrictness = true;
                }
            }
            let _ = error_type;
            if !self.base.iterator.done
                && (self.base.iterator.pos.buffer_pos >= self.base.iterator.end_pos()
                    || self.base.iterator.pos.buffer_idx > self.base.iterator.buffer_idx()
                    || self.base.finished_file())
            {
                self.base.iterator.done = true;
            }
        } else {
            while !self.base.finished_file() && (self.result.number_of_rows as u64) < self.result.result_size
            {
                self.move_to_next_buffer()?;
                if self.result.number_of_rows as u64 >= self.result.result_size {
                    return Ok(());
                }
                if self.base.cur_buffer_handle.is_some() {
                    self.process()?;
                }
            }
            self.base.iterator.done = self.base.finished_file();
            if self.result.null_padding
                && self.result.number_of_rows < STANDARD_VECTOR_SIZE as i64
                && self.result.chunk_col_id > 0
            {
                while (self.result.chunk_col_id as usize) < self.result.parse_chunk.len() {
                    let row = self.result.number_of_rows as usize;
                    let column = self.result.chunk_col_id as usize;
                    self.result.validity_set_invalid(column, row);
                    self.result.chunk_col_id += 1;
                    self.result.cur_col_id += 1;
                }
                self.result.number_of_rows += 1;
            }
        }
        Ok(())
    }

    /// The parsed vectors and row count of the last `parse_chunk`.
    pub fn parsed_chunk(&self) -> Result<DataChunk> {
        let rows = self.parsed_rows()?;
        Ok(DataChunk::new(self.result.parse_chunk.clone(), rows))
    }

    /// `Flush`: parses a chunk and converts it to `output_types` (in projection order);
    /// `None` when the scan is exhausted. Rows that failed are left out when errors are
    /// ignored.
    #[allow(clippy::too_many_lines)]
    pub fn flush(&mut self, output_types: &[LogicalType]) -> Result<Option<DataChunk>> {
        loop {
            self.parse_chunk()?;
            let rows = self.parsed_rows()?;
            self.base.error_handler.error_if_needed()?;
            if rows == 0 {
                return Ok(None);
            }
            let Some(file_scan) = self.csv_file_scan.clone() else {
                return Err(Error::internal("CSV flush without a file scan"));
            };
            let names = &file_scan.names;
            let mut output: Vec<Option<Vector>> = vec![None; output_types.len()];
            for i in 0..file_scan.column_ids.len() {
                let result_idx = if file_scan.projection_ids.is_empty() {
                    i
                } else {
                    file_scan.projection_ids[i].1 as usize
                };
                if i >= self.result.parse_chunk.len() {
                    return Err(Error::invalid_input(
                        "Mismatch between the schema of different files",
                    ));
                }
                let parse_vector = &self.result.parse_chunk[i];
                let ty = output_types
                    .get(result_idx)
                    .cloned()
                    .unwrap_or_else(|| parse_vector.logical_type().clone());
                let parse_type = parse_vector.logical_type();
                if !is_json(&ty)
                    && (ty.id() == LogicalTypeId::Varchar && !is_json(&ty)
                        || (ty.id() != LogicalTypeId::Varchar && parse_type.id() != LogicalTypeId::Varchar))
                {
                    output[result_idx] = Some(if ty == *parse_type {
                        parse_vector.clone()
                    } else {
                        parse_vector.reinterpret(ty.clone())?
                    });
                    continue;
                }
                let mut params = CastParameters::try_cast();
                let result_vector = cast_vector(&file_scan.casts, parse_vector, &ty, rows, &mut params)?;
                let failed_rows: Vec<usize> = (0..rows)
                    .filter(|&row| result_vector.is_null(row) && !parse_vector.is_null(row))
                    .collect();
                if !failed_rows.is_empty() {
                    let options = &self.base.state_machine.options;
                    for (n, &line_error) in failed_rows.iter().enumerate() {
                        if !options.ignore_errors_only() {
                            let lines_per_batch = LinesPerBoundary::new(
                                self.base.iterator.boundary_idx(),
                                (self.base.lines_read + line_error as u64).wrapping_sub(rows as u64),
                            );
                            let mut first_nl = false;
                            let print = self.result.print_error_line(&self.base.state_machine);
                            let position = self
                                .result
                                .line_positions_per_row
                                .get(&(line_error as u64))
                                .copied()
                                .unwrap_or_default();
                            let borked_line = position.reconstruct_current_line(
                                &mut first_nl,
                                &self.result.buffer_handles,
                                print,
                            );
                            let value_text = parse_vector
                                .get_value(line_error)
                                .to_varchar()
                                .unwrap_or_default();
                            let type_text = if n == 0 {
                                ty.to_string()
                            } else {
                                ty.id().name().to_owned()
                            };
                            let error_msg = sanitize_error(
                                format!("Could not convert string \"{value_text}\" to '{type_text}'")
                                    .as_bytes(),
                            );
                            let row_byte_position = if n == 0 && position.begin == position.end {
                                0
                            } else {
                                position.begin.global_position(self.result.result_size, first_nl)
                            };
                            let error = CsvError::cast_error(
                                options,
                                names.get(i).map_or("", String::as_str),
                                &error_msg,
                                i as u64,
                                borked_line,
                                lines_per_batch,
                                row_byte_position,
                                None,
                                ty.id(),
                                &self.result.path,
                            );
                            self.base.error_handler.error(error, false)?;
                        }
                        self.result.borked_rows.insert(line_error as u64);
                    }
                }
                output[result_idx] = Some(result_vector);
            }
            let columns: Vec<Vector> = output
                .into_iter()
                .zip(output_types)
                .map(|(vector, ty)| vector.unwrap_or_else(|| Vector::constant_null(ty.clone())))
                .collect();
            let mut chunk = DataChunk::new(columns, rows);
            if !self.result.borked_rows.is_empty() {
                let rows_kept: Vec<u32> = (0..rows as u32)
                    .filter(|row| !self.result.borked_rows.contains(&u64::from(*row)))
                    .collect();
                let count = rows_kept.len();
                let sel = SelectionVector::new(rows_kept);
                chunk = DataChunk::new(
                    chunk.columns().iter().map(|column| column.slice(&sel)).collect(),
                    count,
                );
                self.result.borked_rows.clear();
            }
            if chunk.is_empty()
                && let Some(handle) = &self.base.cur_buffer_handle
            {
                let to_pos = if self.base.iterator.is_boundary_set() {
                    self.base.iterator.end_pos().min(handle.actual_size())
                } else {
                    handle.actual_size()
                };
                if self.base.iterator.pos.buffer_pos < to_pos {
                    continue;
                }
            }
            return Ok(Some(chunk));
        }
    }
}

impl CsvProcess for StringValueScanner {
    fn base(&self) -> &BaseScanner {
        &self.base
    }

    fn base_mut(&mut self) -> &mut BaseScanner {
        &mut self.base
    }

    fn invalid_state(&mut self) -> Result<()> {
        self.invalid_state_impl()
    }

    fn empty_line(&mut self, buffer_pos: u64) -> Result<bool> {
        Ok(self.empty_line_impl(buffer_pos))
    }

    fn is_comment_set(&self) -> bool {
        self.result.comment
    }

    fn unset_comment(&mut self, buffer_pos: u64) -> Result<bool> {
        self.unset_comment_impl(buffer_pos)
    }

    fn add_row(&mut self, buffer_pos: u64) -> Result<bool> {
        self.add_row_impl(buffer_pos)
    }

    fn add_value(&mut self, buffer_pos: u64) -> Result<()> {
        self.add_value_impl(buffer_pos)
    }

    fn set_escaped(&mut self) {
        self.result.escaped = true;
    }

    fn set_quoted(&mut self, buffer_pos: u64) {
        if !self.result.quoted {
            self.result.quoted_position = buffer_pos;
        }
        self.result.quoted = true;
        self.result.unquoted = true;
    }

    fn set_unquoted(&mut self) {
        if self.base.states.states[0] == CsvState::Unquoted
            && self.base.states.states[1] == CsvState::Unquoted
            && self
                .base
                .state_machine
                .dialect_options
                .state_machine_options
                .escape
                .value()
                != 0
        {
            self.result.escaped = true;
        }
        self.result.quoted = true;
    }

    fn quoted_new_line(&mut self) {
        self.result.quoted_new_line = true;
    }

    fn set_comment(&mut self, buffer_pos: u64) {
        if !self.result.comment {
            self.result.position_before_comment = buffer_pos;
            self.result.comment = true;
        }
    }
}
