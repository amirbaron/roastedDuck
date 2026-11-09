//! roastedduck's benchmark harness (milestone M0).
//!
//! Runs DuckDB's own benchmark suites (`benchmark/**/*.benchmark`) on C++ DuckDB 1.5.6
//! and on roastedduck, on the same machine and with the same settings, and compares them:
//!
//! - [`format`]: parser for DuckDB's interpreted `.benchmark` format (a port of
//!   `interpreted_benchmark.cpp`), resolving templates, includes and arguments.
//! - [`engine`]: the engines behind one interface — C++ DuckDB through `libduckdb`'s C
//!   API (loaded at run time) and roastedduck (linked).
//! - [`driver`]: executes one benchmark in its own process, following DuckDB's runner:
//!   initialize (with caches), assert, warm-up and timed runs of the `run` section,
//!   result checks, cleanup.
//! - [`runner`] and [`measure`]: the harness side — one process per benchmark, peak RSS
//!   from `wait4`, time budgets — producing [`record`]s (JSON lines).
//! - [`report`] and [`ratchet`]: `bench compare` (speedups, memory ratios, the plan's
//!   gates) and the perf ratchet in `bench/baseline.json`.
//! - [`data`]: local mirrors of remote benchmark data.

pub mod data;
pub mod discover;
pub mod driver;
pub mod engine;
pub mod format;
pub mod machine;
pub mod measure;
pub mod ratchet;
pub mod record;
pub mod report;
pub mod runner;
pub mod verify;
