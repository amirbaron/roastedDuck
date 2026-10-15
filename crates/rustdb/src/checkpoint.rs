//! `checkpoint` and `force_checkpoint` (DuckDB's `function/table/checkpoint.cpp`), the
//! table functions behind `CHECKPOINT [name]` / `FORCE CHECKPOINT [name]`. In-memory
//! databases have nothing to write, so a checkpoint only validates the database name.

use rustdb_common::{Error, Result};
use rustdb_function::{TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet};
use rustdb_types::{DataChunk, LogicalType};

/// `CheckpointBind`: one BOOLEAN column `Success`; a named database must be attached.
fn checkpoint_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    if let Some(name) = input.inputs.first() {
        if name.is_null() {
            return Err(Error::binder("Database cannot be NULL"));
        }
        let name = name.to_varchar()?;
        let attached = input
            .binder
            .and_then(|binder| binder.downcast_ref::<rustdb_planner::ClientContext>())
            .is_some_and(|context| context.catalog(&name).is_some());
        if !attached {
            return Err(Error::binder(format!("Database \"{name}\" not found")));
        }
    }
    Ok(TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned()],
        bind_data: None,
    })
}

/// The `checkpoint` and `force_checkpoint` sets.
pub(crate) fn checkpoint_functions() -> Vec<TableFunctionSet> {
    ["checkpoint", "force_checkpoint"]
        .into_iter()
        .map(|name| {
            let mut set = TableFunctionSet::new(name);
            for arguments in [Vec::new(), vec![LogicalType::VARCHAR]] {
                set.add(
                    TableFunction::new(name, arguments)
                        .with_bind(checkpoint_bind)
                        .with_scan(|_, _, _, chunk| {
                            *chunk = DataChunk::with_types(&[LogicalType::BOOLEAN], 0);
                            Ok(false)
                        }),
                );
            }
            set
        })
        .collect()
}
