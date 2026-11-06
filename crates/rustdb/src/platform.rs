//! Platform probes used by connection initialization (`ClientContext`).

#[cfg(all(target_vendor = "apple", target_arch = "x86_64"))]
pub(crate) fn is_rosetta_translated() -> bool {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old_value: *mut c_void,
            old_size: *mut usize,
            new_value: *mut c_void,
            new_size: usize,
        ) -> c_int;
    }
    let mut translated: c_int = 0;
    let mut size = std::mem::size_of_val(&translated);
    // SAFETY: the fixed NUL-terminated name selects a read-only integer probe;
    // both output pointers refer to live, correctly sized local variables.
    let status = unsafe {
        sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            (&raw mut translated).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    status == 0 && translated == 1
}

#[cfg(not(all(target_vendor = "apple", target_arch = "x86_64")))]
pub(crate) const fn is_rosetta_translated() -> bool {
    false
}
