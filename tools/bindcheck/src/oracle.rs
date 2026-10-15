//! Results of the reference CLI, one process per statement (errors must be attributed
//! to their statement, and a failing statement must not affect the next).

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Result;

/// Field and record separators of the CLI's list mode (ASCII unit / record separators).
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';
const TIMEOUT: Duration = Duration::from_secs(20);

/// A query result: column names, types as DuckDB prints them, and rendered rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub names: Vec<String>,
    pub types: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// The CLI's outcome for one statement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleResult {
    pub id: u64,
    /// The full error message, if the statement failed.
    pub error: Option<String>,
    #[serde(default)]
    pub table: Table,
    /// `json_serialize_plan` of a statement that succeeded.
    #[serde(default)]
    pub plan: Option<String>,
}

impl OracleResult {
    pub fn outcome(&self) -> Result<Table, String> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(self.table.clone()),
        }
    }
}

/// Runs the CLI with `sql`; `Ok(stdout)` or `Err(stderr)`.
fn run_cli(duckdb: &Path, commands: &[&str]) -> Result<String, String> {
    let mut child = Command::new(duckdb)
        .args(["-init", "/dev/null", "-list", "-header", "-nullvalue", "NULL"])
        .args(["-separator", &FIELD.to_string(), "-newline", &RECORD.to_string()])
        .args(commands.iter().flat_map(|command| ["-c", command]))
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start the CLI: {error}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(stdout) = &mut stdout {
            let _ = stdout.read_to_end(&mut out);
        }
        if let Some(stderr) = &mut stderr {
            let _ = stderr.read_to_end(&mut err);
        }
        (out, err)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("<timeout>".to_owned());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(error) => return Err(format!("<wait failed: {error}>")),
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        let message = String::from_utf8_lossy(&err).trim_end().to_owned();
        Err(if message.is_empty() {
            format!("<CLI exited with {status}>")
        } else {
            message
        })
    }
}

/// The records of list-mode output (every record, including the header, ends with the
/// record separator).
fn records(output: &str) -> Vec<Vec<String>> {
    let output = output.strip_suffix(RECORD).unwrap_or(output);
    if output.is_empty() {
        return Vec::new();
    }
    output
        .split(RECORD)
        .map(|record| record.split(FIELD).map(str::to_owned).collect())
        .collect()
}

/// Single-quotes `text` for a SQL string literal.
fn sql_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The CLI's result for one statement: its rows, then (in a second process, so that a
/// failing statement keeps its own error) the column names and types from `DESCRIBE`
/// and the unoptimized plan from `json_serialize_plan`.
pub fn run_one(duckdb: &Path, id: u64, sql: &str) -> OracleResult {
    let failed = |error: String| OracleResult {
        id,
        error: Some(error),
        table: Table::default(),
        plan: None,
    };
    let rows = match run_cli(duckdb, &[sql]) {
        // The first record is the header.
        Ok(output) => records(&output).into_iter().skip(1).collect(),
        Err(error) => return failed(error),
    };
    let trimmed = sql.trim_end().trim_end_matches(';');
    let describe = format!("DESCRIBE {trimmed}");
    let plan = format!(
        "SELECT json_serialize_plan({}, skip_null := true, skip_empty := true)::VARCHAR",
        sql_literal(sql)
    );
    match run_cli(duckdb, &[&describe, &plan]) {
        Ok(output) => {
            let mut records = records(&output);
            // DESCRIBE: a header and one six-field record per column; then the plan's
            // header and value.
            let plan = match records.as_slice() {
                [.., last] if last.len() == 1 => records.pop().and_then(|mut record| record.pop()),
                _ => None,
            };
            let columns: Vec<Vec<String>> = records
                .into_iter()
                .skip(1)
                .filter(|record| record.len() == 6)
                .collect();
            OracleResult {
                id,
                error: None,
                table: Table {
                    names: columns.iter().map(|column| column[0].clone()).collect(),
                    types: columns.iter().map(|column| column[1].clone()).collect(),
                    rows,
                },
                plan,
            }
        }
        Err(error) => failed(format!("<DESCRIBE failed> {error}")),
    }
}

/// The CLI's results for many statements, `jobs` processes at a time.
pub fn run_all(duckdb: &Path, statements: &[(u64, &str)], jobs: usize) -> Vec<OracleResult> {
    let next = Mutex::new(0usize);
    let results = Mutex::new(Vec::with_capacity(statements.len()));
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = {
                        let Ok(mut next) = next.lock() else { return };
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some(&(id, sql)) = statements.get(index) else {
                        return;
                    };
                    let result = run_one(duckdb, id, sql);
                    if let Ok(mut results) = results.lock() {
                        results.push(result);
                    }
                    if index % 1000 == 999 {
                        eprintln!("{} / {}", index + 1, statements.len());
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_default();
    results.sort_by_key(|result| result.id);
    results
}

/// `json_serialize_sql(sql)` as the parser builds it (`storage_compatibility_version =
/// 'latest'`).
pub fn serialize(duckdb: &Path, sql: &str) -> Result<String> {
    let query = format!(
        "SET storage_compatibility_version = 'latest'; SELECT json_serialize_sql({})::VARCHAR",
        sql_literal(sql)
    );
    let output = run_cli(duckdb, &[&query])?;
    // Skip the header.
    records(&output)
        .into_iter()
        .nth(1)
        .and_then(|row| row.into_iter().next())
        .ok_or_else(|| "json_serialize_sql returned nothing".into())
}
