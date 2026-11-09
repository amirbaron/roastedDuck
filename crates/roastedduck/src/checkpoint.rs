//! `checkpoint` and `force_checkpoint` (DuckDB's `function/table/checkpoint.cpp`), the
//! table functions behind `CHECKPOINT [name]` / `FORCE CHECKPOINT [name]`. In-memory
//! databases have nothing to write. Execution requests the connection's checkpoint
//! action, including when the function belongs to a prepared or pending statement.

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{
    ClientRequest, FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet,
};
use roastedduck_types::{DataChunk, LogicalType};

#[derive(Debug)]
struct CheckpointData {
    database: Option<roastedduck_common::ByteString>,
}

impl FunctionData for CheckpointData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `CheckpointBind`: one BOOLEAN column `Success`; a named database must be attached.
fn checkpoint_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = input
        .binder
        .and_then(|binder| binder.downcast_ref::<roastedduck_planner::ClientContext>());
    let database = if let Some(name) = input.inputs.first() {
        if name.is_null() {
            return Err(Error::binder("Database cannot be NULL"));
        }
        let name = name.to_varchar_bytes()?;
        let attached = context.is_some_and(|context| context.catalog(&name).is_some());
        if !attached {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Database \"{name}\" not found"
            )));
        }
        Some(name)
    } else {
        // CheckpointBind selects the default attached database while binding.
        context.map(roastedduck_planner::ClientContext::default_catalog)
    };
    Ok(TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned().into()],
        bind_data: Some(Arc::new(CheckpointData { database })),
    })
}

/// The `checkpoint` and `force_checkpoint` sets.
pub(crate) fn checkpoint_functions() -> Vec<TableFunctionSet> {
    ["checkpoint", "force_checkpoint"]
        .into_iter()
        .map(|name| {
            let force = name == "force_checkpoint";
            let mut set = TableFunctionSet::new(name);
            for arguments in [Vec::new(), vec![LogicalType::VARCHAR]] {
                set.add(
                    TableFunction::new(name, arguments)
                        .with_bind(checkpoint_bind)
                        .with_scan(move |bind_data, _, state, chunk| {
                            if state.position == 0 {
                                let data = bind_data
                                    .and_then(|data| data.as_any().downcast_ref::<CheckpointData>())
                                    .ok_or_else(|| Error::internal("checkpoint scan without bind data"))?;
                                let client =
                                    roastedduck_function::file_system::opener().ok_or_else(|| {
                                        Error::internal("checkpoint scan without client context")
                                    })?;
                                client.push_client_request(ClientRequest::Checkpoint {
                                    database: data.database.clone(),
                                    force,
                                });
                                state.position = 1;
                            }
                            *chunk = DataChunk::with_types(&[LogicalType::BOOLEAN], 0);
                            Ok(false)
                        }),
                );
            }
            set
        })
        .collect()
}
