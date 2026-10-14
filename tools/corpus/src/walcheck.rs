//! The binary check: replay the DDL of each test file through the reference CLI into a
//! database whose checkpoints are disabled, then require that every catalog entry in
//! the resulting write-ahead log reads into rustdb's parse tree and writes back to the
//! very same bytes.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use corpus::slt::{RecordKind, Statement, has_unsubstituted_variable};
use corpus::wal;
use rustdb_serde::SerializationCompatibility;

/// Statements that would detach the log under test, checkpoint it away, or touch files.
const SKIPPED_PREFIXES: &[&str] = &[
    "ATTACH",
    "DETACH",
    "USE",
    "CHECKPOINT",
    "FORCE",
    "PRAGMA",
    "SET",
    "RESET",
    "EXPORT",
    "IMPORT",
    "LOAD",
    "INSTALL",
    "COPY",
    "CALL",
];

/// Storage versions to replay with: the default (serialization version 1) and the latest.
pub const STORAGE_VERSIONS: &[(&str, &str)] = &[("v0.10.2", "v0.10.2"), ("v1.5.0", "latest")];

const TIMEOUT: Duration = Duration::from_secs(30);

fn replayable(statement: &Statement) -> bool {
    if !matches!(
        statement.kind,
        RecordKind::StatementOk | RecordKind::StatementMaybe
    ) || has_unsubstituted_variable(&statement.sql)
    {
        return false;
    }
    let head = statement.sql.trim_start().to_ascii_uppercase();
    !SKIPPED_PREFIXES.iter().any(|prefix| head.starts_with(prefix))
}

/// The replayable statements of one test file.
#[derive(Debug)]
pub struct Script<'a> {
    pub file: &'a str,
    pub statements: Vec<&'a str>,
}

/// Scripts for the test files that create or alter catalog entries.
pub fn scripts(statements: &[Statement]) -> Vec<Script<'_>> {
    let mut by_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let is_test = |s: &Statement| Path::new(&s.file).extension().is_some_and(|e| e == "test");
    for statement in statements.iter().filter(|s| is_test(s) && replayable(s)) {
        by_file.entry(&statement.file).or_default().push(&statement.sql);
    }
    by_file
        .into_iter()
        .filter(|(_, sql)| {
            sql.iter().any(|s| {
                let head = s.trim_start().to_ascii_uppercase();
                head.starts_with("CREATE") || head.starts_with("ALTER")
            })
        })
        .map(|(file, statements)| Script { file, statements })
        .collect()
}

/// Runs a script against a fresh database file; returns the WAL bytes.
fn replay(
    duckdb: &Path,
    cwd: &Path,
    db: &Path,
    storage_version: &str,
    statements: &[&str],
) -> Option<Vec<u8>> {
    let wal_path = PathBuf::from(format!("{}.wal", db.display()));
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_file(&wal_path);
    let mut script = format!(
        "PRAGMA disable_checkpoint_on_shutdown;\nSET checkpoint_threshold = '1TB';\n\
         ATTACH '{}' AS walcheck (STORAGE_VERSION '{storage_version}');\nUSE walcheck;\n",
        db.display()
    );
    let temp_dir = db.parent().map(|p| p.display().to_string()).unwrap_or_default();
    for sql in statements {
        script.push_str(&sql.replace("__TEST_DIR__", &temp_dir));
        script.push_str("\n;\n");
    }
    let script_path = PathBuf::from(format!("{}.sql", db.display()));
    std::fs::write(&script_path, script).ok()?;
    let mut child = Command::new(duckdb)
        .current_dir(cwd)
        .stdin(std::fs::File::open(&script_path).ok()?)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    let wal = std::fs::read(&wal_path).ok();
    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_file(&wal_path);
    wal
}

/// Outcome counts of the binary check.
#[derive(Debug, Default)]
pub struct WalReport {
    pub scripts: usize,
    pub logs: usize,
    /// Checked catalog entries by kind, and how many were reproduced exactly.
    pub entries: BTreeMap<String, (usize, usize)>,
    pub mismatches: BTreeMap<String, Vec<String>>,
}

impl WalReport {
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "replayed scripts:           {}", self.scripts);
        let _ = writeln!(out, "logs with entries:          {}", self.logs);
        let total: usize = self.entries.values().map(|(n, _)| n).sum();
        let exact: usize = self.entries.values().map(|(_, e)| e).sum();
        let _ = writeln!(out, "catalog entries checked:    {total}");
        let _ = writeln!(out, "reproduced exactly:         {exact}");
        for (kind, (n, e)) in &self.entries {
            let _ = writeln!(out, "  {kind:<46}{e}/{n}");
        }
        for (category, samples) in &self.mismatches {
            let _ = writeln!(out, "  MISMATCH {category}: {}", samples.len());
        }
        out
    }
}

/// Checks the catalog entries of one log; adds to `report`, logs mismatches to `log`.
fn check_log(data: &[u8], compatibility: &SerializationCompatibility, file: &str, report: &mut WalReport) {
    let Some(entries) = wal::entries(data) else {
        report
            .mismatches
            .entry("malformed WAL".to_owned())
            .or_default()
            .push(file.to_owned());
        return;
    };
    report.logs += 1;
    for entry in entries {
        let Some(outcome) = wal::check_entry(&entry, compatibility) else {
            continue;
        };
        let version = &compatibility.duckdb_version;
        match outcome {
            Ok(kind) => {
                let counts = report.entries.entry(format!("{kind} @{version}")).or_default();
                counts.0 += 1;
                counts.1 += 1;
            }
            Err(mismatch) => {
                let category = format!("{} @{version}", mismatch.kind);
                report.entries.entry(category.clone()).or_default().0 += 1;
                report
                    .mismatches
                    .entry(category)
                    .or_default()
                    .push(format!("{file}: {}", mismatch.detail));
            }
        }
    }
}

/// Replays every script under every storage version and checks the logs.
pub fn check_wal(
    duckdb: &Path,
    cwd: &Path,
    work_dir: &Path,
    scripts: &[Script<'_>],
    jobs: usize,
) -> WalReport {
    let tasks: Vec<(&Script<'_>, &str, &str)> = scripts
        .iter()
        .flat_map(|script| {
            STORAGE_VERSIONS
                .iter()
                .map(move |(storage, serialization)| (script, *storage, *serialization))
        })
        .collect();
    let next = Mutex::new(0usize);
    let report = Mutex::new(WalReport {
        scripts: tasks.len(),
        ..WalReport::default()
    });
    std::thread::scope(|scope| {
        for worker in 0..jobs.max(1) {
            let (next, report, tasks) = (&next, &report, &tasks);
            scope.spawn(move || {
                let db = work_dir.join(format!("walcheck-{worker}.db"));
                loop {
                    let index = {
                        let Ok(mut next) = next.lock() else { return };
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some((script, storage, serialization)) = tasks.get(index) else {
                        return;
                    };
                    let Ok(compatibility) = SerializationCompatibility::from_version_name(serialization)
                    else {
                        return;
                    };
                    let Some(data) = replay(duckdb, cwd, &db, storage, &script.statements) else {
                        continue;
                    };
                    if let Ok(mut report) = report.lock() {
                        check_log(&data, &compatibility, script.file, &mut report);
                    }
                }
            });
        }
    });
    report.into_inner().unwrap_or_default()
}
