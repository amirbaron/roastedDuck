//! Lazy Arrow C streams owning their native result and connection state.
use crate::connection::{ResultCursor, lock};
use crate::{error, host};
use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyDict, PyTuple};
use rustdb_arrow::ffi::{ArrowArray, ArrowArrayStream, ArrowSchema};
use rustdb_common::{Error, Result};
use rustdb_types::DataChunk;
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::{Arc, Mutex};

pub(super) fn batch_arguments(
    owner: &Bound<'_, PyAny>,
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    method: &str,
    relation: bool,
) -> PyResult<usize> {
    let batch = if method == "fetch_record_batch" || (!relation && method == "arrow") {
        "rows_per_batch"
    } else {
        "batch_size"
    };
    let generic = relation && matches!(method, "fetch_record_batch" | "fetch_arrow_reader");
    let class = if generic {
        "object"
    } else if relation {
        "_duckdb.DuckDBPyRelation"
    } else {
        "_duckdb.DuckDBPyConnection"
    };
    let result = if generic {
        "object"
    } else {
        "pyarrow.lib.RecordBatchReader"
    };
    let signature =
        format!("(self: {class}, {batch}: typing.SupportsInt | typing.SupportsIndex = 1000000) -> {result}");
    let bad = || error::arguments(owner.py(), method, &signature, Some(owner), args, kwargs);
    if args.len() > 1 {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            if key.extract::<String>()? != batch {
                return Err(bad()?);
            }
        }
    }
    let keyword = kwargs.map(|kw| kw.get_item(batch)).transpose()?.flatten();
    if !args.is_empty() && keyword.is_some() {
        return Err(bad()?);
    }
    let value = if args.is_empty() {
        keyword
    } else {
        Some(args.get_item(0)?)
    };
    match value {
        Some(value) => match value.extract::<usize>() {
            Ok(value) => Ok(value),
            Err(_) => Err(bad()?),
        },
        None => Ok(1_000_000),
    }
}

struct ReaderData {
    connection: Arc<Mutex<rustdb::Connection>>,
    cursor: Option<ResultCursor>,
    metadata: host::Metadata,
    chunk: Option<DataChunk>,
    offset: usize,
    batch_size: usize,
    schema_read: bool,
    error_on_closed: bool,
    exhausted: bool,
    last_error: CString,
}
impl ReaderData {
    fn next(&mut self) -> Result<Option<ArrowArray>> {
        if self.exhausted {
            return Ok(None);
        }
        let mut connection = self
            .connection
            .try_lock()
            .map_err(|_| Error::invalid_input("Connection is already in use"))?;

        let Some(cursor) = &mut self.cursor else {
            return Ok(None);
        };
        if !cursor.is_open(&connection) {
            self.chunk = None;
            if self.error_on_closed {
                return Err(Error::new(
                    rustdb_common::ErrorKind::Invalid,
                    "Query Stream is closed",
                ));
            }
            return Ok(None);
        }
        let mut appender = rustdb_arrow::ArrowAppender::new(
            self.metadata.types.clone(),
            self.batch_size.min(2048),
            self.metadata.arrow.clone(),
        )?;
        let mut count = 0;
        while count < self.batch_size {
            if self.chunk.as_ref().is_none_or(|chunk| self.offset == chunk.len()) {
                self.chunk = cursor.bulk_next(&mut connection)?;
                self.offset = 0;
            }
            let Some(chunk) = &self.chunk else {
                self.exhausted = true;
                break;
            };
            let take = (self.batch_size - count).min(chunk.len() - self.offset);
            appender.append(chunk, self.offset, self.offset + take, chunk.len())?;
            self.offset += take;
            count += take;
        }
        if count == 0 {
            return Ok(None);
        }
        appender.finalize().map(Some)
    }
}
impl Drop for ReaderData {
    fn drop(&mut self) {
        if let Ok(mut connection) = self.connection.try_lock()
            && let Some(cursor) = self.cursor.take()
        {
            cursor.close(&mut connection);
        }
    }
}
#[repr(transparent)]
struct OwnedStream(ArrowArrayStream);
impl Drop for OwnedStream {
    fn drop(&mut self) {
        if let Some(release) = self.0.release {
            // SAFETY: this wrapper owns the stream until a consumer clears release.
            unsafe { release(&raw mut self.0) };
        }
    }
}
// SAFETY: the private data is a mutex of Send native values; Arrow calls serialize access.
unsafe impl Send for OwnedStream {}

unsafe fn data<'a>(stream: *mut ArrowArrayStream) -> &'a Mutex<ReaderData> {
    // SAFETY: live streams are created below with a boxed mutex as private_data.
    unsafe { &*((*stream).private_data.cast::<Mutex<ReaderData>>()) }
}
fn caught(action: impl FnOnce() -> Result<()>) -> Result<()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(action))
        .unwrap_or_else(|_| Err(Error::internal("Panic while fetching Python Arrow stream")))
}
unsafe extern "C" fn get_schema(stream: *mut ArrowArrayStream, out: *mut ArrowSchema) -> c_int {
    if stream.is_null() || out.is_null() {
        return 22;
    }
    // SAFETY: pointers were checked and the consumer provides writable output.
    unsafe {
        out.write(ArrowSchema::empty());
    }
    // SAFETY: the caller owns a live C stream.
    if unsafe { (*stream).release.is_none() } {
        return -1;
    }
    // SAFETY: as above.
    let mut value = lock(unsafe { data(stream) });
    let outcome = caught(|| {
        if !value.schema_read {
            let connection = value
                .connection
                .try_lock()
                .map_err(|_| Error::invalid_input("Connection is already in use"))?;
            let open = value
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.is_open(&connection));
            if !open {
                return Err(Error::invalid_input("Query Stream is closed"));
            }
        }
        // SAFETY: out is writable for the duration of this callback.
        rustdb_arrow::to_arrow_schema(
            unsafe { &mut *out },
            &value.metadata.types,
            &value.metadata.names,
            &value.metadata.arrow,
        )?;
        value.schema_read = true;
        Ok(())
    });
    finish(&mut value, outcome)
}
unsafe extern "C" fn get_next(stream: *mut ArrowArrayStream, out: *mut ArrowArray) -> c_int {
    if stream.is_null() || out.is_null() {
        return 22;
    }
    // SAFETY: the consumer provides writable output and a live stream.
    unsafe {
        out.write(ArrowArray::empty());
    }
    // SAFETY: pointer is non-null.
    if unsafe { (*stream).release.is_none() } {
        return -1;
    }
    // SAFETY: this stream was created below.
    let mut value = lock(unsafe { data(stream) });
    if !value.last_error.as_bytes().is_empty() {
        return -1;
    }
    let outcome = caught(|| {
        if let Some(array) = value.next()? {
            // SAFETY: transfer the owned Arrow array to the consumer.
            unsafe {
                out.write(array);
            }
        }
        Ok(())
    });
    let drain_references = value.exhausted || outcome.is_err();
    let status = finish(&mut value, outcome);
    drop(value);
    if drain_references {
        // Arrow can call us without a Python thread state. Query completion drops
        // producers there; flush PyO3's deferred decrefs after releasing our locks.
        let _ = Python::try_attach(|_| {});
    }
    status
}
fn finish(value: &mut ReaderData, outcome: Result<()>) -> c_int {
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            value.last_error = CString::new(error.to_bytes()).unwrap_or_default();
            -1
        }
    }
}
unsafe extern "C" fn last_error(stream: *mut ArrowArrayStream) -> *const c_char {
    if stream.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live stream's private data stays allocated until release.
    lock(unsafe { data(stream) }).last_error.as_ptr()
}
unsafe extern "C" fn release(stream: *mut ArrowArrayStream) {
    if stream.is_null() {
        return;
    }
    // SAFETY: the consumer releases a live stream once; clear its ownership first.
    unsafe {
        if (*stream).release.take().is_none() {
            return;
        }
        let pointer = std::mem::replace(&mut (*stream).private_data, std::ptr::null_mut());
        let _ = std::panic::catch_unwind(|| drop(Box::from_raw(pointer.cast::<Mutex<ReaderData>>())));
    }
    let _ = Python::try_attach(|_| {});
}

pub(super) fn export(
    py: Python<'_>,
    connection: Arc<Mutex<rustdb::Connection>>,
    cursor: ResultCursor,
    batch_size: usize,
    capsule: bool,
) -> PyResult<Py<PyAny>> {
    export_with_options(py, connection, cursor, batch_size, capsule, false, false)
}
pub(super) fn export_polars(
    py: Python<'_>,
    connection: Arc<Mutex<rustdb::Connection>>,
    cursor: ResultCursor,
) -> PyResult<Py<PyAny>> {
    export_with_options(py, connection, cursor, 1_000_000, false, false, true)
}
fn export_with_options(
    py: Python<'_>,
    connection: Arc<Mutex<rustdb::Connection>>,
    mut cursor: ResultCursor,
    batch_size: usize,
    capsule: bool,
    deduplicate: bool,
    error_on_closed: bool,
) -> PyResult<Py<PyAny>> {
    if batch_size == 0 {
        py.detach(|| cursor.close(&mut lock(&connection)));
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Approximate Batch Size of Record Batch MUST be higher than 0",
        ));
    }
    let mut metadata = cursor.begin_bulk();
    if deduplicate {
        metadata.names = host::names(&metadata.names);
    }
    if metadata.materialized {
        let table = host::arrow(py, &metadata, batch_size, || {
            py.detach(|| {
                let mut connection = lock(&connection);
                cursor.bulk_next(&mut connection)
            })
            .map_err(|e| error::native(py, &e))
        })?;
        if capsule {
            return Ok(table.bind(py).call_method0("__arrow_c_stream__")?.unbind());
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_chunksize", batch_size)?;
        return Ok(table
            .bind(py)
            .call_method("to_reader", (), Some(&kwargs))?
            .unbind());
    }
    let private = Box::new(Mutex::new(ReaderData {
        connection,
        cursor: Some(cursor),
        metadata,
        chunk: None,
        offset: 0,
        batch_size,
        schema_read: false,
        error_on_closed,
        exhausted: false,
        last_error: CString::default(),
    }));
    let mut stream = OwnedStream(ArrowArrayStream {
        get_schema: Some(get_schema),
        get_next: Some(get_next),
        get_last_error: Some(last_error),
        release: Some(release),
        private_data: Box::into_raw(private).cast::<c_void>(),
    });
    if capsule {
        return Ok(PyCapsule::new_with_value(py, stream, c"arrow_array_stream")?
            .unbind()
            .into_any());
    }
    Ok(py
        .import("pyarrow")?
        .getattr("RecordBatchReader")?
        .call_method1("_import_from_c", ((&raw mut stream.0) as usize,))?
        .unbind())
}
