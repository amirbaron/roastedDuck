//! Owned Arrow C data buffers for the dense unions required by ADBC metadata.
use super::*;
use std::ffi::c_void;
struct Buffer(Box<[u64]>);
impl Buffer {
    fn new(bytes: &[u8]) -> Self {
        let mut storage = vec![0u64; bytes.len().div_ceil(8).max(1)].into_boxed_slice();
        // SAFETY: storage has at least bytes.len() writable bytes and u64 alignment.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), storage.as_mut_ptr().cast::<u8>(), bytes.len());
        }
        Self(storage)
    }
    fn pointer(&self) -> *const c_void {
        self.0.as_ptr().cast()
    }
}
struct ArrayOwner {
    storage: Vec<Option<Buffer>>,
    buffers: Vec<*const c_void>,
    children: Vec<*mut ArrowArray>,
}
impl Drop for ArrayOwner {
    fn drop(&mut self) {
        // SAFETY: each boxed child is uniquely owned here, and callbacks own their descendants.
        unsafe {
            for pointer in &mut self.children {
                let mut child = Box::from_raw(*pointer);
                if let Some(release) = child.release {
                    release(&raw mut *child);
                }
            }
        }
    }
}
unsafe extern "C" fn release(array: *mut ArrowArray) {
    // SAFETY: private_data is allocated by array and removed exactly once.
    unsafe {
        if let Some(array) = array.as_mut()
            && array.release.is_some()
        {
            drop(Box::from_raw(array.private_data.cast::<ArrayOwner>()));
            *array = ArrowArray::empty();
        }
    }
}
pub(super) fn array(
    length: usize,
    null_count: usize,
    buffers: Vec<Option<Vec<u8>>>,
    children: Vec<ArrowArray>,
) -> ArrowArray {
    let mut owner = Box::new(ArrayOwner {
        storage: buffers.into_iter().map(|b| b.map(|b| Buffer::new(&b))).collect(),
        buffers: Vec::new(),
        children: children.into_iter().map(|v| Box::into_raw(Box::new(v))).collect(),
    });
    owner.buffers = owner
        .storage
        .iter()
        .map(|b| b.as_ref().map_or(std::ptr::null(), Buffer::pointer))
        .collect();
    ArrowArray {
        length: length as i64,
        null_count: null_count as i64,
        offset: 0,
        n_buffers: owner.buffers.len() as i64,
        n_children: owner.children.len() as i64,
        buffers: owner.buffers.as_mut_ptr(),
        children: if owner.children.is_empty() {
            std::ptr::null_mut()
        } else {
            owner.children.as_mut_ptr()
        },
        dictionary: std::ptr::null_mut(),
        release: Some(release),
        private_data: Box::into_raw(owner).cast(),
    }
}
pub(super) fn varchar(values: &[&[u8]]) -> ArrowArray {
    let mut offsets = 0i32.to_ne_bytes().to_vec();
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value);
        offsets.extend_from_slice(&(data.len() as i32).to_ne_bytes());
    }
    array(values.len(), 0, vec![None, Some(offsets), Some(data)], Vec::new())
}
pub(super) fn null_varchar(length: usize) -> ArrowArray {
    array(
        length,
        length,
        vec![
            Some(vec![0; length.div_ceil(8)]),
            Some(vec![0; (length + 1) * 4]),
            None,
        ],
        Vec::new(),
    )
}
pub(super) fn primitive(length: usize, bytes: Option<Vec<u8>>) -> ArrowArray {
    array(length, 0, vec![None, bytes], Vec::new())
}
pub(super) fn struct_array(length: usize, children: Vec<ArrowArray>) -> ArrowArray {
    array(length, 0, vec![None], children)
}
pub(super) fn list(offsets: &[i32], child: ArrowArray) -> ArrowArray {
    array(
        offsets.len() - 1,
        0,
        vec![None, Some(offsets.iter().flat_map(|v| v.to_ne_bytes()).collect())],
        vec![child],
    )
}
