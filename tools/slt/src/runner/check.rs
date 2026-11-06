//! Result checks (port of `TestResultHelper::CheckQueryResult` / `CheckStatementResult`).
//!
//! The checks return `Err(message)` where DuckDB's helper logs a failure and returns `false`;
//! the caller turns that into a test failure (or, in a concurrent loop, a deferred one).

use std::fmt::Write as _;
use std::sync::PoisonError;

use super::Runner;
use crate::backend::{ColumnClass, Engine, QueryResult};
use crate::commands::{ExecuteContext, QueryCommand, StatementCommand};
use crate::compare;
use crate::parser::ExpectedResult;
use crate::text;

/// Values shown in failure messages before eliding the rest.
const MAX_VALUES_SHOWN: usize = 40;

impl<E: Engine> Runner<'_, E> {
    /// `SkipErrorMessage`: errors that end the test as skipped (e.g. no network).
    fn skip_error_message(&self, message: &str) -> bool {
        let Some(ignored) = self
            .ignore_error_messages
            .iter()
            .find(|ignored| message.contains(ignored.as_str()))
        else {
            return false;
        };
        self.skip_test(format!("skip on error_message matching '{ignored}'"));
        true
    }

    /// `FailureSummary::SkipLoggingSameError`: `true` if `file` already reported a failure.
    fn failure_already_reported(&self, file: &str) -> bool {
        if file.is_empty() {
            return false;
        }
        let mut reported = self.reported_files.lock().unwrap_or_else(PoisonError::into_inner);
        !reported.insert(file.to_owned())
    }

    /// `TestResultHelper::CheckStatementResult`.
    pub(super) fn check_statement_result(
        &self,
        statement: &StatementCommand,
        ctx: &ExecuteContext,
        result: &E::Result,
    ) -> Result<(), String> {
        let error = result.error();
        if self.modes.result || self.modes.debug {
            print_result(result);
        }
        let mut failed = error.is_some();
        if statement.expected != ExpectedResult::Success {
            if let Some(error) = error
                && self
                    .always_fail_error_messages
                    .iter()
                    .any(|message| error.contains(message.as_str()))
            {
                return Err(format!(
                    "{}\nQuery failed with an internal error:\n{error}",
                    ctx.sql
                ));
            }
            failed = match statement.expected {
                ExpectedResult::Unknown | ExpectedResult::DontCare => false,
                _ => !failed,
            };
            if let Some(error) = error
                && !statement.expected_error.is_empty()
                && !error.contains(&statement.expected_error)
                && !error.contains(&self.replace_keywords(&statement.expected_error))
            {
                let matched = compare::is_regex(&statement.expected_error)
                    && compare::matches_regex(&format!("{error}\n"), &statement.expected_error);
                if !matched
                    && !self.skip_error_message(error)
                    && !self.failure_already_reported(&self.file_name)
                {
                    return Err(format!(
                        "{}\nQuery failed, but error message did not match expected error message: {}\nActual \
                         error: {error}",
                        ctx.sql, statement.expected_error
                    ));
                }
                return Ok(());
            }
        }
        if !failed {
            return Ok(());
        }
        if statement.expected == ExpectedResult::Success
            && let Some(error) = error
            && self.skip_error_message(error)
        {
            self.finish_processing_file();
            return Ok(());
        }
        self.failure_already_reported(&self.file_name);
        Err(match error {
            Some(error) if statement.expected == ExpectedResult::Success => {
                format!("{}\nQuery unexpectedly failed:\n{error}", ctx.sql)
            }
            _ => format!("{}\nQuery unexpectedly succeeded!", ctx.sql),
        })
    }

    /// `TestResultHelper::CheckQueryResult`.
    pub(super) fn check_query_result(
        &self,
        query: &QueryCommand,
        ctx: &ExecuteContext,
        mut result: E::Result,
    ) -> Result<(), String> {
        if let Some(error) = result.error() {
            if self.skip_error_message(error) {
                self.finish_processing_file();
                return Ok(());
            }
            return Err(format!("{}\nQuery unexpectedly failed:\n{error}", ctx.sql));
        }
        let context = self.main_connection().map_err(|failure| failure.message)?;
        let row_count = result.row_count();
        let column_count = result.column_count();
        let total_values = row_count * column_count;
        let result_is_hash = query.values.len() == 1 && compare::result_is_hash(&query.values[0]);
        let compare_hash = query.label.is_some()
            || usize::try_from(self.hash_threshold)
                .is_ok_and(|threshold| threshold > 0 && total_values > threshold)
            || result_is_hash;

        let mut actual = self
            .engine
            .render(context, &mut result, self.original_sqlite_test)?;
        if self.modes.result {
            print_values(&result, &actual);
        }
        compare::sort_values(query.sort_style, &mut actual, column_count)?;

        let mut expected_column_count = query.expected_column_count;
        let expected = match query.values.as_slice() {
            [line] if line.starts_with("<FILE>:") => {
                let file = self.replace_keywords(&line.replace("<FILE>:", ""));
                let file = self
                    .loop_replacement(&file, &ctx.running_loops)
                    .map_err(|failure| failure.message)?;
                let names: Vec<String> = (0..column_count)
                    .map(|c| {
                        std::str::from_utf8(result.column_name(c))
                            .map(str::to_owned)
                            .map_err(|_| "Result-file SQL requires a UTF-8 column name".to_owned())
                    })
                    .collect::<Result<_, _>>()?;
                let (values, columns) = self.engine.load_result_file(context, &file, &names)?;
                expected_column_count = columns;
                values
            }
            _ => query.values.clone(),
        };

        let hash = (self.modes.hash || compare_hash).then(|| compare::hash_values(&actual[..total_values]));
        if self.modes.hash {
            println!("{}", hash.unwrap_or_default());
            return Ok(());
        }
        match hash {
            Some(hash) => self.compare_hash(query, result_is_hash, &hash),
            None => self.compare_values(
                &result,
                &actual,
                expected,
                expected_column_count,
                query.expected_column_count,
            ),
        }
    }

    /// The hash branch of `CheckQueryResult`: labels must agree with earlier queries of the same
    /// label, and an explicit `N values hashing to ...` must match.
    fn compare_hash(&self, query: &QueryCommand, result_is_hash: bool, hash: &str) -> Result<(), String> {
        let mut mismatch = None;
        if let Some(label) = &query.label {
            let mut labels = self.hash_labels.lock().unwrap_or_else(PoisonError::into_inner);
            match labels.get(label) {
                None => {
                    labels.insert(label.clone(), hash.to_owned());
                }
                Some(previous) if previous != hash => {
                    mismatch = Some(format!("label {label} expected {previous}"));
                }
                Some(_) => {}
            }
        }
        if result_is_hash {
            mismatch = (query.values[0] != hash).then(|| format!("expected {}", query.values[0]));
        }
        match mismatch {
            Some(expected) => Err(format!("Wrong result hash! {expected}, got {hash}")),
            None => Ok(()),
        }
    }

    /// The value branch of `CheckQueryResult`.
    fn compare_values(
        &self,
        result: &E::Result,
        actual: &[String],
        mut expected: Vec<String>,
        expected_column_count: usize,
        original_expected_columns: usize,
    ) -> Result<(), String> {
        let row_count = result.row_count();
        let total_values = row_count * result.column_count();
        let column_count_mismatch = expected_column_count != result.column_count();
        let columns = result.column_count();
        if columns == 0 {
            return Err("Query returned no columns".to_owned());
        }
        let mismatch_message = |what: &str| {
            if column_count_mismatch {
                format!(
                    "Wrong column count in query! Expected {original_expected_columns} columns, got {columns} \
                     columns\n{}",
                    describe_values(actual, columns)
                )
            } else {
                what.to_owned()
            }
        };
        let mut row_wise = columns > 1 && expected.len() == row_count;
        if !row_wise {
            row_wise = expected.iter().all(|value| value.contains('\t'));
        }
        let expected_rows = if row_wise {
            expected.len()
        } else {
            if !expected.len().is_multiple_of(columns) {
                return Err(mismatch_message(&format!(
                    "Error in test! Column count mismatch after splitting on tab on row! Expected {} values, \
                     which is not cleanly divisible by {columns} columns",
                    expected.len()
                )));
            }
            expected.len() / columns
        };
        if expected_rows != row_count {
            return Err(mismatch_message(&format!(
                "Wrong row count in query! Expected {expected_rows} rows, got {row_count} rows\nActual result:\n{}",
                describe_values(actual, columns)
            )));
        }
        if row_wise {
            let mut split_values = Vec::with_capacity(total_values);
            for (row, line) in expected.iter().take(total_values).enumerate() {
                let splits = text::split(line, "\t");
                if splits.len() != columns {
                    return Err(mismatch_message(&format!(
                        "Error in test! Column count mismatch after splitting on tab on row {}! Expected {columns} \
                         columns, got {} columns",
                        row + 1,
                        splits.len()
                    )));
                }
                split_values.extend(splits);
            }
            expected = split_values;
        }
        // DuckDB retries with the configured default sort style; the default (NO_SORT) means a
        // single comparison pass.
        for (index, (actual_value, expected_value)) in
            actual.iter().zip(&expected).take(total_values).enumerate()
        {
            let column = index % columns;
            if !self.value_matches(result, column, actual_value, expected_value) {
                return Err(format!(
                    "Wrong result in query! Mismatch on row {}, column {}(index {})\n{actual_value} <> \
                     {expected_value}\nActual result:\n{}",
                    index / columns + 1,
                    String::from_utf8_lossy(result.column_name(column)),
                    column + 1,
                    describe_values(actual, columns)
                ));
            }
        }
        if column_count_mismatch {
            return Err(format!(
                "Wrong column count in query! Expected {original_expected_columns} columns, got {columns} columns \
                 (the values themselves are correct)"
            ));
        }
        Ok(())
    }

    /// `TestResultHelper::CompareValues` without the logging.
    fn value_matches(&self, result: &E::Result, column: usize, actual: &str, expected: &str) -> bool {
        if actual == expected || actual == self.replace_keywords(expected) {
            return true;
        }
        if compare::is_regex(expected) && compare::matches_regex(actual, expected) {
            return true;
        }
        match result.column_class(column) {
            ColumnClass::Numeric => self.main_connection().is_ok_and(|context| {
                self.engine
                    .values_equal(context, result, column, actual, expected)
            }),
            ColumnClass::Boolean => compare::boolean_values_equal(actual, expected),
            ColumnClass::Other => false,
        }
    }
}

/// Up to [`MAX_VALUES_SHOWN`] values formatted as tab-separated rows.
fn describe_values(values: &[String], columns: usize) -> String {
    let mut out = String::new();
    for (index, value) in values.iter().take(MAX_VALUES_SHOWN).enumerate() {
        out.push_str(value);
        out.push(if (index + 1) % columns.max(1) == 0 {
            '\n'
        } else {
            '\t'
        });
    }
    if values.len() > MAX_VALUES_SHOWN {
        let _ = write!(out, "... ({} values total)", values.len());
    }
    out
}

fn print_result(result: &impl QueryResult) {
    match result.error() {
        Some(error) => eprintln!("{error}"),
        None => eprintln!("[ Rows: {}]", result.row_count()),
    }
}

fn print_values(result: &impl QueryResult, values: &[String]) {
    let columns = result.column_count();
    let header: Vec<String> = (0..columns).map(|c| result.column_type(c)).collect();
    eprintln!("{}\n{}", header.join("\t"), describe_values(values, columns));
}
