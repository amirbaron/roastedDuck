//! Ownership of a streaming result and of batches buffered when the connection is reused.
use super::*;

pub(super) struct StreamState {
    pub result: DuckdbResult,
    buffered: Option<VecDeque<ArrowArray>>,
    last_error: CString,
    status: u8,
    adbc_error: AdbcError,
}
// SAFETY: the result and exported batches are exclusively owned by this state.
// ADBC permits moving a stream between threads with serialized access. All result
// operations are protected by its Mutex; the underlying runtime connection is shared
// through SharedConnection's synchronization, and callbacks own no thread-local data.
unsafe impl Send for StreamState {}
impl StreamState {
    pub fn new(result: DuckdbResult) -> Self {
        Self {
            result,
            buffered: None,
            last_error: CString::default(),
            status: OK,
            adbc_error: AdbcError::default(),
        }
    }
    fn record_error(&mut self, message: &[u8], kind: DuckdbErrorType) {
        self.last_error = to_cstring(message);
        // SAFETY: the full error object lives with this stream; its message borrows last_error.
        unsafe {
            errors::stream_error(
                &raw mut self.adbc_error,
                self.last_error.as_ptr().cast_mut(),
                kind,
            );
        }
        self.status = if message == b"Interrupted!" {
            CANCELLED
        } else {
            INTERNAL
        };
    }
    fn fetch(&mut self) -> Option<ArrowArray> {
        // SAFETY: this state exclusively owns its live result, chunk, options and error handles.
        unsafe {
            let mut chunk = duckdb_fetch_chunk(std::ptr::read(&raw const self.result));
            if chunk.is_null() {
                if let Some(message) =
                    opt_bytes(duckdb_result_error(&raw mut self.result)).filter(|v| !v.is_empty())
                {
                    let kind = duckdb_result_error_type(&raw mut self.result);
                    self.record_error(message, kind);
                }
                return None;
            }
            let mut options = duckdb_result_get_arrow_options(&raw mut self.result);
            let mut array = ArrowArray::empty();
            let mut error = duckdb_data_chunk_to_arrow(options, chunk, &raw mut array);
            duckdb_destroy_arrow_options(&raw mut options);
            duckdb_destroy_data_chunk(&raw mut chunk);
            if !error.is_null() {
                self.record_error(
                    opt_bytes(duckdb_error_data_message(error)).unwrap_or_default(),
                    duckdb_error_data_error_type(error),
                );
                duckdb_destroy_error_data(&raw mut error);
                if let Some(release) = array.release {
                    release(&raw mut array);
                }
                return None;
            }
            Some(array)
        }
    }
    pub fn materialize(&mut self) {
        if self.buffered.is_some() {
            return;
        }
        let mut buffered = VecDeque::new();
        while let Some(array) = self.fetch() {
            buffered.push_back(array);
        }
        self.buffered = Some(buffered);
    }
}
impl Drop for StreamState {
    fn drop(&mut self) {
        // SAFETY: all queued arrays and the result remain exclusively owned until release.
        unsafe {
            if let Some(arrays) = &mut self.buffered {
                for array in arrays {
                    if let Some(release) = array.release {
                        release(array);
                    }
                }
            }
            if let Some(release) = self.adbc_error.release {
                release(&raw mut self.adbc_error);
            }
            duckdb_destroy_result(&raw mut self.result);
        }
    }
}
unsafe fn state<'a>(stream: *mut ArrowArrayStream) -> Option<&'a Arc<Mutex<StreamState>>> {
    // SAFETY: the ADBC stream private_data is allocated by export and removed by release.
    unsafe {
        stream
            .as_ref()?
            .private_data
            .cast::<Arc<Mutex<StreamState>>>()
            .as_ref()
    }
}
unsafe extern "C" fn get_next(stream: *mut ArrowArrayStream, out: *mut ArrowArray) -> c_int {
    // SAFETY: caller supplies a live stream and writable output array.
    unsafe {
        if out.is_null() {
            return 1;
        }
        (*out).release = None;
        let Some(shared) = state(stream) else {
            return 1;
        };
        let mut state = lock(shared);
        let next = if let Some(buffered) = &mut state.buffered {
            buffered.pop_front()
        } else {
            state.fetch()
        };
        if let Some(array) = next {
            *out = array;
            0
        } else {
            c_int::from(state.status != OK)
        }
    }
}
unsafe extern "C" fn get_schema(stream: *mut ArrowArrayStream, out: *mut ArrowSchema) -> c_int {
    // SAFETY: result metadata and temporary logical type handles stay live during conversion.
    unsafe {
        if out.is_null() {
            return 1;
        }
        let Some(shared) = state(stream) else {
            return 1;
        };
        let mut state = lock(shared);
        let count = duckdb_column_count(&raw mut state.result);
        let mut types: Vec<_> = (0..count)
            .map(|i| duckdb_column_logical_type(&raw mut state.result, i))
            .collect();
        let names: Vec<_> = (0..count)
            .map(|i| duckdb_column_name(&raw mut state.result, i))
            .collect();
        let mut options = duckdb_result_get_arrow_options(&raw mut state.result);
        let mut error = duckdb_to_arrow_schema(options, types.as_mut_ptr(), names.as_ptr(), count, out);
        for item in &mut types {
            duckdb_destroy_logical_type(item);
        }
        duckdb_destroy_arrow_options(&raw mut options);
        let failed = !error.is_null();
        duckdb_destroy_error_data(&raw mut error);
        c_int::from(failed)
    }
}
unsafe extern "C" fn get_last_error(stream: *mut ArrowArrayStream) -> *const c_char {
    // SAFETY: string storage is owned by the stream and valid until a subsequent stream call.
    unsafe {
        state(stream).map_or(std::ptr::null(), |state| {
            let state = lock(state);
            if state.last_error.as_bytes().is_empty() {
                std::ptr::null()
            } else {
                state.last_error.as_ptr()
            }
        })
    }
}
unsafe extern "C" fn release(stream: *mut ArrowArrayStream) {
    // SAFETY: private_data is an owned Box removed exactly once; repeated release is harmless.
    unsafe {
        if let Some(stream) = stream.as_mut() {
            if !stream.private_data.is_null() {
                drop(Box::from_raw(
                    stream.private_data.cast::<Arc<Mutex<StreamState>>>(),
                ));
            }
            stream.private_data = std::ptr::null_mut();
            stream.release = None;
        }
    }
}
pub(super) unsafe fn export(connection: &ConnectionState, result: DuckdbResult, out: *mut ArrowArrayStream) {
    let shared = Arc::new(Mutex::new(StreamState::new(result)));
    if !out.is_null() {
        lock(&connection.streams).push(Arc::downgrade(&shared));
        // SAFETY: caller provided writable output, ownership transfers to stream release.
        unsafe {
            *out = ArrowArrayStream {
                get_schema: Some(get_schema),
                get_next: Some(get_next),
                get_last_error: Some(get_last_error),
                release: Some(release),
                private_data: Box::into_raw(Box::new(shared)).cast(),
            };
        }
    }
}

pub(super) unsafe extern "C" fn error_from_stream(
    stream: *mut ArrowArrayStream,
    status: *mut u8,
) -> *const AdbcError {
    // SAFETY: origin is checked through the callback before any driver-specific private_data access.
    unsafe {
        let Some(value) = stream.as_ref() else {
            return std::ptr::null();
        };
        if !value.release.is_some_and(|callback| {
            std::ptr::fn_addr_eq(callback, release as unsafe extern "C" fn(*mut ArrowArrayStream))
        }) {
            return std::ptr::null();
        }
        let Some(shared) = state(stream) else {
            return std::ptr::null();
        };
        let state = lock(shared);
        if state.last_error.as_bytes().is_empty() {
            return std::ptr::null();
        }
        if !status.is_null() {
            *status = state.status;
        }
        &raw const state.adbc_error
    }
}
