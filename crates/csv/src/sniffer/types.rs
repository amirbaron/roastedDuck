//! Type detection, refinement and replacement (DuckDB's `type_detection.cpp`,
//! `type_refinement.cpp` and `type_replacement.cpp`).

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::text_parse::{TextFloat, TextInteger, parse_bool_strict, parse_decimal};
use roastedduck_function::cast::{CastFunctionSet, CastParameters, ValueCast};
use roastedduck_functions::strtime::StrpTimeFormat;
use roastedduck_types::{DataChunk, Date, LogicalType, LogicalTypeId, Time, Timestamp, Value, Vector};

use super::{CsvSniffer, DialectCandidates};
use crate::error::{CsvError, CsvErrorType};
use crate::options::{DialectOptions, FormatType};
use crate::scanner::base::BaseScanner;
use crate::state_machine::CsvStateMachine;

/// `format_template_candidates`, in order of preference (the last is tried first).
fn format_template_candidates(ty: FormatType) -> &'static [&'static str] {
    match ty {
        FormatType::Date => &[
            "%m-%d-%Y", "%m-%d-%y", "%d-%m-%Y", "%d-%m-%y", "%Y-%m-%d", "%y-%m-%d",
        ],
        FormatType::Timestamp => &[
            "%Y-%m-%d %H:%M:%S.%f",
            "%m-%d-%Y %I:%M:%S %p",
            "%m-%d-%y %I:%M:%S %p",
            "%d-%m-%Y %H:%M:%S",
            "%d-%m-%y %H:%M:%S",
            "%Y-%m-%d %H:%M:%S",
            "%y-%m-%d %H:%M:%S",
        ],
    }
}

/// `StartsWithNumericDate`: whether the value starts with three numbers separated by the
/// same literal; returns that literal (with `%` escaped).
fn starts_with_numeric_date(value: &[u8]) -> Option<String> {
    let is_space = |byte: &u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r');
    let field1 = value.iter().position(|byte| !is_space(byte))?;
    if !value[field1].is_ascii_digit() {
        return None;
    }
    let literal1 = field1 + value[field1..].iter().position(|byte| !byte.is_ascii_digit())?;
    let field2 = literal1 + value[literal1..].iter().position(u8::is_ascii_digit)?;
    let literal2 = field2 + value[field2..].iter().position(|byte| !byte.is_ascii_digit())?;
    let field3 = literal2 + value[literal2..].iter().position(u8::is_ascii_digit)?;
    if field3 - literal2 != field2 - literal1 || value[literal1..field2] != value[literal2..field3] {
        return None;
    }
    let mut separator = String::new();
    for &byte in &value[literal1..field2] {
        if byte == b'%' {
            separator.push('%');
        }
        separator.push(char::from(byte));
    }
    Some(separator)
}

/// `GenerateDateFormat`: the template with `-` replaced by the separator.
fn generate_date_format(separator: &str, format_template: &str) -> String {
    format_template.replace('-', separator)
}

/// `ValidSeparator`.
fn valid_separator(separator: &str) -> bool {
    matches!(separator, "-" | "." | "/" | " ")
}

/// `SetDateFormat`.
fn set_date_format(candidate: &mut CsvStateMachine, format_specifier: &str, ty: FormatType) {
    let format = StrpTimeFormat::parse(format_specifier).unwrap_or_default();
    candidate.dialect_options.date_format_mut(ty).set(format, false);
}

/// `CSVSniffer::CanYouCastIt`: whether a value parses as the type.
pub fn can_you_cast_it(
    casts: &CastFunctionSet,
    value: &[u8],
    ty: &LogicalType,
    dialect_options: &DialectOptions,
    is_null: bool,
    decimal_separator: u8,
    thousands_separator: u8,
) -> Result<bool> {
    if is_null {
        return Ok(true);
    }
    let stripped;
    let value = if ty.is_numeric() && thousands_separator != 0 {
        stripped = BaseScanner::remove_separator(value, thousands_separator);
        &stripped[..]
    } else {
        value
    };
    Ok(match ty.id() {
        LogicalTypeId::Boolean => parse_bool_strict(value).is_some(),
        LogicalTypeId::TinyInt => i8::parse_integer_with(value, false).is_some(),
        LogicalTypeId::SmallInt => i16::parse_integer_with(value, true).is_some(),
        LogicalTypeId::Integer => i32::parse_integer_with(value, true).is_some(),
        LogicalTypeId::BigInt => i64::parse_integer_with(value, true).is_some(),
        LogicalTypeId::UTinyInt => u8::parse_integer_with(value, true).is_some(),
        LogicalTypeId::USmallInt => u16::parse_integer_with(value, true).is_some(),
        LogicalTypeId::UInteger => u32::parse_integer_with(value, true).is_some(),
        LogicalTypeId::UBigInt => u64::parse_integer_with(value, true).is_some(),
        LogicalTypeId::Double => f64::parse_float_with(value, decimal_separator, true).is_some(),
        LogicalTypeId::Float => f32::parse_float_with(value, decimal_separator, true).is_some(),
        LogicalTypeId::Date => {
            let format = dialect_options.date_format(FormatType::Date).get();
            if format.is_empty() {
                Date::parse(value, true).is_ok()
            } else {
                format.try_parse_date(value).is_some()
            }
        }
        LogicalTypeId::Timestamp => {
            let format = dialect_options.date_format(FormatType::Timestamp).get();
            if format.is_empty() {
                // `TryConvertTimestamp(..., strict = true)`: offsets fail (`STRICT_UTC`).
                Timestamp::parse_parts(value, false).is_ok_and(|parsed| {
                    (parsed.time_zone.is_empty() || parsed.time_zone.eq_ignore_ascii_case(b"utc"))
                        && !parsed.has_offset
                })
            } else {
                format.try_parse_timestamp(value).is_some()
            }
        }
        LogicalTypeId::Time => Time::parse(value, true).is_some(),
        LogicalTypeId::Decimal => {
            if decimal_separator != b',' && decimal_separator != b'.' {
                return Err(Error::invalid_input(
                    "Decimals can only have ',' and '.' as decimal separators",
                ));
            }
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            parse_decimal(value, width, scale, decimal_separator).is_some()
        }
        LogicalTypeId::Varchar => true,
        _ => {
            let text = String::from_utf8_lossy(value);
            Value::varchar(text.as_ref()).try_cast_as(casts, ty, true).is_ok()
        }
    })
}

/// Whether a VARCHAR vector converts to the type without errors (`TryCastVector`).
fn try_cast_vector(
    casts: &CastFunctionSet,
    state_machine: &CsvStateMachine,
    decimal_separator: &str,
    parse_chunk_col: &Vector,
    size: usize,
    sql_type: &LogicalType,
) -> Result<bool> {
    let all_rows = |check: &dyn Fn(&[u8]) -> bool| {
        (0..size).all(|row| parse_chunk_col.is_null(row) || check(parse_chunk_col.string_bytes(row)))
    };
    let dialect = &state_machine.dialect_options;
    let date_format = dialect.date_format(FormatType::Date).get();
    if !date_format.is_empty() && sql_type.id() == LogicalTypeId::Date {
        return Ok(all_rows(&|value| date_format.try_parse_date(value).is_some()));
    }
    let timestamp_format = dialect.date_format(FormatType::Timestamp).get();
    if !timestamp_format.is_empty() && sql_type.id() == LogicalTypeId::Timestamp {
        return Ok(all_rows(&|value| {
            timestamp_format.try_parse_timestamp(value).is_some()
        }));
    }
    if matches!(sql_type.id(), LogicalTypeId::Double | LogicalTypeId::Float) && decimal_separator == "," {
        return Ok(if sql_type.id() == LogicalTypeId::Double {
            all_rows(&|value| f64::parse_float(value, b',').is_some())
        } else {
            all_rows(&|value| f32::parse_float(value, b',').is_some())
        });
    }
    if sql_type.id() == LogicalTypeId::Decimal && decimal_separator == "," {
        let (width, scale) = sql_type.decimal_properties().unwrap_or((18, 3));
        return Ok(all_rows(&|value| {
            parse_decimal(value, width, scale, b',').is_some()
        }));
    }
    let cast = casts.get_cast_function(parse_chunk_col.logical_type(), sql_type)?;
    let mut result = Vector::flat(sql_type.clone(), size);
    let mut params = CastParameters::try_cast();
    params.strict = true;
    match cast.execute(parse_chunk_col, &mut result, size, &mut params) {
        Ok(all_converted) => Ok(all_converted),
        Err(_) => Ok(false),
    }
}

impl CsvSniffer<'_> {
    /// `InitializeDateAndTimeStampDetection`.
    fn initialize_date_and_timestamp_detection(
        &mut self,
        candidate: &mut CsvStateMachine,
        separator: &str,
        ty: FormatType,
    ) {
        let user_format = self.options.dialect_options.date_format(ty).clone();
        let format_candidate = self.format_candidates.entry(ty).or_default();
        if !format_candidate.initialized {
            format_candidate.initialized = true;
            if user_format.is_set_by_user() {
                format_candidate
                    .format
                    .push(user_format.get().format_specifier().to_owned());
            } else {
                for template in format_template_candidates(ty) {
                    let format_string = generate_date_format(separator, template);
                    if !format_string.contains("%Y-%m-%d") {
                        format_candidate.format.push(format_string);
                    }
                }
            }
            self.original_format_candidates = self.format_candidates.clone();
        }
        let last = self.format_candidates[&ty]
            .format
            .last()
            .cloned()
            .unwrap_or_default();
        set_date_format(candidate, &last, ty);
    }

    /// `DetectDateAndTimeStampFormats`.
    fn detect_date_and_timestamp_formats(
        &mut self,
        candidate: &mut CsvStateMachine,
        ty: FormatType,
        separator: &str,
        value: &[u8],
    ) {
        if !valid_separator(separator) {
            return;
        }
        self.initialize_date_and_timestamp_detection(candidate, separator, ty);
        let save_format_candidates = self.format_candidates[&ty].format.clone();
        let had_format_candidates = !save_format_candidates.is_empty();
        let initial_format_candidates = save_format_candidates.len()
            == self
                .original_format_candidates
                .get(&ty)
                .map_or(0, |candidates| candidates.format.len());
        let is_set_by_user = self.options.dialect_options.date_format(ty).is_set_by_user();
        while !self.format_candidates[&ty].format.is_empty() && !is_set_by_user {
            if candidate
                .dialect_options
                .date_format(ty)
                .get()
                .matches(value, true)
            {
                if let Some(entry) = self.format_candidates.get_mut(&ty) {
                    entry.had_match = true;
                }
                break;
            }
            let entry = self.format_candidates.entry(ty).or_default();
            entry.format.pop();
            if let Some(last) = entry.format.last().cloned() {
                set_date_format(candidate, &last, ty);
            }
        }
        if self.format_candidates[&ty].format.is_empty() && had_format_candidates {
            let entry = self.format_candidates.entry(ty).or_default();
            if initial_format_candidates && !entry.had_match {
                entry.initialized = false;
                entry.format.clear();
                set_date_format(candidate, "", ty);
                return;
            }
            entry.format = save_format_candidates;
            let last = entry.format.last().cloned().unwrap_or_default();
            set_date_format(candidate, &last, ty);
        }
    }

    /// `SniffTypes`: narrows the type candidates of each column with the values of a chunk.
    pub(super) fn sniff_types(
        &mut self,
        data_chunk: &DataChunk,
        state_machine: &mut CsvStateMachine,
        info_sql_types_candidates: &mut Vec<Vec<LogicalType>>,
        start_idx_detection: usize,
    ) -> Result<()> {
        let chunk_size = data_chunk.len();
        let mut has_date = false;
        let mut has_timestamp = false;
        let decimal_separator = state_machine
            .options
            .decimal_separator
            .as_bytes()
            .first()
            .copied()
            .unwrap_or(0);
        let thousands_separator = state_machine.options.thousands_separator;
        for (col_idx, cur_vector) in data_chunk.columns().iter().enumerate() {
            if col_idx >= info_sql_types_candidates.len() {
                info_sql_types_candidates.resize(col_idx + 1, Vec::new());
            }
            if info_sql_types_candidates[col_idx].is_empty() {
                continue;
            }
            for row_idx in start_idx_detection..chunk_size {
                let is_null = cur_vector.is_null(row_idx);
                let value: &[u8] = if is_null {
                    &[]
                } else {
                    cur_vector.string_bytes(row_idx)
                };
                let cur_top_candidate = info_sql_types_candidates[col_idx]
                    .last()
                    .cloned()
                    .unwrap_or(LogicalType::VARCHAR);
                while info_sql_types_candidates[col_idx].len() > 1 {
                    let col_type_candidates = &info_sql_types_candidates[col_idx];
                    let sql_type = col_type_candidates[col_type_candidates.len() - 1].clone();
                    if !is_null {
                        let detect = (sql_type.id() == LogicalTypeId::Timestamp && !has_timestamp)
                            || (sql_type.id() == LogicalTypeId::Date && !has_date);
                        if detect && let Some(separator) = starts_with_numeric_date(value) {
                            let ty = if sql_type.id() == LogicalTypeId::Date {
                                FormatType::Date
                            } else {
                                FormatType::Timestamp
                            };
                            self.detect_date_and_timestamp_formats(state_machine, ty, &separator, value);
                        }
                    }
                    if sql_type == LogicalType::VARCHAR {
                        // DuckDB `continue`s here, which can only happen with VARCHAR on top.
                        break;
                    }
                    if can_you_cast_it(
                        &self.casts,
                        value,
                        &sql_type,
                        &state_machine.dialect_options,
                        is_null,
                        decimal_separator,
                        thousands_separator,
                    )? {
                        break;
                    }
                    let col_type_candidates = &mut info_sql_types_candidates[col_idx];
                    let second = col_type_candidates
                        .len()
                        .checked_sub(2)
                        .map(|index| col_type_candidates[index].clone());
                    if row_idx != start_idx_detection
                        && (cur_top_candidate == LogicalType::BOOLEAN
                            || cur_top_candidate == LogicalType::DATE
                            || cur_top_candidate == LogicalType::TIME
                            || (cur_top_candidate == LogicalType::TIMESTAMP
                                && second != Some(LogicalType::TIMESTAMP_TZ))
                            || cur_top_candidate == LogicalType::TIMESTAMP_TZ)
                    {
                        while col_type_candidates
                            .last()
                            .is_some_and(|ty| *ty != LogicalType::VARCHAR)
                        {
                            col_type_candidates.pop();
                        }
                        break;
                    }
                    col_type_candidates.pop();
                }
            }
            match info_sql_types_candidates[col_idx].last().map(LogicalType::id) {
                Some(LogicalTypeId::Date) => has_date = true,
                Some(LogicalTypeId::Timestamp) => has_timestamp = true,
                _ => {}
            }
        }
        Ok(())
    }

    /// `SetUserDefinedDateTimeFormat`.
    fn set_user_defined_date_time_format(&self, candidate: &mut CsvStateMachine) {
        for ty in [FormatType::Date, FormatType::Timestamp] {
            let user_option = self.options.dialect_options.date_format(ty);
            if user_option.is_set_by_user() {
                set_date_format(candidate, user_option.get().format_specifier(), ty);
            }
        }
    }

    /// `DetectTypes`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn detect_types(&mut self) -> Result<()> {
        let mut min_varchar_cols = self.max_columns_found + 1;
        let mut min_errors = usize::MAX;
        let candidates = std::mem::take(&mut self.candidates);
        for candidate_cc in &candidates {
            let mut sniffing_state_machine = (*candidate_cc.base.state_machine).clone();
            let mut info_sql_types_candidates =
                vec![
                    sniffing_state_machine.options.auto_type_candidates.clone();
                    self.max_columns_found as usize
                ];
            let mut candidate = candidate_cc.upgrade_to_string_value_scanner()?;
            self.set_user_defined_date_time_format(&mut sniffing_state_machine);
            candidate.parse_chunk()?;
            let data_chunk = candidate.parsed_chunk()?;
            let handler = &candidate.base.error_handler;
            if handler.any_errors()
                && !handler.has_error(CsvErrorType::MaximumLineSize)
                && !candidate.state_machine().options.ignore_errors.value()
            {
                continue;
            }
            let chunk_size = data_chunk.len();
            let header = &self.options.dialect_options.header;
            let start_idx_detection =
                usize::from(chunk_size > 1 && (!header.is_set_by_user() || header.value()));
            self.sniff_types(
                &data_chunk,
                &mut sniffing_state_machine,
                &mut info_sql_types_candidates,
                start_idx_detection,
            )?;
            let varchar_cols = info_sql_types_candidates
                .iter()
                .filter(|candidates| candidates.last() == Some(&LogicalType::VARCHAR))
                .count() as u64;
            let has_less_varchar_cols = varchar_cols < min_varchar_cols;
            #[allow(clippy::cast_precision_loss)]
            let acceptable_best_num_cols =
                info_sql_types_candidates.len() as f64 > self.max_columns_found as f64 * 0.7;
            let number_of_errors = candidate.base.error_handler.size();
            let better_strictness = !self.best_candidate_is_strict || !candidate.base.used_unstrictness;
            let acceptable_candidate = has_less_varchar_cols && acceptable_best_num_cols && better_strictness;
            if self.best_candidate.is_none()
                || (acceptable_candidate
                    && (!self.options.ignore_errors.value() || number_of_errors < min_errors))
            {
                min_errors = number_of_errors;
                self.best_header_row.clear();
                min_varchar_cols = varchar_cols;
                self.best_candidate_is_strict = !candidate.base.used_unstrictness;
                self.best_sql_types_candidates_per_column_idx = info_sql_types_candidates;
                for (ty, format_candidate) in &self.format_candidates {
                    self.best_format_candidates
                        .insert(*ty, format_candidate.format.clone());
                }
                if chunk_size > 0 {
                    self.single_row_file = chunk_size == 1;
                    for column in data_chunk.columns() {
                        self.best_header_row.push(if column.is_null(0) {
                            None
                        } else {
                            Some(String::from_utf8_lossy(column.string_bytes(0)).into_owned())
                        });
                    }
                }
                self.best_candidate = Some(candidate);
                self.best_state_machine = Some(sniffing_state_machine);
            }
        }
        self.candidates = candidates;
        if self.best_candidate.is_none() {
            let dialect_candidates =
                DialectCandidates::new(&self.options.dialect_options.state_machine_options);
            let set = self.set_columns.to_display_string();
            let error = CsvError::sniffing_error(
                self.options,
                &dialect_candidates.print(),
                self.max_columns_found,
                Some((&set, self.set_columns.size() as usize)),
                true,
            );
            self.error_handler.error(error, true)?;
        }
        Ok(())
    }

    /// `RefineTypes`: narrows the types with the remaining sample chunks.
    pub(super) fn refine_types(&mut self) -> Result<()> {
        let (num_cols, all_varchar, sample_size_chunks) = {
            let sm = self.best_state_machine()?;
            (
                sm.dialect_options.num_cols,
                sm.options.all_varchar,
                sm.options.sample_size_chunks,
            )
        };
        self.detected_types = vec![LogicalType::VARCHAR; num_cols as usize];
        if all_varchar {
            return Ok(());
        }
        let Some(mut best_candidate) = self.best_candidate.take() else {
            return Err(Error::internal("CSV sniffer has no best candidate"));
        };
        let decimal_separator = self.options.decimal_separator.clone();
        let mut i = 1;
        while i < sample_size_chunks {
            if best_candidate.base.finished_file() {
                self.detected_types = self
                    .best_sql_types_candidates_per_column_idx
                    .iter()
                    .map(|candidates| candidates.last().cloned().unwrap_or(LogicalType::VARCHAR))
                    .collect();
                self.best_candidate = Some(best_candidate);
                return Ok(());
            }
            best_candidate.parse_chunk()?;
            let parse_chunk = best_candidate.parsed_chunk()?;
            let state_machine = self
                .best_state_machine
                .as_ref()
                .ok_or_else(|| Error::internal("CSV sniffer has no best candidate"))?;
            for (col, vector) in parse_chunk.columns().iter().enumerate() {
                let Some(col_type_candidates) = self.best_sql_types_candidates_per_column_idx.get(col) else {
                    continue;
                };
                let mut col_type_candidates = col_type_candidates.clone();
                let is_bool_type = col_type_candidates.last() == Some(&LogicalType::BOOLEAN);
                while col_type_candidates.len() > 1 {
                    let sql_type = col_type_candidates[col_type_candidates.len() - 1].clone();
                    if try_cast_vector(
                        &self.casts,
                        state_machine,
                        &decimal_separator,
                        vector,
                        parse_chunk.len(),
                        &sql_type,
                    )? {
                        break;
                    }
                    if sql_type == LogicalType::BOOLEAN && is_bool_type {
                        while col_type_candidates
                            .last()
                            .is_some_and(|ty| *ty != LogicalType::VARCHAR)
                        {
                            col_type_candidates.pop();
                        }
                        break;
                    }
                    col_type_candidates.pop();
                }
                self.best_sql_types_candidates_per_column_idx[col] = col_type_candidates;
            }
            i += 1;
        }
        let auto_candidates = self.best_state_machine()?.options.auto_type_candidates.len();
        let finished = best_candidate.base.finished_file();
        self.detected_types = self
            .best_sql_types_candidates_per_column_idx
            .iter()
            .map(|candidates| {
                if candidates.len() == auto_candidates && self.default_null_to_varchar && !finished {
                    LogicalType::VARCHAR
                } else {
                    candidates.last().cloned().unwrap_or(LogicalType::VARCHAR)
                }
            })
            .collect();
        self.best_candidate = Some(best_candidate);
        Ok(())
    }

    /// `ReplaceTypes`: applies the types the user gave.
    pub(super) fn replace_types(&mut self) -> Result<()> {
        self.manually_set = vec![false; self.detected_types.len()];
        let (sql_type_list, columns_set, sql_types_per_column) = {
            let sm = self.best_state_machine()?;
            (
                sm.options.sql_type_list.clone(),
                sm.options.columns_set,
                sm.options.sql_types_per_column.clone(),
            )
        };
        if sql_type_list.is_empty() || columns_set {
            return Ok(());
        }
        if !sql_types_per_column.is_empty() {
            let mut found = 0;
            for i in 0..self.names.len() {
                if let Some(&index) = sql_types_per_column.get(&self.names[i]) {
                    let ty = sql_type_list[index].clone();
                    if i < self.best_sql_types_candidates_per_column_idx.len() {
                        self.best_sql_types_candidates_per_column_idx[i] = vec![ty.clone()];
                    }
                    self.detected_types[i] = ty;
                    self.manually_set[i] = true;
                    found += 1;
                }
            }
            if !self.file_options.union_by_name && found < sql_types_per_column.len() {
                let error = CsvError::column_types_error(
                    sql_types_per_column
                        .iter()
                        .map(|(name, _)| name.to_owned())
                        .collect(),
                    &self.names,
                );
                self.error_handler.error(error, false)?;
            }
            return Ok(());
        }
        if self.names.len() < sql_type_list.len() {
            return Err(Error::binder(format!(
                "read_csv: {} types were provided, but CSV file only has {} columns",
                sql_type_list.len(),
                self.names.len()
            )));
        }
        for (i, ty) in sql_type_list.iter().enumerate() {
            self.detected_types[i] = ty.clone();
            self.manually_set[i] = true;
        }
        Ok(())
    }
}
