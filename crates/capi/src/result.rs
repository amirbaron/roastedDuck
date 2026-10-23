//! Query results (DuckDB's `capi/result-c.cpp` and `capi/stream-c.cpp`).
//!
//! A `duckdb_result` keeps the engine's result behind `internal_data`. It is read either
//! through data chunks (`duckdb_result_get_chunk`, `duckdb_fetch_chunk`) or through the
//! deprecated per-value API, which first converts every column into the C arrays of
//! `deprecated_columns` (`DeprecatedMaterializeResult`); a result serves only one of the
//! two.

use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use rustdb::{MaterializedResult, QueryResult, StatementReturnType, StatementType, StreamQueryResult};
use rustdb_common::Error;
use rustdb_types::{
    DataChunk, LogicalType, LogicalTypeId, PhysicalType, STANDARD_VECTOR_SIZE, Timestamp, Vector,
};

use crate::data_chunk::{DuckdbDataChunk, flat_chunk, wrap_chunk};
use crate::helper::{
    c_type_size, catch, duckdb_free, duckdb_malloc, error_type_to_c, logical_type_id_to_c, malloc_bytes,
    statement_type_to_c, to_cstring,
};
use crate::logical_types::{DuckdbLogicalType, wrap_type};
use crate::open::SharedConnection;
use crate::types::*;

/// Which API a result serves (DuckDB's `CAPIResultSetType`): it starts as `None` and is
/// fixed by the first call of either API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultSetType {
    None,
    Materialized,
    Streaming,
    Deprecated,
}

/// The engine's result in a `duckdb_result`.
#[derive(Debug)]
enum ResultState {
    Materialized(MaterializedResult),
    Stream(StreamQueryResult),
    /// The query, or fetching from its stream, failed.
    Failed,
}

/// What `duckdb_result::internal_data` points to (DuckDB's `DuckDBResultData`).
#[derive(Debug)]
pub(crate) struct ResultData {
    /// The connection a streaming result fetches from.
    context: Arc<SharedConnection>,
    state: ResultState,
    pub(crate) result_set_type: ResultSetType,
    names: Vec<CString>,
    pub(crate) types: Vec<LogicalType>,
    statement_type: StatementType,
    return_type: StatementReturnType,
    /// The error and its message (`QueryResult::GetError`).
    error: Option<(Error, CString)>,
    /// The rows in chunks of `STANDARD_VECTOR_SIZE` rows, as DuckDB's
    /// `ColumnDataCollection` holds them; made on first use.
    collection: Option<Vec<DataChunk>>,
    /// The next chunk `duckdb_fetch_chunk` hands out of a materialized result.
    next_chunk: usize,
}

impl ResultData {
    fn new(context: &Arc<SharedConnection>, outcome: rustdb::Result<QueryResult>) -> Self {
        let mut data = Self {
            context: Arc::clone(context),
            state: ResultState::Failed,
            result_set_type: ResultSetType::None,
            names: Vec::new(),
            types: Vec::new(),
            statement_type: StatementType::InvalidStatement,
            return_type: StatementReturnType::QueryResult,
            error: None,
            collection: None,
            next_chunk: 0,
        };
        match outcome {
            Ok(QueryResult::Materialized(result)) => {
                data.set_columns(result.names(), result.types());
                data.statement_type = result.statement_type();
                data.return_type = result.return_type();
                data.state = ResultState::Materialized(result);
            }
            Ok(QueryResult::Stream(stream)) => {
                data.set_columns(stream.names(), stream.types());
                data.statement_type = stream.statement_type();
                data.return_type = stream.return_type();
                data.state = ResultState::Stream(stream);
            }
            Err(error) => data.set_error(error),
        }
        data
    }

    fn set_columns(&mut self, names: &[String], types: &[LogicalType]) {
        self.names = names.iter().map(to_cstring).collect();
        self.types = types.to_vec();
    }

    /// Records a failure of the result (`QueryResult::SetError`).
    fn set_error(&mut self, error: Error) {
        let message = to_cstring(error.to_bytes());
        self.error = Some((error, message));
        self.state = ResultState::Failed;
    }

    /// The connection the result came from.
    pub(crate) fn context(&self) -> &Arc<SharedConnection> {
        &self.context
    }

    pub(crate) fn has_error(&self) -> bool {
        self.error.is_some()
    }

    fn is_streaming(&self) -> bool {
        matches!(self.state, ResultState::Stream(_))
    }

    pub(crate) fn column_count(&self) -> usize {
        self.types.len()
    }

    /// The materialized result, fetching the remaining rows of a streaming one first
    /// (`StreamQueryResult::Materialize`).
    fn materialize(&mut self) -> Option<&MaterializedResult> {
        if let ResultState::Stream(stream) = &self.state {
            let stream = stream.clone();
            let materialized = catch(|| self.context.lock().materialize(&stream));
            match materialized {
                Ok(result) => self.state = ResultState::Materialized(result),
                Err(error) => self.set_error(error),
            }
        }
        match &self.state {
            ResultState::Materialized(result) => Some(result),
            ResultState::Stream(_) | ResultState::Failed => None,
        }
    }

    /// The rows in chunks of `STANDARD_VECTOR_SIZE` rows (`MaterializedQueryResult::Collection`).
    fn collection(&mut self) -> &[DataChunk] {
        if self.collection.is_none() {
            let packed = match &self.state {
                ResultState::Materialized(result) => pack_chunks(&self.types, &materialized_chunks(result)),
                ResultState::Stream(_) | ResultState::Failed => Vec::new(),
            };
            self.collection = Some(packed);
        }
        self.collection.as_deref().unwrap_or_default()
    }

    /// The next chunk of the result (`QueryResult::Fetch`): of the collection for a
    /// materialized result, from the query for a streaming one; `None` at the end.
    fn fetch(&mut self) -> rustdb::Result<Option<DataChunk>> {
        match &self.state {
            ResultState::Materialized(_) => {
                let index = self.next_chunk;
                let chunk = self.collection().get(index).map(flat_chunk);
                if chunk.is_some() {
                    self.next_chunk += 1;
                }
                Ok(chunk)
            }
            ResultState::Stream(stream) => {
                let stream = stream.clone();
                loop {
                    match catch(|| self.context.lock().fetch(&stream))? {
                        Some(chunk) if chunk.is_empty() => {}
                        chunk => return Ok(chunk.map(|chunk| flat_chunk(&chunk))),
                    }
                }
            }
            ResultState::Failed => Ok(None),
        }
    }
}

impl Drop for ResultData {
    fn drop(&mut self) {
        // A stream still open ends with its result (`StreamQueryResult::Close`).
        if let ResultState::Stream(stream) = &self.state {
            self.context.lock().close_query(stream.id());
        }
    }
}

/// The chunks of a materialized result: those of an in-memory result, or copies read
/// from a buffer-managed one (none if they cannot be read).
pub(crate) fn materialized_chunks(result: &MaterializedResult) -> std::borrow::Cow<'_, [DataChunk]> {
    match result.in_memory_chunks() {
        Some(chunks) => std::borrow::Cow::Borrowed(chunks),
        None => std::borrow::Cow::Owned(result.collect_chunks().unwrap_or_default()),
    }
}

/// Packs the rows of `chunks` into flat chunks of `STANDARD_VECTOR_SIZE` rows, like
/// appending them to a `ColumnDataCollection`.
pub(crate) fn pack_chunks(types: &[LogicalType], chunks: &[DataChunk]) -> Vec<DataChunk> {
    let mut packed = Vec::new();
    let mut current = DataChunk::with_types(types, STANDARD_VECTOR_SIZE);
    for chunk in chunks {
        if current.is_empty() && chunk.len() == STANDARD_VECTOR_SIZE {
            packed.push(flat_chunk(chunk));
            continue;
        }
        let mut offset = 0;
        while offset < chunk.len() {
            let count = (chunk.len() - offset).min(STANDARD_VECTOR_SIZE - current.len());
            let start = current.len();
            for column in 0..chunk.column_count() {
                // Copying between vectors of one type only fails for corrupt vectors.
                let _ = rustdb_types::vector_ops::copy_rows(
                    chunk.column(column),
                    offset,
                    count,
                    current.column_mut(column),
                    start,
                );
            }
            current.set_len(start + count);
            offset += count;
            if current.len() == STANDARD_VECTOR_SIZE {
                packed.push(std::mem::replace(
                    &mut current,
                    DataChunk::with_types(types, STANDARD_VECTOR_SIZE),
                ));
            }
        }
    }
    if !current.is_empty() {
        packed.push(current);
    }
    packed
}

/// The data of a result.
///
/// # Safety
/// `result` must be NULL or point to a `duckdb_result` that is zeroed or was filled in by
/// this library.
pub(crate) unsafe fn result_data<'a>(result: *const DuckdbResult) -> Option<&'a mut ResultData> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        result
            .as_ref()
            .and_then(|result| result.internal_data.cast::<ResultData>().as_mut())
    }
}

/// `DuckDBTranslateResult`: fills in `out` with the outcome of a query; without `out`
/// only the state is returned.
///
/// # Safety
/// `out` must be NULL or point to writable `duckdb_result` storage.
pub(crate) unsafe fn translate_result(
    context: &Arc<SharedConnection>,
    outcome: rustdb::Result<QueryResult>,
    out: *mut DuckdbResult,
) -> DuckdbState {
    let state = DuckdbState::from_ok(outcome.is_ok());
    // SAFETY: `out` is NULL or writable per the caller.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return state;
    };
    let data = Box::new(ResultData::new(context, outcome));
    *out = DuckdbResult {
        deprecated_column_count: data.column_count() as Idx,
        deprecated_row_count: 0,
        deprecated_rows_changed: 0,
        deprecated_columns: std::ptr::null_mut(),
        deprecated_error_message: data
            .error
            .as_ref()
            .map_or(std::ptr::null_mut(), |(_, message)| message.as_ptr().cast_mut()),
        internal_data: Box::into_raw(data).cast::<c_void>(),
    };
    state
}

/// `DeprecatedMaterializeResult`: converts the result into the C arrays of
/// `deprecated_columns`, once; false if the result failed or serves the chunk API.
///
/// # Safety
/// `result` must be NULL or a `duckdb_result` filled in by this library.
pub(crate) unsafe fn deprecated_materialize(result: *mut DuckdbResult) -> bool {
    // SAFETY: guaranteed by the caller.
    let Some(result) = (unsafe { result.as_mut() }) else {
        return false;
    };
    // SAFETY: as above.
    let Some(data) = (unsafe { result_data(result) }) else {
        return false;
    };
    if data.has_error() {
        return false;
    }
    match data.result_set_type {
        ResultSetType::Deprecated => return true,
        ResultSetType::Materialized | ResultSetType::Streaming => return false,
        ResultSetType::None => {}
    }
    data.result_set_type = ResultSetType::Deprecated;
    let column_count = data.column_count();
    let columns = duckdb_malloc(size_of::<DuckdbColumn>() * column_count).cast::<DuckdbColumn>();
    if columns.is_null() && column_count > 0 {
        return false;
    }
    for (index, ty) in data.types.iter().enumerate() {
        // SAFETY: `columns` has room for `column_count` columns.
        unsafe {
            columns.add(index).write(DuckdbColumn {
                deprecated_data: std::ptr::null_mut(),
                deprecated_nullmask: std::ptr::null_mut(),
                deprecated_type: logical_type_id_to_c(ty.id()),
                deprecated_name: data.names[index].as_ptr().cast_mut(),
                internal_data: std::ptr::null_mut(),
            });
        }
    }
    result.deprecated_columns = columns;
    let types = data.types.clone();
    let Some(materialized) = data.materialize() else {
        return false;
    };
    result.deprecated_row_count = materialized.row_count() as Idx;
    if materialized.row_count() > 0 && materialized.return_type() == StatementReturnType::ChangedRows {
        let changed = materialized.value(0, 0);
        if let Some(count) = rustdb_function::cast::ValueCast::try_cast(&changed, &LogicalType::BIGINT)
            .ok()
            .and_then(|count| count.as_i64())
        {
            result.deprecated_rows_changed = count as Idx;
        }
    }
    for (index, ty) in types.iter().enumerate() {
        // SAFETY: `columns` holds `column_count` initialized columns.
        let column = unsafe { &mut *columns.add(index) };
        if !translate_column(materialized, index, ty, column) {
            return false;
        }
    }
    true
}

/// `deprecated_duckdb_translate_column`: the null mask and the C values of column `col`.
/// Types without a C representation get an all-false null mask and no data.
fn translate_column(
    result: &MaterializedResult,
    col: usize,
    ty: &LogicalType,
    column: &mut DuckdbColumn,
) -> bool {
    let row_count = result.row_count();
    column.deprecated_nullmask = duckdb_malloc(row_count).cast::<bool>();
    let type_size = c_type_size(column.deprecated_type);
    if type_size == 0 {
        if !column.deprecated_nullmask.is_null() {
            // SAFETY: the mask has `row_count` bytes.
            unsafe { std::ptr::write_bytes(column.deprecated_nullmask, 0, row_count) };
        }
        return true;
    }
    column.deprecated_data = duckdb_malloc(type_size * row_count);
    if column.deprecated_nullmask.is_null() || column.deprecated_data.is_null() {
        return row_count == 0;
    }
    let mut writer = ColumnWriter {
        nullmask: column.deprecated_nullmask,
        data: column.deprecated_data,
        row: 0,
    };
    for chunk in materialized_chunks(result).iter() {
        let vector = chunk.column(col).flattened(chunk.len());
        let written = match ty.id() {
            LogicalTypeId::Boolean => writer.copy(&vector, chunk.len(), |value: bool| value),
            LogicalTypeId::TinyInt => writer.copy(&vector, chunk.len(), |value: i8| value),
            LogicalTypeId::SmallInt => writer.copy(&vector, chunk.len(), |value: i16| value),
            LogicalTypeId::Integer | LogicalTypeId::Date => {
                writer.copy(&vector, chunk.len(), |value: i32| value)
            }
            LogicalTypeId::BigInt
            | LogicalTypeId::Time
            | LogicalTypeId::TimeNs
            | LogicalTypeId::Timestamp
            | LogicalTypeId::TimestampTz => writer.copy(&vector, chunk.len(), |value: i64| value),
            LogicalTypeId::UTinyInt => writer.copy(&vector, chunk.len(), |value: u8| value),
            LogicalTypeId::USmallInt => writer.copy(&vector, chunk.len(), |value: u16| value),
            LogicalTypeId::UInteger => writer.copy(&vector, chunk.len(), |value: u32| value),
            LogicalTypeId::UBigInt => writer.copy(&vector, chunk.len(), |value: u64| value),
            LogicalTypeId::Float => writer.copy(&vector, chunk.len(), |value: f32| value),
            LogicalTypeId::Double => writer.copy(&vector, chunk.len(), |value: f64| value),
            // `CTimestamp*Converter`: finite values become microseconds.
            LogicalTypeId::TimestampNs => writer.copy(&vector, chunk.len(), |nanos: i64| {
                finite_or(nanos, || Timestamp::from_epoch_nanoseconds(nanos).0)
            }),
            LogicalTypeId::TimestampMs => writer.copy(&vector, chunk.len(), |millis: i64| {
                finite_or(millis, || {
                    Timestamp::try_from_epoch_ms(millis).map_or(millis, |timestamp| timestamp.0)
                })
            }),
            LogicalTypeId::TimestampSec => writer.copy(&vector, chunk.len(), |seconds: i64| {
                finite_or(seconds, || {
                    Timestamp::try_from_epoch_seconds(seconds).map_or(seconds, |timestamp| timestamp.0)
                })
            }),
            LogicalTypeId::HugeInt => writer.copy(&vector, chunk.len(), DuckdbHugeint::from_i128),
            LogicalTypeId::UHugeInt => writer.copy(&vector, chunk.len(), DuckdbUhugeint::from_u128),
            LogicalTypeId::Interval => {
                writer.copy(&vector, chunk.len(), |interval: rustdb_types::Interval| {
                    DuckdbInterval {
                        months: interval.months,
                        days: interval.days,
                        micros: interval.micros,
                    }
                })
            }
            // `CDecimalConverter`: the internal value widened to 64 bits in `lower`.
            LogicalTypeId::Decimal => match ty.physical_type() {
                PhysicalType::Int16 => writer.copy(&vector, chunk.len(), decimal_lower::<i16>),
                PhysicalType::Int32 => writer.copy(&vector, chunk.len(), decimal_lower::<i32>),
                PhysicalType::Int64 => writer.copy(&vector, chunk.len(), decimal_lower::<i64>),
                _ => writer.copy(&vector, chunk.len(), DuckdbHugeint::from_i128),
            },
            LogicalTypeId::Varchar => writer.strings(&vector, chunk.len()),
            LogicalTypeId::Blob => writer.blobs(&vector, chunk.len()),
            _ => false,
        };
        if !written {
            return false;
        }
    }
    true
}

/// `value` itself unless it is a finite timestamp, which `convert` turns into
/// microseconds.
fn finite_or(value: i64, convert: impl FnOnce() -> i64) -> i64 {
    if Timestamp::is_finite_raw(value) {
        convert()
    } else {
        value
    }
}

/// `CDecimalConverter<T>::Convert`.
fn decimal_lower<T: Into<i64>>(value: T) -> DuckdbHugeint {
    DuckdbHugeint {
        lower: value.into() as u64,
        upper: 0,
    }
}

/// Writes the values of a column into its C arrays (`WriteData`).
struct ColumnWriter {
    nullmask: *mut bool,
    data: *mut c_void,
    /// The next row to write.
    row: usize,
}

impl ColumnWriter {
    /// Writes `count` rows of `vector` (flat) converted by `convert`; NULL rows get a
    /// zeroed value.
    fn copy<T: rustdb_types::NativeType, C: Copy>(
        &mut self,
        vector: &Vector,
        count: usize,
        convert: impl Fn(T) -> C,
    ) -> bool {
        let values = vector.values::<T>();
        let validity = vector.validity();
        let target = self.data.cast::<C>();
        for (k, value) in values.iter().take(count).enumerate() {
            let valid = validity.is_valid(k);
            // SAFETY: the arrays have room for every row of the result; `self.row` counts
            // the rows written so far.
            unsafe {
                *self.nullmask.add(self.row) = !valid;
                if valid {
                    target.add(self.row).write_unaligned(convert(*value));
                } else {
                    std::ptr::write_bytes(target.add(self.row).cast::<u8>(), 0, size_of::<C>());
                }
            }
            self.row += 1;
        }
        true
    }

    /// Writes VARCHAR rows as NUL-terminated copies (`CStringConverter`), NULL for NULLs.
    fn strings(&mut self, vector: &Vector, count: usize) -> bool {
        let target = self.data.cast::<*mut c_char>();
        for k in 0..count {
            let valid = vector.validity().is_valid(k);
            let value = if valid {
                malloc_bytes(vector.string_bytes(k), true).cast::<c_char>()
            } else {
                std::ptr::null_mut()
            };
            // SAFETY: the arrays have room for every row of the result.
            unsafe {
                *self.nullmask.add(self.row) = !valid;
                target.add(self.row).write_unaligned(value);
            }
            self.row += 1;
        }
        true
    }

    /// Writes BLOB rows as `duckdb_blob` copies (`CBlobConverter`), empty for NULLs.
    fn blobs(&mut self, vector: &Vector, count: usize) -> bool {
        let target = self.data.cast::<DuckdbBlob>();
        for k in 0..count {
            let valid = vector.validity().is_valid(k);
            let blob = if valid {
                let bytes = vector.string_bytes(k);
                DuckdbBlob {
                    data: malloc_bytes(bytes, false).cast(),
                    size: bytes.len() as Idx,
                }
            } else {
                DuckdbBlob {
                    data: std::ptr::null_mut(),
                    size: 0,
                }
            };
            // SAFETY: the arrays have room for every row of the result.
            unsafe {
                *self.nullmask.add(self.row) = !valid;
                target.add(self.row).write_unaligned(blob);
            }
            self.row += 1;
        }
        true
    }
}

/// Frees a deprecated column's arrays (`DuckdbDestroyColumn`).
///
/// # Safety
/// `column` must have been filled in by [`deprecated_materialize`] for `count` rows.
unsafe fn destroy_column(column: &DuckdbColumn, count: Idx) {
    // SAFETY: the arrays and the values they own were allocated with `duckdb_malloc`.
    unsafe {
        if !column.deprecated_data.is_null() {
            if column.deprecated_type == DUCKDB_TYPE_VARCHAR {
                let strings = column.deprecated_data.cast::<*mut c_char>();
                for row in 0..count as usize {
                    duckdb_free(strings.add(row).read_unaligned().cast());
                }
            } else if column.deprecated_type == DUCKDB_TYPE_BLOB {
                let blobs = column.deprecated_data.cast::<DuckdbBlob>();
                for row in 0..count as usize {
                    duckdb_free(blobs.add(row).read_unaligned().data);
                }
            }
            duckdb_free(column.deprecated_data);
        }
        duckdb_free(column.deprecated_nullmask.cast());
    }
}

/// `void duckdb_destroy_result(duckdb_result *result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_result(result: *mut DuckdbResult) {
    // SAFETY: `result` is NULL or a result filled in by this library (or zeroed).
    let Some(result) = (unsafe { result.as_mut() }) else {
        return;
    };
    if !result.deprecated_columns.is_null() {
        for col in 0..result.deprecated_column_count as usize {
            // SAFETY: `deprecated_columns` holds `deprecated_column_count` columns.
            unsafe { destroy_column(&*result.deprecated_columns.add(col), result.deprecated_row_count) };
        }
        // SAFETY: allocated with `duckdb_malloc`.
        unsafe { duckdb_free(result.deprecated_columns.cast()) };
    }
    if !result.internal_data.is_null() {
        // SAFETY: `internal_data` was created by `Box::into_raw` in `translate_result`.
        drop(unsafe { Box::from_raw(result.internal_data.cast::<ResultData>()) });
    }
    *result = DuckdbResult {
        deprecated_column_count: 0,
        deprecated_row_count: 0,
        deprecated_rows_changed: 0,
        deprecated_columns: std::ptr::null_mut(),
        deprecated_error_message: std::ptr::null_mut(),
        internal_data: std::ptr::null_mut(),
    };
}

/// `const char *duckdb_column_name(duckdb_result *result, idx_t col)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_name(result: *mut DuckdbResult, col: Idx) -> *const c_char {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }
        .and_then(|data| data.names.get(col as usize))
        .map_or(std::ptr::null(), |name| name.as_ptr())
}

/// `duckdb_type duckdb_column_type(duckdb_result *result, idx_t col)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_type(result: *mut DuckdbResult, col: Idx) -> DuckdbType {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }
        .and_then(|data| data.types.get(col as usize))
        .map_or(DUCKDB_TYPE_INVALID, |ty| logical_type_id_to_c(ty.id()))
}

/// `duckdb_logical_type duckdb_column_logical_type(duckdb_result *result, idx_t col)`
/// (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_logical_type(
    result: *mut DuckdbResult,
    col: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }
        .and_then(|data| data.types.get(col as usize))
        .map_or(std::ptr::null_mut(), |ty| wrap_type(ty.clone()))
}

/// `idx_t duckdb_column_count(duckdb_result *result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_count(result: *mut DuckdbResult) -> Idx {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }.map_or(0, |data| data.column_count() as Idx)
}

/// `idx_t duckdb_row_count(duckdb_result *result)` (capi/result-c.cpp): 0 for a
/// streaming result, whose rows are not known in advance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_row_count(result: *mut DuckdbResult) -> Idx {
    // SAFETY: `result` is NULL or a result filled in by this library.
    match unsafe { result_data(result) }.map(|data| &data.state) {
        Some(ResultState::Materialized(result)) => result.row_count() as Idx,
        _ => 0,
    }
}

/// `idx_t duckdb_rows_changed(duckdb_result *result)` (capi/result-c.cpp): the count a
/// statement that changes rows returns, 0 for other statements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_rows_changed(result: *mut DuckdbResult) -> Idx {
    // SAFETY: `result` is NULL or a result filled in by this library.
    let Some(data) = (unsafe { result_data(result) }) else {
        return 0;
    };
    if data.result_set_type == ResultSetType::Deprecated {
        // SAFETY: `result` is non-null (it has data).
        return unsafe { (*result).deprecated_rows_changed };
    }
    if data.return_type != StatementReturnType::ChangedRows {
        return 0;
    }
    match &data.state {
        ResultState::Materialized(result) if result.row_count() == 1 && result.column_count() == 1 => {
            rustdb_function::cast::ValueCast::try_cast(&result.value(0, 0), &LogicalType::UBIGINT)
                .ok()
                .and_then(|count| count.as_u64())
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// `void *duckdb_column_data(duckdb_result *result, idx_t col)` (capi/result-c.cpp): the C
/// array of the column's values (deprecated).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_data(result: *mut DuckdbResult, col: Idx) -> *mut c_void {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe {
        match result.as_ref() {
            Some(r) if col < r.deprecated_column_count && deprecated_materialize(result) => {
                (*r.deprecated_columns.add(col as usize)).deprecated_data
            }
            _ => std::ptr::null_mut(),
        }
    }
}

/// `bool *duckdb_nullmask_data(duckdb_result *result, idx_t col)` (capi/result-c.cpp): the
/// C array of the column's NULL flags (deprecated).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_nullmask_data(result: *mut DuckdbResult, col: Idx) -> *mut bool {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe {
        match result.as_ref() {
            Some(r) if col < r.deprecated_column_count && deprecated_materialize(result) => {
                (*r.deprecated_columns.add(col as usize)).deprecated_nullmask
            }
            _ => std::ptr::null_mut(),
        }
    }
}

/// `const char *duckdb_result_error(duckdb_result *result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_error(result: *mut DuckdbResult) -> *const c_char {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }
        .and_then(|data| data.error.as_ref())
        .map_or(std::ptr::null(), |(_, message)| message.as_ptr())
}

/// `duckdb_error_type duckdb_result_error_type(duckdb_result *result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_error_type(result: *mut DuckdbResult) -> DuckdbErrorType {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe { result_data(result) }
        .and_then(|data| data.error.as_ref())
        .map_or(DUCKDB_ERROR_INVALID, |(error, _)| error_type_to_c(error.kind()))
}

/// `idx_t duckdb_result_chunk_count(duckdb_result result)` (capi/result-c.cpp): 0 for
/// streaming and deprecated results.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_chunk_count(result: DuckdbResult) -> Idx {
    // SAFETY: `result` was filled in by this library (or zeroed).
    let Some(data) = (unsafe { result_data(&raw const result) }) else {
        return 0;
    };
    if data.result_set_type == ResultSetType::Deprecated || data.is_streaming() {
        return 0;
    }
    data.collection().len() as Idx
}

/// `duckdb_data_chunk duckdb_result_get_chunk(duckdb_result result, idx_t chunk_index)`
/// (capi/result-c.cpp): a copy of chunk `chunk_index` of a materialized result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_get_chunk(result: DuckdbResult, chunk_index: Idx) -> DuckdbDataChunk {
    // SAFETY: `result` was filled in by this library (or zeroed).
    let Some(data) = (unsafe { result_data(&raw const result) }) else {
        return std::ptr::null_mut();
    };
    if data.result_set_type == ResultSetType::Deprecated || data.is_streaming() || data.has_error() {
        return std::ptr::null_mut();
    }
    data.result_set_type = ResultSetType::Materialized;
    data.collection()
        .get(chunk_index as usize)
        .map_or(std::ptr::null_mut(), |chunk| wrap_chunk(flat_chunk(chunk)))
}

/// `bool duckdb_result_is_streaming(duckdb_result result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_is_streaming(result: DuckdbResult) -> bool {
    // SAFETY: `result` was filled in by this library (or zeroed).
    unsafe { result_data(&raw const result) }.is_some_and(|data| !data.has_error() && data.is_streaming())
}

/// `duckdb_result_type duckdb_result_return_type(duckdb_result result)` (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_return_type(result: DuckdbResult) -> DuckdbResultType {
    // SAFETY: `result` was filled in by this library (or zeroed).
    match unsafe { result_data(&raw const result) } {
        Some(data) if !data.has_error() => match data.return_type {
            StatementReturnType::ChangedRows => DUCKDB_RESULT_TYPE_CHANGED_ROWS,
            StatementReturnType::Nothing => DUCKDB_RESULT_TYPE_NOTHING,
            StatementReturnType::QueryResult => DUCKDB_RESULT_TYPE_QUERY_RESULT,
        },
        _ => DUCKDB_RESULT_TYPE_INVALID,
    }
}

/// `duckdb_statement_type duckdb_result_statement_type(duckdb_result result)`
/// (capi/result-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_statement_type(result: DuckdbResult) -> DuckdbStatementType {
    // SAFETY: `result` was filled in by this library (or zeroed).
    match unsafe { result_data(&raw const result) } {
        Some(data) if !data.has_error() => statement_type_to_c(data.statement_type),
        _ => DUCKDB_STATEMENT_TYPE_INVALID,
    }
}

/// `duckdb_data_chunk duckdb_stream_fetch_chunk(duckdb_result result)` (capi/stream-c.cpp):
/// only for streaming results.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_stream_fetch_chunk(result: DuckdbResult) -> DuckdbDataChunk {
    // SAFETY: `result` was filled in by this library (or zeroed).
    match unsafe { result_data(&raw const result) } {
        // SAFETY: forwarded.
        Some(data) if data.is_streaming() => unsafe { duckdb_fetch_chunk(result) },
        _ => std::ptr::null_mut(),
    }
}

/// `duckdb_data_chunk duckdb_fetch_chunk(duckdb_result result)` (capi/stream-c.cpp): the
/// next chunk of the result, NULL at the end; an error while fetching becomes the
/// result's error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_fetch_chunk(result: DuckdbResult) -> DuckdbDataChunk {
    // SAFETY: `result` was filled in by this library (or zeroed).
    let Some(data) = (unsafe { result_data(&raw const result) }) else {
        return std::ptr::null_mut();
    };
    if data.result_set_type == ResultSetType::Deprecated {
        return std::ptr::null_mut();
    }
    data.result_set_type = ResultSetType::Streaming;
    match data.fetch() {
        Ok(chunk) => chunk.map_or(std::ptr::null_mut(), wrap_chunk),
        Err(error) => {
            data.set_error(error);
            std::ptr::null_mut()
        }
    }
}

/// The C deprecated columns of a materialized result, for the value accessors.
///
/// # Safety
/// `result` must be a result filled in by this library whose deprecated columns exist.
pub(crate) unsafe fn deprecated_column<'a>(result: *mut DuckdbResult, col: Idx) -> &'a DuckdbColumn {
    // SAFETY: guaranteed by the caller.
    unsafe { &*(*result).deprecated_columns.add(col as usize) }
}
