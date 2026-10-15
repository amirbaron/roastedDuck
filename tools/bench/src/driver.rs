//! The benchmark driver: executes one benchmark on one engine inside a dedicated
//! process (so the process's peak RSS is that benchmark's), following DuckDB's
//! `BenchmarkRunner::RunBenchmark` and `InterpretedBenchmark::{Initialize, Run, Verify,
//! Cleanup}`.
//!
//! The harness writes a [`Job`] file and starts `bench-driver --job <file> --outcome
//! <file>`; the driver writes an [`Outcome`] file. A run that exceeds the timeout is
//! reported by a watchdog thread, which writes the outcome so far and exits the process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::{
    CppEngine, DbConfig, Engine, EngineError, EngineErrorKind, EngineKind, ExtensionStatus, QueryResult,
    RustEngine,
};
use crate::format::{
    ANSWER_TABLE, BENCHMARK_DIRECTORY, Benchmark, DEFAULT_DB_PATH, ExpectedResult, ResultMode,
};
use crate::measure::self_peak_rss_bytes;
use crate::record::{ResultCheck, Status};
use crate::verify::verify;

/// Extensions DuckDB's runner always loads (`AddExtension` in `Initialize`); missing
/// ones are not an error.
const IMPLICIT_EXTENSIONS: [&str; 2] = ["core_functions", "parquet"];
/// Rows per `INSERT` when materialising [`ANSWER_TABLE`].
const ANSWER_INSERT_BATCH: usize = 1000;

/// What the driver does with the benchmark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Only initialize: builds caches (generated data) outside the measured process.
    Prepare,
    /// Initialize, check asserts, then warm-up and timed runs.
    Measure,
}

/// Settings shared by every benchmark of a harness run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    pub threads: u32,
    pub memory_limit: Option<String>,
    /// Untimed runs before the timed ones (DuckDB's runner: 1 cold run).
    pub warmup: u32,
    /// Timed runs (DuckDB's runner: 5).
    pub runs: u32,
    /// Per-run timeout (DuckDB's runner: 30 s).
    pub timeout_secs: Option<f64>,
    /// Fingerprint the last result only up to this many cells (rows x columns), to keep
    /// result rendering from inflating peak memory.
    pub fingerprint_max_cells: u64,
}

/// The driver's input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub benchmark: Benchmark,
    pub settings: RunSettings,
    pub phase: Phase,
    pub engine: EngineKind,
    /// The `libduckdb` to load for [`EngineKind::Cpp`].
    pub libduckdb: Option<PathBuf>,
}

/// The driver's output.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub status: Status,
    pub error: Option<String>,
    pub engine_version: Option<String>,
    pub setup_ms: Option<f64>,
    pub setup_peak_rss_bytes: Option<u64>,
    pub warmup_ms: Vec<f64>,
    pub timings_ms: Vec<f64>,
    pub result_check: ResultCheck,
    pub result_rows: Option<u64>,
    pub result_fingerprint: Option<String>,
    pub result_types: Vec<String>,
    pub notes: Vec<String>,
}

/// Runs `job` and writes the outcome to `outcome_path` (also on timeout).
pub fn execute(job: &Job, outcome_path: &Path) -> std::io::Result<()> {
    let progress = Arc::new(Mutex::new(Outcome::default()));
    let watchdog = Watchdog::start(Arc::clone(&progress), outcome_path.to_owned());
    // The driver runs with the benchmark root as its working directory.
    let data_dir = Path::new(BENCHMARK_DIRECTORY);
    match job.engine {
        EngineKind::Cpp => match job.libduckdb.as_deref().map(CppEngine::load) {
            Some(Ok(mut engine)) => BenchmarkRun::new(&mut engine, job, &progress, &watchdog, data_dir).run(),
            Some(Err(e)) => fail(&progress, Status::Error, e.message),
            None => fail(&progress, Status::Error, "no libduckdb given".to_owned()),
        },
        EngineKind::Rust => {
            BenchmarkRun::new(&mut RustEngine::new(), job, &progress, &watchdog, data_dir).run();
        }
        EngineKind::CppRunner => fail(
            &progress,
            Status::Error,
            "cpp-runner is measured by benchmark_runner, not the driver".to_owned(),
        ),
    }
    let outcome = lock(&progress).clone();
    write_outcome(outcome_path, &outcome)
}

fn write_outcome(path: &Path, outcome: &Outcome) -> std::io::Result<()> {
    let json = serde_json::to_string(outcome).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fail(progress: &Mutex<Outcome>, status: Status, message: String) {
    let mut outcome = lock(progress);
    outcome.status = status;
    outcome.error = Some(message);
}

/// Why a benchmark stopped early.
struct Failure {
    status: Status,
    message: String,
}

impl From<EngineError> for Failure {
    fn from(error: EngineError) -> Self {
        Self {
            status: match error.kind {
                EngineErrorKind::Unsupported => Status::Unsupported,
                EngineErrorKind::Failed => Status::Error,
            },
            message: error.message,
        }
    }
}

impl Failure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            status: Status::Error,
            message: message.into(),
        }
    }
}

/// Kills runs that exceed the timeout: when armed and the deadline passes, it reports
/// [`Status::Timeout`] with the outcome so far and exits the process (like DuckDB's
/// runner, which stops the benchmark after a timeout).
struct Watchdog {
    deadline: Arc<(Mutex<Option<Instant>>, Condvar)>,
}

impl Watchdog {
    fn start(progress: Arc<Mutex<Outcome>>, outcome_path: PathBuf) -> Self {
        let deadline = Arc::new((Mutex::new(None::<Instant>), Condvar::new()));
        let shared = Arc::clone(&deadline);
        std::thread::spawn(move || {
            let (mutex, condvar) = &*shared;
            let mut guard = lock(mutex);
            loop {
                guard = match *guard {
                    None => condvar.wait(guard).unwrap_or_else(PoisonError::into_inner),
                    Some(at) if Instant::now() >= at => {
                        let mut outcome = lock(&progress).clone();
                        outcome.status = Status::Timeout;
                        outcome.error = Some("run exceeded the timeout".to_owned());
                        let code = i32::from(write_outcome(&outcome_path, &outcome).is_err());
                        std::process::exit(code);
                    }
                    Some(at) => {
                        let wait = at.saturating_duration_since(Instant::now());
                        condvar
                            .wait_timeout(guard, wait)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
            }
        });
        Self { deadline }
    }

    fn arm(&self, timeout: Option<Duration>) {
        let (mutex, condvar) = &*self.deadline;
        *lock(mutex) = timeout.map(|t| Instant::now() + t);
        condvar.notify_all();
    }

    fn disarm(&self) {
        self.arm(None);
    }
}

/// One benchmark on one engine.
struct BenchmarkRun<'a, E: Engine> {
    engine: &'a mut E,
    job: &'a Job,
    benchmark: &'a Benchmark,
    progress: &'a Mutex<Outcome>,
    watchdog: &'a Watchdog,
    /// `duckdb_benchmark_data`: caches and generated files.
    data_dir: &'a Path,
    /// `cache_db`, cleared after building a `no_connect` cache.
    cache_database: Option<String>,
    /// `!in_memory`.
    persistent: bool,
    /// Set once a `no_connect` cache is built: re-initialize without `load`.
    skip_load: bool,
}

impl<'a, E: Engine> BenchmarkRun<'a, E> {
    fn new(
        engine: &'a mut E,
        job: &'a Job,
        progress: &'a Mutex<Outcome>,
        watchdog: &'a Watchdog,
        data_dir: &'a Path,
    ) -> Self {
        Self {
            engine,
            job,
            benchmark: &job.benchmark,
            progress,
            watchdog,
            data_dir,
            cache_database: job.benchmark.cache.database.clone(),
            persistent: job.benchmark.storage.persistent,
            skip_load: false,
        }
    }

    fn run(mut self) {
        lock(self.progress).engine_version = Some(self.engine.version());
        if let Err(failure) = self.run_phases() {
            fail(self.progress, failure.status, failure.message);
        }
        self.engine.close();
    }

    fn run_phases(&mut self) -> Result<(), Failure> {
        let start = Instant::now();
        std::fs::create_dir_all(self.data_dir)
            .map_err(|e| Failure::error(format!("cannot create {}: {e}", self.data_dir.display())))?;
        self.initialize()?;
        if self.job.phase == Phase::Prepare {
            return Ok(());
        }
        for assert in &self.benchmark.asserts {
            let query = assert.query.as_deref().unwrap_or_default();
            let table = self.engine.query(query)?.to_table()?;
            verify(assert, &table).map_err(|e| Failure::error(format!("Assertion query failed:\n{e}")))?;
        }
        {
            let mut progress = lock(self.progress);
            progress.setup_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
            progress.setup_peak_rss_bytes = self_peak_rss_bytes();
            progress.result_check = match (&self.benchmark.result, self.benchmark.result_mode) {
                (_, ResultMode::Arrow { .. }) => ResultCheck::NotChecked,
                (Some(_), _) => ResultCheck::Passed,
                (None, _) => ResultCheck::None,
            };
            if self.benchmark.result_mode == ResultMode::Streaming {
                progress
                    .notes
                    .push("resultmode streaming measured as a materialized result".to_owned());
            }
        }
        self.run_loop()
    }

    /// `InterpretedBenchmark::Initialize`.
    fn initialize(&mut self) -> Result<(), Failure> {
        let path = self.database_path();
        let config = DbConfig {
            threads: self.job.settings.threads,
            memory_limit: self.job.settings.memory_limit.clone(),
            storage_version: self.benchmark.storage.version.clone(),
        };
        if self.engine.open(path.as_deref(), &config).is_err() {
            // Probably a storage format error: delete the file and connect again.
            if let Some(path) = &path {
                delete_database(path);
            }
            self.engine.open(path.as_deref(), &config)?;
        }
        for extension in &self.benchmark.extensions {
            self.load_extension(extension, true)?;
        }
        for extension in IMPLICIT_EXTENSIONS {
            if !self.benchmark.extensions.iter().any(|e| e == extension) {
                self.load_extension(extension, false)?;
            }
        }
        if let Some(init) = &self.benchmark.queries.init {
            self.engine.query(init)?;
        }
        let queries = &self.benchmark.queries;
        let load = if self.skip_load {
            None
        } else {
            queries.load.as_deref()
        };
        let reload = queries.reload.as_deref();
        if let Some(file) = &self.benchmark.cache.file {
            if !self.data_dir.join(file).exists() {
                self.run_load(load)?;
            } else if reload.is_some() {
                self.run_load(reload)?;
            }
        } else if self.cache_database.is_none() {
            self.run_load(load)?;
        } else {
            let has_data = self.engine.query("SHOW TABLES")?.row_count() > 0;
            if !has_data {
                self.run_load(load)?;
            } else if reload.is_some() {
                self.run_load(reload)?;
            }
        }
        if self.cache_database.is_some() && self.benchmark.cache.no_connect && !self.skip_load {
            // The cache exists now; run the benchmark on a fresh in-memory database.
            self.cache_database = None;
            self.persistent = false;
            self.skip_load = true;
            return self.initialize();
        }
        Ok(())
    }

    /// `InterpretedBenchmark::GetDatabasePath`; `None` is in-memory.
    fn database_path(&self) -> Option<PathBuf> {
        if let Some(file) = &self.cache_database {
            return Some(self.data_dir.join(file));
        }
        if !self.persistent {
            return None;
        }
        let path = self.data_dir.join(DEFAULT_DB_PATH);
        delete_database(&path);
        Some(path)
    }

    fn load_extension(&mut self, name: &str, required: bool) -> Result<(), Failure> {
        let status = match self.engine.load_extension(name) {
            Ok(status) => status,
            Err(_) if !required => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match status {
            ExtensionStatus::Loaded => Ok(()),
            _ if !required => Ok(()),
            ExtensionStatus::NotAvailable => Err(Failure {
                status: Status::Unsupported,
                message: format!(
                    "Extension {name} is not available/was not compiled. Cannot run this benchmark."
                ),
            }),
            ExtensionStatus::Unknown => Err(Failure::error(format!("Unknown extension {name}"))),
        }
    }

    /// `RunLoadQuery`: load-only extensions, then the query with `retry load` retries.
    fn run_load(&mut self, query: Option<&str>) -> Result<(), Failure> {
        for extension in &self.benchmark.load_extensions {
            self.load_extension(extension, true)?;
        }
        let Some(query) = query else {
            return Ok(());
        };
        let mut result = self.engine.query(query).map(drop);
        for _ in 0..self.benchmark.retry_load {
            if result.is_ok() {
                break;
            }
            result = self.engine.query(query).map(drop);
        }
        Ok(result?)
    }

    /// The warm-up and timed runs of `BenchmarkRunner::RunBenchmark`.
    fn run_loop(&mut self) -> Result<(), Failure> {
        let settings = &self.job.settings;
        let timeout = settings.timeout_secs.map(Duration::from_secs_f64);
        let total = settings.warmup + settings.runs;
        for i in 0..total {
            if i > 0 && self.benchmark.require_reinit {
                self.initialize()?;
            }
            self.watchdog.arm(timeout);
            let start = Instant::now();
            let result = self.engine.query(&self.benchmark.queries.run);
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.watchdog.disarm();
            let result = result?;
            let timed = i >= settings.warmup;
            if timed {
                self.check_result(&result)?;
                if i + 1 == total {
                    self.fingerprint(&result)?;
                }
                lock(self.progress).timings_ms.push(elapsed_ms);
            } else {
                lock(self.progress).warmup_ms.push(elapsed_ms);
            }
            drop(result);
            if let Some(cleanup) = &self.benchmark.queries.cleanup {
                self.engine.query(cleanup)?;
            }
        }
        Ok(())
    }

    /// `InterpretedBenchmark::Verify`.
    fn check_result(&mut self, result: &E::Result) -> Result<(), Failure> {
        if matches!(self.benchmark.result_mode, ResultMode::Arrow { .. }) {
            return Ok(());
        }
        let Some(expected) = &self.benchmark.result else {
            return Ok(());
        };
        let outcome = self.compare(expected, result);
        if let Err(message) = outcome {
            lock(self.progress).result_check = ResultCheck::Failed;
            return Err(Failure {
                status: Status::Incorrect,
                message,
            });
        }
        Ok(())
    }

    fn compare(&mut self, expected: &ExpectedResult, result: &E::Result) -> Result<(), String> {
        let table = match &expected.query {
            None => {
                // Fail fast without rendering a (possibly huge) wrong result.
                if result.row_count() != expected.rows.len() {
                    return Err(format!(
                        "Error in result: expected {} rows but got {}",
                        expected.rows.len(),
                        result.row_count()
                    ));
                }
                result.to_table().map_err(|e| e.message)?
            }
            Some(query) => {
                if expected.needs_answer_table() {
                    self.store_answer(result).map_err(|e| e.message)?;
                }
                let checked = self.engine.query(query).map_err(|e| e.message)?;
                checked.to_table().map_err(|e| e.message)?
            }
        };
        verify(expected, &table)
    }

    /// Stores the run result as the temporary table `__answer` (typed, via literals cast
    /// to each column type) so that `result_query` can inspect it.
    fn store_answer(&mut self, result: &E::Result) -> Result<(), Failure> {
        let table = result.to_table()?;
        let mut seen = std::collections::HashSet::new();
        for column in &table.columns {
            if !seen.insert(column.name.to_lowercase()) {
                return Err(Failure::error(format!(
                    "Duplicate column name \"{}\" in benchmark query",
                    column.name
                )));
            }
        }
        let columns: Vec<String> = table
            .columns
            .iter()
            .map(|c| format!("{} {}", quote_identifier(&c.name), c.type_name))
            .collect();
        self.engine.query(&format!(
            "CREATE OR REPLACE TEMP TABLE {ANSWER_TABLE}({})",
            columns.join(", ")
        ))?;
        for chunk in table.rows.chunks(ANSWER_INSERT_BATCH) {
            let rows: Vec<String> = chunk
                .iter()
                .map(|row| {
                    let values: Vec<String> = row
                        .iter()
                        .zip(&table.columns)
                        .map(|(value, column)| match value {
                            Some(v) => format!("CAST('{}' AS {})", v.replace('\'', "''"), column.type_name),
                            None => "NULL".to_owned(),
                        })
                        .collect();
                    format!("({})", values.join(", "))
                })
                .collect();
            self.engine
                .query(&format!("INSERT INTO {ANSWER_TABLE} VALUES {}", rows.join(", ")))?;
        }
        Ok(())
    }

    /// Records the row count and, for small results, a fingerprint of the last result.
    fn fingerprint(&self, result: &E::Result) -> Result<(), Failure> {
        let rows = result.row_count() as u64;
        lock(self.progress).result_rows = Some(rows);
        if matches!(self.benchmark.result_mode, ResultMode::Arrow { .. }) {
            return Ok(());
        }
        let cells = rows.saturating_mul(result.column_count() as u64);
        if cells <= self.job.settings.fingerprint_max_cells {
            let table = result.to_table()?;
            let mut progress = lock(self.progress);
            progress.result_fingerprint = Some(format!("{:016x}", table.fingerprint()));
            progress.result_types = table.columns.into_iter().map(|c| c.type_name).collect();
        }
        Ok(())
    }
}

/// `DeleteDatabase`: the database file and its WAL.
fn delete_database(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut wal = path.as_os_str().to_owned();
    wal.push(".wal");
    let _ = std::fs::remove_file(wal);
}

/// Double-quotes an identifier.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::format::{Cache, Queries, Storage};
    use crate::verify::{ResultColumn, ResultTable, ValueKind};

    /// An engine that logs calls and answers every query with one integer cell.
    #[derive(Default)]
    struct FakeEngine {
        log: Vec<String>,
        /// The value every query returns.
        value: String,
        /// `SHOW TABLES` reports tables (the cache is populated).
        has_tables: bool,
        /// Queries containing this text fail with the given error kind...
        failing: Option<(&'static str, EngineErrorKind)>,
        /// ...this many times (then succeed).
        failures_left: usize,
    }

    struct FakeResult(ResultTable);

    impl QueryResult for FakeResult {
        fn row_count(&self) -> usize {
            self.0.rows.len()
        }
        fn column_count(&self) -> usize {
            self.0.columns.len()
        }
        fn to_table(&self) -> Result<ResultTable, EngineError> {
            Ok(self.0.clone())
        }
    }

    impl Engine for FakeEngine {
        type Result = FakeResult;

        fn version(&self) -> String {
            "fake".into()
        }

        fn open(&mut self, path: Option<&Path>, config: &DbConfig) -> Result<(), EngineError> {
            let path = path.map_or_else(|| ":memory:".to_owned(), |p| p.display().to_string());
            self.log.push(format!("open {path} threads={}", config.threads));
            Ok(())
        }

        fn close(&mut self) {}

        fn query(&mut self, sql: &str) -> Result<FakeResult, EngineError> {
            self.log.push(sql.to_owned());
            if let Some((text, kind)) = self.failing
                && sql.contains(text)
                && self.failures_left > 0
            {
                self.failures_left -= 1;
                return Err(EngineError {
                    kind,
                    message: format!("failed: {sql}"),
                });
            }
            let rows = match sql {
                "SHOW TABLES" if !self.has_tables => vec![],
                _ => vec![vec![Some(self.value.clone())]],
            };
            Ok(FakeResult(ResultTable {
                columns: vec![ResultColumn {
                    name: "x".into(),
                    type_name: "INTEGER".into(),
                    kind: ValueKind::Integer,
                }],
                rows,
            }))
        }

        fn load_extension(&mut self, name: &str) -> Result<ExtensionStatus, EngineError> {
            self.log.push(format!("LOAD {name}"));
            Ok(match name {
                "missing" => ExtensionStatus::NotAvailable,
                _ => ExtensionStatus::Loaded,
            })
        }
    }

    fn benchmark() -> Benchmark {
        Benchmark {
            name: "benchmark/micro/x.benchmark".into(),
            display_name: "x".into(),
            group: "micro".into(),
            subgroup: String::new(),
            description: None,
            queries: Queries {
                load: Some("LOAD DATA".into()),
                run: "RUN QUERY".into(),
                cleanup: Some("CLEANUP".into()),
                ..Queries::default()
            },
            extensions: vec![],
            load_extensions: vec![],
            cache: Cache::default(),
            storage: Storage::default(),
            require_reinit: false,
            result_mode: ResultMode::Materialized,
            retry_load: 0,
            asserts: vec![],
            result: Some(ExpectedResult {
                query: None,
                column_count: 1,
                rows: vec![vec!["42".into()]],
            }),
            arguments: BTreeMap::new(),
        }
    }

    fn job(benchmark: Benchmark) -> Job {
        Job {
            benchmark,
            settings: RunSettings {
                threads: 4,
                memory_limit: None,
                warmup: 1,
                runs: 2,
                timeout_secs: None,
                fingerprint_max_cells: 100,
            },
            phase: Phase::Measure,
            engine: EngineKind::Rust,
            libduckdb: None,
        }
    }

    /// Runs `job` on `engine` with a scratch data directory.
    fn run(engine: &mut FakeEngine, job: &Job) -> Outcome {
        let data_dir = std::env::temp_dir().join(format!("rustdb-bench-driver-{}", std::process::id()));
        let progress = Arc::new(Mutex::new(Outcome::default()));
        let watchdog = Watchdog::start(Arc::clone(&progress), data_dir.join("outcome"));
        BenchmarkRun::new(engine, job, &progress, &watchdog, &data_dir).run();
        lock(&progress).clone()
    }

    fn engine(value: &str) -> FakeEngine {
        FakeEngine {
            value: value.into(),
            ..FakeEngine::default()
        }
    }

    #[test]
    fn runs_warmup_and_timed_runs_with_checks_and_cleanup() {
        let mut engine = engine("42");
        let outcome = run(&mut engine, &job(benchmark()));
        assert_eq!(outcome.status, Status::Ok, "{outcome:?}");
        assert_eq!(outcome.result_check, ResultCheck::Passed);
        assert_eq!(outcome.warmup_ms.len(), 1);
        assert_eq!(outcome.timings_ms.len(), 2);
        assert_eq!(outcome.result_rows, Some(1));
        assert!(outcome.result_fingerprint.is_some());
        let queries: Vec<&str> = engine.log.iter().map(String::as_str).collect();
        assert_eq!(
            queries,
            [
                "open :memory: threads=4",
                "LOAD core_functions",
                "LOAD parquet",
                "LOAD DATA",
                "RUN QUERY",
                "CLEANUP",
                "RUN QUERY",
                "CLEANUP",
                "RUN QUERY",
                "CLEANUP",
            ]
        );
    }

    #[test]
    fn wrong_results_are_incorrect() {
        let outcome = run(&mut engine("41"), &job(benchmark()));
        assert_eq!(outcome.status, Status::Incorrect);
        assert_eq!(outcome.result_check, ResultCheck::Failed);
        assert_eq!(outcome.timings_ms, Vec::<f64>::new());
        assert!(outcome.error.unwrap().contains("expected value \"42\""));
    }

    #[test]
    fn not_implemented_is_unsupported_and_other_errors_are_errors() {
        let mut unsupported = engine("42");
        unsupported.failing = Some(("RUN", EngineErrorKind::Unsupported));
        unsupported.failures_left = 1;
        assert_eq!(
            run(&mut unsupported, &job(benchmark())).status,
            Status::Unsupported
        );

        let mut failing = engine("42");
        failing.failing = Some(("LOAD DATA", EngineErrorKind::Failed));
        failing.failures_left = 1;
        let outcome = run(&mut failing, &job(benchmark()));
        assert_eq!(outcome.status, Status::Error);
        assert_eq!(outcome.error.as_deref(), Some("failed: LOAD DATA"));

        let mut b = benchmark();
        b.extensions.push("missing".into());
        assert_eq!(run(&mut engine("42"), &job(b)).status, Status::Unsupported);
    }

    #[test]
    fn retries_failing_loads() {
        let mut b = benchmark();
        b.retry_load = 2;
        let mut flaky = engine("42");
        flaky.failing = Some(("LOAD DATA", EngineErrorKind::Failed));
        flaky.failures_left = 2;
        assert_eq!(run(&mut flaky, &job(b.clone())).status, Status::Ok);

        b.retry_load = 1;
        flaky.failures_left = 2;
        assert_eq!(run(&mut flaky, &job(b)).status, Status::Error);
    }

    #[test]
    fn cached_databases_skip_load_and_run_reload() {
        let mut b = benchmark();
        b.cache.database = Some("tpch_sf1.duckdb".into());
        b.queries.reload = Some("RELOAD".into());
        let mut cached = engine("42");
        cached.has_tables = true;
        let outcome = run(&mut cached, &job(b.clone()));
        assert_eq!(outcome.status, Status::Ok);
        assert!(cached.log[0].starts_with("open ") && cached.log[0].ends_with("tpch_sf1.duckdb threads=4"));
        assert!(cached.log.iter().any(|q| q == "RELOAD"));
        assert!(!cached.log.iter().any(|q| q == "LOAD DATA"));

        let mut empty = engine("42");
        run(&mut empty, &job(b));
        assert!(empty.log.iter().any(|q| q == "LOAD DATA"));
        assert!(!empty.log.iter().any(|q| q == "RELOAD"));
    }

    #[test]
    fn no_connect_caches_reconnect_in_memory_and_reinit_reopens() {
        let mut b = benchmark();
        b.cache.database = Some("ingest.duckdb".into());
        b.cache.no_connect = true;
        b.require_reinit = true;
        let mut fake = engine("42");
        assert_eq!(run(&mut fake, &job(b)).status, Status::Ok);
        let opens: Vec<&String> = fake.log.iter().filter(|q| q.starts_with("open ")).collect();
        // Cache build, the in-memory benchmark database, then one reopen per later run.
        assert_eq!(opens.len(), 4, "{opens:?}");
        assert!(opens[0].contains("ingest.duckdb"));
        assert!(opens[1..].iter().all(|o| o.starts_with("open :memory:")));
        assert_eq!(fake.log.iter().filter(|q| *q == "LOAD DATA").count(), 1);
    }

    #[test]
    fn result_queries_see_the_answer_table() {
        let mut b = benchmark();
        b.result = Some(ExpectedResult {
            query: Some("SELECT * FROM __answer".into()),
            column_count: 1,
            rows: vec![vec!["42".into()]],
        });
        let mut fake = engine("42");
        assert_eq!(run(&mut fake, &job(b)).status, Status::Ok);
        assert!(
            fake.log
                .iter()
                .any(|q| q == "CREATE OR REPLACE TEMP TABLE __answer(\"x\" INTEGER)")
        );
        assert!(
            fake.log
                .iter()
                .any(|q| q == "INSERT INTO __answer VALUES (CAST('42' AS INTEGER))")
        );
    }

    #[test]
    fn prepare_only_initializes() {
        let mut job = job(benchmark());
        job.phase = Phase::Prepare;
        let mut fake = engine("42");
        let outcome = run(&mut fake, &job);
        assert_eq!(outcome.status, Status::Ok);
        assert!(!fake.log.iter().any(|q| q == "RUN QUERY"));
    }
}
