//! `checkpoint` and `force_checkpoint` (DuckDB's `function/table/checkpoint.cpp`), the
//! table functions behind `CHECKPOINT [name]` / `FORCE CHECKPOINT [name]`. In-memory
//! databases have nothing to write, so a checkpoint only validates the database name.

use rustdb_common::{Error, Result};
use rustdb_function::{TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet};
use rustdb_types::{DataChunk, LogicalType};

/// `CheckpointBind`: one BOOLEAN column `Success`; a named database must exist.
fn checkpoint_bind(
    input: &TableFunctionBindInput<'_>,
    databases: &[String],
) -> Result<TableFunctionBindOutput> {
    if let Some(name) = input.inputs.first() {
        if name.is_null() {
            return Err(Error::binder("Database cannot be NULL"));
        }
        let name = name.to_varchar()?;
        if !databases
            .iter()
            .any(|database| database.eq_ignore_ascii_case(&name))
        {
            return Err(Error::binder(format!("Database \"{name}\" not found")));
        }
    }
    Ok(TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned()],
        bind_data: None,
    })
}

/// The `checkpoint` and `force_checkpoint` sets, for the attached `databases`.
pub(crate) fn checkpoint_functions(databases: &[String]) -> Vec<TableFunctionSet> {
    ["checkpoint", "force_checkpoint"]
        .into_iter()
        .map(|name| {
            let mut set = TableFunctionSet::new(name);
            for arguments in [Vec::new(), vec![LogicalType::VARCHAR]] {
                let databases = databases.to_vec();
                set.add(
                    TableFunction::new(name, arguments)
                        .with_bind(move |input| checkpoint_bind(input, &databases))
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
