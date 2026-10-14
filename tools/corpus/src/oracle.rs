//! Running the reference DuckDB CLI over many statements.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;

use serde::{Deserialize, Serialize};

/// The `json_serialize_sql` outputs for one statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedSql {
    pub id: u64,
    /// Default options.
    pub json: String,
    /// `skip_null`, `skip_empty` and `skip_default` all set.
    pub json_skip: String,
    /// `skip_null` and `skip_default` set: reduced, but still readable (unlike
    /// `skip_empty`, which drops required properties).
    pub json_skip_defaults: String,
    /// `format := true`.
    pub json_pretty: String,
}

#[derive(Serialize)]
struct Input<'a> {
    id: u64,
    sql: &'a str,
}

/// Single quotes a path for a SQL string literal.
fn sql_literal(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Runs one batch through the CLI; `None` if the process failed.
fn run_batch(
    duckdb: &Path,
    work_dir: &Path,
    batch_id: usize,
    batch: &[(u64, &str)],
) -> Option<Vec<SerializedSql>> {
    let input = work_dir.join(format!("batch-{batch_id}.in.jsonl"));
    let output = work_dir.join(format!("batch-{batch_id}.out.jsonl"));
    let mut text = Vec::new();
    for (id, sql) in batch {
        serde_json::to_writer(&mut text, &Input { id: *id, sql }).ok()?;
        text.push(b'\n');
    }
    std::fs::write(&input, text).ok()?;
    let query = format!(
        "COPY (SELECT id, \
           json_serialize_sql(sql)::VARCHAR AS json, \
           json_serialize_sql(sql, skip_null := true, skip_empty := true, skip_default := true)::VARCHAR AS json_skip, \
           json_serialize_sql(sql, skip_null := true, skip_default := true)::VARCHAR AS json_skip_defaults, \
           json_serialize_sql(sql, format := true)::VARCHAR AS json_pretty \
         FROM read_json({}, format = 'newline_delimited', columns = {{id: 'UBIGINT', sql: 'VARCHAR'}}) \
         ORDER BY id) TO {} (FORMAT json);",
        sql_literal(&input),
        sql_literal(&output)
    );
    let status = Command::new(duckdb)
        .arg(":memory:")
        .arg("-c")
        .arg(query)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&input);
    if !status.status.success() {
        return None;
    }
    let text = std::fs::read_to_string(&output).ok()?;
    let _ = std::fs::remove_file(&output);
    text.lines().map(|line| serde_json::from_str(line).ok()).collect()
}

/// Runs a batch, splitting it on failure so that one statement crashing the CLI only
/// loses itself. Returns the results and the ids that could not be processed.
fn run_resilient(
    duckdb: &Path,
    work_dir: &Path,
    batch_id: usize,
    batch: &[(u64, &str)],
) -> (Vec<SerializedSql>, Vec<u64>) {
    if let Some(result) = run_batch(duckdb, work_dir, batch_id, batch) {
        return (result, Vec::new());
    }
    if batch.len() == 1 {
        return (Vec::new(), vec![batch[0].0]);
    }
    let (left, right) = batch.split_at(batch.len() / 2);
    let (mut result, mut failed) = run_resilient(duckdb, work_dir, batch_id, left);
    let (more, more_failed) = run_resilient(duckdb, work_dir, batch_id, right);
    result.extend(more);
    failed.extend(more_failed);
    (result, failed)
}

/// Serializes every statement with the reference CLI, in parallel batches.
pub fn serialize_sql(
    duckdb: &Path,
    work_dir: &Path,
    statements: &[(u64, &str)],
    jobs: usize,
) -> (Vec<SerializedSql>, Vec<u64>) {
    const BATCH: usize = 2000;
    let batches: Vec<&[(u64, &str)]> = statements.chunks(BATCH).collect();
    let next = Mutex::new(0usize);
    let results = Mutex::new((Vec::new(), Vec::new()));
    thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = {
                        let Ok(mut next) = next.lock() else { return };
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some(batch) = batches.get(index) else { return };
                    let (rows, failed) = run_resilient(duckdb, work_dir, index, batch);
                    if let Ok(mut results) = results.lock() {
                        results.0.extend(rows);
                        results.1.extend(failed);
                    }
                    eprint!(".");
                    let _ = std::io::stderr().flush();
                }
            });
        }
    });
    eprintln!();
    let (mut rows, mut failed) = results.into_inner().unwrap_or_default();
    rows.sort_by_key(|row: &SerializedSql| row.id);
    failed.sort_unstable();
    (rows, failed)
}

/// The default location of the reference CLI.
pub fn default_duckdb(workspace: &Path) -> PathBuf {
    workspace.join(".context/duckdb-download/duckdb-1.5.6/build/release/duckdb")
}
