//! `bench-driver`: runs one benchmark job on one engine; started by `bench run` once per
//! benchmark so that each process's peak RSS belongs to a single benchmark.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use rustdb_bench::driver::{Job, execute};
use rustdb_bench::engine::{CppEngine, Engine, EngineKind, RustEngine};

#[derive(Debug, Parser)]
#[command(
    name = "bench-driver",
    about = "Runs one benchmark job (started by `bench run`)."
)]
struct Args {
    /// The job file written by `bench run`.
    #[arg(long, required_unless_present = "print_version")]
    job: Option<PathBuf>,
    /// Where to write the outcome.
    #[arg(long, required_unless_present = "print_version")]
    outcome: Option<PathBuf>,
    /// Print the engine version and exit.
    #[arg(long)]
    print_version: bool,
    /// The engine for --print-version.
    #[arg(long, value_enum, default_value = "rust")]
    engine: EngineKind,
    /// The libduckdb for --print-version --engine cpp.
    #[arg(long)]
    libduckdb: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = if args.print_version {
        print_version(&args)
    } else {
        run(&args)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bench-driver: {message}");
            ExitCode::FAILURE
        }
    }
}

fn print_version(args: &Args) -> Result<(), String> {
    let version = match args.engine {
        EngineKind::Rust => RustEngine::new().version(),
        EngineKind::Cpp | EngineKind::CppRunner => {
            let library = args.libduckdb.as_deref().ok_or("--libduckdb is required")?;
            CppEngine::load(library).map_err(|e| e.message)?.version()
        }
    };
    println!("{version}");
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    let (Some(job_path), Some(outcome_path)) = (&args.job, &args.outcome) else {
        return Err("--job and --outcome are required".to_owned());
    };
    let bytes = std::fs::read(job_path).map_err(|e| format!("cannot read {}: {e}", job_path.display()))?;
    let job: Job =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid job {}: {e}", job_path.display()))?;
    execute(&job, outcome_path).map_err(|e| format!("cannot write {}: {e}", outcome_path.display()))
}
