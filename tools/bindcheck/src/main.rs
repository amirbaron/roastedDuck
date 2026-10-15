//! `bindcheck`: the binder/planner dev loop. Until rustdb has a parser, statements are
//! parsed by the reference CLI (`json_serialize_sql`), deserialized into rustdb's parse
//! tree, and bound, planned and executed by rustdb; results and errors are compared with
//! the CLI's.
//!
//! ```text
//! cargo run -p corpus -- extract && cargo run -p corpus -- oracle   # statements + parse trees
//! cargo run --release -p bindcheck -- oracle    # CLI results of single SELECT statements (cached)
//! cargo run --release -p bindcheck -- check     # rustdb against them → target/bindcheck/
//! cargo run --release -p bindcheck -- sql "SELECT 1 + 1"   # one statement, both engines
//! ```
//!
//! Options: `--duckdb <cli>` (default: `target/duckdb-ref`, then the reference checkout),
//! `--dir <dir>` (default `target`), `--jobs <n>` (default 8), `--filter <substring>`
//! (check only statements whose SQL contains it).

mod compare;
mod oracle;

use std::collections::BTreeMap;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use corpus::oracle::SerializedSql;
use corpus::slt::Statement;
use serde::{Deserialize, Serialize};

use crate::compare::{Outcome, Report};
use crate::oracle::OracleResult;

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

/// One extracted statement with its id (the `corpus extract` format).
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    id: u64,
    #[serde(flatten)]
    statement: Statement,
}

struct Options {
    duckdb: PathBuf,
    dir: PathBuf,
    jobs: usize,
    filter: Option<String>,
}

fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::BufReader::new(file)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

fn write_jsonl<T: Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    for row in rows {
        serde_json::to_writer(&mut out, row)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// A statement the CLI parsed into exactly one SELECT statement.
struct Candidate {
    id: u64,
    file: String,
    line: usize,
    sql: String,
    json: String,
}

/// The single-SELECT statements of the corpus, with their parse trees.
fn candidates(options: &Options) -> Result<Vec<Candidate>> {
    let corpus = options.dir.join("corpus");
    let entries: Vec<Entry> = read_jsonl(&corpus.join("statements.jsonl"))?;
    let serialized: Vec<SerializedSql> = read_jsonl(&corpus.join("serialized.jsonl"))?;
    let by_id: BTreeMap<u64, SerializedSql> = serialized.into_iter().map(|row| (row.id, row)).collect();
    let mut result = Vec::new();
    for entry in entries {
        let Some(row) = by_id.get(&entry.id) else { continue };
        let Ok(statements) = rustdb_ast::json::deserialize_statements(&row.json_latest) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        if let Some(filter) = &options.filter
            && !entry.statement.sql.contains(filter.as_str())
        {
            continue;
        }
        result.push(Candidate {
            id: entry.id,
            file: entry.statement.file,
            line: entry.statement.line,
            sql: entry.statement.sql,
            json: row.json_latest.clone(),
        });
    }
    Ok(result)
}

/// `oracle`: the CLI's result for every candidate, cached in `bindcheck/oracle.jsonl`.
fn run_oracle(options: &Options) -> Result<()> {
    let candidates = candidates(options)?;
    let out_dir = options.dir.join("bindcheck");
    std::fs::create_dir_all(&out_dir)?;
    let path = out_dir.join("oracle.jsonl");
    let mut cached: BTreeMap<u64, OracleResult> = if path.exists() {
        read_jsonl::<OracleResult>(&path)?.into_iter().map(|row| (row.id, row)).collect()
    } else {
        BTreeMap::new()
    };
    let missing: Vec<(u64, &str)> = candidates
        .iter()
        .filter(|candidate| !cached.contains_key(&candidate.id))
        .map(|candidate| (candidate.id, candidate.sql.as_str()))
        .collect();
    eprintln!("{} candidates, {} not cached", candidates.len(), missing.len());
    for row in oracle::run_all(&options.duckdb, &missing, options.jobs) {
        cached.insert(row.id, row);
    }
    let rows: Vec<OracleResult> = cached.into_values().collect();
    write_jsonl(&path, &rows)?;
    println!("{} oracle results → {}", rows.len(), path.display());
    Ok(())
}

/// `check`: rustdb against the cached oracle results.
fn run_check(options: &Options) -> Result<bool> {
    let candidates = candidates(options)?;
    let out_dir = options.dir.join("bindcheck");
    let oracle: BTreeMap<u64, OracleResult> = read_jsonl::<OracleResult>(&out_dir.join("oracle.jsonl"))?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let mut report = Report::default();
    for candidate in &candidates {
        let Some(expected) = oracle.get(&candidate.id) else { continue };
        let actual = compare::run_rustdb(&candidate.sql, &candidate.json);
        let outcome = compare::compare(expected, &actual);
        report.add(candidate.id, &candidate.file, candidate.line, &candidate.sql, &outcome, expected, &actual);
    }
    let log_path = out_dir.join("mismatches.txt");
    std::fs::write(&log_path, report.log())?;
    print!("{}", report.summary());
    println!("details → {}", log_path.display());
    Ok(report.counts.get("mismatch").copied().unwrap_or(0) == 0)
}

/// `sql <text>`: one statement through both engines.
fn run_sql(options: &Options, sql: &str) -> Result<bool> {
    let json = oracle::serialize(&options.duckdb, sql)?;
    let expected = oracle::run_one(&options.duckdb, 0, sql);
    let actual = compare::run_rustdb(sql, &json);
    println!("== duckdb\n{}", compare::render(&expected.outcome()));
    println!("== rustdb\n{}", compare::render(&actual));
    let outcome = compare::compare(&expected, &actual);
    println!("== {}", outcome.label());
    Ok(matches!(outcome, Outcome::Pass))
}

fn run() -> Result<bool> {
    let root = workspace_root();
    let local_cli = root.join("target/duckdb-ref");
    let mut options = Options {
        duckdb: if local_cli.exists() {
            local_cli
        } else {
            corpus::oracle::default_duckdb(&root)
        },
        dir: root.join("target"),
        jobs: 8,
        filter: None,
    };
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        if !arg.starts_with("--") {
            positional.push(arg);
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--duckdb" => options.duckdb = value.into(),
            "--dir" => options.dir = value.into(),
            "--jobs" => options.jobs = value.parse()?,
            "--filter" => options.filter = Some(value),
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    match command.as_str() {
        "oracle" => run_oracle(&options).map(|()| true),
        "check" => run_check(&options),
        "sql" => run_sql(&options, &positional.join(" ")),
        _ => Err("usage: bindcheck <oracle | check | sql TEXT> [--duckdb CLI] [--dir DIR] [--jobs N] [--filter S]".into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("bindcheck: {error}");
            ExitCode::FAILURE
        }
    }
}
