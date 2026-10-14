//! `bench`: runs DuckDB's benchmark suites on C++ DuckDB and rustdb, compares the
//! results and maintains the perf ratchet.
//!
//! ```text
//! cargo build --release -p rustdb-bench          # builds `bench` and `bench-driver`
//! B=target/release/bench
//! $B list 'tpch/sf1/'                            # what would run (cache/network/checked)
//! $B fetch h2oai imdb                            # mirror remote data into target/bench/downloads
//! $B run --engine cpp  'tpch/sf1/' --out cpp.jsonl
//! $B run --engine rust 'tpch/sf1/' --out rust.jsonl
//! $B run --engine cpp  'tpch/sf1/' --arg sf=10   # benchmark arguments (TPC-H SF10)
//! $B run --engine cpp-runner 'tpch/sf1/'         # cross-check through DuckDB's benchmark_runner
//! $B compare cpp.jsonl rust.jsonl --out report.md
//! $B ratchet check  --rust rust.jsonl            # CI: fail on regressions vs bench/baseline.json
//! $B ratchet update --rust rust.jsonl --cpp cpp.jsonl
//! ```
//!
//! Patterns are regular expressions searched in benchmark paths. Every benchmark runs in
//! a fresh `bench-driver` process (peak RSS from `wait4`); caches (`CALL dbgen(...)`
//! databases, generated files) are built first in a separate, unmeasured process.
//! Generated data lives under `target/bench` (`--work-dir`, `RUSTDB_BENCH_DIR`); the
//! DuckDB tree defaults to `.context/duckdb-download/duckdb-1.5.6` (`--duckdb-src`,
//! `DUCKDB_SRC`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use rustdb_bench::data;
use rustdb_bench::discover::{Selection, discover};
use rustdb_bench::driver::RunSettings;
use rustdb_bench::engine::EngineKind;
use rustdb_bench::format::{ParseOptions, parse_benchmark};
use rustdb_bench::machine;
use rustdb_bench::ratchet::{self, Baseline};
use rustdb_bench::record::{Line, Results, Status, Writer};
use rustdb_bench::report::{self, format_bytes, format_ms};
use rustdb_bench::runner::{Harness, Paths};

#[derive(Debug, Parser)]
#[command(name = "bench", about = "DuckDB benchmark suites on C++ DuckDB and rustdb.")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run benchmarks on one engine; writes one JSON line per benchmark.
    Run(RunArgs),
    /// List the selected benchmarks.
    List(Selector),
    /// Download the remote data of the selected benchmarks (h2oai, imdb, ...).
    Fetch {
        #[command(flatten)]
        selector: Selector,
        /// Only print the URLs.
        #[arg(long)]
        dry_run: bool,
    },
    /// Compare a C++ and a rustdb results file; prints a markdown report.
    Compare {
        cpp: PathBuf,
        rust: PathBuf,
        /// Write the report here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Check or update the perf ratchet (`bench/baseline.json`).
    Ratchet {
        #[command(subcommand)]
        action: RatchetCmd,
    },
}

#[derive(Debug, Subcommand)]
enum RatchetCmd {
    /// Fail if a baseline benchmark regressed beyond noise or stopped passing.
    Check {
        /// The rustdb results file.
        #[arg(long)]
        rust: PathBuf,
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Benchmarks missing from the run are not failures (partial runs).
        #[arg(long)]
        allow_missing: bool,
    },
    /// Record the current rustdb numbers as the new baseline.
    Update {
        #[arg(long)]
        rust: PathBuf,
        /// C++ results recorded alongside, for context.
        #[arg(long)]
        cpp: Option<PathBuf>,
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Drop baseline benchmarks that no longer pass.
        #[arg(long)]
        force: bool,
    },
}

/// Which benchmarks, and where they live.
#[derive(Debug, Args)]
struct Selector {
    /// Regular expressions searched in benchmark names (`benchmark/tpch/sf1/q01.benchmark`);
    /// none selects everything.
    patterns: Vec<String>,
    /// Exclude benchmarks matching these regular expressions.
    #[arg(long)]
    exclude: Vec<String>,
    /// Benchmark argument overrides (DuckDB's `--<name> <value>`), e.g. `--arg sf=10`.
    #[arg(long = "arg", value_parser = parse_key_value)]
    arguments: Vec<(String, String)>,
    /// The DuckDB 1.5.6 source tree with a release build.
    #[arg(long, env = "DUCKDB_SRC")]
    duckdb_src: Option<PathBuf>,
    /// Scratch directory for generated data, downloads and logs.
    #[arg(long, env = "RUSTDB_BENCH_DIR")]
    work_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    selector: Selector,
    /// The engine to measure.
    #[arg(long, value_enum, default_value = "cpp")]
    engine: EngineKind,
    /// The results file (default: `<work-dir>/results/<engine>.jsonl`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Append to the results file instead of replacing it.
    #[arg(long)]
    append: bool,
    /// Append, skipping benchmarks already in the results file (continue a stopped run).
    #[arg(long)]
    resume: bool,
    /// `threads` for both engines (default: all logical cores, like DuckDB's runner).
    #[arg(long)]
    threads: Option<u32>,
    /// `memory_limit`, e.g. `16GB` (default: the engine default).
    #[arg(long)]
    memory_limit: Option<String>,
    /// Timed runs per benchmark.
    #[arg(long, default_value_t = 5)]
    runs: u32,
    /// Untimed runs before the timed ones (the first is the cold run).
    #[arg(long, default_value_t = 1)]
    warmup: u32,
    /// Per-run timeout in seconds (0 disables).
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// Time budget in seconds for building a cache or other setup.
    #[arg(long, default_value_t = 3600)]
    setup_timeout: u64,
    /// The C++ libduckdb (default: `<duckdb-src>/build/release/src/libduckdb.*`).
    #[arg(long)]
    libduckdb: Option<PathBuf>,
    /// DuckDB's `benchmark_runner` (default: `<duckdb-src>/build/release/benchmark/benchmark_runner`).
    #[arg(long)]
    benchmark_runner: Option<PathBuf>,
    /// Fingerprint results up to this many cells, for cross-engine result checks.
    #[arg(long, default_value_t = 100_000)]
    fingerprint_max_cells: u64,
}

fn parse_key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("expected NAME=VALUE, got {s}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Run(args) => run(&args),
        Cmd::List(selector) => list(&selector),
        Cmd::Fetch { selector, dry_run } => fetch(&selector, dry_run),
        Cmd::Compare { cpp, rust, out } => compare(&cpp, &rust, out.as_deref()),
        Cmd::Ratchet { action } => ratchet_command(action),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("bench: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The rustdb checkout this binary was built from.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    root.canonicalize().unwrap_or(root)
}

impl Selector {
    fn duckdb_src(&self) -> Result<PathBuf, String> {
        let src = self
            .duckdb_src
            .clone()
            .unwrap_or_else(|| repo_root().join(".context/duckdb-download/duckdb-1.5.6"));
        if src.join("benchmark").is_dir() {
            Ok(src)
        } else {
            Err(format!(
                "{} is not a DuckDB source tree (pass --duckdb-src or set DUCKDB_SRC)",
                src.display()
            ))
        }
    }

    fn work_dir(&self) -> PathBuf {
        self.work_dir
            .clone()
            .unwrap_or_else(|| repo_root().join("target/bench"))
    }

    fn arguments(&self) -> BTreeMap<String, String> {
        self.arguments.iter().cloned().collect()
    }

    /// Selected benchmark names, discovered in `root`.
    fn names(&self, root: &Path) -> Result<Vec<String>, String> {
        let selection = Selection::new(&self.patterns, &self.exclude).map_err(|e| e.to_string())?;
        let names =
            discover(root).map_err(|e| format!("cannot list benchmarks in {}: {e}", root.display()))?;
        let selected: Vec<String> = names.into_iter().filter(|n| selection.matches(n)).collect();
        if selected.is_empty() {
            return Err("no benchmark matches the given patterns".to_owned());
        }
        Ok(selected)
    }
}

fn require_file(path: &Path, what: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{what} not found at {}", path.display()))
    }
}

fn engine_version(paths: &Paths, engine: EngineKind) -> String {
    let driver_engine = if engine == EngineKind::Rust { "rust" } else { "cpp" };
    let output = Command::new(&paths.driver)
        .args(["--print-version", "--engine", driver_engine, "--libduckdb"])
        .arg(&paths.libduckdb)
        .output();
    let version = output.ok().filter(|o| o.status.success()).map_or_else(
        || "unknown".to_owned(),
        |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
    );
    if engine == EngineKind::CppRunner {
        format!("{version} via benchmark_runner")
    } else {
        version
    }
}

fn run(args: &RunArgs) -> Result<ExitCode, String> {
    let duckdb_src = args.selector.duckdb_src()?;
    let release = duckdb_src.join("build/release");
    let library_name = if cfg!(target_os = "macos") {
        "libduckdb.dylib"
    } else {
        "libduckdb.so"
    };
    let driver = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("bench-driver");
    let paths = Paths {
        libduckdb: args
            .libduckdb
            .clone()
            .unwrap_or_else(|| release.join("src").join(library_name)),
        benchmark_runner: args
            .benchmark_runner
            .clone()
            .unwrap_or_else(|| release.join("benchmark/benchmark_runner")),
        driver,
        work_dir: args.selector.work_dir(),
        duckdb_src,
    };
    require_file(
        &paths.driver,
        "bench-driver (build with `cargo build --release -p rustdb-bench`)",
    )?;
    if args.engine != EngineKind::Rust {
        require_file(&paths.libduckdb, "libduckdb")?;
    }
    if args.engine == EngineKind::CppRunner {
        require_file(&paths.benchmark_runner, "benchmark_runner")?;
    }
    let threads = args
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get() as u32));
    let harness = Harness {
        engine: args.engine,
        settings: RunSettings {
            threads,
            memory_limit: args.memory_limit.clone(),
            warmup: args.warmup,
            runs: args.runs,
            timeout_secs: (args.timeout > 0.0).then_some(args.timeout),
            fingerprint_max_cells: args.fingerprint_max_cells,
        },
        arguments: args.selector.arguments(),
        setup_timeout: Duration::from_secs(args.setup_timeout),
        paths,
    };
    let root = harness
        .prepare_root()
        .map_err(|e| format!("cannot prepare the benchmark root: {e}"))?;
    let mut names = args.selector.names(&root)?;

    let out = args.out.clone().unwrap_or_else(|| {
        harness
            .paths
            .work_dir
            .join("results")
            .join(format!("{}.jsonl", args.engine))
    });
    if args.resume && out.exists() {
        let done = read_results(&out)?;
        names.retain(|name| {
            !done.benchmarks.iter().any(|r| {
                &r.name == name
                    && harness
                        .arguments
                        .iter()
                        .all(|(k, v)| r.arguments.get(k) == Some(v))
            })
        });
        if names.is_empty() {
            eprintln!("bench: every selected benchmark is already in {}", out.display());
            return Ok(ExitCode::SUCCESS);
        }
    }
    let mut writer = Writer::create(&out, args.append || args.resume)
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let mut info = machine::collect(&repo_root());
    args.engine.as_str().clone_into(&mut info.engine);
    info.engine_version = engine_version(&harness.paths, args.engine);
    info.threads = threads;
    info.memory_limit.clone_from(&args.memory_limit);
    info.warmup = args.warmup;
    info.runs = args.runs;
    info.timeout_secs = harness.settings.timeout_secs;
    eprintln!(
        "bench: {} benchmark(s) on {} ({}), threads={threads}, writing {}",
        names.len(),
        args.engine,
        info.engine_version,
        out.display()
    );
    writer
        .write(&Line::Machine(info))
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;

    let mut counts: BTreeMap<Status, usize> = BTreeMap::new();
    for (index, name) in names.iter().enumerate() {
        let record = harness.run(name);
        *counts.entry(record.status).or_default() += 1;
        let detail = match record.status {
            Status::Ok => format!(
                "median {}  peak RSS {}",
                record.median_ms.map_or_else(|| "-".to_owned(), format_ms),
                record.peak_rss_bytes.map_or_else(|| "-".to_owned(), format_bytes)
            ),
            _ => record
                .error
                .as_deref()
                .and_then(|e| e.lines().next())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect(),
        };
        eprintln!(
            "[{:>4}/{}] {:<11} {name}  {detail}",
            index + 1,
            names.len(),
            record.status.as_str()
        );
        writer
            .write(&Line::Benchmark(record))
            .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    }
    let summary: Vec<String> = counts.iter().map(|(status, n)| format!("{n} {status}")).collect();
    eprintln!("bench: done: {}", summary.join(", "));
    Ok(ExitCode::SUCCESS)
}

fn list(selector: &Selector) -> Result<ExitCode, String> {
    let src = selector.duckdb_src()?;
    let arguments = selector.arguments();
    let options = ParseOptions {
        root: &src,
        arguments: &arguments,
    };
    for name in selector.names(&src)? {
        match parse_benchmark(&name, &options) {
            Ok(b) => {
                let mut flags = Vec::new();
                if !b.cache.is_empty() {
                    flags.push("cache");
                }
                if data::needs_network(&b) {
                    flags.push("network");
                }
                if b.result.is_some() {
                    flags.push("checked");
                }
                println!(
                    "{name}\t{}\t{}\t{}\t{}",
                    b.display_name,
                    b.group,
                    b.subgroup,
                    flags.join(",")
                );
            }
            Err(e) => println!("{name}\tPARSE ERROR: {e}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn fetch(selector: &Selector, dry_run: bool) -> Result<ExitCode, String> {
    let src = selector.duckdb_src()?;
    let downloads = selector.work_dir().join("downloads");
    let arguments = selector.arguments();
    let options = ParseOptions {
        root: &src,
        arguments: &arguments,
    };
    let mut urls: Vec<String> = selector
        .names(&src)?
        .iter()
        .filter_map(|name| parse_benchmark(name, &options).ok())
        .flat_map(|b| data::urls(&b))
        .collect();
    urls.sort();
    urls.dedup();
    let mut failures = 0;
    for url in &urls {
        if url.contains('{') {
            eprintln!("skip (URL built at run time): {url}");
            continue;
        }
        let local = data::local_path(&downloads, url);
        if dry_run {
            println!("{url}\t{}", local.display());
            continue;
        }
        eprintln!("fetch {url}");
        if let Err(e) = data::download(&downloads, url) {
            eprintln!("  failed: {e}");
            failures += 1;
        }
    }
    eprintln!("bench: {} URL(s), {failures} failed", urls.len());
    Ok(if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn read_results(path: &Path) -> Result<Results, String> {
    Results::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn compare(cpp: &Path, rust: &Path, out: Option<&Path>) -> Result<ExitCode, String> {
    let report = report::compare(
        &read_results(cpp)?,
        &read_results(rust)?,
        &cpp.display().to_string(),
        &rust.display().to_string(),
    );
    match out {
        Some(path) => {
            std::fs::write(path, report).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        None => print!("{report}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn ratchet_command(action: RatchetCmd) -> Result<ExitCode, String> {
    let default_baseline = || repo_root().join("bench/baseline.json");
    match action {
        RatchetCmd::Check {
            rust,
            baseline,
            allow_missing,
        } => {
            let path = baseline.unwrap_or_else(default_baseline);
            let baseline =
                Baseline::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let check = ratchet::check(&baseline, &read_results(&rust)?, allow_missing);
            print!("{}", check.render());
            Ok(if check.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        RatchetCmd::Update {
            rust,
            cpp,
            baseline,
            force,
        } => {
            let path = baseline.unwrap_or_else(default_baseline);
            let existing = path
                .exists()
                .then(|| Baseline::read(&path))
                .transpose()
                .map_err(|e| e.to_string())?;
            let cpp = cpp.as_deref().map(read_results).transpose()?;
            let updated = ratchet::update(existing, &read_results(&rust)?, cpp.as_ref(), force)?;
            updated
                .write(&path)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            eprintln!(
                "bench: {} benchmark(s) in {}",
                updated.benchmarks.len(),
                path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
