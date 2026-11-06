//! The structs of the Arrow C data and C stream interfaces (DuckDB's
//! `common/arrow/arrow.hpp`) and the owning wrappers DuckDB keeps them in
//! (`arrow_wrapper.hpp` / `arrow_wrapper.cpp`).
//!
//! The layouts are those of the Arrow specification, so values of these types cross the
//! C ABI unchanged; the assertions below pin them.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

use rustdb_common::{Error, Result};

/// `ARROW_FLAG_NULLABLE`: the field may hold NULLs.
pub const ARROW_FLAG_NULLABLE: i64 = 2;

/// `struct ArrowSchema`: the type of an array.
#[repr(C)]
#[derive(Debug)]
pub struct ArrowSchema {
    pub format: *const c_char,
    pub name: *const c_char,
    pub metadata: *const c_char,
    pub flags: i64,
    pub n_children: i64,
    pub children: *mut *mut ArrowSchema,
    pub dictionary: *mut ArrowSchema,
    pub release: Option<unsafe extern "C" fn(*mut ArrowSchema)>,
    pub private_data: *mut c_void,
}

/// `struct ArrowArray`: the data of an array.
#[repr(C)]
#[derive(Debug)]
pub struct ArrowArray {
    pub length: i64,
    pub null_count: i64,
    pub offset: i64,
    pub n_buffers: i64,
    pub n_children: i64,
    pub buffers: *mut *const c_void,
    pub children: *mut *mut ArrowArray,
    pub dictionary: *mut ArrowArray,
    pub release: Option<unsafe extern "C" fn(*mut ArrowArray)>,
    pub private_data: *mut c_void,
}

/// `struct ArrowArrayStream`: a producer of arrays of one schema.
#[repr(C)]
#[derive(Debug)]
pub struct ArrowArrayStream {
    pub get_schema: Option<unsafe extern "C" fn(*mut ArrowArrayStream, *mut ArrowSchema) -> c_int>,
    pub get_next: Option<unsafe extern "C" fn(*mut ArrowArrayStream, *mut ArrowArray) -> c_int>,
    pub get_last_error: Option<unsafe extern "C" fn(*mut ArrowArrayStream) -> *const c_char>,
    pub release: Option<unsafe extern "C" fn(*mut ArrowArrayStream)>,
    pub private_data: *mut c_void,
}

const _: () = {
    assert!(size_of::<ArrowSchema>() == 72);
    assert!(std::mem::offset_of!(ArrowSchema, flags) == 24);
    assert!(std::mem::offset_of!(ArrowSchema, children) == 40);
    assert!(std::mem::offset_of!(ArrowSchema, release) == 56);
    assert!(std::mem::offset_of!(ArrowSchema, private_data) == 64);
    assert!(size_of::<ArrowArray>() == 80);
    assert!(std::mem::offset_of!(ArrowArray, n_buffers) == 24);
    assert!(std::mem::offset_of!(ArrowArray, buffers) == 40);
    assert!(std::mem::offset_of!(ArrowArray, dictionary) == 56);
    assert!(std::mem::offset_of!(ArrowArray, release) == 64);
    assert!(std::mem::offset_of!(ArrowArray, private_data) == 72);
    assert!(size_of::<ArrowArrayStream>() == 40);
    assert!(std::mem::offset_of!(ArrowArrayStream, release) == 24);
    assert!(std::mem::offset_of!(ArrowArrayStream, private_data) == 32);
};

impl ArrowSchema {
    /// A released schema (`ArrowSchema::Init`).
    pub const fn empty() -> Self {
        Self {
            format: std::ptr::null(),
            name: std::ptr::null(),
            metadata: std::ptr::null(),
            flags: 0,
            n_children: 0,
            children: std::ptr::null_mut(),
            dictionary: std::ptr::null_mut(),
            release: None,
            private_data: std::ptr::null_mut(),
        }
    }

    /// The child schemas.
    ///
    /// # Safety
    /// The schema must follow the C data interface: `children` holds `n_children` valid
    /// schemas that outlive `'a`.
    pub unsafe fn child_schemas<'a>(&self) -> impl Iterator<Item = &'a ArrowSchema> {
        let count = usize::try_from(self.n_children).unwrap_or(0);
        let children = self.children;
        // SAFETY: guaranteed by the caller.
        (0..count).map(move |index| unsafe { &**children.add(index) })
    }

    /// The format string.
    ///
    /// # Safety
    /// `format` must be a NUL-terminated string outliving `'a`, as the C data interface
    /// requires.
    pub unsafe fn format_str<'a>(&self) -> &'a str {
        // SAFETY: guaranteed by the caller.
        unsafe { c_str_or_empty(self.format) }
    }

    /// The native field-name bytes, read through the Arrow C-string boundary.
    ///
    /// # Safety
    /// `name` must be NULL or a NUL-terminated string.
    pub unsafe fn name_string(&self) -> rustdb_common::ByteString {
        if self.name.is_null() {
            return rustdb_common::ByteString::new();
        }
        // SAFETY: guaranteed by the caller.
        unsafe { CStr::from_ptr(self.name) }.to_bytes().into()
    }
}

impl ArrowArray {
    /// A released array of no rows (`ArrowArray::Init`).
    pub const fn empty() -> Self {
        Self {
            length: 0,
            null_count: 0,
            offset: 0,
            n_buffers: 0,
            n_children: 0,
            buffers: std::ptr::null_mut(),
            children: std::ptr::null_mut(),
            dictionary: std::ptr::null_mut(),
            release: None,
            private_data: std::ptr::null_mut(),
        }
    }
}

impl ArrowArrayStream {
    /// A released stream.
    pub const fn empty() -> Self {
        Self {
            get_schema: None,
            get_next: None,
            get_last_error: None,
            release: None,
            private_data: std::ptr::null_mut(),
        }
    }
}

/// The text of a C string, empty for NULL; invalid UTF-8 reads as empty, as no Arrow
/// format, name or error message DuckDB handles needs it.
///
/// # Safety
/// `text` must be NULL or a NUL-terminated string outliving `'a`.
unsafe fn c_str_or_empty<'a>(text: *const c_char) -> &'a str {
    if text.is_null() {
        return "";
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("")
}

/// An owned schema, released when dropped (DuckDB's `ArrowSchemaWrapper`).
#[derive(Debug)]
pub struct ArrowSchemaWrapper {
    pub arrow_schema: ArrowSchema,
}

impl Default for ArrowSchemaWrapper {
    fn default() -> Self {
        Self {
            arrow_schema: ArrowSchema::empty(),
        }
    }
}

impl Drop for ArrowSchemaWrapper {
    fn drop(&mut self) {
        if let Some(release) = self.arrow_schema.release {
            // SAFETY: a schema with a release callback is owned by this wrapper.
            unsafe { release(&raw mut self.arrow_schema) };
        }
    }
}

// SAFETY: the wrapper only reads the schema and releases it once; DuckDB hands schemas
// between threads the same way, as the C data interface allows.
unsafe impl Send for ArrowSchemaWrapper {}
// SAFETY: shared access only reads the schema.
unsafe impl Sync for ArrowSchemaWrapper {}

/// An owned array, released when dropped (DuckDB's `ArrowArrayWrapper`). Vectors that
/// point into the array's buffers keep it alive through an `Arc`.
#[derive(Debug)]
pub struct ArrowArrayWrapper {
    pub arrow_array: ArrowArray,
}

impl Default for ArrowArrayWrapper {
    fn default() -> Self {
        Self {
            arrow_array: ArrowArray::empty(),
        }
    }
}

impl Drop for ArrowArrayWrapper {
    fn drop(&mut self) {
        if let Some(release) = self.arrow_array.release {
            // SAFETY: an array with a release callback is owned by this wrapper.
            unsafe { release(&raw mut self.arrow_array) };
        }
    }
}

// SAFETY: the array's buffers are immutable once produced and it is released exactly
// once, from whichever thread drops the last reference, as the C data interface allows
// (DuckDB shares `ArrowArrayWrapper`s between its scan threads the same way).
unsafe impl Send for ArrowArrayWrapper {}
// SAFETY: shared access only reads the array.
unsafe impl Sync for ArrowArrayWrapper {}

/// An owned stream, released when dropped (DuckDB's `ArrowArrayStreamWrapper`).
#[derive(Debug)]
pub struct ArrowArrayStreamWrapper {
    pub arrow_array_stream: ArrowArrayStream,
}

impl Default for ArrowArrayStreamWrapper {
    fn default() -> Self {
        Self {
            arrow_array_stream: ArrowArrayStream::empty(),
        }
    }
}

impl Drop for ArrowArrayStreamWrapper {
    fn drop(&mut self) {
        if let Some(release) = self.arrow_array_stream.release {
            // SAFETY: a stream with a release callback is owned by this wrapper.
            unsafe { release(&raw mut self.arrow_array_stream) };
        }
    }
}

// SAFETY: the stream is used by one thread at a time (scans call it under a lock), which
// is all the C stream interface asks of its consumers.
unsafe impl Send for ArrowArrayStreamWrapper {}

impl ArrowArrayStreamWrapper {
    /// The stream's schema (`ArrowArrayStreamWrapper::GetSchema`).
    pub fn get_schema(&mut self, schema: &mut ArrowSchemaWrapper) -> Result<()> {
        let get_schema = self
            .arrow_array_stream
            .get_schema
            .ok_or_else(|| Error::internal("arrow_scan: stream without get_schema"))?;
        // SAFETY: a live stream's callbacks take the stream and an output schema.
        if unsafe { get_schema(&raw mut self.arrow_array_stream, &raw mut schema.arrow_schema) } != 0 {
            return Err(Error::invalid_input(format!(
                "arrow_scan: get_schema failed(): {}",
                self.get_error()
            )));
        }
        if schema.arrow_schema.release.is_none() {
            return Err(Error::invalid_input("arrow_scan: released schema passed"));
        }
        if schema.arrow_schema.n_children < 1 {
            return Err(Error::invalid_input("arrow_scan: empty schema passed"));
        }
        Ok(())
    }

    /// The next array of the stream; a released one marks the end
    /// (`ArrowArrayStreamWrapper::GetNextChunk`).
    pub fn get_next_chunk(&mut self) -> Result<Arc<ArrowArrayWrapper>> {
        let mut current_chunk = ArrowArrayWrapper::default();
        let get_next = self
            .arrow_array_stream
            .get_next
            .ok_or_else(|| Error::internal("arrow_scan: stream without get_next"))?;
        // SAFETY: a live stream's callbacks take the stream and an output array.
        if unsafe {
            get_next(
                &raw mut self.arrow_array_stream,
                &raw mut current_chunk.arrow_array,
            )
        } != 0
        {
            return Err(Error::invalid_input(format!(
                "arrow_scan: get_next failed(): {}",
                self.get_error()
            )));
        }
        Ok(Arc::new(current_chunk))
    }

    /// The stream's last error (`ArrowArrayStreamWrapper::GetError`).
    fn get_error(&mut self) -> String {
        let Some(get_last_error) = self.arrow_array_stream.get_last_error else {
            return String::new();
        };
        // SAFETY: the stream is live; the message, if any, is valid until the next call.
        unsafe { c_str_or_empty(get_last_error(&raw mut self.arrow_array_stream)) }.to_owned()
    }
}
