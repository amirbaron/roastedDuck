//! SQL surface for roastedDuck's compatible implementation of VSS.
//!
//! Pinned source: duckdb-vss b833341c8737fd3f3558c7720cc575ae8fc82598,
//! as selected by DuckDB 1.5.6 (plus its `ResetStorage` patch). The table macro
//! bodies are copied verbatim from `src/hnsw/hnsw_index_macros.cpp`.
//! Copyright 2018-2025 Stichting DuckDB Foundation; MIT license in `LICENSE`.
//!
//! This module does not provide a placeholder index or scan. Index/optimizer
//! integration registers these helpers when the actual VSS implementation loads.

mod catalog;
mod join;
mod macros;
mod metadata;
mod optimizer;
mod scan;
mod search;

#[cfg(test)]
mod tests;

pub(crate) use catalog::execute_compact;
pub(crate) use metadata::{compact_pragma, index_info_schema, options};
pub(crate) use optimizer::optimizer_rules;

pub(crate) const EXTENSION_NAME: &str = "vss";

pub(crate) fn register_catalog(system: &roastedduck_catalog::Catalog) -> roastedduck_common::Result<()> {
    macros::register_macros(system)?;
    system.register_table_functions(catalog::index_info_function())?;
    let mut scan = roastedduck_function::TableFunctionSet::new("hnsw_index_scan");
    scan.add(scan::scan_function());
    system.register_table_functions(scan)?;
    Ok(())
}
