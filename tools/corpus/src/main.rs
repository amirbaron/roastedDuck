//! `corpus`: statements from DuckDB's test corpus, checked against the reference CLI.
//!
//! ```text
//! cargo run -p corpus -- extract                # SQL of every record → target/corpus/statements.jsonl
//! cargo run -p corpus -- oracle                 # json_serialize_sql via the C++ CLI → target/corpus/serialized.jsonl
//! cargo run --release -p corpus -- serialization # rustdb must reproduce DuckDB's output exactly
//! ```
//!
//! Options: `--tests <dir>` (default: the reference checkout's `test/`), `--duckdb <cli>`,
//! `--dir <output dir>` (default `target/corpus`).

mod check;
mod walcheck;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use corpus::oracle::{self, SerializedSql};
use corpus::slt::{self, Statement};
use serde::{Deserialize, Serialize};

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

/// One extracted statement with its id.
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    id: u64,
    #[serde(flatten)]
    statement: Statement,
}

struct Options {
    tests: PathBuf,
    duckdb: PathBuf,
    dir: PathBuf,
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

/// `extract`: unique statement texts, without unsubstituted loop variables.
fn extract(options: &Options) -> Result<()> {
    let (statements, unreadable) = slt::extract_dir(&options.tests)?;
    let total = statements.len();
    let mut seen = HashSet::new();
    let mut with_variables = 0;
    let mut entries = Vec::new();
    for statement in statements {
        if slt::has_unsubstituted_variable(&statement.sql) {
            with_variables += 1;
            continue;
        }
        if seen.insert(statement.sql.clone()) {
            entries.push(Entry {
                id: entries.len() as u64,
                statement,
            });
        }
    }
    std::fs::create_dir_all(&options.dir)?;
    let path = options.dir.join("statements.jsonl");
    write_jsonl(&path, &entries)?;
    println!(
        "{total} records, {with_variables} with loop variables, {} unique statements ({unreadable} non-UTF-8 files skipped) → {}",
        entries.len(),
        path.display()
    );
    Ok(())
}

/// `oracle`: `json_serialize_sql` of every statement from the reference CLI.
fn oracle(options: &Options) -> Result<()> {
    let entries: Vec<Entry> = read_jsonl(&options.dir.join("statements.jsonl"))?;
    let statements: Vec<(u64, &str)> = entries.iter().map(|e| (e.id, e.statement.sql.as_str())).collect();
    let work = options.dir.join("work");
    std::fs::create_dir_all(&work)?;
    let jobs = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let (rows, failed) = oracle::serialize_sql(&options.duckdb, &work, &statements, jobs);
    let path = options.dir.join("serialized.jsonl");
    write_jsonl(&path, &rows)?;
    println!(
        "{} statements serialized by {}; {} crashed the CLI → {}",
        rows.len(),
        options.duckdb.display(),
        failed.len(),
        path.display()
    );
    Ok(())
}

/// `serialization`: rustdb's JSON and binary serialization against DuckDB's output.
fn serialization(options: &Options) -> Result<bool> {
    let entries: Vec<Entry> = read_jsonl(&options.dir.join("statements.jsonl"))?;
    let rows: Vec<SerializedSql> = read_jsonl(&options.dir.join("serialized.jsonl"))?;
    let sql_by_id: BTreeMap<u64, &str> = entries.iter().map(|e| (e.id, e.statement.sql.as_str())).collect();
    let (report, log) = check::check_serialization(&rows, &sql_by_id);
    let log_path = options.dir.join("serialization-mismatches.txt");
    std::fs::write(&log_path, log)?;
    print!("{}", report.summary());
    println!("mismatch samples → {}", log_path.display());
    Ok(report.mismatches.is_empty())
}

/// `wal`: rustdb's binary format against the catalog entries of DuckDB's WAL.
fn wal(options: &Options) -> Result<bool> {
    let (statements, _) = slt::extract_dir(&options.tests)?;
    let scripts = walcheck::scripts(&statements);
    let work = options.dir.join("wal");
    std::fs::create_dir_all(&work)?;
    let cwd = options.tests.parent().unwrap_or(&options.tests);
    let jobs = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let report = walcheck::check_wal(&options.duckdb, cwd, &work, &scripts, jobs);
    let mut log = String::new();
    for (category, samples) in &report.mismatches {
        let _ = writeln!(log, "=== {category}");
        for sample in samples.iter().take(5) {
            log.push_str(sample);
            log.push('\n');
        }
    }
    let log_path = options.dir.join("wal-mismatches.txt");
    std::fs::write(&log_path, log)?;
    print!("{}", report.summary());
    println!("mismatch samples → {}", log_path.display());
    Ok(report.mismatches.is_empty())
}

fn run() -> Result<bool> {
    let root = workspace_root();
    let mut options = Options {
        tests: root.join(".context/duckdb-download/duckdb-1.5.6/test"),
        duckdb: oracle::default_duckdb(&root),
        dir: root.join("target/corpus"),
    };
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--tests" => options.tests = value.into(),
            "--duckdb" => options.duckdb = value.into(),
            "--dir" => options.dir = value.into(),
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    match command.as_str() {
        "extract" => extract(&options).map(|()| true),
        "oracle" => oracle(&options).map(|()| true),
        "serialization" => serialization(&options),
        "wal" => wal(&options),
        _ => Err(
            "usage: corpus <extract | oracle | serialization | wal> [--tests DIR] [--duckdb CLI] [--dir DIR]"
                .into(),
        ),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("corpus: {error}");
            ExitCode::FAILURE
        }
    }
}
