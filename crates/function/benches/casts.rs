//! Cast throughput: rustdb's vector casts against DuckDB 1.5.6, single-threaded.
//!
//! Each benchmark casts 10M pre-generated values in flat vectors of 2048 rows through a
//! cast bound once (like an expression executor would), runs twice and reports the second
//! run. The inputs match `casts_duckdb.sql`, which runs the same casts in the DuckDB CLI
//! (`SET threads=1`, `SELECT count(CAST(x AS T)) FROM t`, second of two runs).
//!
//! ```text
//! cargo bench -p rustdb-function --bench casts               # all, plus DuckDB if found
//! cargo bench -p rustdb-function --bench casts -- to_varchar  # name filter
//! duckdb < crates/function/benches/casts_duckdb.sql           # the C++ side alone
//! ```
//!
//! The DuckDB CLI is taken from `$DUCKDB_ORACLE` (default: the oracle used by
//! `tests/cast_oracle.rs`); set `CASTS_BENCH_DUCKDB=0` to skip it. Both wall-clock and CPU
//! time are reported: on a loaded machine compare CPU times (thread CPU time for rustdb,
//! `user + sys` of the query for DuckDB). Casts that rustdb does not implement yet are
//! reported as skipped.

use std::error::Error;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rustdb_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use rustdb_types::{LogicalType, STANDARD_VECTOR_SIZE, Vector};

type BenchResult<T> = Result<T, Box<dyn Error>>;

const ROWS: usize = 10_000_000;

const DEFAULT_DUCKDB: &str = "/Users/amirbaron/conductor/workspaces/duckdb-rust/ottawa/.context/duckdb-download/duckdb-1.5.6/build/release/duckdb";

/// Wall-clock and CPU time of one run.
#[derive(Clone, Copy, Debug)]
struct Timing {
    wall: Duration,
    cpu: Option<Duration>,
}

/// CPU time consumed by the calling thread.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn thread_cpu_time() -> Option<Duration> {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
    }
    const CLOCK_THREAD_CPUTIME_ID: i32 = if cfg!(target_os = "macos") { 16 } else { 3 };
    let mut time = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` only writes a `timespec` (two 64-bit fields on the supported
    // 64-bit targets) through the valid pointer we pass.
    let status = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &raw mut time) };
    (status == 0).then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

/// `count` values in flat vectors of at most [`STANDARD_VECTOR_SIZE`] rows.
fn chunked(ty: &LogicalType, fill: impl Fn(&mut Vector, usize, usize)) -> Vec<(Vector, usize)> {
    (0..ROWS)
        .step_by(STANDARD_VECTOR_SIZE)
        .map(|start| {
            let len = STANDARD_VECTOR_SIZE.min(ROWS - start);
            let mut vector = Vector::flat(ty.clone(), len);
            for row in 0..len {
                fill(&mut vector, row, start + row);
            }
            (vector, len)
        })
        .collect()
}

fn integer_input(i: usize) -> i32 {
    (i as i64 * 7919 % 2_000_000_000) as i32
}

fn integers() -> Vec<(Vector, usize)> {
    chunked(&LogicalType::INTEGER, |vector, row, i| {
        vector.values_mut::<i32>()[row] = integer_input(i);
    })
}

fn doubles() -> Vec<(Vector, usize)> {
    chunked(&LogicalType::DOUBLE, |vector, row, i| {
        vector.values_mut::<f64>()[row] = f64::from(integer_input(i)) / 1000.0;
    })
}

/// The VARCHAR forms of `inputs`, rendered by rustdb's own cast.
fn as_text(set: &CastFunctionSet, inputs: &[(Vector, usize)]) -> BenchResult<Vec<(Vector, usize)>> {
    inputs
        .iter()
        .map(|(vector, len)| {
            let text = cast_vector(
                set,
                vector,
                &LogicalType::VARCHAR,
                *len,
                &mut CastParameters::new(),
            )?;
            Ok((text, *len))
        })
        .collect()
}

/// `(year, month, day)` of a day number relative to 1970-01-01 (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn date_texts() -> Vec<(Vector, usize)> {
    chunked(&LogicalType::VARCHAR, |vector, row, i| {
        let (year, month, day) = civil_from_days((i % 100_000) as i64);
        let text = format!("{year:04}-{month:02}-{day:02}");
        let string = vector.add_string(text.as_bytes());
        vector.values_mut()[row] = string;
    })
}

fn timestamps() -> Vec<(Vector, usize)> {
    chunked(&LogicalType::TIMESTAMP, |vector, row, i| {
        vector.values_mut::<i64>()[row] = i as i64 * 7919 * 1000;
    })
}

/// Casts every chunk once; `Err` if the cast fails (e.g. is not implemented yet).
fn run_once(set: &CastFunctionSet, inputs: &[(Vector, usize)], target: &LogicalType) -> BenchResult<Timing> {
    let Some((first, _)) = inputs.first() else {
        return Err("no input".into());
    };
    let cast = set.get_cast_function(first.logical_type(), target)?;
    let mut result = Vector::flat(target.clone(), STANDARD_VECTOR_SIZE);
    let mut params = CastParameters::new();
    let mut converted = 0usize;
    let cpu_start = thread_cpu_time();
    let start = Instant::now();
    for (vector, len) in inputs {
        cast.execute(vector, &mut result, *len, &mut params)?;
        converted += if result.validity().is_all_valid() {
            *len
        } else {
            (0..*len).filter(|&row| !result.is_null(row)).count()
        };
        black_box(&result);
    }
    let wall = start.elapsed();
    let cpu = thread_cpu_time()
        .zip(cpu_start)
        .map(|(end, start)| end.saturating_sub(start));
    if converted != ROWS {
        return Err(format!("only {converted} of {ROWS} rows converted").into());
    }
    Ok(Timing { wall, cpu })
}

/// Runs a benchmark twice and keeps the second timing.
fn measure(
    set: &CastFunctionSet,
    inputs: &[(Vector, usize)],
    target: &LogicalType,
) -> Result<Timing, String> {
    run_once(set, inputs, target).map_err(|e| e.to_string())?;
    run_once(set, inputs, target).map_err(|e| e.to_string())
}

type InputFn = fn(&CastFunctionSet) -> BenchResult<Vec<(Vector, usize)>>;

/// `(name, input generator, target)`; names match the labels in `casts_duckdb.sql`.
fn benchmarks() -> Vec<(&'static str, InputFn, LogicalType)> {
    vec![
        (
            "varchar_to_integer",
            |set| as_text(set, &integers()),
            LogicalType::INTEGER,
        ),
        (
            "varchar_to_double",
            |set| as_text(set, &doubles()),
            LogicalType::DOUBLE,
        ),
        ("integer_to_varchar", |_| Ok(integers()), LogicalType::VARCHAR),
        ("double_to_varchar", |_| Ok(doubles()), LogicalType::VARCHAR),
        ("varchar_to_date", |_| Ok(date_texts()), LogicalType::DATE),
        ("timestamp_to_varchar", |_| Ok(timestamps()), LogicalType::VARCHAR),
    ]
}

/// Runs `casts_duckdb.sql` in the DuckDB CLI; returns the second timing per benchmark.
fn duckdb_timings(cli: &Path) -> BenchResult<Vec<(String, Timing)>> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/casts_duckdb.sql");
    let output = Command::new(cli)
        .args(["-list", "-noheader"])
        .stdin(std::fs::File::open(script)?)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut timings: Vec<(String, Vec<Timing>)> = Vec::new();
    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("bench ") {
            timings.push((name.to_owned(), Vec::new()));
        } else if let Some(fields) = line.strip_prefix("Run Time (s): ")
            && let Some((_, runs)) = timings.last_mut()
        {
            // `real 0.123 user 0.120000 sys 0.001000`
            let numbers: Vec<f64> = fields
                .split_whitespace()
                .skip(1)
                .step_by(2)
                .filter_map(|n| n.parse().ok())
                .collect();
            if let [real, user, sys] = numbers[..] {
                runs.push(Timing {
                    wall: Duration::from_secs_f64(real),
                    cpu: Some(Duration::from_secs_f64(user + sys)),
                });
            }
        }
    }
    if !output.status.success() && timings.is_empty() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(timings
        .into_iter()
        .filter_map(|(name, runs)| Some((name, *runs.last()?)))
        .collect())
}

fn millis(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64() * 1000.0)
}

fn format_timing(timing: Option<Timing>) -> (String, String) {
    timing.map_or_else(
        || ("-".to_owned(), "-".to_owned()),
        |t| (millis(t.wall), t.cpu.map_or_else(|| "-".to_owned(), millis)),
    )
}

fn ratio(rust: Option<Duration>, duckdb: Option<Duration>) -> String {
    match (rust, duckdb) {
        (Some(rust), Some(duckdb)) if !rust.is_zero() => {
            format!("{:.2}x", duckdb.as_secs_f64() / rust.as_secs_f64())
        }
        _ => "-".to_owned(),
    }
}

fn main() -> BenchResult<()> {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    let selected = |name: &str| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str()));
    let set = CastFunctionSet::new();
    let mut rust = Vec::new();
    for (name, inputs, target) in benchmarks() {
        if !selected(name) {
            continue;
        }
        let inputs = inputs(&set)?;
        let timing = measure(&set, &inputs, &target);
        match &timing {
            Ok(t) => println!(
                "rustdb {name:<22} wall {:>8} ms  cpu {:>8} ms",
                millis(t.wall),
                t.cpu.map_or_else(|| "-".to_owned(), millis)
            ),
            Err(error) => println!("rustdb {name:<22} skipped: {error}"),
        }
        rust.push((name, timing.ok()));
    }
    let cli = std::env::var_os("DUCKDB_ORACLE").map_or_else(|| PathBuf::from(DEFAULT_DUCKDB), PathBuf::from);
    let duckdb = if std::env::var("CASTS_BENCH_DUCKDB").is_ok_and(|v| v == "0") || !cli.exists() {
        println!("DuckDB CLI not run (set DUCKDB_ORACLE, or unset CASTS_BENCH_DUCKDB=0)");
        Vec::new()
    } else {
        duckdb_timings(&cli).unwrap_or_else(|error| {
            println!("DuckDB CLI failed: {error}");
            Vec::new()
        })
    };
    println!(
        "\n{:<22} {:>10} {:>10} {:>10} {:>10} {:>9} {:>9}",
        "10M values, 1 thread", "rust wall", "rust cpu", "duck wall", "duck cpu", "wall x", "cpu x"
    );
    for (name, timing) in rust {
        let theirs = duckdb.iter().find(|(n, _)| n == name).map(|(_, t)| *t);
        let (rust_wall, rust_cpu) = format_timing(timing);
        let (duck_wall, duck_cpu) = format_timing(theirs);
        println!(
            "{name:<22} {rust_wall:>10} {rust_cpu:>10} {duck_wall:>10} {duck_cpu:>10} {:>9} {:>9}",
            ratio(timing.map(|t| t.wall), theirs.map(|t| t.wall)),
            ratio(timing.and_then(|t| t.cpu), theirs.and_then(|t| t.cpu)),
        );
    }
    println!("(times in ms; x = DuckDB time / rustdb time, above 1 means rustdb is faster)");
    Ok(())
}
