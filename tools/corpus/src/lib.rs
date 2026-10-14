//! DuckDB's sqllogictest corpus as a source of SQL statements, and the reference CLI as
//! an oracle for them.
//!
//! [`slt`] extracts the SQL of every `statement` and `query` record; [`oracle`] runs the
//! C++ CLI over many statements at once. The `corpus` binary wires these into checks of
//! rustdb against DuckDB.

pub mod oracle;
pub mod slt;
pub mod wal;
