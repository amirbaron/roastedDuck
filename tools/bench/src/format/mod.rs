//! DuckDB's interpreted `.benchmark` format.
//!
//! [`parse_benchmark`] is a port of `InterpretedBenchmark::ProcessFile` /
//! `LoadBenchmark` (`benchmark/interpreted_benchmark.cpp`): it resolves templates,
//! includes, arguments and expected results into a self-contained [`Benchmark`] that a
//! driver process can execute without touching the benchmark files again.

mod csv;
mod parser;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use parser::{ParseError, ParseOptions, parse_benchmark};

/// `BenchmarkRunner::DUCKDB_BENCHMARK_DIRECTORY`: where caches and generated files live,
/// relative to the benchmark root.
pub const BENCHMARK_DIRECTORY: &str = "duckdb_benchmark_data";

/// `DEFAULT_DB_PATH`: the database file of `storage persistent` benchmarks without a cache.
pub const DEFAULT_DB_PATH: &str = "duckdb_benchmark_db.db";

/// A fully resolved benchmark.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Benchmark {
    /// The benchmark path relative to the benchmark root, e.g.
    /// `benchmark/tpch/sf1/q01.benchmark`. This is the runner's benchmark name.
    pub name: String,
    /// The `name` directive, or [`Benchmark::name`] when absent.
    pub display_name: String,
    /// The `group` directive, or `[<parent directory>]` when absent.
    pub group: String,
    /// The `subgroup` directive (empty when absent).
    pub subgroup: String,
    /// The `# description:` header comment of the benchmark file, if any.
    pub description: Option<String>,
    /// The SQL of each section.
    pub queries: Queries,
    /// Extensions loaded right after opening the database (`require <ext>`).
    pub extensions: Vec<String>,
    /// Extensions loaded only before running the load query (`require <ext> load_only`).
    pub load_extensions: Vec<String>,
    /// The `cache` / `cache_file` directives.
    pub cache: Cache,
    /// The `storage` directive.
    pub storage: Storage,
    /// `require_reinit`: re-initialize (reopen + init + load) before every timed run.
    pub require_reinit: bool,
    /// The `resultmode` directive.
    pub result_mode: ResultMode,
    /// `retry load <n>`: how many times a failing load query is retried.
    pub retry_load: u32,
    /// `assert` sections: checked once after initialization, before any run.
    pub asserts: Vec<ExpectedResult>,
    /// The `result` / `result_query` section matching the current arguments, if any.
    pub result: Option<ExpectedResult>,
    /// The values of all `argument` directives after applying overrides.
    pub arguments: BTreeMap<String, String>,
}

/// The SQL sections of a benchmark. Multi-line sections are joined with spaces, exactly
/// like DuckDB's runner does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queries {
    /// `load`: builds the data set (skipped when a cache is present).
    pub load: Option<String>,
    /// `reload`: runs instead of `load` when the cache is present.
    pub reload: Option<String>,
    /// `init`: runs after opening the database, before `load`.
    pub init: Option<String>,
    /// `run`: the timed query.
    pub run: String,
    /// `cleanup`: runs after every run.
    pub cleanup: Option<String>,
}

/// The `cache` / `cache_file` directives.
///
/// Both can be present; like DuckDB, a cache file takes precedence when deciding whether
/// `load` must run, while a cache database decides which database file is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    /// `cache <file>`: the database lives in this file (relative to
    /// [`BENCHMARK_DIRECTORY`]); `load` only runs when it has no tables yet.
    pub database: Option<String>,
    /// `cache <file> no_connect`: build the cache, then run the benchmark on a fresh
    /// in-memory database.
    pub no_connect: bool,
    /// `cache <file>.csv|.csv.gz|.parquet` or `cache_file <file>`: `load` only runs when
    /// this file (relative to [`BENCHMARK_DIRECTORY`]) does not exist yet.
    pub file: Option<String>,
}

impl Cache {
    /// Whether the benchmark caches anything.
    pub fn is_empty(&self) -> bool {
        self.database.is_none() && self.file.is_none()
    }

    /// The paths (relative to the benchmark root) whose existence means the cache is
    /// built.
    pub fn paths(&self) -> impl Iterator<Item = String> + '_ {
        self.database
            .iter()
            .chain(&self.file)
            .map(|file| format!("{BENCHMARK_DIRECTORY}/{file}"))
    }
}

/// The `storage` directive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    /// `storage persistent`: use an on-disk database instead of an in-memory one.
    pub persistent: bool,
    /// The optional storage (serialization compatibility) version, e.g. `v1.2.0`.
    pub version: Option<String>,
}

/// The `resultmode` directive: which result collector the run query uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultMode {
    /// A materialized result (the default).
    #[default]
    Materialized,
    /// A streaming result that is materialized after the query.
    Streaming,
    /// An Arrow result collector with the given batch size; the result is not checked.
    Arrow {
        /// Rows per Arrow batch.
        batch_size: u64,
    },
}

/// An expected result: `result`, `result_query` or `assert`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedResult {
    /// The query whose result is checked. `None` checks the run query's own result
    /// (DuckDB's `select * from __answer`).
    pub query: Option<String>,
    /// The number of columns.
    pub column_count: usize,
    /// The expected rows, as strings.
    pub rows: Vec<Vec<String>>,
}

/// The table name under which `result_query` sees the run query's result.
pub const ANSWER_TABLE: &str = "__answer";

impl ExpectedResult {
    /// Whether checking this result needs the run result stored as [`ANSWER_TABLE`].
    pub fn needs_answer_table(&self) -> bool {
        self.query
            .as_deref()
            .is_some_and(|q| q.to_ascii_lowercase().contains(ANSWER_TABLE))
    }
}

impl Benchmark {
    /// The suite this benchmark belongs to: the first directory below `benchmark/`
    /// (`micro`, `tpch`, `h2oai`, ...).
    pub fn suite(&self) -> &str {
        suite_of(&self.name)
    }
}

/// The suite of a benchmark name: the directory right below `benchmark/`.
pub fn suite_of(name: &str) -> &str {
    let rest = name.strip_prefix("benchmark/").unwrap_or(name);
    rest.split('/').next().unwrap_or(rest)
}
