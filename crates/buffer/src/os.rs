//! Operating-system services: page mappings that go straight back to the OS when
//! released, the machine's physical memory and free disk space.
//!
//! Buffer memory is mapped directly instead of going through the global allocator so
//! that releasing it really returns it to the OS (the process allocator would keep freed
//! large blocks around), and so every buffer starts page-aligned.

use std::path::Path;
use std::ptr::NonNull;

/// Granularity of mappings.
pub(crate) const PAGE_SIZE: usize = 4096;

/// Maps `size` bytes of zeroed, page-aligned, read-write memory.
#[cfg(unix)]
pub(crate) fn map(size: usize) -> Option<NonNull<u8>> {
    debug_assert!(size > 0 && size.is_multiple_of(PAGE_SIZE));
    // SAFETY: an anonymous private mapping with a null hint has no preconditions.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        None
    } else {
        NonNull::new(ptr.cast())
    }
}

/// Returns a mapping created by [`map`] to the OS.
///
/// # Safety
/// `ptr`/`size` must come from a single [`map`] call and the memory must not be used
/// afterwards.
#[cfg(unix)]
pub(crate) unsafe fn unmap(ptr: NonNull<u8>, size: usize) {
    // SAFETY: guaranteed by the caller. munmap only fails for invalid arguments, which
    // the caller rules out.
    unsafe { libc::munmap(ptr.as_ptr().cast(), size) };
}

#[cfg(not(unix))]
fn page_layout(size: usize) -> Option<std::alloc::Layout> {
    std::alloc::Layout::from_size_align(size, PAGE_SIZE).ok()
}

#[cfg(not(unix))]
pub(crate) fn map(size: usize) -> Option<NonNull<u8>> {
    let layout = page_layout(size)?;
    // SAFETY: `layout` has non-zero size.
    NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
}

#[cfg(not(unix))]
pub(crate) unsafe fn unmap(ptr: NonNull<u8>, size: usize) {
    if let Some(layout) = page_layout(size) {
        // SAFETY: allocated by `map` with the same layout.
        unsafe { std::alloc::dealloc(ptr.as_ptr(), layout) };
    }
}

/// Total physical memory of the machine, if it can be determined.
#[cfg(unix)]
pub fn physical_memory() -> Option<u64> {
    // SAFETY: sysconf has no preconditions.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    if pages <= 0 || page_size <= 0 {
        return None;
    }
    (pages as u64).checked_mul(page_size as u64)
}

#[cfg(not(unix))]
pub fn physical_memory() -> Option<u64> {
    None
}

/// Free disk space available to unprivileged users on the file system holding `path`.
#[cfg(unix)]
pub fn available_disk_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c_path` is NUL-terminated and `stats` points to writable storage.
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: statvfs succeeded and initialised the struct.
    let stats = unsafe { stats.assume_init() };
    #[allow(clippy::unnecessary_cast, reason = "the field types differ between platforms")]
    (stats.f_bavail as u64).checked_mul(stats.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn available_disk_space(_path: &Path) -> Option<u64> {
    None
}

/// A small per-thread number used to spread hot counters and queues over shards
/// (stands in for DuckDB's `TaskScheduler::GetEstimatedCPUId`).
pub(crate) fn thread_slot() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SLOT: usize = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    SLOT.with(|slot| *slot)
}
