//! Running statements in rustdb and classifying the differences to the oracle.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use rustdb::{Database, MaterializedResult};
use rustdb_ast::{SqlStatement, SqlStatementKind};

use crate::oracle::{OracleResult, Table};

/// Runs a statement parsed by DuckDB (`json`) in a fresh in-memory rustdb database.
pub fn run_rustdb(sql: &str, json: &str) -> Result<Table, String> {
    let mut statements = rustdb_ast::json::deserialize_statements(json).map_err(|error| error.to_string())?;
    if statements.len() != 1 {
        return Err(format!("<{} statements>", statements.len()));
    }
    let statement = SqlStatement {
        location: 0,
        length: sql.len() as u64,
        query: sql.to_owned(),
        kind: SqlStatementKind::Select(statements.remove(0)),
    };
    let run = || -> Result<Table, String> {
        let database = Database::open_in_memory().map_err(|error| error.to_string())?;
        let mut connection = database.connect().map_err(|error| error.to_string())?;
        let result = connection
            .execute_statement(sql, statement)
            .map_err(|error| error.to_string())?;
        render_result(&result)
    };
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default();
            Err(format!("<panic> {message}"))
        }
    }
}

fn render_result(result: &MaterializedResult) -> Result<Table, String> {
    let mut rows = Vec::with_capacity(result.row_count());
    for row in 0..result.row_count() {
        let mut values = Vec::with_capacity(result.column_count());
        for column in 0..result.column_count() {
            let value = result.value(column, row);
            values.push(if value.is_null() {
                "NULL".to_owned()
            } else {
                value.to_varchar().map_err(|error| format!("<render failed> {error}"))?
            });
        }
        rows.push(values);
    }
    Ok(Table {
        names: result.names().to_vec(),
        types: result.types().iter().map(ToString::to_string).collect(),
        rows,
    })
}

/// A readable form of an outcome.
pub fn render(outcome: &Result<Table, String>) -> String {
    match outcome {
        Err(error) => format!("ERROR {error}"),
        Ok(table) => {
            let mut text = String::new();
            let _ = writeln!(text, "{}", table.names.join(" | "));
            let _ = writeln!(text, "{}", table.types.join(" | "));
            for row in table.rows.iter().take(20) {
                let _ = writeln!(text, "{}", row.join(" | "));
            }
            if table.rows.len() > 20 {
                let _ = writeln!(text, "... {} rows", table.rows.len());
            }
            text
        }
    }
}

/// How rustdb's outcome relates to DuckDB's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// DuckDB needs tables the statement's test file created earlier.
    NeedsTables,
    /// rustdb reports `Not implemented` (the message is the bucket).
    NotImplemented(String),
    /// A function rustdb does not provide yet.
    MissingFunction(String),
    Panic(String),
    /// A genuine difference.
    Mismatch(String),
}

impl Outcome {
    pub fn label(&self) -> String {
        match self {
            Self::Pass => "pass".to_owned(),
            Self::NeedsTables => "needs tables".to_owned(),
            Self::NotImplemented(what) => format!("not implemented: {what}"),
            Self::MissingFunction(name) => format!("missing function: {name}"),
            Self::Panic(message) => format!("panic: {message}"),
            Self::Mismatch(kind) => format!("mismatch: {kind}"),
        }
    }

    fn category(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::NeedsTables => "needs tables",
            Self::NotImplemented(_) => "not implemented",
            Self::MissingFunction(_) => "missing function",
            Self::Panic(_) => "panic",
            Self::Mismatch(_) => "mismatch",
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// The name in `... Function with name <name> does not exist!`.
fn missing_function(error: &str) -> Option<String> {
    let rest = first_line(error).split("Function with name ").nth(1)?;
    let name = rest.split(" does not exist").next()?;
    Some(name.trim_matches('"').to_owned())
}

/// Truncates bucket keys built from messages (which may embed SQL text).
fn bucket(text: &str) -> String {
    let line = first_line(text);
    let mut key: String = line.chars().take(90).collect();
    if key.len() < line.len() {
        key.push('…');
    }
    key
}

/// Classifies rustdb's outcome against DuckDB's.
pub fn compare(expected: &OracleResult, actual: &Result<Table, String>) -> Outcome {
    let expected = expected.outcome();
    match (&expected, actual) {
        (Ok(expected), Ok(actual)) => {
            if expected.names != actual.names {
                Outcome::Mismatch("column names differ".to_owned())
            } else if expected.types != actual.types {
                Outcome::Mismatch("column types differ".to_owned())
            } else if expected.rows != actual.rows {
                Outcome::Mismatch("rows differ".to_owned())
            } else {
                Outcome::Pass
            }
        }
        (Err(expected), Err(actual)) if expected == actual => Outcome::Pass,
        (_, Err(actual)) if actual.starts_with("<panic>") => Outcome::Panic(bucket(actual)),
        (Err(expected), Err(_)) if expected.starts_with("Catalog Error: Table with name") => Outcome::NeedsTables,
        (_, Err(actual)) if actual.starts_with("Not implemented Error: ") => {
            Outcome::NotImplemented(bucket(&actual["Not implemented Error: ".len()..]))
        }
        (_, Err(actual)) if missing_function(actual).is_some_and(|name| !expected_mentions(&expected, &name)) => {
            Outcome::MissingFunction(missing_function(actual).unwrap_or_default())
        }
        (Err(expected), Err(actual)) => {
            let expected_kind = expected.split(':').next().unwrap_or("");
            let actual_kind = actual.split(':').next().unwrap_or("");
            if expected_kind == actual_kind {
                Outcome::Mismatch(format!("{expected_kind} message differs"))
            } else {
                Outcome::Mismatch(format!("error kind differs ({expected_kind} vs {actual_kind})"))
            }
        }
        (Ok(_), Err(actual)) => Outcome::Mismatch(format!("rustdb error: {}", bucket(actual))),
        (Err(expected), Ok(_)) => Outcome::Mismatch(format!("duckdb error: {}", bucket(expected))),
    }
}

fn expected_mentions(expected: &Result<Table, String>, name: &str) -> bool {
    expected.as_ref().is_err_and(|error| error.contains(name))
}

/// One sample of a bucket.
struct Sample {
    id: u64,
    location: String,
    sql: String,
    expected: String,
    actual: String,
}

/// Counts by category and bucket, with samples.
#[derive(Default)]
pub struct Report {
    pub counts: BTreeMap<&'static str, usize>,
    buckets: BTreeMap<String, (usize, Vec<Sample>)>,
}

impl Report {
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        id: u64,
        file: &str,
        line: usize,
        sql: &str,
        outcome: &Outcome,
        expected: &OracleResult,
        actual: &Result<Table, String>,
    ) {
        *self.counts.entry(outcome.category()).or_default() += 1;
        if matches!(outcome, Outcome::Pass | Outcome::NeedsTables) {
            return;
        }
        let entry = self.buckets.entry(outcome.label()).or_default();
        entry.0 += 1;
        if entry.1.len() < 5 {
            entry.1.push(Sample {
                id,
                location: format!("{file}:{line}"),
                sql: sql.to_owned(),
                expected: render(&expected.outcome()),
                actual: render(actual),
            });
        }
    }

    pub fn summary(&self) -> String {
        let total: usize = self.counts.values().sum();
        let mut text = format!("{total} statements\n");
        for (category, count) in &self.counts {
            let _ = writeln!(text, "  {category:>16}: {count}");
        }
        let comparable = total - self.counts.get("needs tables").copied().unwrap_or(0);
        let passed = self.counts.get("pass").copied().unwrap_or(0);
        if comparable > 0 {
            let _ = writeln!(
                text,
                "  pass rate (without needs-tables): {:.1}%",
                passed as f64 * 100.0 / comparable as f64
            );
        }
        let mut buckets: Vec<_> = self.buckets.iter().collect();
        buckets.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        text.push_str("top buckets:\n");
        for (label, (count, _)) in buckets.iter().take(40) {
            let _ = writeln!(text, "  {count:>6}  {label}");
        }
        text
    }

    pub fn log(&self) -> String {
        let mut buckets: Vec<_> = self.buckets.iter().collect();
        buckets.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        let mut text = String::new();
        for (label, (count, samples)) in buckets {
            let _ = writeln!(text, "=== {label} ({count})");
            for sample in samples {
                let _ = writeln!(text, "--- #{} {}\n{}", sample.id, sample.location, sample.sql.trim_end());
                let _ = writeln!(text, "-- duckdb:\n{}", sample.expected.trim_end());
                let _ = writeln!(text, "-- rustdb:\n{}", sample.actual.trim_end());
            }
            text.push('\n');
        }
        text
    }
}
