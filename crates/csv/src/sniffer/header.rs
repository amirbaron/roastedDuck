//! Header detection (DuckDB's `header_detection.cpp`): whether the first row is a header,
//! and the column names.

use std::collections::HashMap;

use roastedduck_ast::keyword::{KeywordCategory, keyword_category};
use roastedduck_common::{ByteString, Error, Result, byte_write};
use roastedduck_function::cast::CastFunctionSet;
use roastedduck_types::LogicalType;
use unicode_normalization::UnicodeNormalization;

use super::{CsvSniffer, SetColumns, can_you_cast_it};
use crate::error::{CsvError, CsvErrorHandler};
use crate::options::{CsvOption, CsvReaderOptions, MultiFileOptionsView};
use crate::state_machine::CsvStateMachine;

/// The number of decimal digits of a number (`NumericHelper::UnsignedLength`).
fn unsigned_length(value: u64) -> usize {
    value.checked_ilog10().map_or(1, |digits| digits as usize + 1)
}

/// `GenerateColumnName`: `column` with the number zero-padded to the widest number.
pub fn generate_column_name(total_cols: u64, col_number: u64) -> String {
    let max_digits = unsigned_length(total_cols.wrapping_sub(1));
    let digits = unsigned_length(col_number);
    format!(
        "column{}{col_number}",
        "0".repeat(max_digits.saturating_sub(digits))
    )
}

/// Whether a character is in the Unicode category Zs (space separator).
fn is_space_separator(c: char) -> bool {
    matches!(
        c,
        '\u{0020}' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// `TrimWhitespace`: trims Unicode space separators.
fn trim_whitespace(col_name: &str) -> String {
    col_name.trim_matches(is_space_separator).to_owned()
}

/// `NormalizeColumnName`: ASCII letters, digits and underscores, lowercase, prefixed with
/// `_` when starting with a digit or being a keyword.
fn normalize_column_name(col_name: &str) -> String {
    let nfkd: String = col_name.nfkd().collect();
    let mut ascii = String::new();
    for byte in nfkd.bytes() {
        if byte == b'_' || byte.is_ascii_alphanumeric() {
            ascii.push(char::from(byte));
        } else if matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
            ascii.push(' ');
        }
    }
    let trimmed = trim_whitespace(&ascii);
    let mut cleaned = String::new();
    let mut in_whitespace = false;
    for c in trimmed.chars() {
        if c == ' ' {
            if !in_whitespace {
                cleaned.push('_');
                in_whitespace = true;
            }
        } else {
            cleaned.push(c);
            in_whitespace = false;
        }
    }
    if cleaned.is_empty() {
        "_".clone_into(&mut cleaned);
    } else {
        cleaned = cleaned.to_lowercase();
    }
    let category = keyword_category(&cleaned);
    let is_keyword = matches!(
        category,
        KeywordCategory::Unreserved | KeywordCategory::TypeFunc | KeywordCategory::Reserved
    );
    if is_keyword || cleaned.as_bytes()[0].is_ascii_digit() {
        cleaned = format!("_{cleaned}");
    }
    cleaned
}

/// `ReplaceNames`: applies the names the user gave.
fn replace_names(
    detected_names: &mut Vec<ByteString>,
    state_machine: &mut CsvStateMachine,
    best_sql_types_candidates_per_column_idx: &mut Vec<Vec<LogicalType>>,
    options: &CsvReaderOptions,
    file_options: &MultiFileOptionsView,
    best_header_row: &[Option<String>],
    error_handler: &CsvErrorHandler,
) -> Result<()> {
    if options.columns_set {
        return Ok(());
    }
    if file_options.hive_partitioning || file_options.union_by_name || options.multi_file_reader {
        for (detected, name) in detected_names.iter_mut().zip(&options.name_list) {
            detected.clone_from(name);
        }
        return Ok(());
    }
    let num_cols = state_machine.dialect_options.num_cols;
    let delimiter = state_machine
        .dialect_options
        .state_machine_options
        .delimiter
        .get()
        .clone();
    if options.name_list.len() as u64 > num_cols {
        if options.null_padding {
            for (col, i) in (num_cols as usize..options.name_list.len()).enumerate() {
                detected_names.push(generate_column_name(options.name_list.len() as u64, col as u64).into());
                if i >= best_sql_types_candidates_per_column_idx.len() {
                    best_sql_types_candidates_per_column_idx.resize(i + 1, Vec::new());
                }
                best_sql_types_candidates_per_column_idx[i] = vec![LogicalType::VARCHAR];
            }
            state_machine.dialect_options.num_cols = options.name_list.len() as u64;
        } else {
            let error = CsvError::header_sniffing_error(
                options,
                best_header_row,
                options.name_list.len() as u64,
                &delimiter,
            );
            error_handler.error(error, false)?;
        }
    }
    if options.name_list.len() > detected_names.len() {
        let error = CsvError::header_sniffing_error(
            options,
            best_header_row,
            options.name_list.len() as u64,
            &delimiter,
        );
        error_handler.error(error, false)?;
    } else {
        for (detected, name) in detected_names.iter_mut().zip(&options.name_list) {
            detected.clone_from(name);
        }
    }
    Ok(())
}

/// `DetectHeaderWithSetColumn`.
fn detect_header_with_set_column(
    casts: &CastFunctionSet,
    best_header_row: &[Option<String>],
    set_columns: &SetColumns,
    options: &mut CsvReaderOptions,
) -> Result<bool> {
    let size = set_columns.types.len();
    if size != best_header_row.len() && size + 1 != best_header_row.len() {
        return Ok(false);
    }
    let mut has_header = true;
    let mut error = ByteString::new();
    for (i, header) in best_header_row.iter().enumerate().take(size) {
        let Some(value) = header else {
            return Ok(false);
        };
        if *value != set_columns.names[i] {
            let _ = byte_write!(error, "Header mismatch at position: {i}\n");
            let _ = byte_write!(error, "Expected name: \"{}\", ", set_columns.names[i]);
            let _ = byte_write!(error, "Actual name: \"{value}\".\n");
            has_header = false;
            break;
        }
    }
    if !has_header {
        let mut all_varchar = true;
        let mut first_row_consistent = true;
        let decimal_separator = options.decimal_separator.as_bytes().first().copied().unwrap_or(0);
        for (col, sql_type) in set_columns.types.iter().enumerate() {
            if *sql_type != LogicalType::VARCHAR {
                all_varchar = false;
                let value = best_header_row[col].as_deref().unwrap_or("");
                if !can_you_cast_it(
                    casts,
                    value.as_bytes(),
                    sql_type,
                    &options.dialect_options,
                    best_header_row[col].is_none(),
                    decimal_separator,
                    options.thousands_separator,
                )? {
                    first_row_consistent = false;
                }
            }
        }
        if !first_row_consistent {
            options.sniffer_user_mismatch_error.push_bytes(&error);
        }
        if all_varchar {
            return Ok(true);
        }
        return Ok(!first_row_consistent);
    }
    Ok(has_header)
}

/// `EmptyHeader`.
fn empty_header(col_name: Option<&str>, normalize: bool) -> bool {
    let Some(col_name) = col_name else {
        return true;
    };
    if col_name.is_empty() {
        return true;
    }
    if normalize {
        return false;
    }
    col_name
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'))
}

/// `DetectHeaderInternal`: whether the first row is a header, and the column names.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn detect_header_internal(
    casts: &CastFunctionSet,
    best_header_row: &mut [Option<String>],
    state_machine: &mut CsvStateMachine,
    set_columns: &SetColumns,
    best_sql_types_candidates_per_column_idx: &mut Vec<Vec<LogicalType>>,
    options: &mut CsvReaderOptions,
    file_options: &MultiFileOptionsView,
    error_handler: &CsvErrorHandler,
) -> Result<Vec<ByteString>> {
    let mut detected_names = Vec::new();
    state_machine.dialect_options.num_cols = best_sql_types_candidates_per_column_idx.len() as u64;
    let num_cols = state_machine.dialect_options.num_cols;
    let generated = |num_cols: u64| {
        (0..num_cols)
            .map(|col| ByteString::from(generate_column_name(num_cols, col)))
            .collect::<Vec<_>>()
    };
    if best_header_row.is_empty() {
        state_machine.dialect_options.header = CsvOption::new(false);
        detected_names = generated(num_cols);
        replace_names(
            &mut detected_names,
            state_machine,
            best_sql_types_candidates_per_column_idx,
            options,
            file_options,
            best_header_row,
            error_handler,
        )?;
        return Ok(detected_names);
    }
    if !options.null_padding && best_sql_types_candidates_per_column_idx.len() != best_header_row.len() {
        if options.ignore_errors.value() {
            state_machine.dialect_options.header = CsvOption::new(false);
            detected_names = generated(num_cols);
            state_machine.dialect_options.rows_until_header += 1;
            replace_names(
                &mut detected_names,
                state_machine,
                best_sql_types_candidates_per_column_idx,
                options,
                file_options,
                best_header_row,
                error_handler,
            )?;
            return Ok(detected_names);
        }
        let error = CsvError::header_sniffing_error(
            options,
            best_header_row,
            best_sql_types_candidates_per_column_idx.len() as u64,
            state_machine
                .dialect_options
                .state_machine_options
                .delimiter
                .get(),
        );
        error_handler.error(error, false)?;
    }
    let mut has_header = if set_columns.is_set() {
        detect_header_with_set_column(casts, best_header_row, set_columns, options)?
    } else {
        let mut first_row_consistent = true;
        let mut all_varchar = true;
        let mut first_row_nulls = true;
        let decimal_separator = options.decimal_separator.as_bytes().first().copied().unwrap_or(0);
        for (col, value) in best_header_row.iter().enumerate() {
            if value.is_some() {
                first_row_nulls = false;
            }
            let Some(sql_type) = best_sql_types_candidates_per_column_idx
                .get(col)
                .and_then(|candidates| candidates.last())
            else {
                continue;
            };
            if *sql_type != LogicalType::VARCHAR {
                all_varchar = false;
                if !can_you_cast_it(
                    casts,
                    value.as_deref().unwrap_or("").as_bytes(),
                    sql_type,
                    &state_machine.dialect_options,
                    value.is_none(),
                    decimal_separator,
                    options.thousands_separator,
                )? {
                    first_row_consistent = false;
                }
            }
        }
        all_varchar || first_row_nulls || !first_row_consistent
    };
    if options.dialect_options.header.is_set_by_user() {
        has_header = options.dialect_options.header.value();
    }
    if has_header {
        state_machine.dialect_options.header = CsvOption::new(true);
        if options.null_padding && !options.dialect_options.skip_rows.is_set_by_user() {
            let skip_rows = state_machine.dialect_options.skip_rows.value();
            if skip_rows > 0 {
                state_machine.dialect_options.skip_rows = CsvOption::new(skip_rows - 1);
            }
        }
        // Names compare case-insensitively for collisions.
        let mut name_collision_count: HashMap<String, u64> = HashMap::new();
        for (col, value) in best_header_row.iter().enumerate() {
            let mut col_name = if empty_header(value.as_deref(), options.normalize_names) {
                generate_column_name(num_cols, col as u64)
            } else {
                value.clone().unwrap_or_default()
            };
            col_name = if options.normalize_names {
                normalize_column_name(&col_name)
            } else {
                trim_whitespace(&col_name)
            };
            while let Some(count) = name_collision_count.get_mut(&col_name.to_ascii_lowercase()) {
                *count += 1;
                col_name = format!("{col_name}_{count}");
            }
            name_collision_count.insert(col_name.to_ascii_lowercase(), 0);
            detected_names.push(col_name.into());
        }
        if (best_header_row.len() as u64) < num_cols && options.null_padding {
            for col in best_header_row.len() as u64..num_cols {
                detected_names.push(generate_column_name(num_cols, col).into());
            }
        } else if (best_header_row.len() as u64) < num_cols {
            return Err(Error::internal(
                "Detected header has number of columns inferior to dialect detection",
            ));
        }
    } else {
        state_machine.dialect_options.header = CsvOption::new(false);
        detected_names = generated(num_cols);
    }
    replace_names(
        &mut detected_names,
        state_machine,
        best_sql_types_candidates_per_column_idx,
        options,
        file_options,
        best_header_row,
        error_handler,
    )?;
    Ok(detected_names)
}

impl CsvSniffer<'_> {
    /// `DetectHeader`.
    pub(super) fn detect_header(&mut self) -> Result<()> {
        let Some(mut state_machine) = self.best_state_machine.take() else {
            return Err(Error::internal("CSV sniffer has no best candidate"));
        };
        let mut header_row = std::mem::take(&mut self.best_header_row);
        let mut candidates = std::mem::take(&mut self.best_sql_types_candidates_per_column_idx);
        let names = detect_header_internal(
            &self.casts,
            &mut header_row,
            &mut state_machine,
            &self.set_columns,
            &mut candidates,
            self.options,
            self.file_options,
            &self.error_handler,
        );
        self.best_header_row = header_row;
        self.best_sql_types_candidates_per_column_idx = candidates;
        self.best_state_machine = Some(state_machine);
        self.names = names?;
        if self.empty_or_only_header() {
            self.detected_types = vec![LogicalType::SQLNULL; self.names.len()];
        }
        for _ in self.detected_types.len()..self.names.len() {
            self.detected_types.push(LogicalType::VARCHAR);
        }
        self.max_columns_found = self.names.len() as u64;
        Ok(())
    }
}
