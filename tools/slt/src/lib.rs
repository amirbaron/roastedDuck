//! A port of DuckDB's sqllogictest runner (`test/sqlite/`) that drives both the Rust engine
//! (`rustdb`) and the C++ DuckDB 1.5.6 oracle, plus the tooling to run the corpus at scale.
//!
//! - [`parser`]: line-level record parsing (`sqllogic_parser.cpp`).
//! - [`commands`]: executable records (`sqllogic_command.hpp`).
//! - [`runner`]: the file runner (`sqllogic_test_runner.cpp`, `result_helper.cpp`).
//! - [`compare`]: result comparison helpers, including DuckDB's numeric semantics for Rust.
//! - [`backend`]: the [`backend::Engine`] trait with the Rust and C++ implementations.
//! - [`harness`]: parallel, crash-isolated runs; [`report`] and [`ratchet`] consume the results.
//!
//! Typical use (`$DUCKDB_REF` is the DuckDB 1.5.6 source tree with a release build):
//!
//! ```text
//! tools/oracle-shim/build.sh $DUCKDB_REF                  # target/oracle-shim/libslt_oracle.*
//! slt run --engine cpp  --out cpp.jsonl  $DUCKDB_REF/test # C++ oracle
//! slt run --engine rust --out rust.jsonl $DUCKDB_REF/test # rustdb
//! slt run --engine unittest --out ref.jsonl $DUCKDB_REF/test   # DuckDB's own runner
//! slt diff ref.jsonl cpp.jsonl                            # harness validation
//! slt compare cpp.jsonl rust.jsonl --out dashboard.md
//! slt ratchet check rust.jsonl                            # conformance/passing.txt
//! slt exec --engine cpp test/sql/select/test_select.test  # one file, in-process
//! ```

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
