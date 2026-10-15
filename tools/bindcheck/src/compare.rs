//! Running statements in rustdb and classifying the differences to the oracle.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use rustdb::{Database, MaterializedResult};
use rustdb_ast::{SqlStatement, SqlStatementKind};

use crate::oracle::{OracleResult, Table};
use crate::plan_shape::{duckdb_shape, rust_shape};

/// rustdb's outcome for a statement: its result, and the shape of its logical plan.
#[derive(Debug)]
pub struct RustOutcome {
    pub result: Result<Table, String>,
    pub plan: Result<String, String>,
}

/// Runs `run`, turning a panic into an error message.
fn guarded<T>(run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
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

/// Runs a statement parsed by DuckDB (`json`) in a fresh in-memory rustdb database.
pub fn run_rustdb(sql: &str, json: &str) -> RustOutcome {
    let statement = || -> Result<SqlStatement, String> {
        let mut statements = rustdb_ast::json::deserialize_statements(json).map_err(|error| error.to_string())?;
        if statements.len() != 1 {
            return Err(format!("<{} statements>", statements.len()));
        }
        Ok(SqlStatement {
            location: 0,
            length: sql.len() as u64,
            query: sql.to_owned(),
            named_param_map: Default::default(),
            kind: SqlStatementKind::Select(statements.remove(0)),
        })
    };
    let connect = || -> Result<rustdb::Connection, String> {
        let database = Database::open_in_memory().map_err(|error| error.to_string())?;
        database.connect().map_err(|error| error.to_string())
    };
    let result = guarded(|| {
        let result = connect()?
            .execute_statement(sql, statement()?)
            .map_err(|error| error.to_string())?;
        render_result(&result)
    });
    let plan = guarded(|| {
        let plan = connect()?
            .logical_plan(sql, statement()?)
            .map_err(|error| error.to_string())?;
        Ok(rust_shape(&plan))
    });
    RustOutcome { result, plan }
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
    /// rustdb cannot execute the plan yet, but built the same logical plan as DuckDB.
    PlanPass,
    /// DuckDB needs tables the statement's test file created earlier.
    NeedsTables,
    /// rustdb reports `Not implemented` (the message is the bucket).
    NotImplemented(String),
    /// A function, overload, cast or system table rustdb does not provide yet (outside
    /// the binder's scope).
    Environment(String),
    Panic(String),
    /// A genuine difference.
    Mismatch(String),
}

impl Outcome {
    pub fn label(&self) -> String {
        match self {
            Self::Pass => "pass".to_owned(),
            Self::PlanPass => "pass (plan)".to_owned(),
            Self::NeedsTables => "needs tables".to_owned(),
            Self::NotImplemented(what) => format!("not implemented: {what}"),
            Self::Environment(what) => format!("environment: {what}"),
            Self::Panic(message) => format!("panic: {message}"),
            Self::Mismatch(kind) => format!("mismatch: {kind}"),
        }
    }

    fn category(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::PlanPass => "pass (plan)",
            Self::NeedsTables => "needs tables",
            Self::NotImplemented(_) => "not implemented",
            Self::Environment(_) => "environment",
            Self::Panic(_) => "panic",
            Self::Mismatch(_) => "mismatch",
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// The name in `... Function with name <name> does not exist!` (or `type <name>` for
/// `Type with name <name> does not exist!`).
fn missing_function(error: &str) -> Option<String> {
    let line = first_line(error);
    let (rest, prefix) = match line.split("Function with name ").nth(1) {
        Some(rest) => (rest, ""),
        None => (line.split("Type with name ").nth(1)?, "type "),
    };
    let name = rest.split(" does not exist").next()?;
    Some(format!("{prefix}{}", name.trim_matches('"')))
}

/// Whether DuckDB's error says the statement needs tables or files from its test file.
fn needs_environment(expected: &str) -> bool {
    expected.starts_with("Catalog Error: Table with name")
        || expected.starts_with("IO Error: No files found")
        // The CLI cannot describe SHOW/DESCRIBE/SUMMARIZE statements, so their column
        // types are unknown.
        || expected.starts_with("<DESCRIBE failed> Parser Error")
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

/// Compares plan shapes when DuckDB serialized its plan.
fn compare_plans(expected: &OracleResult, actual: &RustOutcome) -> Option<Outcome> {
    let expected = duckdb_shape(expected.plan.as_deref()?).ok()?;
    Some(match &actual.plan {
        Ok(plan) if *plan == expected => Outcome::PlanPass,
        Ok(_) => Outcome::Mismatch("plan shape differs".to_owned()),
        Err(error) => Outcome::Mismatch(format!("planning failed: {}", bucket(error))),
    })
}

/// Classifies rustdb's outcome against DuckDB's.
pub fn compare(oracle: &OracleResult, rust: &RustOutcome) -> Outcome {
    let expected = oracle.outcome();
    let actual = &rust.result;
    if expected.is_ok()
        && let Err(error) = actual
        && error.starts_with("Not implemented Error: physical plan for logical operator")
        && let Some(outcome) = compare_plans(oracle, rust)
    {
        return outcome;
    }
    match (&expected, actual) {
        (Ok(expected), Ok(actual)) => {
            if expected.names != actual.names {
                Outcome::Mismatch("column names differ".to_owned())
            } else if expected.types != actual.types {
                Outcome::Mismatch("column types differ".to_owned())
            } else if expected.rows != actual.rows {
                Outcome::Mismatch("rows differ".to_owned())
            } else {
                match compare_plans(oracle, rust) {
                    Some(Outcome::Mismatch(kind)) => Outcome::Mismatch(format!("result ok, {kind}")),
                    _ => Outcome::Pass,
                }
            }
        }
        (Err(expected), Err(actual)) if expected == actual => Outcome::Pass,
        (_, Err(actual)) if actual.starts_with("<panic>") => Outcome::Panic(bucket(actual)),
        (Err(expected), _) if needs_environment(expected) => Outcome::NeedsTables,
        (_, Err(actual)) if actual.starts_with("Not implemented Error: ") => {
            Outcome::NotImplemented(bucket(&actual["Not implemented Error: ".len()..]))
        }
        (_, Err(actual)) if let Some(gap) = environment_gap(&expected, actual) => Outcome::Environment(gap),
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

/// What rustdb lacks outside the binder when its error stems from a missing function,
/// overload, cast or system table.
fn environment_gap(expected: &Result<Table, String>, actual: &str) -> Option<String> {
    let expected_line = expected.as_ref().err().map(|error| first_line(error));
    let actual_line = first_line(actual);
    if let Some(name) = missing_function(actual) {
        return Some(if same_missing_entry(expected, actual) {
            // Both miss the entry; the suggestions come from different catalogs.
            "missing function (suggestions differ)".to_owned()
        } else {
            format!("missing function {name}")
        });
    }
    if let Some(rest) = actual_line.strip_prefix("Binder Error: Function \"")
        && actual_line.contains("is a table function but it was used as a scalar function")
        && expected_line != Some(actual_line)
    {
        return Some(format!("missing scalar function {}", rest.split('"').next().unwrap_or(rest)));
    }
    if let Some(rest) = actual_line.split("Unimplemented type for cast (").nth(1)
        && expected_line != Some(actual_line)
    {
        return Some(format!("missing cast {}", rest.trim_end_matches(')')));
    }
    if let Some(rest) = actual_line.split("No function matches the given name and argument types '").nth(1) {
        let call = rest.split("'.").next().unwrap_or(rest);
        let name = call.split('(').next().unwrap_or(call);
        let duckdb_also = expected_line.is_some_and(|line| line.contains("No function matches"));
        return Some(if duckdb_also {
            format!("overloads of {name} differ")
        } else {
            format!("missing overload {name}")
        });
    }
    if expected.is_ok()
        && let Some(rest) = actual_line.strip_prefix("Catalog Error: Table with name ")
    {
        return Some(format!("missing system table {}", rest.trim_end_matches(" does not exist!")));
    }
    None
}

/// Whether DuckDB also failed to find the entry rustdb is missing (then the messages
/// are compared as usual).
fn same_missing_entry(expected: &Result<Table, String>, actual: &str) -> bool {
    expected
        .as_ref()
        .is_err_and(|error| first_line(error) == first_line(actual))
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
        actual: &RustOutcome,
    ) {
        *self.counts.entry(outcome.category()).or_default() += 1;
        if matches!(outcome, Outcome::Pass | Outcome::PlanPass | Outcome::NeedsTables) {
            return;
        }
        let entry = self.buckets.entry(outcome.label()).or_default();
        entry.0 += 1;
        if entry.1.len() < 5 {
            entry.1.push(Sample {
                id,
                location: format!("{file}:{line}"),
                sql: sql.to_owned(),
                expected: format!(
                    "{}{}",
                    render(&expected.outcome()),
                    expected
                        .plan
                        .as_deref()
                        .map(|plan| format!("plan: {}", duckdb_shape(plan).unwrap_or_else(|error| error)))
                        .unwrap_or_default()
                ),
                actual: format!(
                    "{}plan: {}",
                    render(&actual.result),
                    actual.plan.as_ref().unwrap_or_else(|error| error)
                ),
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
        let passed =
            self.counts.get("pass").copied().unwrap_or(0) + self.counts.get("pass (plan)").copied().unwrap_or(0);
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
