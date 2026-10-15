//! The CSV sniffer (DuckDB's `csv_sniffer.hpp/.cpp`): detects the dialect, the column
//! types, the header and the names of a file.

pub mod dialect;
mod header;
mod types;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::cast::CastFunctionSet;
use rustdb_types::LogicalType;

pub use dialect::DialectCandidates;
pub use types::can_you_cast_it;

use crate::buffer::CsvBufferManager;
use crate::error::{CsvError, CsvErrorHandler, CsvErrorType};
use crate::options::{
    CsvOption, CsvReaderOptions, DialectOptions, FormatOption, FormatType, MultiFileOptionsView,
};
use crate::scanner::boundary::CsvIterator;
use crate::scanner::column_count::ColumnCountScanner;
use crate::scanner::string_value::StringValueScanner;
use crate::schema::CsvSchema;
use crate::state_machine::CsvStateMachine;

/// The types and names a sniffer detected (DuckDB's `SnifferResult`).
#[derive(Clone, Debug, Default)]
pub struct SnifferResult {
    pub return_types: Vec<LogicalType>,
    pub names: Vec<String>,
}

/// The result of the minimal sniffer (DuckDB's `AdaptiveSnifferResult`).
#[derive(Clone, Debug, Default)]
pub struct AdaptiveSnifferResult {
    pub result: SnifferResult,
    pub more_than_one_row: bool,
}

/// The columns the user or the query set (DuckDB's `SetColumns`).
#[derive(Clone, Debug, Default)]
pub struct SetColumns {
    pub types: Vec<LogicalType>,
    pub names: Vec<String>,
}

impl SetColumns {
    pub fn is_set(&self) -> bool {
        !self.types.is_empty()
    }

    pub fn size(&self) -> u64 {
        self.types.len() as u64
    }

    /// `IsCandidateUnacceptable`.
    pub fn is_candidate_unacceptable(
        &self,
        num_cols: u64,
        null_padding: bool,
        ignore_errors: bool,
        last_value_always_empty: bool,
    ) -> bool {
        if !self.is_set() || ignore_errors {
            return false;
        }
        let size = self.size();
        if num_cols == size || num_cols == size + u64::from(last_value_always_empty) {
            return false;
        }
        if null_padding && num_cols > size {
            return false;
        }
        true
    }

    /// `ToString`.
    pub fn to_display_string(&self) -> String {
        let mut result = "columns = { ".to_owned();
        for (i, (name, ty)) in self.names.iter().zip(&self.types).enumerate() {
            let _ = write!(result, "'{name}' : '{ty}'");
            if i + 1 != self.types.len() {
                result += ", ";
            }
        }
        result += "}";
        result
    }
}

/// The state of date/timestamp format detection for one type (DuckDB's
/// `DateTimestampSniffing`).
#[derive(Clone, Debug, Default)]
struct DateTimestampSniffing {
    initialized: bool,
    had_match: bool,
    format: Vec<String>,
}

/// Sniffs one CSV file (DuckDB's `CSVSniffer`).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct CsvSniffer<'a> {
    all_fail_max_line_size: bool,
    line_error: CsvError,
    max_columns_found: u64,
    max_columns_found_error: u64,
    best_candidate_is_strict: bool,
    candidates: Vec<ColumnCountScanner>,
    options: &'a mut CsvReaderOptions,
    file_options: &'a MultiFileOptionsView,
    buffer_manager: Arc<CsvBufferManager>,
    casts: Arc<CastFunctionSet>,
    set_columns: SetColumns,
    error_handler: Arc<CsvErrorHandler>,
    detection_error_handler: Arc<CsvErrorHandler>,
    lines_sniffed: u64,
    best_sql_types_candidates_per_column_idx: Vec<Vec<LogicalType>>,
    best_format_candidates: BTreeMap<FormatType, Vec<String>>,
    best_candidate: Option<StringValueScanner>,
    /// The dialect of the best candidate, which the sniffer refines (the state machine of
    /// DuckDB's `best_candidate`).
    best_state_machine: Option<CsvStateMachine>,
    best_header_row: Vec<Option<String>>,
    format_candidates: BTreeMap<FormatType, DateTimestampSniffing>,
    original_format_candidates: BTreeMap<FormatType, DateTimestampSniffing>,
    detected_types: Vec<LogicalType>,
    default_null_to_varchar: bool,
    names: Vec<String>,
    single_row_file: bool,
    manually_set: Vec<bool>,
}

/// `MatchAndReplace`: takes the sniffed value unless the user set the option, in which
/// case a mismatch is recorded.
fn match_and_replace<T: Clone + PartialEq + FormatOption>(
    original: &mut CsvOption<T>,
    sniffed: &CsvOption<T>,
    name: &str,
    error: &mut String,
) {
    if original.is_set_by_user() {
        if original != sniffed {
            let _ = write!(
                error,
                "CSV Sniffer: Sniffer detected value different than the user input for the {name}"
            );
            let _ = writeln!(
                error,
                " options \n Set: {}, Sniffed: {}",
                original.format_value(),
                sniffed.format_value()
            );
        }
    } else {
        original.set(sniffed.get().clone(), false);
    }
}

/// `MatchAndReplaceUserSetVariables`.
fn match_and_replace_user_set_variables(
    original: &mut DialectOptions,
    sniffed: &DialectOptions,
    error: &mut String,
    found_date: bool,
    found_timestamp: bool,
) {
    match_and_replace(&mut original.header, &sniffed.header, "Header", error);
    if sniffed.state_machine_options.new_line.value() != crate::options::NewLineIdentifier::NotSet {
        match_and_replace(
            &mut original.state_machine_options.new_line,
            &sniffed.state_machine_options.new_line,
            "New Line",
            error,
        );
    }
    match_and_replace(&mut original.skip_rows, &sniffed.skip_rows, "Skip Rows", error);
    match_and_replace(
        &mut original.state_machine_options.delimiter,
        &sniffed.state_machine_options.delimiter,
        "Delimiter",
        error,
    );
    match_and_replace(
        &mut original.state_machine_options.quote,
        &sniffed.state_machine_options.quote,
        "Quote",
        error,
    );
    match_and_replace(
        &mut original.state_machine_options.escape,
        &sniffed.state_machine_options.escape,
        "Escape",
        error,
    );
    match_and_replace(
        &mut original.state_machine_options.comment,
        &sniffed.state_machine_options.comment,
        "Comment",
        error,
    );
    if found_date {
        match_and_replace(
            original.date_format_mut(FormatType::Date),
            sniffed.date_format(FormatType::Date),
            "Date Format",
            error,
        );
    }
    if found_timestamp {
        match_and_replace(
            original.date_format_mut(FormatType::Timestamp),
            sniffed.date_format(FormatType::Timestamp),
            "Timestamp Format",
            error,
        );
    }
}

impl<'a> CsvSniffer<'a> {
    /// `CSVSniffer::CSVSniffer`.
    pub fn new(
        options: &'a mut CsvReaderOptions,
        file_options: &'a MultiFileOptionsView,
        buffer_manager: Arc<CsvBufferManager>,
        casts: Arc<CastFunctionSet>,
        default_null_to_varchar: bool,
    ) -> Self {
        let mut format_candidates = BTreeMap::new();
        let mut best_format_candidates = BTreeMap::new();
        for ty in [FormatType::Date, FormatType::Timestamp] {
            best_format_candidates.insert(ty, Vec::new());
            format_candidates.insert(ty, DateTimestampSniffing::default());
        }
        let error_handler = Arc::new(CsvErrorHandler::new(options.ignore_errors.value()));
        let set_columns = if options.columns_set {
            SetColumns {
                types: options.sql_type_list.clone(),
                names: options.name_list.clone(),
            }
        } else {
            SetColumns::default()
        };
        Self {
            all_fail_max_line_size: true,
            line_error: CsvError::default(),
            // DuckDB reads the size of the not yet initialized set columns: always 0.
            max_columns_found: 0,
            max_columns_found_error: 0,
            best_candidate_is_strict: false,
            candidates: Vec::new(),
            options,
            file_options,
            buffer_manager,
            casts,
            set_columns,
            error_handler,
            detection_error_handler: Arc::new(CsvErrorHandler::new(true)),
            lines_sniffed: 0,
            best_sql_types_candidates_per_column_idx: Vec::new(),
            best_format_candidates,
            best_candidate: None,
            best_state_machine: None,
            best_header_row: Vec::new(),
            format_candidates,
            original_format_candidates: BTreeMap::new(),
            detected_types: Vec::new(),
            default_null_to_varchar,
            names: Vec::new(),
            single_row_file: false,
            manually_set: Vec::new(),
        }
    }

    /// `LinesSniffed`.
    pub fn lines_sniffed(&self) -> u64 {
        self.lines_sniffed
    }

    /// `EmptyOrOnlyHeader`.
    pub fn empty_or_only_header(&self) -> bool {
        let header = self
            .best_state_machine
            .as_ref()
            .is_some_and(|sm| sm.dialect_options.header.value());
        (self.single_row_file && header) || self.lines_sniffed == 0
    }

    fn best_state_machine(&self) -> Result<&CsvStateMachine> {
        self.best_state_machine
            .as_ref()
            .ok_or_else(|| Error::internal("CSV sniffer has no best candidate"))
    }

    /// `SetResultOptions`.
    fn set_result_options(&mut self) -> Result<()> {
        let found_date = self.detected_types.contains(&LogicalType::DATE);
        let found_timestamp = self.detected_types.contains(&LogicalType::TIMESTAMP);
        let best = self
            .best_state_machine
            .as_ref()
            .ok_or_else(|| Error::internal("CSV sniffer has no best candidate"))?;
        let mut error = std::mem::take(&mut self.options.sniffer_user_mismatch_error);
        match_and_replace_user_set_variables(
            &mut self.options.dialect_options,
            &best.dialect_options,
            &mut error,
            found_date,
            found_timestamp,
        );
        self.options.sniffer_user_mismatch_error = error;
        self.options.dialect_options.num_cols = best.dialect_options.num_cols;
        self.options.dialect_options.rows_until_header = best.dialect_options.rows_until_header;
        Ok(())
    }

    /// `MinimalSniff`: sniffs only the first rows, against an already known schema.
    pub fn minimal_sniff(&mut self) -> Result<AdaptiveSnifferResult> {
        if self.set_columns.is_set() {
            return Ok(AdaptiveSnifferResult {
                result: SnifferResult {
                    return_types: self.set_columns.types.clone(),
                    names: self.set_columns.names.clone(),
                },
                more_than_one_row: true,
            });
        }
        self.buffer_manager.set_sniffing(true);
        let result_size = STANDARD_VECTOR_SIZE as u64;
        let mut state_machine =
            CsvStateMachine::new(self.options, &self.options.dialect_options.state_machine_options);
        let mut count_scanner = ColumnCountScanner::new(
            Arc::clone(&self.buffer_manager),
            Arc::new(state_machine.clone()),
            Arc::clone(&self.error_handler),
            result_size,
            CsvIterator::default(),
        )?;
        count_scanner.parse_chunk()?;
        if count_scanner.result_position == 0 {
            return Ok(AdaptiveSnifferResult::default());
        }
        let num_cols = count_scanner.column_counts[0].number_of_columns;
        count_scanner.state_machine_mut().dialect_options.num_cols = num_cols;
        state_machine.dialect_options.num_cols = num_cols;
        self.options.dialect_options.num_cols = num_cols;

        let mut scanner = count_scanner.upgrade_to_string_value_scanner()?;
        scanner.base.error_handler.set_ignore_errors(true);
        scanner.parse_chunk()?;
        let data_chunk = scanner.parsed_chunk()?;
        let start_row = usize::from(count_scanner.result_position == 2);
        self.best_sql_types_candidates_per_column_idx =
            vec![state_machine.options.auto_type_candidates.clone(); num_cols as usize];
        let mut candidates = std::mem::take(&mut self.best_sql_types_candidates_per_column_idx);
        self.sniff_types(&data_chunk, &mut state_machine, &mut candidates, start_row)?;
        let mut potential_header = Vec::with_capacity(data_chunk.column_count());
        for column in data_chunk.columns() {
            potential_header.push(if column.is_null(0) {
                None
            } else {
                Some(String::from_utf8_lossy(column.string_bytes(0)).into_owned())
            });
        }
        let names = header::detect_header_internal(
            &self.casts,
            &mut potential_header,
            &mut state_machine,
            &self.set_columns,
            &mut candidates,
            self.options,
            self.file_options,
            &self.error_handler,
        )?;
        for column_candidates in &candidates {
            let mut d_type = column_candidates.last().cloned().unwrap_or(LogicalType::VARCHAR);
            if column_candidates.len() == self.options.auto_type_candidates.len() {
                d_type = LogicalType::VARCHAR;
            }
            self.detected_types.push(d_type);
        }
        self.best_sql_types_candidates_per_column_idx = candidates;
        Ok(AdaptiveSnifferResult {
            result: SnifferResult {
                return_types: self.detected_types.clone(),
                names,
            },
            more_than_one_row: count_scanner.result_position > 1,
        })
    }

    /// `AdaptiveSniff`: the minimal sniffer, falling back to the full one when its result
    /// does not match the schema of the first file.
    pub fn adaptive_sniff(&mut self, file_schema: &CsvSchema) -> Result<SnifferResult> {
        let mut min_sniff_res = self.minimal_sniff()?;
        let mut run_full = self.error_handler.any_errors() || self.detection_error_handler.any_errors();
        if !run_full && !self.set_columns.is_set() && !self.file_options.any_set() {
            let mut error = String::new();
            run_full = !file_schema.schemas_match(
                &mut error,
                &mut min_sniff_res.result,
                &self.options.file_path,
                Some(min_sniff_res.more_than_one_row),
            );
        }
        if run_full {
            let mut full_sniffer = self.sniff_csv(false)?;
            if !self.set_columns.is_set() && !self.file_options.any_set() {
                let mut error = String::new();
                if !file_schema.schemas_match(&mut error, &mut full_sniffer, &self.options.file_path, None)
                    && !self.options.ignore_errors.value()
                {
                    return Err(Error::invalid_input(error));
                }
            }
            return Ok(full_sniffer);
        }
        Ok(min_sniff_res.result)
    }

    /// `SniffCSV`: the full sniffer.
    pub fn sniff_csv(&mut self, force_match: bool) -> Result<SnifferResult> {
        self.buffer_manager.set_sniffing(true);
        // 1. Dialect Detection
        self.detect_dialect()?;
        // 2. Type Detection
        self.detect_types()?;
        // 3. Type Refinement
        self.refine_types()?;
        // 4. Header Detection
        self.detect_header()?;
        // 5. Type Replacement
        self.replace_types()?;

        if self.buffer_manager.compression() != crate::options::FileCompressionType::Uncompressed {
            self.buffer_manager.reset_buffer_manager()?;
        }
        self.buffer_manager.set_sniffing(false);
        if let Some(best) = &self.best_candidate {
            let strict = best
                .state_machine()
                .dialect_options
                .state_machine_options
                .strict_mode
                .value();
            if best.base.error_handler.any_errors() && !self.options.ignore_errors.value() && strict {
                best.base
                    .error_handler
                    .error_if_type_exists(CsvErrorType::MaximumLineSize)?;
            }
        }
        self.set_result_options()?;
        self.options.auto_detect = true;
        let mut error = std::mem::take(&mut self.options.sniffer_user_mismatch_error);
        if self.set_columns.is_set() {
            let mut matches = true;
            if self.options.dialect_options.header.value() {
                let mut header_error =
                    "The Column names set by the user do not match the ones found by the sniffer. \n"
                        .to_owned();
                let set_names = &self.set_columns.names;
                if set_names.len() == self.names.len() {
                    for (i, (set_name, name)) in set_names.iter().zip(&self.names).enumerate() {
                        if set_name != name {
                            let _ = writeln!(
                                header_error,
                                "Column at position: {i}, Set name: {set_name}, Sniffed Name: {name}"
                            );
                            matches = false;
                        }
                    }
                }
                if !matches {
                    error += &header_error;
                }
            }
            matches = true;
            let mut type_error =
                "The Column types set by the user do not match the ones found by the sniffer. \n".to_owned();
            if self.detected_types.len() as u64 == self.set_columns.size() {
                for i in 0..self.set_columns.types.len() {
                    let set_type = &self.set_columns.types[i];
                    if *set_type != self.detected_types[i] {
                        let _ = writeln!(
                            type_error,
                            "Column at position: {i} Set type: {set_type} Sniffed type: {}",
                            self.detected_types[i]
                        );
                        self.detected_types[i] = set_type.clone();
                        self.manually_set[i] = true;
                        matches = false;
                    }
                }
            }
            if !matches {
                error += &type_error;
            }
            if !error.is_empty() && force_match {
                return Err(Error::invalid_input(error));
            }
            self.options.was_type_manually_set.clone_from(&self.manually_set);
        }
        if !error.is_empty() && force_match {
            return Err(Error::invalid_input(error));
        }
        self.options.sniffer_user_mismatch_error = error;
        self.options.was_type_manually_set.clone_from(&self.manually_set);
        if self.set_columns.is_set() {
            return Ok(SnifferResult {
                return_types: self.set_columns.types.clone(),
                names: self.set_columns.names.clone(),
            });
        }
        Ok(SnifferResult {
            return_types: self.detected_types.clone(),
            names: self.names.clone(),
        })
    }
}
