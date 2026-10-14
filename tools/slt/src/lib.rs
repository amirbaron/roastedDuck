//! A port of DuckDB's sqllogictest runner (`test/sqlite/`) that drives both the Rust engine
//! (`rustdb`) and the C++ DuckDB 1.5.6 oracle, plus the tooling to run the corpus at scale.
//!
//! - [`parser`]: line-level record parsing (`sqllogic_parser.cpp`).
//! - [`commands`]: executable records (`sqllogic_command.hpp`).
//! - [`runner`]: the file runner (`sqllogic_test_runner.cpp`, `result_helper.cpp`).
//! - [`compare`]: result comparison helpers, including DuckDB's numeric semantics for Rust.
//! - [`backend`]: the [`backend::Engine`] trait with the Rust and C++ implementations.
//! - [`harness`]: parallel, crash-isolated runs; [`report`] and [`ratchet`] consume the results.

pub mod backend;
pub mod commands;
pub mod compare;
pub mod config;
pub mod harness;
pub mod outcome;
pub mod parser;
pub mod ratchet;
pub mod report;
pub mod result_file;
pub mod runner;
mod sys;
mod text;
