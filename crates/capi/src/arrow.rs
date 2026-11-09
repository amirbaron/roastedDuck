//! The Arrow functions of the C API (DuckDB's `capi/arrow-c.cpp`, with the Arrow
//! options functions of `duckdb-c.cpp` and `result-c.cpp`): results and data chunks as
//! Arrow schemas and arrays, Arrow arrays as data chunks, and views over Arrow streams.
//!
//! The structs are those of the Arrow C data interface ([`roastedduck_arrow::ffi`]); the
//! conversions are [`roastedduck_arrow`]'s.

use std::ffi::{c_char, c_int};

use roastedduck::{MaterializedResult, QueryResult, StatementReturnType};
use roastedduck_arrow::ffi::{ArrowArray, ArrowArrayStream, ArrowArrayStreamWrapper, ArrowSchema};
use roastedduck_arrow::{
    ArrowStreamParameters, ArrowTableSchema, ClientProperties, StreamFactoryGetSchema, StreamFactoryProduce,
};
use roastedduck_ast::visit::{VisitorMut, walk_expression};
use roastedduck_ast::{ParsedExpression, ParsedExpressionKind};
use roastedduck_common::Error;
use roastedduck_types::{DataChunk, LogicalType, Value};

use crate::data_chunk::{DuckdbDataChunk, wrap_chunk};
use crate::error_data::{DuckdbErrorData, ErrorDataWrapper};
use crate::helper::{catch, lossy_str, opt_str};
use crate::logical_types::{DuckdbLogicalType, type_ref};
use crate::open::{DuckdbConnection, connection_ref};
use crate::prepared::{CError, DuckdbPreparedStatement};
use crate::result::{pack_chunks, result_data};
use crate::types::*;

/// What a `duckdb_arrow_options` points to (DuckDB's `CClientArrowOptionsWrapper`).
#[derive(Debug)]
pub struct ArrowOptionsWrapper {
    properties: ClientProperties,
}

/// `duckdb_arrow_options`
pub type DuckdbArrowOptions = *mut ArrowOptionsWrapper;

/// What a `duckdb_arrow` points to (DuckDB's `ArrowResultWrapper`): a materialized
/// result, read chunk by chunk.
#[derive(Debug)]
pub struct ArrowResultWrapper {
    /// The result, or why the query failed.
    result: Result<MaterializedResult, CError>,
    /// The properties the result is converted with (`client_properties`).
    properties: ClientProperties,
    /// The rows in chunks of `STANDARD_VECTOR_SIZE` rows, as DuckDB's collection holds
    /// them.
    chunks: Vec<DataChunk>,
    /// The next chunk to fetch.
    next_chunk: usize,
}

impl ArrowResultWrapper {
    fn new(outcome: roastedduck::Result<MaterializedResult>, properties: ClientProperties) -> Self {
        let chunks = match &outcome {
            Ok(result) => pack_chunks(result.types(), &crate::result::materialized_chunks(result)),
            Err(_) => Vec::new(),
        };
        Self {
            result: outcome.map_err(CError::new),
            properties,
            chunks,
            next_chunk: 0,
        }
    }
}

/// `duckdb_arrow`
pub type DuckdbArrow = *mut ArrowResultWrapper;
/// `duckdb_arrow_schema`
pub type DuckdbArrowSchema = *mut ArrowSchema;
/// `duckdb_arrow_array`
pub type DuckdbArrowArray = *mut ArrowArray;
/// `duckdb_arrow_stream`
pub type DuckdbArrowStream = *mut ArrowArrayStream;
/// `duckdb_arrow_converted_schema`
pub type DuckdbArrowConvertedSchema = *mut ArrowTableSchema;

/// An `INVALID_INPUT` error object.
fn invalid_input(message: &str) -> DuckdbErrorData {
    ErrorDataWrapper::new(Some(Error::invalid_input(message.to_owned()))).into_handle()
}

/// The error object of a failed conversion: the error as `std::exception::what()` gives
/// it, a JSON object, reported as `INVALID_INPUT`.
fn conversion_error(error: Error) -> DuckdbErrorData {
    ErrorDataWrapper::new(Some(Error::new_bytes(
        roastedduck_common::ErrorKind::InvalidInput,
        error.into_json().raw_message_bytes().to_vec(),
    )))
    .into_handle()
}

/// `duckdb_error_data duckdb_to_arrow_schema(duckdb_arrow_options arrow_options,
/// duckdb_logical_type *types, const char **names, idx_t column_count, struct ArrowSchema
/// *out_schema)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_to_arrow_schema(
    arrow_options: DuckdbArrowOptions,
    types: *mut DuckdbLogicalType,
    names: *const *const c_char,
    column_count: Idx,
    out_schema: *mut ArrowSchema,
) -> DuckdbErrorData {
    const INVALID: &str = "Invalid argument(s) to duckdb_to_arrow_schema";
    // SAFETY: `arrow_options` is NULL or a live handle; `out_schema` NULL or writable.
    let (Some(options), Some(out_schema)) =
        (unsafe { arrow_options.as_ref() }, unsafe { out_schema.as_mut() })
    else {
        return invalid_input(INVALID);
    };
    // `types` and `names` may be NULL without columns.
    if column_count > 0 && (types.is_null() || names.is_null()) {
        return invalid_input(INVALID);
    }
    let mut schema_types = Vec::with_capacity(column_count as usize);
    let mut schema_names = Vec::with_capacity(column_count as usize);
    for index in 0..column_count as usize {
        // SAFETY: `types` and `names` hold `column_count` entries (API contract).
        let (ty, name) = unsafe { (type_ref(*types.add(index)), *names.add(index)) };
        let Some(ty) = ty else {
            return invalid_input(INVALID);
        };
        schema_types.push(ty.clone());
        // SAFETY: as above, each name is a C string.
        schema_names.push(roastedduck_common::ByteString::from(
            unsafe { crate::helper::opt_bytes(name) }.unwrap_or_default(),
        ));
    }
    match catch(|| {
        roastedduck_arrow::to_arrow_schema(out_schema, &schema_types, &schema_names, &options.properties)
    }) {
        Ok(()) => std::ptr::null_mut(),
        Err(error) => conversion_error(error),
    }
}

/// `duckdb_error_data duckdb_data_chunk_to_arrow(duckdb_arrow_options arrow_options,
/// duckdb_data_chunk chunk, struct ArrowArray *out_arrow_array)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_to_arrow(
    arrow_options: DuckdbArrowOptions,
    chunk: DuckdbDataChunk,
    out_arrow_array: *mut ArrowArray,
) -> DuckdbErrorData {
    // SAFETY: the handles are NULL or live; `out_arrow_array` is NULL or writable.
    let (Some(options), Some(chunk), Some(out)) =
        (unsafe { (arrow_options.as_ref(), chunk.as_ref(), out_arrow_array.as_mut()) })
    else {
        return invalid_input("Invalid argument(s) to duckdb_data_chunk_to_arrow");
    };
    match catch(|| roastedduck_arrow::to_arrow_array(chunk, &options.properties)) {
        Ok(array) => {
            *out = array;
            std::ptr::null_mut()
        }
        Err(error) => conversion_error(error),
    }
}

/// `duckdb_error_data duckdb_schema_from_arrow(duckdb_connection connection, struct
/// ArrowSchema *schema, duckdb_arrow_converted_schema *out_types)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_schema_from_arrow(
    connection: DuckdbConnection,
    schema: *mut ArrowSchema,
    out_types: *mut DuckdbArrowConvertedSchema,
) -> DuckdbErrorData {
    // SAFETY: `connection` is NULL or live; `schema` NULL or an Arrow schema.
    let (Some(_), Some(schema)) = (unsafe { connection_ref(connection) }, unsafe { schema.as_ref() }) else {
        return invalid_input("Invalid argument(s) to duckdb_data_chunk_to_arrow");
    };
    if out_types.is_null() {
        return invalid_input("Invalid argument(s) to duckdb_data_chunk_to_arrow");
    }
    let mut arrow_table = ArrowTableSchema::default();
    // SAFETY: `schema` is a struct schema of the C data interface (API contract).
    if let Err(error) =
        catch(|| unsafe { roastedduck_arrow::populate_arrow_table_schema(&mut arrow_table, schema) })
    {
        return conversion_error(error);
    }
    // SAFETY: checked non-null above.
    unsafe { *out_types = Box::into_raw(Box::new(arrow_table)) };
    std::ptr::null_mut()
}

/// `duckdb_error_data duckdb_data_chunk_from_arrow(duckdb_connection connection, struct
/// ArrowArray *arrow_array, duckdb_arrow_converted_schema converted_schema,
/// duckdb_data_chunk *out_chunk)` (capi/arrow-c.cpp): the chunk takes ownership of the
/// array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_from_arrow(
    connection: DuckdbConnection,
    arrow_array: *mut ArrowArray,
    converted_schema: DuckdbArrowConvertedSchema,
    out_chunk: *mut DuckdbDataChunk,
) -> DuckdbErrorData {
    // SAFETY: the handles are NULL or live; `arrow_array` is NULL or an Arrow array.
    let (Some(_), Some(arrow_array), Some(arrow_table)) = (unsafe {
        (
            connection_ref(connection),
            arrow_array.as_mut(),
            converted_schema.as_ref(),
        )
    }) else {
        return invalid_input("Invalid argument(s) to duckdb_data_chunk_to_arrow");
    };
    if out_chunk.is_null() {
        return invalid_input("Invalid argument(s) to duckdb_data_chunk_to_arrow");
    }
    // SAFETY: the array has the schema it was converted from (API contract).
    match catch(|| unsafe { roastedduck_arrow::data_chunk_from_arrow(arrow_array, arrow_table) }) {
        Ok(chunk) => {
            // SAFETY: checked non-null above.
            unsafe { *out_chunk = wrap_chunk(chunk) };
            std::ptr::null_mut()
        }
        Err(error) => conversion_error(error),
    }
}

/// `void duckdb_destroy_arrow_converted_schema(duckdb_arrow_converted_schema
/// *arrow_converted_schema)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_arrow_converted_schema(
    arrow_converted_schema: *mut DuckdbArrowConvertedSchema,
) {
    // SAFETY: NULL or points to NULL or a handle from `duckdb_schema_from_arrow`.
    unsafe {
        let Some(slot) = arrow_converted_schema.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_connection_get_arrow_options(duckdb_connection connection,
/// duckdb_arrow_options *out_arrow_options)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_connection_get_arrow_options(
    connection: DuckdbConnection,
    out_arrow_options: *mut DuckdbArrowOptions,
) {
    // SAFETY: `connection` is NULL or live; `out_arrow_options` NULL or writable.
    let (Some(context), Some(out)) = (unsafe { connection_ref(connection) }, unsafe {
        out_arrow_options.as_mut()
    }) else {
        return;
    };
    let properties = context.lock().client_properties();
    *out = Box::into_raw(Box::new(ArrowOptionsWrapper { properties }));
}

/// `duckdb_arrow_options duckdb_result_get_arrow_options(duckdb_result *result)`
/// (capi/result-c.cpp): the conversion options captured when the result was produced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_get_arrow_options(result: *mut DuckdbResult) -> DuckdbArrowOptions {
    // SAFETY: `result` is NULL or a result filled in by this library.
    let Some(data) = (unsafe { result_data(result) }) else {
        return std::ptr::null_mut();
    };
    let properties = data.properties.clone();
    Box::into_raw(Box::new(ArrowOptionsWrapper { properties }))
}

/// `void duckdb_destroy_arrow_options(duckdb_arrow_options *arrow_options)`
/// (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_arrow_options(arrow_options: *mut DuckdbArrowOptions) {
    // SAFETY: NULL or points to NULL or a handle from this module.
    unsafe {
        let Some(slot) = arrow_options.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_query_arrow(duckdb_connection connection, const char *query,
/// duckdb_arrow *out_result)` (capi/arrow-c.cpp): the result is set even if the query
/// fails.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query_arrow(
    connection: DuckdbConnection,
    query: *const c_char,
    out_result: *mut DuckdbArrow,
) -> DuckdbState {
    // SAFETY: `connection` is NULL or live; `query` NULL or a C string.
    let (Some(context), Some(sql)) = (unsafe { connection_ref(connection) }, unsafe { opt_str(query) })
    else {
        return DuckdbState::DuckDBError;
    };
    if out_result.is_null() {
        return DuckdbState::DuckDBError;
    }
    let (outcome, properties) = {
        let mut connection = context.lock();
        let outcome = catch(|| connection.query(sql));
        let properties = if outcome.is_ok() {
            connection.client_properties()
        } else {
            ClientProperties::default()
        };
        (outcome, properties)
    };
    let wrapper = ArrowResultWrapper::new(outcome, properties);
    let state = DuckdbState::from_ok(wrapper.result.is_ok());
    // SAFETY: checked non-null above.
    unsafe { *out_result = Box::into_raw(Box::new(wrapper)) };
    state
}

/// `duckdb_state duckdb_query_arrow_schema(duckdb_arrow result, duckdb_arrow_schema
/// *out_schema)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query_arrow_schema(
    result: DuckdbArrow,
    out_schema: *mut DuckdbArrowSchema,
) -> DuckdbState {
    if out_schema.is_null() {
        return DuckdbState::DuckDBSuccess;
    }
    // SAFETY: `result` is NULL or live; `out_schema` points to NULL or an Arrow schema.
    let (Some(wrapper), Some(schema)) = (unsafe { result.as_ref() }, unsafe { (*out_schema).as_mut() })
    else {
        return DuckdbState::DuckDBError;
    };
    let (types, names) = match &wrapper.result {
        Ok(result) => (result.types(), result.names()),
        Err(_) => (&[][..], &[][..]),
    };
    let converted = catch(|| roastedduck_arrow::to_arrow_schema(schema, types, names, &wrapper.properties));
    DuckdbState::from_ok(converted.is_ok())
}

/// `duckdb_state duckdb_prepared_arrow_schema(duckdb_prepared_statement prepared,
/// duckdb_arrow_schema *out_schema)` (capi/arrow-c.cpp): the parameters, named by
/// position and typed NULL (as ADBC's `AdbcStatementGetParameterSchema` asks).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_arrow_schema(
    prepared: DuckdbPreparedStatement,
    out_schema: *mut DuckdbArrowSchema,
) -> DuckdbState {
    if out_schema.is_null() {
        return DuckdbState::DuckDBSuccess;
    }
    // SAFETY: `prepared` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    let Ok(statement) = &wrapper.statement else {
        return DuckdbState::DuckDBError;
    };
    let properties = wrapper.context.lock().client_properties();
    let count = statement.parameter_count();
    let types = vec![LogicalType::SQLNULL; count];
    let names: Vec<roastedduck_common::ByteString> =
        (0..count).map(|index| index.to_string().into()).collect();
    // SAFETY: `out_schema` points to NULL or an Arrow schema.
    let Some(schema) = (unsafe { (*out_schema).as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    // An existing schema is released before it is overwritten.
    if let Some(release) = schema.release {
        // SAFETY: the caller's schema is live and owned by it.
        unsafe { release(schema) };
    }
    let converted = catch(|| roastedduck_arrow::to_arrow_schema(schema, &types, &names, &properties));
    DuckdbState::from_ok(converted.is_ok())
}

/// `duckdb_state duckdb_query_arrow_array(duckdb_arrow result, duckdb_arrow_array
/// *out_array)` (capi/arrow-c.cpp): the next chunk of the result; nothing is written
/// once the result is exhausted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query_arrow_array(
    result: DuckdbArrow,
    out_array: *mut DuckdbArrowArray,
) -> DuckdbState {
    if out_array.is_null() {
        return DuckdbState::DuckDBSuccess;
    }
    // SAFETY: `result` is NULL or a live handle.
    let Some(wrapper) = (unsafe { result.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    // Fetching from a failed result fails (`TryFetch`).
    if wrapper.result.is_err() {
        return DuckdbState::DuckDBError;
    }
    let Some(chunk) = wrapper.chunks.get(wrapper.next_chunk) else {
        return DuckdbState::DuckDBSuccess;
    };
    wrapper.next_chunk += 1;
    if chunk.is_empty() {
        return DuckdbState::DuckDBSuccess;
    }
    // SAFETY: `out_array` points to NULL or writable Arrow array storage.
    let Some(target) = (unsafe { (*out_array).as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    match catch(|| roastedduck_arrow::to_arrow_array(chunk, &wrapper.properties)) {
        Ok(array) => {
            *target = array;
            DuckdbState::DuckDBSuccess
        }
        Err(_) => DuckdbState::DuckDBError,
    }
}

/// `void duckdb_result_arrow_array(duckdb_result result, duckdb_data_chunk chunk,
/// duckdb_arrow_array *out_array)` (capi/arrow-c.cpp): a chunk of the result as an Arrow
/// array, converted with the options captured in the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_arrow_array(
    result: DuckdbResult,
    chunk: DuckdbDataChunk,
    out_array: *mut DuckdbArrowArray,
) {
    if out_array.is_null() {
        return;
    }
    // SAFETY: `result` was filled in by this library; `chunk` is NULL or live;
    // `out_array` points to NULL or writable Arrow array storage.
    let (Some(data), Some(chunk), Some(target)) = (unsafe {
        (
            result_data(&raw const result),
            chunk.as_ref(),
            (*out_array).as_mut(),
        )
    }) else {
        return;
    };
    // DuckDB lets a failed conversion escape; the array is then left alone.
    if let Ok(array) = catch(|| roastedduck_arrow::to_arrow_array(chunk, &data.properties)) {
        *target = array;
    }
}

/// `idx_t duckdb_arrow_row_count(duckdb_arrow result)` (capi/arrow-c.cpp): 0 for a
/// failed query.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_arrow_row_count(result: DuckdbArrow) -> Idx {
    // SAFETY: `result` is NULL or a live handle.
    unsafe { result.as_ref() }
        .and_then(|wrapper| wrapper.result.as_ref().ok())
        .map_or(0, |result| result.row_count() as Idx)
}

/// `idx_t duckdb_arrow_column_count(duckdb_arrow result)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_arrow_column_count(result: DuckdbArrow) -> Idx {
    // SAFETY: `result` is NULL or a live handle.
    unsafe { result.as_ref() }
        .and_then(|wrapper| wrapper.result.as_ref().ok())
        .map_or(0, |result| result.column_count() as Idx)
}

/// `idx_t duckdb_arrow_rows_changed(duckdb_arrow result)` (capi/arrow-c.cpp): the count
/// an INSERT, UPDATE or DELETE reports, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_arrow_rows_changed(result: DuckdbArrow) -> Idx {
    // SAFETY: `result` is NULL or a live handle.
    let Some(Ok(result)) = (unsafe { result.as_ref() }).map(|wrapper| &wrapper.result) else {
        return 0;
    };
    if result.row_count() == 0 || result.return_type() != StatementReturnType::ChangedRows {
        return 0;
    }
    roastedduck_function::cast::ValueCast::try_cast(&result.value(0, 0), &LogicalType::BIGINT)
        .ok()
        .and_then(|count| count.as_i64())
        .map_or(0, |count| count as Idx)
}

/// `const char *duckdb_query_arrow_error(duckdb_arrow result)` (capi/arrow-c.cpp): the
/// error message, empty for a successful query; owned by the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query_arrow_error(result: DuckdbArrow) -> *const c_char {
    // SAFETY: `result` is NULL or a live handle.
    match unsafe { result.as_ref() }.map(|wrapper| &wrapper.result) {
        Some(Err(error)) => error.message.as_ptr(),
        Some(Ok(_)) => c"".as_ptr(),
        None => std::ptr::null(),
    }
}

/// `void duckdb_destroy_arrow(duckdb_arrow *result)` (capi/arrow-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_arrow(result: *mut DuckdbArrow) {
    // SAFETY: NULL or points to NULL or a handle from this module.
    unsafe {
        let Some(slot) = result.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_destroy_arrow_stream(duckdb_arrow_stream *stream_p)` (capi/arrow-c.cpp):
/// releases and frees a stream made by `duckdb_arrow_array_scan`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_arrow_stream(stream_p: *mut DuckdbArrowStream) {
    // SAFETY: NULL or points to NULL or a stream allocated by this module.
    unsafe {
        let Some(slot) = stream_p.as_mut() else {
            return;
        };
        if slot.is_null() {
            return;
        }
        let mut stream = Box::from_raw(*slot);
        if let Some(release) = stream.release {
            release(&raw mut *stream);
        }
        *slot = std::ptr::null_mut();
    }
}

/// `duckdb_state duckdb_execute_prepared_arrow(duckdb_prepared_statement
/// prepared_statement, duckdb_arrow *out_result)` (capi/arrow-c.cpp): the result is set
/// even if the execution fails.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_prepared_arrow(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbArrow,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_statement.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    let (Ok(statement), false) = (&wrapper.statement, out_result.is_null()) else {
        return DuckdbState::DuckDBError;
    };
    let (outcome, properties) = {
        let mut connection = wrapper.context.lock();
        let outcome = catch(
            || match connection.execute_prepared(statement, &wrapper.values, false)? {
                QueryResult::Materialized(result) => Ok(result),
                QueryResult::Stream(stream) => connection.materialize(&stream),
            },
        );
        let properties = if outcome.is_ok() {
            connection.client_properties()
        } else {
            ClientProperties::default()
        };
        (outcome, properties)
    };
    let arrow_wrapper = ArrowResultWrapper::new(outcome, properties);
    let state = DuckdbState::from_ok(arrow_wrapper.result.is_ok());
    // SAFETY: checked non-null above.
    unsafe { *out_result = Box::into_raw(Box::new(arrow_wrapper)) };
    state
}

/// The state of a stream over one caller-supplied array (`arrow_array_stream_wrapper`'s
/// `PrivateData`).
#[derive(Debug)]
struct PrivateData {
    schema: *mut ArrowSchema,
    array: *mut ArrowArray,
    /// Whether the array was handed out.
    done: bool,
}

/// A no-op release for schemas the caller keeps ownership of (`EmptySchemaRelease`).
unsafe extern "C" fn empty_schema_release(schema: *mut ArrowSchema) {
    // SAFETY: called with a live schema.
    unsafe { (*schema).release = None };
}

/// A no-op release for the array the caller keeps ownership of (`EmptyArrayRelease`).
unsafe extern "C" fn empty_array_release(array: *mut ArrowArray) {
    // SAFETY: called with a live array.
    unsafe { (*array).release = None };
}

/// A no-op release for the copies of the caller's stream (`EmptyStreamRelease`).
unsafe extern "C" fn empty_stream_release(stream: *mut ArrowArrayStream) {
    // SAFETY: called with a live stream.
    unsafe { (*stream).release = None };
}

/// The schema function `arrow_scan` reads the stream's schema with
/// (`FactoryGetSchema`): the caller keeps ownership of the schema.
///
/// # Safety
/// `stream` must be a live Arrow stream.
#[allow(clippy::unnecessary_wraps, reason = "a `StreamFactoryGetSchema`")]
unsafe fn factory_get_schema(
    stream: *mut ArrowArrayStream,
    schema: &mut ArrowSchema,
) -> roastedduck::Result<()> {
    // SAFETY: guaranteed by the caller.
    let stream_ref = unsafe { &*stream };
    if let Some(get_schema) = stream_ref.get_schema {
        // SAFETY: as above; the result is ignored, as in DuckDB.
        unsafe { get_schema(stream, schema) };
    }
    // Streams cannot set the release function of their schema, so the root's is cleared
    // here (its children's in `duckdb_arrow_scan`).
    schema.release = None;
    Ok(())
}

/// The produce function `arrow_scan` reads the stream with (`FactoryGetNext`): a copy
/// of the caller's stream, which stays the caller's to release.
///
/// # Safety
/// `stream_factory_ptr` must be the address of a live Arrow stream.
#[allow(clippy::unnecessary_wraps, reason = "a `StreamFactoryProduce`")]
unsafe fn factory_get_next(
    stream_factory_ptr: usize,
    _: &ArrowStreamParameters,
) -> roastedduck::Result<Box<ArrowArrayStreamWrapper>> {
    // SAFETY: guaranteed by the caller.
    let stream = unsafe { &*(stream_factory_ptr as *const ArrowArrayStream) };
    Ok(Box::new(ArrowArrayStreamWrapper {
        arrow_array_stream: ArrowArrayStream {
            get_schema: stream.get_schema,
            get_next: stream.get_next,
            get_last_error: stream.get_last_error,
            release: Some(empty_stream_release),
            private_data: stream.private_data,
        },
    }))
}

/// The private data of a stream made by `duckdb_arrow_array_scan`.
///
/// # Safety
/// `stream` must be such a stream, not released.
unsafe fn private_data<'a>(stream: *mut ArrowArrayStream) -> &'a mut PrivateData {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *(*stream).private_data.cast::<PrivateData>() }
}

/// `get_schema` of the stream over a caller's array (`GetSchema`).
unsafe extern "C" fn stream_get_schema(stream: *mut ArrowArrayStream, out: *mut ArrowSchema) -> c_int {
    // SAFETY: called by a consumer of the stream.
    let private_data = unsafe { private_data(stream) };
    if private_data.schema.is_null() {
        return DuckdbState::DuckDBError as c_int;
    }
    // SAFETY: the caller's schema and `out` are live; the copy does not own the schema.
    unsafe {
        out.write(std::ptr::read(private_data.schema));
        (*out).release = Some(empty_schema_release);
    }
    DuckdbState::DuckDBSuccess as c_int
}

/// `get_next` of the stream over a caller's array (`GetNext`): the array once, then the
/// end of the stream.
unsafe extern "C" fn stream_get_next(stream: *mut ArrowArrayStream, out: *mut ArrowArray) -> c_int {
    // SAFETY: called by a consumer of the stream.
    let private_data = unsafe { private_data(stream) };
    // SAFETY: the caller's array and `out` are live; the copy does not own the array.
    unsafe {
        out.write(std::ptr::read(private_data.array));
        (*out).release = if private_data.done {
            None
        } else {
            Some(empty_array_release)
        };
    }
    private_data.done = true;
    DuckdbState::DuckDBSuccess as c_int
}

/// `get_last_error` of the stream over a caller's array (`GetLastError`): it cannot fail.
unsafe extern "C" fn stream_get_last_error(_: *mut ArrowArrayStream) -> *const c_char {
    std::ptr::null()
}

/// `release` of the stream over a caller's array (`Release`).
unsafe extern "C" fn stream_release(stream: *mut ArrowArrayStream) {
    // SAFETY: called by the stream's owner.
    let stream = unsafe { &mut *stream };
    if !stream.private_data.is_null() {
        // SAFETY: the private data was allocated by `duckdb_arrow_array_scan`.
        drop(unsafe { Box::from_raw(stream.private_data.cast::<PrivateData>()) });
    }
    stream.private_data = std::ptr::null_mut();
    stream.release = None;
}

/// Replaces the arguments of `arrow_scan(NULL, NULL, NULL)` by pointers.
struct PointerArguments(std::vec::IntoIter<Value>);

impl VisitorMut for PointerArguments {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::Constant(constant) = &mut expression.kind
            && let Some(pointer) = self.0.next()
        {
            constant.value = pointer;
        }
        walk_expression(self, expression);
    }
}

/// Creates (or replaces) the view `table_name` over an `arrow_scan` of `input` (`Ingest`;
/// DuckDB builds it as `TableFunction("arrow_scan", ...)->CreateView(name, true, false)`).
///
/// # Safety
/// `connection` must be NULL or live, `table_name` NULL or a C string, `input` a live
/// Arrow stream that outlives the view's scans.
unsafe fn ingest(
    connection: DuckdbConnection,
    table_name: *const c_char,
    input: *mut ArrowArrayStream,
) -> DuckdbState {
    // SAFETY: guaranteed by the caller.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return DuckdbState::DuckDBError;
    };
    if table_name.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: as above.
    let view_name = unsafe { lossy_str(table_name) };
    let produce: StreamFactoryProduce = factory_get_next;
    let get_schema: StreamFactoryGetSchema = factory_get_schema;
    let pointers: Vec<Value> = [input as usize, produce as usize, get_schema as usize]
        .into_iter()
        .map(|pointer| Value::ubigint(pointer as u64).reinterpret(LogicalType::POINTER))
        .collect();
    let sql = format!(
        "CREATE OR REPLACE VIEW {} AS SELECT * FROM arrow_scan(NULL, NULL, NULL)",
        roastedduck_ast::keyword::write_quoted(&view_name, '"')
    );
    let created = catch(|| {
        let mut connection = context.lock();
        let mut statements = connection.extract_statements(&sql)?;
        let mut statement = statements
            .pop()
            .ok_or_else(|| Error::internal("arrow scan view without a statement"))?;
        let roastedduck_ast::SqlStatementKind::Create(info) = &mut statement.kind else {
            return Err(Error::internal("arrow scan view is not a CREATE statement"));
        };
        let roastedduck_ast::CreateInfoKind::View(view) = &mut info.kind else {
            return Err(Error::internal("arrow scan view is not a view"));
        };
        PointerArguments(pointers.into_iter()).visit_query_node(&mut view.query.node);
        connection.execute_statement(&sql, statement)
    });
    DuckdbState::from_ok(created.is_ok())
}

/// `duckdb_state duckdb_arrow_scan(duckdb_connection connection, const char *table_name,
/// duckdb_arrow_stream arrow)` (capi/arrow-c.cpp): creates the view `table_name` over the
/// stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_arrow_scan(
    connection: DuckdbConnection,
    table_name: *const c_char,
    arrow: DuckdbArrowStream,
) -> DuckdbState {
    // SAFETY: `arrow` is NULL or a live Arrow stream.
    let Some(stream) = (unsafe { arrow.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    let Some(get_schema) = stream.get_schema else {
        return DuckdbState::DuckDBError;
    };
    let mut schema = ArrowSchema::empty();
    // SAFETY: the stream is live.
    if unsafe { get_schema(arrow, &raw mut schema) } == DuckdbState::DuckDBError as c_int {
        return DuckdbState::DuckDBError;
    }
    // The caller keeps ownership of the schema's children: their release functions are
    // replaced while the view is created and restored after. (Arrow releases only reach
    // immediate children.)
    // SAFETY: the schema follows the C data interface.
    let children: Vec<*mut ArrowSchema> = (0..usize::try_from(schema.n_children).unwrap_or(0))
        .map(|index| unsafe { *schema.children.add(index) })
        .collect();
    let release_fns: Vec<_> = children
        .iter()
        // SAFETY: as above.
        .map(|&child| unsafe { (*child).release.replace(empty_schema_release) })
        .collect();
    // SAFETY: forwarded API contract.
    let state = unsafe { ingest(connection, table_name, arrow) };
    for (&child, release) in children.iter().zip(release_fns) {
        // SAFETY: as above.
        unsafe { (*child).release = release };
    }
    state
}

/// `duckdb_state duckdb_arrow_array_scan(duckdb_connection connection, const char
/// *table_name, duckdb_arrow_schema arrow_schema, duckdb_arrow_array arrow_array,
/// duckdb_arrow_stream *out_stream)` (capi/arrow-c.cpp): creates the view `table_name`
/// over a stream of the caller's array; destroy the stream with
/// `duckdb_destroy_arrow_stream` once the view is no longer scanned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_arrow_array_scan(
    connection: DuckdbConnection,
    table_name: *const c_char,
    arrow_schema: DuckdbArrowSchema,
    arrow_array: DuckdbArrowArray,
    out_stream: *mut DuckdbArrowStream,
) -> DuckdbState {
    if out_stream.is_null() {
        return DuckdbState::DuckDBError;
    }
    let private_data = Box::new(PrivateData {
        schema: arrow_schema,
        array: arrow_array,
        done: false,
    });
    let stream = Box::into_raw(Box::new(ArrowArrayStream {
        get_schema: Some(stream_get_schema),
        get_next: Some(stream_get_next),
        get_last_error: Some(stream_get_last_error),
        release: Some(stream_release),
        private_data: Box::into_raw(private_data).cast(),
    }));
    // SAFETY: checked non-null above.
    unsafe { *out_stream = stream };
    // SAFETY: forwarded API contract; the stream was just made.
    unsafe { duckdb_arrow_scan(connection, table_name, stream) }
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;

    use roastedduck_arrow::{ArrowAppender, to_arrow_schema};
    use roastedduck_types::{STANDARD_VECTOR_SIZE, Vector};

    use super::*;
    use crate::helper::duckdb_free;
    use crate::open::{
        DuckdbDatabase, duckdb_close, duckdb_connect, duckdb_disconnect, duckdb_open, duckdb_query,
    };
    use crate::result::{duckdb_destroy_result, duckdb_row_count};
    use crate::value_fetch::duckdb_value_varchar;

    /// An Arrow array of `chunk_count` chunks of `STANDARD_VECTOR_SIZE` rows, `i` and
    /// `'string number ' || i`, and its schema.
    fn arrow_input(chunk_count: usize) -> (ArrowSchema, ArrowArray) {
        let types = vec![LogicalType::INTEGER, LogicalType::VARCHAR];
        let options = ClientProperties::default();
        let mut schema = ArrowSchema::empty();
        to_arrow_schema(&mut schema, &types, &["i".into(), "s".into()], &options).unwrap();
        let mut appender =
            ArrowAppender::new(types.clone(), chunk_count * STANDARD_VECTOR_SIZE, options).unwrap();
        for chunk_index in 0..chunk_count {
            let mut columns: Vec<Vector> = types
                .iter()
                .map(|ty| Vector::flat(ty.clone(), STANDARD_VECTOR_SIZE))
                .collect();
            for row in 0..STANDARD_VECTOR_SIZE {
                let i = (chunk_index * STANDARD_VECTOR_SIZE + row) as i32;
                columns[0].set_value(row, &Value::integer(i)).unwrap();
                columns[1]
                    .set_value(row, &Value::varchar(format!("string number {i}")))
                    .unwrap();
            }
            let chunk = DataChunk::new(columns, STANDARD_VECTOR_SIZE);
            appender.append(&chunk, 0, chunk.len(), chunk.len()).unwrap();
        }
        (schema, appender.finalize().unwrap())
    }

    /// Runs `sql` on `connection`: the row count and the text of column 1 in row `row`.
    ///
    /// # Safety
    /// `connection` must be live.
    unsafe fn query(connection: DuckdbConnection, sql: &CStr, row: Idx) -> (Idx, Option<String>) {
        // SAFETY: a zeroed result is an empty one; the connection is live.
        unsafe {
            let mut result: DuckdbResult = std::mem::zeroed();
            assert_eq!(
                duckdb_query(connection, sql.as_ptr(), &raw mut result),
                DuckdbState::DuckDBSuccess
            );
            let count = duckdb_row_count(&raw mut result);
            let value = duckdb_value_varchar(&raw mut result, 1, row);
            let text = (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned());
            duckdb_free(value.cast());
            duckdb_destroy_result(&raw mut result);
            (count, text)
        }
    }

    #[test]
    fn array_scan_views_read_the_array_once() {
        let mut database: DuckdbDatabase = std::ptr::null_mut();
        let mut connection: DuckdbConnection = std::ptr::null_mut();
        let (mut schema, mut array) = arrow_input(3);
        let mut stream: DuckdbArrowStream = std::ptr::null_mut();
        // SAFETY: the handles are made and destroyed here; the schema and the array
        // outlive the scans of the view.
        unsafe {
            assert_eq!(
                duckdb_open(std::ptr::null(), &raw mut database),
                DuckdbState::DuckDBSuccess
            );
            assert_eq!(
                duckdb_connect(database, &raw mut connection),
                DuckdbState::DuckDBSuccess
            );
            let state = duckdb_arrow_array_scan(
                connection,
                c"t".as_ptr(),
                &raw mut schema,
                &raw mut array,
                &raw mut stream,
            );
            assert_eq!(state, DuckdbState::DuckDBSuccess);
            let (count, text) = query(connection, c"SELECT * FROM t", 5000);
            assert_eq!(count, 3 * STANDARD_VECTOR_SIZE as Idx);
            assert_eq!(text.as_deref(), Some("string number 5000"));
            // The stream hands out its array once, as DuckDB's does.
            assert_eq!(query(connection, c"SELECT * FROM t", 0).0, 0);
            duckdb_destroy_arrow_stream(&raw mut stream);
            assert!(stream.is_null());
            // The caller still owns the array and the schema.
            (array.release.unwrap())(&raw mut array);
            (schema.release.unwrap())(&raw mut schema);
            // A stream without a schema cannot be scanned.
            let mut array = ArrowArray::empty();
            let state = duckdb_arrow_array_scan(
                connection,
                c"u".as_ptr(),
                std::ptr::null_mut(),
                &raw mut array,
                &raw mut stream,
            );
            assert_eq!(state, DuckdbState::DuckDBError);
            duckdb_destroy_arrow_stream(&raw mut stream);
            duckdb_disconnect(&raw mut connection);
            duckdb_close(&raw mut database);
        }
    }
}
