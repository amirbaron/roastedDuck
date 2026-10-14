//! The JSON-lines result format: one [`MachineInfo`] header line per run, then one
//! [`BenchmarkRecord`] line per benchmark.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The record schema version; bump on incompatible changes.
pub const SCHEMA_VERSION: u32 = 1;

/// How a benchmark ended on an engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// All runs completed and every result check passed. Only these timings count.
    #[default]
    Ok,
    /// A result check failed.
    Incorrect,
    /// The engine reported a `Not implemented` error (expected for rustdb today).
    Unsupported,
    /// The engine reported another error (setup or run).
    Error,
    /// A run exceeded the per-run timeout, or setup exceeded the setup timeout.
    Timeout,
    /// The driver process died without reporting (signal, abort, panic).
    Crashed,
    /// Not run: missing data, unavailable extension or an unsupported result mode.
    Skipped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Incorrect => "incorrect",
            Self::Unsupported => "unsupported",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Crashed => "crashed",
            Self::Skipped => "skipped",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the benchmark's results were verified.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultCheck {
    /// Every timed run's result matched the benchmark's expected result.
    Passed,
    /// A result did not match.
    Failed,
    /// The benchmark has no expected result (for these arguments).
    #[default]
    None,
    /// The result is not observable (`resultmode arrow`) or no run completed.
    NotChecked,
}

/// The machine and settings of a run; the first line of every results file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub schema: u32,
    /// Seconds since the Unix epoch when the run started.
    pub started_unix: u64,
    /// The same instant in UTC, ISO 8601.
    pub started_utc: String,
    pub hostname: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu: String,
    pub logical_cores: u32,
    pub physical_cores: Option<u32>,
    pub memory_bytes: Option<u64>,
    /// 1/5/15-minute load averages when the run started: other load makes timings noisy.
    pub load_average: Option<[f64; 3]>,
    /// `git rev-parse HEAD` of the rustdb checkout.
    pub git_commit: Option<String>,
    pub engine: String,
    pub engine_version: String,
    pub threads: u32,
    pub memory_limit: Option<String>,
    pub warmup: u32,
    pub runs: u32,
    pub timeout_secs: Option<f64>,
}

/// One benchmark on one engine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkRecord {
    pub engine: String,
    /// The benchmark path, e.g. `benchmark/tpch/sf1/q01.benchmark`.
    pub name: String,
    pub display_name: String,
    pub group: String,
    pub subgroup: String,
    /// The directory below `benchmark/` (`micro`, `tpch`, ...).
    pub suite: String,
    /// Resolved `argument` values (e.g. `sf=1`).
    pub arguments: BTreeMap<String, String>,
    pub status: Status,
    pub error: Option<String>,
    pub result_check: ResultCheck,
    pub threads: u32,
    pub memory_limit: Option<String>,
    /// Untimed warm-up runs (the first one is the cold run).
    pub warmup_ms: Vec<f64>,
    /// Timed runs, in order; only the `run` section, like DuckDB's runner.
    pub timings_ms: Vec<f64>,
    pub median_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub mean_ms: Option<f64>,
    /// Peak resident set size of the whole benchmark process (`wait4` rusage).
    pub peak_rss_bytes: Option<u64>,
    /// Peak RSS reached by the end of setup (open + init + load), before any run.
    pub setup_peak_rss_bytes: Option<u64>,
    /// Wall time of setup (open + init + load + asserts).
    pub setup_ms: Option<f64>,
    /// Wall time of the whole measured process.
    pub process_ms: Option<f64>,
    /// Rows in the last timed result.
    pub result_rows: Option<u64>,
    /// FNV-1a fingerprint of the last timed result (when small enough), used to cross
    /// check engines on benchmarks without an expected result.
    pub result_fingerprint: Option<String>,
    /// Column types of the fingerprinted result.
    pub result_types: Vec<String>,
    /// Anything that makes this measurement differ from DuckDB's runner.
    pub notes: Vec<String>,
}

impl BenchmarkRecord {
    /// The key that identifies a benchmark across engines: name plus arguments.
    pub fn key(&self) -> String {
        if self.arguments.is_empty() {
            return self.name.clone();
        }
        let arguments: Vec<String> = self.arguments.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{}[{}]", self.name, arguments.join(","))
    }
}

/// A line of a results file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    Machine(MachineInfo),
    Benchmark(BenchmarkRecord),
}

/// A parsed results file.
#[derive(Clone, Debug, Default)]
pub struct Results {
    pub machine: Vec<MachineInfo>,
    pub benchmarks: Vec<BenchmarkRecord>,
}

impl Results {
    /// Reads a JSON-lines results file. Later records of the same benchmark replace
    /// earlier ones, so re-running a subset and appending works.
    pub fn read(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut results = Self::default();
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        for (number, line) in io::BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let parsed: Line = serde_json::from_str(&line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}:{}: {e}", path.display(), number + 1),
                )
            })?;
            match parsed {
                Line::Machine(machine) => results.machine.push(machine),
                Line::Benchmark(record) => {
                    if let Some(&i) = index.get(&record.key()) {
                        results.benchmarks[i] = record;
                    } else {
                        index.insert(record.key(), results.benchmarks.len());
                        results.benchmarks.push(record);
                    }
                }
            }
        }
        Ok(results)
    }
}

/// Appends lines to a results file, flushing after each so partial runs are usable.
#[derive(Debug)]
pub struct Writer {
    file: std::fs::File,
}

impl Writer {
    /// Opens `path`, truncating it unless `append`.
    pub fn create(path: &Path, append: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)?;
        Ok(Self { file })
    }

    pub fn write(&mut self, line: &Line) -> io::Result<()> {
        let json = serde_json::to_string(line).map_err(io::Error::other)?;
        writeln!(self.file, "{json}")?;
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_and_later_records_win() {
        let dir = std::env::temp_dir().join(format!("rustdb-bench-record-{}", std::process::id()));
        let path = dir.join("r.jsonl");
        let mut writer = Writer::create(&path, false).unwrap();
        writer.write(&Line::Machine(MachineInfo::default())).unwrap();
        let mut record = BenchmarkRecord {
            name: "benchmark/a.benchmark".into(),
            status: Status::Error,
            ..BenchmarkRecord::default()
        };
        writer.write(&Line::Benchmark(record.clone())).unwrap();
        record.status = Status::Ok;
        record.arguments.insert("sf".into(), "10".into());
        writer.write(&Line::Benchmark(record.clone())).unwrap();
        record.arguments.clear();
        writer.write(&Line::Benchmark(record)).unwrap();

        let results = Results::read(&path).unwrap();
        assert_eq!(results.machine.len(), 1);
        assert_eq!(results.benchmarks.len(), 2);
        assert_eq!(results.benchmarks[0].status, Status::Ok);
        assert_eq!(results.benchmarks[1].key(), "benchmark/a.benchmark[sf=10]");
        let _ = std::fs::remove_dir_all(dir);
    }
}
