//! The harness side of a run: prepares the benchmark root, builds caches outside the
//! measured process, runs every benchmark in its own process (the bench driver, or
//! DuckDB's `benchmark_runner` for [`EngineKind::CppRunner`]) and turns the outcome into
//! a [`BenchmarkRecord`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::data;
use crate::driver::{Job, Outcome, Phase, RunSettings};
use crate::engine::EngineKind;
use crate::format::{BENCHMARK_DIRECTORY, Benchmark, ParseOptions, ResultMode, parse_benchmark, suite_of};
use crate::measure::{Exit, Measured, Summary, run_measured};
use crate::record::{BenchmarkRecord, ResultCheck, Status};

/// File locations used by a run.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The DuckDB 1.5.6 source tree (benchmarks, data, answers).
    pub duckdb_src: PathBuf,
    /// The C++ `libduckdb` shared library.
    pub libduckdb: PathBuf,
    /// DuckDB's `benchmark_runner`.
    pub benchmark_runner: PathBuf,
    /// The `bench-driver` executable.
    pub driver: PathBuf,
    /// Scratch space: benchmark roots (with generated data), downloads, logs.
    pub work_dir: PathBuf,
}

impl Paths {
    /// Downloaded remote data.
    pub fn downloads(&self) -> PathBuf {
        self.work_dir.join("downloads")
    }

    /// The benchmark root of an engine: links to the DuckDB tree plus its own
    /// `duckdb_benchmark_data` directory with generated data and caches.
    pub fn root(&self, engine: EngineKind) -> PathBuf {
        self.work_dir.join("roots").join(engine.root_name())
    }
}

/// Everything that is fixed for a run.
#[derive(Clone, Debug)]
pub struct Harness {
    pub engine: EngineKind,
    pub paths: Paths,
    pub settings: RunSettings,
    /// Benchmark `argument` overrides.
    pub arguments: BTreeMap<String, String>,
    /// Time budget for building a cache (the prepare phase) and for setup.
    pub setup_timeout: Duration,
}

impl Harness {
    /// Creates the benchmark root of the engine: a directory whose entries link to the
    /// DuckDB source tree, plus a real `duckdb_benchmark_data` directory.
    pub fn prepare_root(&self) -> io::Result<PathBuf> {
        let root = self.paths.root(self.engine);
        std::fs::create_dir_all(root.join(BENCHMARK_DIRECTORY))?;
        for entry in std::fs::read_dir(&self.paths.duckdb_src)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == BENCHMARK_DIRECTORY {
                continue;
            }
            let link = root.join(&name);
            if link.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(entry.path(), &link)?;
            }
        }
        Ok(root)
    }

    /// Parses a benchmark relative to the engine root.
    pub fn parse(&self, name: &str) -> Result<Benchmark, String> {
        let root = self.paths.root(self.engine);
        parse_benchmark(
            name,
            &ParseOptions {
                root: &root,
                arguments: &self.arguments,
            },
        )
        .map_err(|e| e.to_string())
    }

    /// Runs one benchmark and describes the result.
    pub fn run(&self, name: &str) -> BenchmarkRecord {
        let mut record = BenchmarkRecord {
            engine: self.engine.as_str().to_owned(),
            name: name.to_owned(),
            display_name: name.to_owned(),
            suite: suite_of(name).to_owned(),
            threads: self.settings.threads,
            memory_limit: self.settings.memory_limit.clone(),
            arguments: self.arguments.clone(),
            ..BenchmarkRecord::default()
        };
        let mut benchmark = match self.parse(name) {
            Ok(benchmark) => benchmark,
            Err(e) => return failed(record, Status::Error, format!("parse error: {e}")),
        };
        record.display_name.clone_from(&benchmark.display_name);
        record.group.clone_from(&benchmark.group);
        record.subgroup.clone_from(&benchmark.subgroup);
        record.arguments.clone_from(&benchmark.arguments);

        let original_needs_network = data::needs_network(&benchmark);
        let localized = data::localize(&mut benchmark, &self.paths.downloads());
        if data::needs_network(&benchmark) {
            let reason = if localized.missing.is_empty() {
                "requires the httpfs extension".to_owned()
            } else if localized.missing.iter().any(|url| url.contains('{')) {
                format!(
                    "needs remote data whose URLs are built at run time, which `bench fetch` cannot mirror: {}",
                    localized.missing.join(", ")
                )
            } else {
                format!(
                    "needs remote data not downloaded yet (run `bench fetch`): {}",
                    localized.missing.join(", ")
                )
            };
            return failed(record, Status::Skipped, reason);
        }
        if localized.replaced > 0 {
            record.notes.push(format!(
                "{} remote file(s) read from local copies instead of over HTTP",
                localized.replaced
            ));
        }
        match self.engine {
            EngineKind::CppRunner if original_needs_network => failed(
                record,
                Status::Skipped,
                "benchmark_runner would read remote data over HTTP".to_owned(),
            ),
            EngineKind::CppRunner => self.run_benchmark_runner(record, &benchmark),
            _ if matches!(benchmark.result_mode, ResultMode::Arrow { .. }) => failed(
                record,
                Status::Skipped,
                "resultmode arrow (Arrow result collector) is not measurable through the driver".to_owned(),
            ),
            EngineKind::Cpp | EngineKind::Rust => self.run_driver(record, &benchmark),
        }
    }

    fn cache_missing(&self, benchmark: &Benchmark) -> bool {
        let root = self.paths.root(self.engine);
        benchmark.cache.paths().any(|path| !root.join(path).exists())
    }

    /// Builds the benchmark's cache in a separate, unmeasured driver process.
    fn prepare(&self, benchmark: &Benchmark, engine: EngineKind) -> Result<(), (Status, String)> {
        let (measured, outcome) = self
            .spawn_driver(benchmark, engine, Phase::Prepare)
            .map_err(|e| (Status::Error, format!("could not start the driver: {e}")))?;
        let outcome = interpret(&measured, outcome);
        if outcome.status == Status::Ok {
            Ok(())
        } else {
            Err((
                outcome.status,
                format!("building the cache failed: {}", outcome.error.unwrap_or_default()),
            ))
        }
    }

    fn run_driver(&self, mut record: BenchmarkRecord, benchmark: &Benchmark) -> BenchmarkRecord {
        if self.cache_missing(benchmark)
            && let Err((status, message)) = self.prepare(benchmark, self.engine)
        {
            return failed(record, status, message);
        }
        let (measured, outcome) = match self.spawn_driver(benchmark, self.engine, Phase::Measure) {
            Ok(result) => result,
            Err(e) => return failed(record, Status::Error, format!("could not start the driver: {e}")),
        };
        let outcome = interpret(&measured, outcome);
        record.status = outcome.status;
        record.error = outcome.error;
        record.result_check = outcome.result_check;
        record.warmup_ms = outcome.warmup_ms;
        record.timings_ms = outcome.timings_ms;
        record.setup_ms = outcome.setup_ms;
        record.setup_peak_rss_bytes = outcome.setup_peak_rss_bytes;
        record.result_rows = outcome.result_rows;
        record.result_fingerprint = outcome.result_fingerprint;
        record.notes.extend(outcome.notes);
        record.peak_rss_bytes = Some(measured.peak_rss_bytes);
        record.process_ms = Some(measured.wall.as_secs_f64() * 1000.0);
        if record.status != Status::Ok && record.result_check == ResultCheck::Passed {
            record.result_check = ResultCheck::NotChecked;
        }
        summarize(record)
    }

    /// Runs the driver for `benchmark`; returns the measurement and the outcome file
    /// (absent when the process died before writing it).
    fn spawn_driver(
        &self,
        benchmark: &Benchmark,
        engine: EngineKind,
        phase: Phase,
    ) -> io::Result<(Measured, Option<Outcome>)> {
        let files = self.scratch_files(benchmark, engine)?;
        let job = Job {
            benchmark: benchmark.clone(),
            settings: self.settings.clone(),
            phase,
            engine,
            libduckdb: Some(self.paths.libduckdb.clone()),
        };
        std::fs::write(&files.job, serde_json::to_vec(&job).map_err(io::Error::other)?)?;
        let _ = std::fs::remove_file(&files.outcome);
        let log = log_file(&files.log, phase == Phase::Prepare)?;
        let mut command = Command::new(&self.paths.driver);
        command
            .arg("--job")
            .arg(&files.job)
            .arg("--outcome")
            .arg(&files.outcome)
            .current_dir(self.paths.root(engine))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        let runs = self.settings.warmup + self.settings.runs;
        let per_run = self
            .settings
            .timeout_secs
            .map_or(self.setup_timeout, Duration::from_secs_f64);
        let budget = self.setup_timeout + per_run * (runs + 1) * 2;
        let measured = run_measured(&mut command, Some(budget))?;
        let outcome = std::fs::read(&files.outcome)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Ok((measured, outcome))
    }

    /// Runs DuckDB's own `benchmark_runner` on one benchmark (cross-check engine).
    fn run_benchmark_runner(&self, mut record: BenchmarkRecord, benchmark: &Benchmark) -> BenchmarkRecord {
        record.notes.push(
            "measured by DuckDB's benchmark_runner: 1 cold run + 5 timed runs, its own result check"
                .to_owned(),
        );
        if self.cache_missing(benchmark)
            && let Err((status, message)) = self.prepare(benchmark, EngineKind::Cpp)
        {
            return failed(record, status, message);
        }
        let files = match self.scratch_files(benchmark, self.engine) {
            Ok(files) => files,
            Err(e) => return failed(record, Status::Error, e.to_string()),
        };
        let root = self.paths.root(self.engine);
        let mut command = Command::new(&self.paths.benchmark_runner);
        command
            .arg("--root-dir")
            .arg(&root)
            .arg(format!("--threads={}", self.settings.threads))
            .arg(format!("--out={}", files.outcome.display()))
            .arg("--no-summary");
        if let Some(limit) = &self.settings.memory_limit {
            command.arg(format!("--memory_limit={limit}"));
        }
        if self.settings.timeout_secs.is_none() {
            command.arg("--disable-timeout");
        }
        for (name, value) in &self.arguments {
            command.arg(format!("--{name}")).arg(value);
        }
        command.arg(regex::escape(&benchmark.name));
        let log = match log_file(&files.log, false) {
            Ok(log) => log,
            Err(e) => return failed(record, Status::Error, e.to_string()),
        };
        let (stdout, stderr) = match log.try_clone() {
            Ok(stdout) => (stdout, log),
            Err(e) => return failed(record, Status::Error, e.to_string()),
        };
        command
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        let budget =
            self.setup_timeout + Duration::from_secs_f64(self.settings.timeout_secs.unwrap_or(3600.0) * 14.0);
        let measured = match run_measured(&mut command, Some(budget)) {
            Ok(measured) => measured,
            Err(e) => {
                return failed(
                    record,
                    Status::Error,
                    format!("could not start benchmark_runner: {e}"),
                );
            }
        };
        record.peak_rss_bytes = Some(measured.peak_rss_bytes);
        record.process_ms = Some(measured.wall.as_secs_f64() * 1000.0);
        let out = std::fs::read_to_string(&files.outcome).unwrap_or_default();
        let log_text = std::fs::read_to_string(&files.log).unwrap_or_default();
        let parsed = parse_runner_output(&out, &log_text);
        record.status = match measured.exit {
            Exit::TimedOut => Status::Timeout,
            Exit::Signal(_) => Status::Crashed,
            Exit::Code(_) => parsed.status,
        };
        record.error = parsed.error;
        record.timings_ms = parsed.timings_ms;
        record.result_check = match (record.status, &benchmark.result) {
            (Status::Incorrect, _) => ResultCheck::Failed,
            (Status::Ok, Some(_)) => ResultCheck::Passed,
            (_, None) => ResultCheck::None,
            _ => ResultCheck::NotChecked,
        };
        summarize(record)
    }

    fn scratch_files(&self, benchmark: &Benchmark, engine: EngineKind) -> io::Result<ScratchFiles> {
        let mut stem = benchmark
            .name
            .trim_start_matches("benchmark/")
            .trim_end_matches(".benchmark")
            .replace('/', "__");
        for (key, value) in &benchmark.arguments {
            let _ = write!(stem, "__{key}-{value}");
        }
        let stem: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let jobs = self.paths.work_dir.join("jobs").join(engine.as_str());
        let logs = self.paths.work_dir.join("logs").join(engine.as_str());
        std::fs::create_dir_all(&jobs)?;
        std::fs::create_dir_all(&logs)?;
        Ok(ScratchFiles {
            job: jobs.join(format!("{stem}.job.json")),
            outcome: jobs.join(format!("{stem}.outcome")),
            log: logs.join(format!("{stem}.log")),
        })
    }
}

struct ScratchFiles {
    job: PathBuf,
    outcome: PathBuf,
    log: PathBuf,
}

fn log_file(path: &Path, truncate: bool) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(!truncate)
        .write(true)
        .truncate(truncate)
        .open(path)
}

/// Combines the process measurement with the driver's own report.
fn interpret(measured: &Measured, outcome: Option<Outcome>) -> Outcome {
    match (measured.exit, outcome) {
        (Exit::Code(0), Some(outcome)) => outcome,
        (Exit::TimedOut, outcome) => Outcome {
            status: Status::Timeout,
            error: Some("the benchmark process exceeded its time budget and was killed".to_owned()),
            ..outcome.unwrap_or_default()
        },
        (exit, outcome) => Outcome {
            status: Status::Crashed,
            error: Some(match exit {
                Exit::Signal(signal) => format!("the benchmark process died from signal {signal}"),
                Exit::Code(code) => format!("the benchmark process exited with code {code} without a report"),
                Exit::TimedOut => String::new(),
            }),
            ..outcome.unwrap_or_default()
        },
    }
}

fn failed(mut record: BenchmarkRecord, status: Status, error: String) -> BenchmarkRecord {
    record.status = status;
    record.error = Some(error);
    record
}

/// Fills in summary statistics; timings only count for correct benchmarks.
fn summarize(mut record: BenchmarkRecord) -> BenchmarkRecord {
    if record.status == Status::Ok
        && let Some(summary) = Summary::of(&record.timings_ms)
    {
        record.median_ms = Some(summary.median);
        record.min_ms = Some(summary.min);
        record.mean_ms = Some(summary.mean);
    }
    record
}

/// What `benchmark_runner` reported.
#[derive(Debug, PartialEq)]
struct RunnerOutput {
    status: Status,
    error: Option<String>,
    timings_ms: Vec<f64>,
}

/// Parses `benchmark_runner`'s `--out` file (one timing in seconds, or `ERROR`,
/// `INCORRECT`, `TIMEOUT` per timed run) and its log for error messages.
fn parse_runner_output(out: &str, log: &str) -> RunnerOutput {
    let mut parsed = RunnerOutput {
        status: Status::Ok,
        error: None,
        timings_ms: Vec::new(),
    };
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let status = match line {
            "ERROR" => Status::Error,
            "INCORRECT" => Status::Incorrect,
            line if line.starts_with("TIMEOUT") || line.starts_with("Benchmark timeout") => Status::Timeout,
            line => match line.parse::<f64>() {
                Ok(seconds) => {
                    parsed.timings_ms.push(seconds * 1000.0);
                    continue;
                }
                Err(_) => Status::Error,
            },
        };
        parsed.status = status;
        let marker = match status {
            Status::Incorrect => "INCORRECT RESULT:",
            _ => line,
        };
        parsed.error = log
            .lines()
            .skip_while(|l| !l.contains(marker))
            .nth(usize::from(status != Status::Incorrect))
            .map(str::to_owned)
            .or_else(|| Some(line.to_owned()));
        break;
    }
    if parsed.status == Status::Ok && parsed.timings_ms.is_empty() {
        parsed.status = Status::Error;
        parsed.error = Some(
            log.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("benchmark_runner produced no timings")
                .to_owned(),
        );
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_benchmark_runner_output() {
        let ok = parse_runner_output("0.5\n0.25\n", "");
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(ok.timings_ms, vec![500.0, 250.0]);

        let error = parse_runner_output("ERROR\n", "name\trun\ttiming\nb\t1\tERROR\nCatalog Error: x\n");
        assert_eq!(error.status, Status::Error);
        assert_eq!(error.error.as_deref(), Some("Catalog Error: x"));

        let incorrect = parse_runner_output("0.1\nINCORRECT\n", "b\t2\tINCORRECT\nINCORRECT RESULT: wrong\n");
        assert_eq!(incorrect.status, Status::Incorrect);
        assert_eq!(incorrect.error.as_deref(), Some("INCORRECT RESULT: wrong"));
        assert_eq!(incorrect.timings_ms, vec![100.0]);

        let empty = parse_runner_output("", "Benchmark to run could not be found.\n");
        assert_eq!(empty.status, Status::Error);
    }

    #[test]
    fn interprets_process_exits() {
        let measured = |exit| Measured {
            exit,
            peak_rss_bytes: 1,
            wall: Duration::ZERO,
        };
        let ok = Outcome::default();
        assert_eq!(
            interpret(&measured(Exit::Code(0)), Some(ok.clone())).status,
            Status::Ok
        );
        assert_eq!(interpret(&measured(Exit::Code(0)), None).status, Status::Crashed);
        assert_eq!(
            interpret(&measured(Exit::Signal(11)), Some(ok)).status,
            Status::Crashed
        );
        assert_eq!(interpret(&measured(Exit::TimedOut), None).status, Status::Timeout);
    }
}
