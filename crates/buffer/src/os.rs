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

/// Memory available to this process, including Linux job/container limits
/// (`DBConfig::GetSystemAvailableMemory`). Physical-memory queries retain their
/// separate meaning for allocator sizing.
pub(crate) fn available_memory() -> Option<u64> {
    let physical = physical_memory();
    #[cfg(target_os = "linux")]
    {
        let node = std::env::var_os("SLURM_MEM_PER_NODE").map(|value| value.to_string_lossy().into_owned());
        let per_cpu = std::env::var_os("SLURM_MEM_PER_CPU").map(|value| value.to_string_lossy().into_owned());
        let cpus = std::env::var_os("SLURM_CPUS_ON_NODE").map(|value| value.to_string_lossy().into_owned());
        if let Some(limit) = linux_memory::slurm_limit(
            node.as_deref(),
            per_cpu.as_deref(),
            cpus.as_deref(),
            std::thread::available_parallelism().map_or(1, usize::from),
        ) {
            return Some(limit);
        }
        if let Ok(groups) = std::fs::read_to_string("/proc/self/cgroup") {
            let limit = linux_memory::cgroup_limit(&groups, |path| {
                std::fs::read_to_string(path).ok()?.trim().parse().ok()
            });
            if let Some(limit) = limit {
                return Some(physical.map_or(limit, |memory| memory.min(limit)));
            }
        }
    }
    physical
}

#[cfg(any(target_os = "linux", test))]
mod linux_memory {
    /// DuckDB checks the member's controller path first, then the mounted root
    /// for containers whose cgroup hierarchy is mapped into their namespace.
    pub(super) fn cgroup_limit(groups: &str, read: impl Fn(&str) -> Option<u64>) -> Option<u64> {
        let mut memory = None;
        let mut unified = None;
        for line in groups.lines() {
            let mut parts = line.splitn(3, ':');
            let (Some(_), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if controllers.is_empty() {
                unified = Some(path);
            } else if controllers.split(',').any(|name| name == "memory") {
                memory = Some(path);
            }
        }
        if let Some(path) = memory {
            let path = format!("/sys/fs/cgroup/memory{path}/memory.limit_in_bytes");
            if let Some(limit) = read(&path).or_else(|| read("/sys/fs/cgroup/memory/memory.limit_in_bytes")) {
                return Some(limit);
            }
        }
        unified.and_then(|path| {
            read(&format!("/sys/fs/cgroup{path}/memory.max")).or_else(|| read("/sys/fs/cgroup/memory.max"))
        })
    }

    fn slurm_bytes(value: &str) -> Option<u64> {
        let (number, multiplier) = match value.as_bytes().last().copied()? {
            b'K' | b'k' => (&value[..value.len() - 1], 1_000.0),
            b'M' | b'm' => (&value[..value.len() - 1], 1_000_000.0),
            b'G' | b'g' => (&value[..value.len() - 1], 1_000_000_000.0),
            b'T' | b't' => (&value[..value.len() - 1], 1_000_000_000_000.0),
            _ => (value, 1_000_000.0),
        };
        let number = number.trim().parse::<f64>().ok()?;
        if number.is_nan() {
            return None;
        }
        if number < 0.0 || number * multiplier >= u64::MAX as f64 {
            return Some(i64::MAX as u64);
        }
        Some((number * multiplier) as u64)
    }

    pub(super) fn slurm_limit(
        per_node: Option<&str>,
        per_cpu: Option<&str>,
        cpus: Option<&str>,
        detected_threads: usize,
    ) -> Option<u64> {
        if let Some(value) = per_node {
            // A present but invalid per-node value does not select per-CPU mode.
            return slurm_bytes(value);
        }
        let memory = slurm_bytes(per_cpu?)?;
        let threads = cpus
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(detected_threads as u64)
            .max(1);
        memory.checked_mul(threads)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unified_member_and_container_root_limits() {
            assert_eq!(
                cgroup_limit("0::/jobs/a\n", |path| {
                    (path == "/sys/fs/cgroup/jobs/a/memory.max").then_some(3 << 30)
                }),
                Some(3 << 30)
            );
            assert_eq!(
                cgroup_limit("0::/outside/container\n", |path| {
                    (path == "/sys/fs/cgroup/memory.max").then_some(2 << 30)
                }),
                Some(2 << 30)
            );
            assert_eq!(cgroup_limit("0::/\n", |_| None), None);
        }

        #[test]
        fn v1_memory_controller_takes_precedence_over_unified() {
            assert_eq!(
                cgroup_limit("4:cpu,memory:/job\n0::/\n", |path| match path {
                    "/sys/fs/cgroup/memory/job/memory.limit_in_bytes" => Some(1 << 30),
                    "/sys/fs/cgroup/memory.max" => Some(2 << 30),
                    _ => None,
                }),
                Some(1 << 30)
            );
            assert_eq!(
                cgroup_limit("4:memory:/unmapped\n", |path| {
                    (path == "/sys/fs/cgroup/memory/memory.limit_in_bytes").then_some(4 << 30)
                }),
                Some(4 << 30)
            );
        }

        #[test]
        fn scheduler_units_and_node_precedence_match_reference() {
            assert_eq!(
                slurm_limit(Some("1500"), Some("9G"), Some("8"), 2),
                Some(1_500_000_000)
            );
            assert_eq!(slurm_limit(Some("1.5G"), None, None, 2), Some(1_500_000_000));
            assert_eq!(slurm_limit(None, Some("250M"), Some("3"), 2), Some(750_000_000));
            assert_eq!(slurm_limit(None, Some("250M"), None, 2), Some(500_000_000));
            assert_eq!(slurm_limit(Some("invalid"), Some("1G"), Some("3"), 2), None);
            assert_eq!(slurm_limit(None, Some("1K"), Some("0"), 2), Some(1000));
        }
    }
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
