//! Single-batch streams own their input and return independent schema copies.
use super::*;
struct SchemaOwner {
    format: Option<CString>,
    name: Option<CString>,
    metadata: Option<Vec<u8>>,
    children: Vec<*mut ArrowSchema>,
    dictionary: *mut ArrowSchema,
}
impl Drop for SchemaOwner {
    fn drop(&mut self) {
        // SAFETY: each boxed child/dictionary and its callback are exclusively owned here.
        unsafe {
            for pointer in self
                .children
                .iter()
                .copied()
                .chain(std::iter::once(self.dictionary))
                .filter(|p| !p.is_null())
            {
                let mut child = Box::from_raw(pointer);
                if let Some(release) = child.release {
                    release(&raw mut *child);
                }
            }
        }
    }
}
unsafe extern "C" fn release_schema(schema: *mut ArrowSchema) {
    // SAFETY: only this callback owns private_data; repeated release is harmless.
    unsafe {
        if let Some(schema) = schema.as_mut()
            && schema.release.is_some()
        {
            drop(Box::from_raw(schema.private_data.cast::<SchemaOwner>()));
            schema.private_data = std::ptr::null_mut();
            schema.release = None;
        }
    }
}
unsafe fn copy_schema(source: &ArrowSchema) -> ArrowSchema {
    // SAFETY: the caller supplies a valid Arrow C schema, including its length-prefixed metadata.
    unsafe {
        let metadata = if source.metadata.is_null() {
            None
        } else {
            let base = source.metadata.cast::<u8>();
            let count = std::ptr::read_unaligned(base.cast::<i32>());
            let mut length = 4;
            for _ in 0..count {
                for _ in 0..2 {
                    let size = std::ptr::read_unaligned(base.add(length).cast::<i32>());
                    length += 4 + size as usize;
                }
            }
            Some(std::slice::from_raw_parts(base, length).to_vec())
        };
        let mut owner = Box::new(SchemaOwner {
            format: opt_bytes(source.format).map(to_cstring),
            name: opt_bytes(source.name).map(to_cstring),
            metadata,
            children: Vec::new(),
            dictionary: std::ptr::null_mut(),
        });
        for i in 0..source.n_children as usize {
            owner
                .children
                .push(Box::into_raw(Box::new(copy_schema(&**source.children.add(i)))));
        }
        if let Some(dictionary) = source.dictionary.as_ref() {
            owner.dictionary = Box::into_raw(Box::new(copy_schema(dictionary)));
        }
        ArrowSchema {
            format: owner.format.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            name: owner.name.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            metadata: owner
                .metadata
                .as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
            flags: source.flags,
            n_children: source.n_children,
            children: if owner.children.is_empty() {
                std::ptr::null_mut()
            } else {
                owner.children.as_mut_ptr()
            },
            dictionary: owner.dictionary,
            release: Some(release_schema),
            private_data: Box::into_raw(owner).cast(),
        }
    }
}
struct Batch {
    schema: ArrowSchema,
    array: ArrowArray,
}
impl Drop for Batch {
    fn drop(&mut self) {
        // SAFETY: the batch owns both Arrow objects until moved to the output.
        unsafe {
            if let Some(release) = self.schema.release {
                release(&raw mut self.schema);
            }
            if let Some(release) = self.array.release {
                release(&raw mut self.array);
            }
        }
    }
}
unsafe extern "C" fn get_schema(stream: *mut ArrowArrayStream, out: *mut ArrowSchema) -> c_int {
    // SAFETY: stream owns Batch and output points to a writable schema slot.
    unsafe {
        let Some(stream) = stream.as_ref() else {
            return libc::EINVAL;
        };
        let Some(batch) = stream.private_data.cast::<Batch>().as_ref() else {
            return libc::EINVAL;
        };
        if out.is_null() {
            return libc::EINVAL;
        }
        *out = copy_schema(&batch.schema);
        0
    }
}
unsafe extern "C" fn get_next(stream: *mut ArrowArrayStream, out: *mut ArrowArray) -> c_int {
    // SAFETY: ownership moves once; subsequent calls return a released empty array.
    unsafe {
        let Some(stream) = stream.as_ref() else {
            return libc::EINVAL;
        };
        let Some(batch) = stream.private_data.cast::<Batch>().as_mut() else {
            return libc::EINVAL;
        };
        if out.is_null() {
            return libc::EINVAL;
        }
        *out = std::mem::replace(&mut batch.array, ArrowArray::empty());
        0
    }
}
unsafe extern "C" fn get_last_error(_stream: *mut ArrowArrayStream) -> *const c_char {
    std::ptr::null()
}
unsafe extern "C" fn release(stream: *mut ArrowArrayStream) {
    // SAFETY: private_data is a unique Box and is reset before another release can occur.
    unsafe {
        if let Some(stream) = stream.as_mut()
            && !stream.private_data.is_null()
        {
            drop(Box::from_raw(stream.private_data.cast::<Batch>()));
            *stream = ArrowArrayStream::empty();
        }
    }
}
pub(super) unsafe fn from_batch(
    array: *mut ArrowArray,
    schema: *mut ArrowSchema,
    stream: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: caller supplies non-null Arrow handles; initialization is checked before ownership moves.
    unsafe {
        if (*array).release.is_none() {
            return fail(error, INTERNAL, b"ArrowArray is not initialized");
        }
        if (*schema).release.is_none() {
            return fail(error, INTERNAL, b"ArrowSchema is not initialized");
        }
        if (*stream).release.is_some() {
            return fail(error, INTERNAL, b"ArrowArrayStream is already initialized");
        }
        let batch = Box::new(Batch {
            schema: std::ptr::replace(schema, ArrowSchema::empty()),
            array: std::ptr::replace(array, ArrowArray::empty()),
        });
        *stream = ArrowArrayStream {
            get_schema: Some(get_schema),
            get_next: Some(get_next),
            get_last_error: Some(get_last_error),
            release: Some(release),
            private_data: Box::into_raw(batch).cast(),
        };
        OK
    }
}

pub(super) fn schema(
    format: &[u8],
    name: Option<&[u8]>,
    flags: i64,
    children: Vec<ArrowSchema>,
) -> ArrowSchema {
    let mut owner = Box::new(SchemaOwner {
        format: Some(to_cstring(format)),
        name: name.map(to_cstring),
        metadata: None,
        children: children.into_iter().map(|v| Box::into_raw(Box::new(v))).collect(),
        dictionary: std::ptr::null_mut(),
    });
    ArrowSchema {
        format: owner.format.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        name: owner.name.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        metadata: std::ptr::null(),
        flags,
        n_children: owner.children.len() as i64,
        children: if owner.children.is_empty() {
            std::ptr::null_mut()
        } else {
            owner.children.as_mut_ptr()
        },
        dictionary: std::ptr::null_mut(),
        release: Some(release_schema),
        private_data: Box::into_raw(owner).cast(),
    }
}
