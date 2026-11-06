//! Dialect detection (DuckDB's `dialect_detection.cpp`): delimiter, quote, escape,
//! comment, newline, rows to skip and the number of columns.

use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_common::Result;

use super::CsvSniffer;
use crate::buffer::CsvBufferManager;
use crate::error::{CsvError, CsvErrorType};
use crate::options::{CsvOption, CsvReaderOptions, NewLineIdentifier, SNIFF_SIZE, StateMachineOptions};
use crate::scanner::boundary::CsvIterator;
use crate::scanner::column_count::ColumnCountScanner;
use crate::scanner::skip::skip_csv_rows;
use crate::state_machine::CsvStateMachine;

/// The dialects the sniffer tries (DuckDB's `DialectCandidates`).
#[derive(Clone, Debug)]
pub struct DialectCandidates {
    pub delim_candidates: Vec<String>,
    pub comment_candidates: Vec<u8>,
    /// (quote, escape) pairs in priority order.
    pub quote_escape_candidates: Vec<(u8, u8)>,
}

/// `GetDefaultQuoteEscapeCombination`.
const DEFAULT_QUOTE_ESCAPE: [(u8, u8); 9] = [
    (0, 0),
    (b'"', 0),
    (b'"', b'"'),
    (b'"', b'\''),
    (b'"', b'\\'),
    (b'\'', 0),
    (b'\'', b'\''),
    (b'\'', b'"'),
    (b'\'', b'\\'),
];

impl DialectCandidates {
    /// `DialectCandidates::DialectCandidates`.
    pub fn new(options: &StateMachineOptions) -> Self {
        let delim_candidates = if options.delimiter.is_set_by_user() {
            vec![options.delimiter.get().clone()]
        } else {
            vec![",".to_owned(), "|".to_owned(), ";".to_owned(), "\t".to_owned()]
        };
        let comment_candidates = if options.comment.is_set_by_user() {
            vec![options.comment.value()]
        } else {
            vec![0, b'#']
        };
        let quote = options.quote.value();
        let escape = options.escape.value();
        let mut quote_escape_candidates = Vec::new();
        if options.quote.is_set_by_user() && options.escape.is_set_by_user() {
            quote_escape_candidates.push((quote, escape));
        } else if options.quote.is_set_by_user() {
            quote_escape_candidates.extend(DEFAULT_QUOTE_ESCAPE.iter().filter(|(q, _)| *q == quote));
            if quote_escape_candidates.is_empty() {
                quote_escape_candidates.extend([
                    (quote, quote),
                    (quote, 0),
                    (quote, b'\\'),
                    (quote, b'"'),
                    (quote, b'\''),
                ]);
            }
        } else if options.escape.is_set_by_user() {
            quote_escape_candidates.extend(DEFAULT_QUOTE_ESCAPE.iter().filter(|(_, e)| *e == escape));
            if quote_escape_candidates.is_empty() {
                quote_escape_candidates.extend([(escape, escape), (b'"', escape), (b'\'', escape)]);
            }
        } else {
            quote_escape_candidates.extend(DEFAULT_QUOTE_ESCAPE);
        }
        Self {
            delim_candidates,
            comment_candidates,
            quote_escape_candidates,
        }
    }

    /// `Print`: the search space for error messages.
    pub fn print(&self) -> String {
        let mut result = "Delimiter Candidates: ".to_owned();
        for (i, delimiter) in self.delim_candidates.iter().enumerate() {
            let _ = write!(result, "'{delimiter}'");
            if i + 1 < self.delim_candidates.len() {
                result += ", ";
            }
        }
        result += "\nQuote/Escape Candidates: ";
        for (i, &(quote, escape)) in self.quote_escape_candidates.iter().enumerate() {
            result += "['";
            if quote == 0 {
                result += "(no quote)";
            } else {
                result.push(char::from(quote));
            }
            result += "','";
            if escape == 0 {
                result += "(no escape)";
            } else {
                result.push(char::from(escape));
            }
            result += "']";
            if i + 1 < self.quote_escape_candidates.len() {
                result += ",";
            }
        }
        result += "\nComment Candidates: ";
        for (i, &comment) in self.comment_candidates.iter().enumerate() {
            result += "'";
            result.push(char::from(comment));
            result += "'";
            if i + 1 < self.comment_candidates.len() {
                result += ", ";
            }
        }
        result += "\n";
        result
    }
}

/// The best candidate so far (DuckDB's `CandidateStats`).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct CandidateStats {
    rows_read: u64,
    best_consistent_rows: u64,
    prev_padding_count: u64,
    min_ignored_rows: u64,
}

/// `AreCommentsAcceptable`.
fn are_comments_acceptable(scanner: &ColumnCountScanner, num_cols: u64, options: &CsvReaderOptions) -> bool {
    if options
        .dialect_options
        .state_machine_options
        .comment
        .is_set_by_user()
    {
        return true;
    }
    let min_majority = 0.6;
    let mut detected_comments = 0.0;
    let mut has_full_line_comment = false;
    let mut valid_comments = 0.0;
    for count in &scanner.column_counts[..scanner.result_position as usize] {
        if count.is_comment || count.is_mid_comment {
            detected_comments += 1.0;
            if count.number_of_columns != num_cols && count.is_comment {
                has_full_line_comment = true;
                valid_comments += 1.0;
            }
            if (count.number_of_columns == num_cols
                || (count.number_of_columns <= num_cols && options.null_padding))
                && count.is_mid_comment
            {
                valid_comments += 1.0;
            }
        }
    }
    let comment = scanner.base.state_machine.state_machine_options.comment.value();
    if valid_comments == 0.0 || !has_full_line_comment {
        return comment == 0;
    }
    valid_comments / detected_comments >= min_majority
}

/// `DetectNewLineDelimiter`.
pub fn detect_new_line_delimiter(buffer_manager: &CsvBufferManager) -> Result<NewLineIdentifier> {
    let Some(buffer) = buffer_manager.get_buffer(0)? else {
        return Ok(NewLineIdentifier::SingleN);
    };
    let mut carriage_return = false;
    let mut n = false;
    for &byte in &buffer.data()[..buffer.actual_size() as usize] {
        if byte == b'\r' {
            carriage_return = true;
        } else if byte == b'\n' {
            n = true;
            break;
        } else if carriage_return {
            break;
        }
    }
    Ok(if carriage_return && n {
        NewLineIdentifier::CarryOn
    } else if carriage_return {
        NewLineIdentifier::SingleR
    } else {
        NewLineIdentifier::SingleN
    })
}

impl CsvSniffer<'_> {
    /// `GenerateStateMachineSearchSpace`.
    fn generate_state_machine_search_space(
        &self,
        dialect_candidates: &DialectCandidates,
    ) -> Result<Vec<ColumnCountScanner>> {
        let mut scanners = Vec::new();
        let sm_options = &self.options.dialect_options.state_machine_options;
        let new_line_id = if sm_options.new_line.is_set_by_user() {
            sm_options.new_line.value()
        } else {
            detect_new_line_delimiter(&self.buffer_manager)?
        };
        let mut first_iterator = CsvIterator::default();
        let mut iterator_set = false;
        for &comment in &dialect_candidates.comment_candidates {
            for &(quote, escape) in &dialect_candidates.quote_escape_candidates {
                for delimiter in &dialect_candidates.delim_candidates {
                    let state_machine_options = StateMachineOptions::new(
                        delimiter.clone(),
                        quote,
                        escape,
                        comment,
                        new_line_id,
                        sm_options.strict_mode.value(),
                    );
                    let sniffing_state_machine =
                        Arc::new(CsvStateMachine::new(self.options, &state_machine_options));
                    let mut iterator = CsvIterator::default();
                    if self.options.dialect_options.skip_rows.is_set_by_user() {
                        if !iterator_set {
                            first_iterator = skip_csv_rows(
                                &self.buffer_manager,
                                &sniffing_state_machine,
                                self.options.dialect_options.skip_rows.value(),
                            )?;
                            iterator_set = true;
                        }
                        iterator = first_iterator;
                    }
                    scanners.push(ColumnCountScanner::new(
                        Arc::clone(&self.buffer_manager),
                        sniffing_state_machine,
                        Arc::clone(&self.detection_error_handler),
                        SNIFF_SIZE,
                        iterator,
                    )?);
                }
            }
        }
        Ok(scanners)
    }

    /// `AnalyzeDialectCandidate`.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn analyze_dialect_candidate(
        &mut self,
        mut scanner: ColumnCountScanner,
        stats: &mut CandidateStats,
        successful_candidates: &mut Vec<ColumnCountScanner>,
    ) -> Result<()> {
        scanner.parse_chunk()?;
        if scanner.error {
            if scanner
                .base
                .error_handler
                .has_error(CsvErrorType::MaximumLineSize)
            {
                self.line_error = scanner
                    .base
                    .error_handler
                    .first_error(CsvErrorType::MaximumLineSize)?;
            } else {
                self.all_fail_max_line_size = false;
            }
            return Ok(());
        }
        self.all_fail_max_line_size = false;
        let mut dirty_notes: u64 = 0;
        let mut dirty_notes_minus_comments: u64 = 0;
        let mut empty_lines: u64 = 0;
        let mut consistent_rows: u64 = 0;
        let result_position = scanner.result_position;
        let mut num_cols = if result_position == 0 {
            1
        } else {
            scanner.column_counts[0].number_of_columns
        };
        let ignore_errors = self.options.ignore_errors.value();
        let use_most_frequent_columns = ignore_errors && !self.options.null_padding;
        if use_most_frequent_columns {
            num_cols = scanner.most_frequent_column_count();
        }
        let mut padding_count: u64 = 0;
        let mut comment_rows: u64 = 0;
        let mut ignored_rows: u64 = 0;
        let allow_padding = self.options.null_padding;
        let mut first_valid = false;
        if result_position > stats.rows_read {
            stats.rows_read = result_position;
        }
        let null_padding = self.options.null_padding;
        if self.set_columns.is_candidate_unacceptable(
            num_cols,
            null_padding,
            ignore_errors,
            scanner.column_counts[0].last_value_always_empty,
        ) {
            self.max_columns_found_error = self.max_columns_found_error.max(num_cols);
            return Ok(());
        }
        let skip_rows_set_by_user = self.options.dialect_options.skip_rows.is_set_by_user();
        let mut header_idx = 0usize;
        let mut rows_until_header = None;
        for row in 0..result_position as usize {
            let count = scanner.column_counts[row];
            if self.set_columns.is_candidate_unacceptable(
                count.number_of_columns,
                null_padding,
                ignore_errors,
                count.last_value_always_empty,
            ) {
                self.max_columns_found_error = self.max_columns_found_error.max(count.number_of_columns);
                return Ok(());
            }
            if count.is_comment {
                comment_rows += 1;
            } else if count.last_value_always_empty
                && count.number_of_columns == scanner.column_counts[header_idx].number_of_columns + 1
            {
                consistent_rows += 1;
            } else if num_cols < count.number_of_columns
                && (!skip_rows_set_by_user || comment_rows > 0)
                && (!self.set_columns.is_set() || null_padding)
                && (!first_valid || !use_most_frequent_columns)
            {
                if !first_valid {
                    first_valid = true;
                    rows_until_header = Some(row as u64);
                }
                padding_count = 0;
                num_cols = count.number_of_columns;
                dirty_notes = row as u64 + count.empty_lines;
                empty_lines = count.empty_lines;
                dirty_notes_minus_comments = dirty_notes.wrapping_sub(comment_rows);
                header_idx = row;
                consistent_rows = 1;
            } else if count.number_of_columns == num_cols || use_most_frequent_columns {
                if !first_valid {
                    first_valid = true;
                    rows_until_header = Some(row as u64);
                    dirty_notes = row as u64 + count.empty_lines;
                    empty_lines = count.empty_lines;
                    dirty_notes_minus_comments = dirty_notes.wrapping_sub(comment_rows);
                    num_cols = count.number_of_columns;
                }
                if count.number_of_columns != num_cols {
                    ignored_rows += 1;
                }
                consistent_rows += 1;
            } else if num_cols >= count.number_of_columns {
                padding_count += 1;
            }
        }
        {
            let sm = scanner.state_machine_mut();
            if let Some(rows) = rows_until_header {
                sm.dialect_options.rows_until_header = rows;
            }
            if sm.options.dialect_options.skip_rows.is_set_by_user() {
                sm.dialect_options.rows_until_header += sm.options.dialect_options.skip_rows.value();
            }
        }
        consistent_rows += padding_count;
        let max_columns_found = self.max_columns_found;
        let more_values = consistent_rows > stats.best_consistent_rows && num_cols >= max_columns_found;
        let more_columns = consistent_rows == stats.best_consistent_rows && num_cols > max_columns_found;
        let require_more_padding = padding_count > stats.prev_padding_count;
        let require_less_padding = padding_count < stats.prev_padding_count;
        let single_column_before =
            max_columns_found < 2 && num_cols > max_columns_found * successful_candidates.len() as u64;
        let skip_rows = self.options.dialect_options.skip_rows.value();
        let rows_consistent = consistent_rows
            .wrapping_add(dirty_notes_minus_comments.wrapping_sub(skip_rows))
            .wrapping_add(comment_rows)
            .wrapping_sub(empty_lines)
            == result_position.wrapping_sub(skip_rows);
        let more_than_one_row = consistent_rows > 1;
        let more_than_one_column = num_cols > 1;
        let start_good = successful_candidates
            .first()
            .is_some_and(|first| dirty_notes <= first.base.state_machine.dialect_options.skip_rows.value());
        let invalid_padding = !allow_padding && padding_count > 0;
        let comments_are_acceptable = are_comments_acceptable(&scanner, num_cols, self.options);
        let quoted = scanner.base.ever_quoted
            && scanner
                .base
                .state_machine
                .dialect_options
                .state_machine_options
                .quote
                .value()
                != 0;
        let set_size = self.set_columns.size();
        let columns_match_set = num_cols == set_size
            || (num_cols == set_size + 1 && scanner.column_counts[0].last_value_always_empty)
            || !self.set_columns.is_set();
        self.max_columns_found_error = self.max_columns_found_error.max(num_cols);
        let null_padding = self.options.null_padding;
        let options_ignore_errors = self.options.ignore_errors.value();

        if columns_match_set
            && (rows_consistent || (self.set_columns.is_set() && ignore_errors))
            && (single_column_before
                || ((more_values || more_columns) && !require_more_padding)
                || (more_than_one_column && require_less_padding)
                || (quoted && comment_rows == 0))
            && !invalid_padding
            && comments_are_acceptable
        {
            if let Some(first) = successful_candidates.first()
                && self.set_columns.is_set()
                && max_columns_found == set_size
                && consistent_rows <= stats.best_consistent_rows
                && (first.base.ever_quoted || !scanner.base.ever_quoted)
            {
                return Ok(());
            }
            let strict = scanner
                .base
                .state_machine
                .dialect_options
                .state_machine_options
                .strict_mode
                .value();
            if let Some(first) = successful_candidates.first()
                && first.base.ever_quoted
            {
                if !scanner.base.ever_quoted {
                    return Ok(());
                }
                if !scanner.base.ever_escaped && first.base.ever_escaped && strict {
                    return Ok(());
                }
                if stats.best_consistent_rows == consistent_rows && num_cols >= max_columns_found {
                    let sm = scanner.state_machine_mut();
                    sm.dialect_options.num_cols = num_cols;
                    if skip_rows_set_by_user {
                        if dirty_notes != 0 && !null_padding && !options_ignore_errors {
                            return Ok(());
                        }
                        sm.dialect_options.skip_rows = CsvOption::new(skip_rows);
                    } else if !null_padding {
                        sm.dialect_options.skip_rows = CsvOption::new(dirty_notes);
                    }
                    sm.dialect_options.num_cols = num_cols;
                    self.lines_sniffed = result_position;
                    successful_candidates.push(scanner);
                    self.max_columns_found = num_cols;
                    return Ok(());
                }
            }
            if max_columns_found == num_cols && ignored_rows > stats.min_ignored_rows {
                return Ok(());
            }
            if max_columns_found > 1
                && num_cols > max_columns_found
                && consistent_rows < stats.best_consistent_rows / 2
                && (null_padding || ignore_errors)
            {
                return Ok(());
            }
            if quoted && num_cols < max_columns_found {
                if scanner.base.ever_escaped && strict {
                    if successful_candidates
                        .iter()
                        .any(|candidate| candidate.base.ever_quoted && candidate.base.ever_escaped)
                    {
                        return Ok(());
                    }
                } else if successful_candidates
                    .iter()
                    .any(|candidate| candidate.base.ever_quoted)
                {
                    return Ok(());
                }
            }
            stats.best_consistent_rows = consistent_rows;
            self.max_columns_found = num_cols;
            stats.prev_padding_count = padding_count;
            stats.min_ignored_rows = ignored_rows;
            let sm = scanner.state_machine_mut();
            if skip_rows_set_by_user {
                if dirty_notes.wrapping_sub(empty_lines) != 0
                    && !null_padding
                    && !options_ignore_errors
                    && comment_rows == 0
                {
                    return Ok(());
                }
                sm.dialect_options.skip_rows = CsvOption::new(skip_rows);
            } else if !null_padding {
                sm.dialect_options.skip_rows = CsvOption::new(dirty_notes_minus_comments);
            }
            successful_candidates.clear();
            sm.dialect_options.num_cols = num_cols;
            self.lines_sniffed = result_position;
            successful_candidates.push(scanner);
            return Ok(());
        }
        if columns_match_set
            && more_than_one_row
            && more_than_one_column
            && start_good
            && rows_consistent
            && !require_more_padding
            && !invalid_padding
            && num_cols == max_columns_found
            && comments_are_acceptable
        {
            let sm = scanner.state_machine_mut();
            if skip_rows_set_by_user {
                if dirty_notes != 0 && !null_padding && !options_ignore_errors {
                    return Ok(());
                }
                sm.dialect_options.skip_rows = CsvOption::new(skip_rows);
            } else if !null_padding {
                sm.dialect_options.skip_rows = CsvOption::new(dirty_notes);
            }
            sm.dialect_options.num_cols = num_cols;
            self.lines_sniffed = result_position;
            successful_candidates.push(scanner);
        }
        Ok(())
    }

    /// `RefineCandidateNextChunk`.
    fn refine_candidate_next_chunk(&self, candidate: &mut ColumnCountScanner) -> Result<bool> {
        candidate.parse_chunk()?;
        let ignore_errors = self.options.ignore_errors.value();
        for count in &candidate.column_counts[..candidate.result_position as usize] {
            if self.set_columns.is_set() {
                return Ok(!self.set_columns.is_candidate_unacceptable(
                    count.number_of_columns,
                    self.options.null_padding,
                    ignore_errors,
                    count.last_value_always_empty,
                ));
            }
            if self.max_columns_found != count.number_of_columns
                && !self.options.null_padding
                && !ignore_errors
                && !count.is_comment
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `RefineCandidates`.
    fn refine_candidates(&mut self) -> Result<()> {
        if self.candidates.is_empty() {
            return Ok(());
        }
        if self.candidates.len() == 1 || self.candidates[0].base.finished_file() {
            return Ok(());
        }
        let mut i = 1;
        while i <= self.options.sample_size_chunks {
            let mut successful_candidates = Vec::new();
            let mut done = self.candidates.is_empty();
            for mut cur_candidate in std::mem::take(&mut self.candidates) {
                let finished_file = cur_candidate.base.finished_file();
                if successful_candidates.is_empty() {
                    self.lines_sniffed += cur_candidate.result_position;
                }
                if finished_file || i == self.options.sample_size_chunks {
                    if !cur_candidate.error {
                        successful_candidates.push(cur_candidate);
                    }
                    done = true;
                    continue;
                }
                if self.refine_candidate_next_chunk(&mut cur_candidate)? && !cur_candidate.error {
                    successful_candidates.push(cur_candidate);
                }
            }
            self.candidates = successful_candidates;
            if done {
                break;
            }
            i += 1;
        }
        let successful_candidates = std::mem::take(&mut self.candidates);
        let mut ever_quoted = false;
        for candidate in successful_candidates {
            if candidate.base.ever_quoted {
                if candidate.base.ever_escaped {
                    self.candidates.clear();
                    self.candidates.push(candidate);
                    return Ok(());
                }
                if !ever_quoted {
                    ever_quoted = true;
                    self.candidates.clear();
                }
                self.candidates.push(candidate);
            } else if !ever_quoted {
                self.candidates.push(candidate);
            }
        }
        if self.candidates.len() > 1 {
            let mut successful_candidates = std::mem::take(&mut self.candidates);
            let position = successful_candidates
                .iter()
                .position(|candidate| {
                    let options = &candidate.base.state_machine.state_machine_options;
                    options.quote.value() == options.escape.value()
                })
                .unwrap_or(0);
            self.candidates.push(successful_candidates.swap_remove(position));
        }
        Ok(())
    }

    /// `DetectDialect`.
    pub(super) fn detect_dialect(&mut self) -> Result<()> {
        let dialect_candidates = DialectCandidates::new(&self.options.dialect_options.state_machine_options);
        let mut stats = CandidateStats::default();
        let scanners = self.generate_state_machine_search_space(&dialect_candidates)?;
        let mut candidates = std::mem::take(&mut self.candidates);
        for scanner in scanners {
            self.analyze_dialect_candidate(scanner, &mut stats, &mut candidates)?;
        }
        self.candidates = candidates;
        self.refine_candidates()?;
        if self.candidates.is_empty() {
            let error = if self.all_fail_max_line_size {
                self.line_error.clone()
            } else {
                let set = self.set_columns.to_display_string();
                CsvError::sniffing_error(
                    self.options,
                    &dialect_candidates.print(),
                    self.max_columns_found_error,
                    Some((&set, self.set_columns.size() as usize)),
                    false,
                )
            };
            self.error_handler.error(error, true)?;
        }
        Ok(())
    }
}
