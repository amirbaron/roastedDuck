//! `COPY ... TO ... (FORMAT BLOB)` (DuckDB's `copy_blob.cpp`): the bytes of a single
//! BLOB column written back to back into one file.

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_function::FunctionData;
use rustdb_function::copy::{CopyFunction, CopyFunctionBindInput, CopyGlobalState};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value};

use crate::copy_csv::CsvSink;
use crate::options::FileCompressionType;

/// `WriteBlobBindData`.
#[derive(Debug)]
struct WriteBlobData {
    compression: FileCompressionType,
    /// The connection writing, whose loggers see the file operations.
    client: Option<Arc<rustdb_function::FunctionClient>>,
}

impl FunctionData for WriteBlobData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `ParseStringOption`: a string, or a list of exactly one string.
fn parse_string_option(value: &Value, option: &str) -> Result<String> {
    if value.is_null() {
        return Ok(String::new());
    }
    if value.logical_type().id() == LogicalTypeId::List {
        return match value.children().unwrap_or_default() {
            [single] => parse_string_option(single, option),
            _ => Err(Error::binder(format!(
                "\"{option}\" expects a single argument as a string value"
            ))),
        };
    }
    if value.logical_type().id() != LogicalTypeId::Varchar {
        return Err(Error::binder(format!("\"{option}\" expects a string argument!")));
    }
    value.to_varchar()
}

/// `WriteBlobBind`.
fn write_blob_bind(
    input: &mut CopyFunctionBindInput<'_>,
    _: &[String],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    if !matches!(types, [ty] if ty.id() == LogicalTypeId::Blob) {
        return Err(Error::binder(
            "\"COPY (FORMAT BLOB)\" only supports a single BLOB column",
        ));
    }
    let mut compression = FileCompressionType::AutoDetect;
    for (name, values) in input.options.iter() {
        if name.eq_ignore_ascii_case("compression") {
            let value = values.first().cloned().unwrap_or_default();
            compression = FileCompressionType::from_string(&parse_string_option(&value, name)?)?;
        } else {
            return Err(Error::binder(format!(
                "Unrecognized option for COPY (FORMAT BLOB): \"{name}\""
            )));
        }
    }
    Ok(Arc::new(WriteBlobData {
        compression,
        client: rustdb_function::file_system::opener(),
    }))
}

/// `WriteBlobGlobalState`: the open file.
struct WriteBlobGlobal {
    sink: Mutex<CsvSink>,
    path: String,
}

fn global_state(state: &CopyGlobalState) -> Result<&WriteBlobGlobal> {
    state
        .downcast_ref::<WriteBlobGlobal>()
        .ok_or_else(|| Error::internal("blob write without its file"))
}

fn write_error(path: &str, error: &std::io::Error) -> Error {
    Error::new(
        rustdb_common::ErrorKind::Io,
        format!("Could not write file \"{path}\": {error}"),
    )
}

/// The `blob` COPY format (`BuiltinFunctions::RegisterCopyFunctions`).
pub fn blob_copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("blob");
    "blob".clone_into(&mut function.extension);
    function.copy_to_bind = Some(Arc::new(write_blob_bind));
    function.copy_to_initialize_global = Some(Arc::new(|bind_data, path| {
        let data = bind_data
            .as_any()
            .downcast_ref::<WriteBlobData>()
            .ok_or_else(|| Error::internal("blob write without bind data"))?;
        let sink = CsvSink::create(path, data.compression, data.client.as_deref())?;
        Ok(Box::new(WriteBlobGlobal {
            sink: Mutex::new(sink),
            path: path.to_owned(),
        }) as CopyGlobalState)
    }));
    // `WriteBlobSink`: every valid blob, in order.
    function.copy_to_sink = Some(Arc::new(|_, state, chunk: &DataChunk| {
        let global = global_state(state)?;
        let mut sink = global.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let column = chunk.column(0);
        for row in 0..chunk.len() {
            let value = column.get_value(row);
            if let Some(bytes) = value.as_bytes().filter(|_| !value.is_null()) {
                sink.write(bytes).map_err(|error| write_error(&global.path, &error))?;
            }
        }
        Ok(())
    }));
    function.copy_to_finalize = Some(Arc::new(|_, state| {
        let global = global_state(state)?;
        let mut sink = global.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.close().map_err(|error| write_error(&global.path, &error))
    }));
    function
}
