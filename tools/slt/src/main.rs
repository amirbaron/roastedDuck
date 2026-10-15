//! `slt`: DuckDB-dialect sqllogictest runner for the Rust engine and the C++ oracle.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use slt::backend::cpp::CppEngine;
use slt::backend::rust::RustEngine;
use slt::config::TestEnvironment;
use slt::harness::{self, RESULT_MARKER, RunOptions, Worker};
use slt::outcome::{FileResult, Status};
use slt::{ratchet, report, runner};

/// DuckDB-dialect sqllogictest runner for rustdb and the C++ DuckDB oracle.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Run test files or directories in parallel worker processes and write a results file.
    Run(RunArgs),
    /// Run a single test file in this process and print its verdict (used by `run` workers).
    Exec(ExecArgs),
    /// Write a markdown dashboard comparing C++ and Rust results per test directory.
    Compare {
        cpp_results: PathBuf,
        rust_results: PathBuf,
        /// Write the dashboard here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List files whose verdict differs between two results files (exit code 1 if any).
    Diff { left: PathBuf, right: PathBuf },
    /// Check or update the list of files that pass on the Rust engine.
    Ratchet {
        #[command(subcommand)]
        action: RatchetAction,
    },
}

#[derive(Debug, Subcommand)]
enum RatchetAction {
    /// Fail if a listed file no longer passes.
    Check(RatchetArgs),
    /// Rewrite the list with the files that pass now (refuses to drop listed files).
    Update(RatchetArgs),
}

#[derive(Debug, clap::Args)]
struct RatchetArgs {
    /// Results of a Rust engine run.
    rust_results: PathBuf,
    /// C++ reference results (skips only count as passing when C++ skips for the same reason).
    #[arg(long, default_value = "conformance/reference-cpp.jsonl")]
    cpp: PathBuf,
    /// The ratchet list.
    #[arg(long, default_value = "conformance/passing.txt")]
    list: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum EngineKind {
    /// The Rust engine (rustdb).
    Rust,
    /// The C++ DuckDB oracle through the shim library.
    Cpp,
    /// DuckDB's own `unittest` binary (reference verdicts for validating the harness).
    Unittest,
}

#[derive(Debug, clap::Args)]
struct EngineArgs {
    /// DuckDB 1.5.6 source root; tests run with it as the working directory [default: `$DUCKDB_REF`].
    #[arg(long, env = "DUCKDB_REF")]
    root: PathBuf,
    /// Oracle shim library [default: `target/oracle-shim/libslt_oracle.{dylib,so}`].
    #[arg(long)]
    shim: Option<PathBuf>,
    /// What `__BUILD_DIRECTORY__` means for the Rust engine [default: `<root>/build/release`].
    #[arg(long)]
    build_dir: Option<PathBuf>,
    /// Fail (instead of skip) when this `require` is missing; may be repeated.
    #[arg(long = "require")]
    requires: Vec<String>,
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    #[arg(long, value_enum)]
    engine: EngineKind,
    #[command(flatten)]
    engine_args: EngineArgs,
    /// DuckDB `unittest` binary for `--engine unittest` [default: `<root>/build/release/test/unittest`].
    #[arg(long)]
    unittest_bin: Option<PathBuf>,
    /// Parallel worker processes [default: number of CPUs].
    #[arg(long)]
    jobs: Option<usize>,
    /// Per-file time limit.
    #[arg(long, default_value_t = 600)]
    timeout_secs: u64,
    /// Results file (JSON lines).
    #[arg(long, default_value = "results.jsonl")]
    out: PathBuf,
    /// Keep every worker's output in this directory.
    #[arg(long)]
    log_dir: Option<PathBuf>,
    /// Read more test files or directories from this file, one per line.
    #[arg(long)]
    files_from: Option<PathBuf>,
    /// Test files or directories, relative to the current directory or to the root.
    #[arg(required_unless_present = "files_from")]
    paths: Vec<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ExecArgs {
    #[arg(long, value_enum)]
    engine: EngineKind,
    #[command(flatten)]
    engine_args: EngineArgs,
    /// Test file, relative to the root (e.g. `test/sql/select/test_select.test`).
    file: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        CliCommand::Run(args) => run(&args),
        CliCommand::Exec(args) => exec(&args),
        CliCommand::Compare {
            cpp_results,
            rust_results,
            out,
        } => compare(&cpp_results, &rust_results, out.as_deref()),
        CliCommand::Diff { left, right } => diff(&left, &right),
        CliCommand::Ratchet { action } => match action {
            RatchetAction::Check(args) => ratchet_check(&args),
            RatchetAction::Update(args) => ratchet_update(&args),
        },
    };
    outcome.unwrap_or_else(|error| {
        eprintln!("error: {error}");
        ExitCode::from(2)
    })
}

fn default_shim() -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "libslt_oracle.dylib"
    } else {
        "libslt_oracle.so"
    };
    Path::new("target/oracle-shim").join(name)
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn run(args: &RunArgs) -> Result<ExitCode, String> {
    let root = absolute(&args.engine_args.root)?;
    let worker = match args.engine {
        EngineKind::Unittest => Worker::Unittest {
            executable: absolute(
                &args
                    .unittest_bin
                    .clone()
                    .unwrap_or_else(|| root.join("build/release/test/unittest")),
            )?,
        },
        EngineKind::Rust | EngineKind::Cpp => {
            let mut worker_args = vec![
                "--engine".to_owned(),
                if args.engine == EngineKind::Rust {
                    "rust"
                } else {
                    "cpp"
                }
                .to_owned(),
            ];
            if args.engine == EngineKind::Cpp {
                let shim = absolute(&args.engine_args.shim.clone().unwrap_or_else(default_shim))?;
                worker_args.extend(["--shim".to_owned(), shim.to_string_lossy().into_owned()]);
            }
            if let Some(build_dir) = &args.engine_args.build_dir {
                worker_args.extend([
                    "--build-dir".to_owned(),
                    absolute(build_dir)?.to_string_lossy().into_owned(),
                ]);
            }
            for require in &args.engine_args.requires {
                worker_args.extend(["--require".to_owned(), require.clone()]);
            }
            Worker::Slt {
                executable: std::env::current_exe().map_err(|error| error.to_string())?,
                args: worker_args,
            }
        }
    };
    let mut paths = args.paths.clone();
    if let Some(list) = &args.files_from {
        let text = std::fs::read_to_string(list).map_err(|error| format!("{}: {error}", list.display()))?;
        paths.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from),
        );
    }
    let files = harness::collect_test_files(&root, &paths).map_err(|error| error.to_string())?;
    let jobs = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get));
    let log_dir = args.log_dir.as_deref().map(absolute_or_new).transpose()?;
    let options = RunOptions {
        worker,
        root,
        jobs,
        timeout: Duration::from_secs(args.timeout_secs),
        log_dir,
    };
    eprintln!("running {} files with {jobs} workers", files.len());
    let done = AtomicUsize::new(0);
    let total = files.len();
    let results = harness::run_files(&options, &files, &|result: &FileResult| {
        let index = done.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!(
            "[{index:>5}/{total}] {:<7} {} ({} ms)",
            result.status.as_str(),
            result.file,
            result.duration_ms
        );
    });
    report::write_results(&args.out, &results).map_err(|error| error.to_string())?;
    print!("{}", report::summary_table(&results));
    println!("results written to {}", args.out.display());
    Ok(ExitCode::SUCCESS)
}

fn absolute_or_new(path: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(path).map_err(|error| format!("{}: {error}", path.display()))?;
    absolute(path)
}

fn exec(args: &ExecArgs) -> Result<ExitCode, String> {
    let engine_args = &args.engine_args;
    let root = absolute(&engine_args.root)?;
    let shim = match args.engine {
        EngineKind::Cpp => Some(absolute(&engine_args.shim.clone().unwrap_or_else(default_shim))?),
        _ => None,
    };
    let build_dir = match &engine_args.build_dir {
        Some(dir) => absolute(dir)?,
        None => root.join("build/release"),
    };
    let file = Path::new(&args.file).strip_prefix(&root).map_or_else(
        |_| args.file.clone(),
        |relative| relative.to_string_lossy().into_owned(),
    );
    std::env::set_current_dir(&root).map_err(|error| format!("{}: {error}", root.display()))?;
    let result = match (args.engine, shim) {
        (EngineKind::Cpp, Some(shim)) => {
            let engine = CppEngine::load(&shim)?;
            let build_dir = slt::backend::Engine::build_directory(&engine).to_owned();
            run_in_test_directory(&engine, &build_dir, &engine_args.requires, &file)?
        }
        (EngineKind::Rust, _) => {
            let build_dir = build_dir.to_string_lossy().into_owned();
            run_in_test_directory(
                &RustEngine::new(build_dir.clone()),
                &build_dir,
                &engine_args.requires,
                &file,
            )?
        }
        _ => return Err("exec supports --engine rust and --engine cpp".to_owned()),
    };
    match (&result.status, &result.reason, &result.failure) {
        (Status::Fail, _, Some(failure)) => eprintln!("FAIL {file}: {failure}"),
        (status, Some(reason), _) => eprintln!("{} {file}: {reason}", status.as_str().to_uppercase()),
        (status, _, _) => eprintln!("{} {file}", status.as_str().to_uppercase()),
    }
    let json = serde_json::to_string(&result).map_err(|error| error.to_string())?;
    println!("{RESULT_MARKER}{json}");
    Ok(ExitCode::SUCCESS)
}

/// Runs `file` with a fresh per-process test directory, like `unittest` does around each run.
fn run_in_test_directory<E: slt::backend::Engine>(
    engine: &E,
    build_dir: &str,
    requires: &[String],
    file: &str,
) -> Result<FileResult, String> {
    let environment =
        TestEnvironment::new(build_dir, requires.iter().cloned()).map_err(|error| error.to_string())?;
    environment
        .reset_test_directory()
        .map_err(|error| error.to_string())?;
    let result = runner::run_file(engine, &environment, file);
    environment
        .remove_test_directory()
        .map_err(|error| error.to_string())?;
    Ok(result)
}

fn compare(cpp: &Path, rust: &Path, out: Option<&Path>) -> Result<ExitCode, String> {
    let cpp = report::read_results(cpp).map_err(|error| error.to_string())?;
    let rust = report::read_results(rust).map_err(|error| error.to_string())?;
    let dashboard = report::comparison_dashboard(&cpp, &rust);
    match out {
        Some(path) => {
            std::fs::write(path, dashboard).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        None => print!("{dashboard}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn diff(left: &Path, right: &Path) -> Result<ExitCode, String> {
    let left_results = report::read_results(left).map_err(|error| error.to_string())?;
    let right_results = report::read_results(right).map_err(|error| error.to_string())?;
    let discrepancies = report::verdict_diff(&left_results, &right_results);
    for discrepancy in &discrepancies {
        println!(
            "{}\t{}\t{}",
            discrepancy.file, discrepancy.left, discrepancy.right
        );
    }
    eprintln!(
        "{} discrepancies between {} ({} files) and {} ({} files)",
        discrepancies.len(),
        left.display(),
        left_results.len(),
        right.display(),
        right_results.len()
    );
    Ok(if discrepancies.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn load_ratchet_inputs(args: &RatchetArgs) -> Result<(Vec<FileResult>, Vec<FileResult>), String> {
    let rust = report::read_results(&args.rust_results).map_err(|error| error.to_string())?;
    let cpp = if args.cpp.exists() {
        report::read_results(&args.cpp).map_err(|error| error.to_string())?
    } else {
        eprintln!(
            "warning: {} not found; skipped files never count as passing",
            args.cpp.display()
        );
        Vec::new()
    };
    Ok((rust, cpp))
}

fn ratchet_check(args: &RatchetArgs) -> Result<ExitCode, String> {
    let (rust, cpp) = load_ratchet_inputs(args)?;
    let listed = ratchet::read_list(&args.list).map_err(|error| error.to_string())?;
    let report = ratchet::check(&listed, &rust, &cpp);
    for (file, verdict) in &report.regressions {
        println!("REGRESSION {file}: {verdict}");
    }
    eprintln!(
        "{} listed, {} regressions, {} newly passing (run `slt ratchet update` to record them)",
        listed.len(),
        report.regressions.len(),
        report.newly_passing.len()
    );
    Ok(if report.regressions.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn ratchet_update(args: &RatchetArgs) -> Result<ExitCode, String> {
    let (rust, cpp) = load_ratchet_inputs(args)?;
    let listed = ratchet::read_list(&args.list).map_err(|error| error.to_string())?;
    let report = ratchet::check(&listed, &rust, &cpp);
    if !report.regressions.is_empty() {
        for (file, verdict) in &report.regressions {
            println!("REGRESSION {file}: {verdict}");
        }
        return Err(format!(
            "refusing to update {}: {} listed files no longer pass",
            args.list.display(),
            report.regressions.len()
        ));
    }
    let passing = ratchet::passing_files(&rust, &cpp);
    ratchet::write_list(&args.list, &passing).map_err(|error| error.to_string())?;
    eprintln!(
        "{}: {} files ({} new)",
        args.list.display(),
        passing.len(),
        report.newly_passing.len()
    );
    Ok(ExitCode::SUCCESS)
}
