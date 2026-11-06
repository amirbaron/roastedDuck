//! ADBC 1.1 detail ownership. The legacy error prefix is accessed before inspecting any 1.1 fields.
use super::*;
const PRIVATE: i32 = i32::MIN;
struct Details {
    value: CString,
}
pub(super) unsafe extern "C" fn release_stream(error: *mut AdbcError) {
    // SAFETY: this callback owns only details; message is borrowed from the owning result stream.
    unsafe {
        if !error.is_null() {
            if !(*error).private_data.is_null() {
                drop(Box::from_raw((*error).private_data.cast::<Details>()));
            }
            (*error).private_data = std::ptr::null_mut();
            (*error).release = None;
        }
    }
}
unsafe extern "C" fn release_owned(error: *mut AdbcError) {
    // SAFETY: this callback owns the malloc message and boxed details.
    unsafe {
        if !error.is_null() {
            duckdb_free((*error).message.cast());
            (*error).message = std::ptr::null_mut();
            release_stream(error);
        }
    }
}
pub(super) unsafe fn append(error: *mut AdbcError, kind: DuckdbErrorType) {
    // SAFETY: only vendor sentinel MIN authorizes access to the 1.1 tail; the preceding setter cleared old ownership.
    unsafe {
        if error.is_null() || (*error).vendor_code != PRIVATE {
            return;
        }
        (*error).private_data = Box::into_raw(Box::new(Details {
            value: to_cstring(type_name(kind)),
        }))
        .cast();
        (*error).release = Some(release_owned);
    }
}
pub(super) unsafe fn stream_error(error: *mut AdbcError, message: *mut c_char, kind: DuckdbErrorType) {
    // SAFETY: streams own a full 1.1 error; old details are released while the borrowed message remains owned externally.
    unsafe {
        if let Some(release) = (*error).release {
            release(error);
        }
        (*error).message = message;
        (*error).vendor_code = PRIVATE;
        (*error).private_data = Box::into_raw(Box::new(Details {
            value: to_cstring(type_name(kind)),
        }))
        .cast();
        (*error).release = Some(release_stream);
    }
}
pub(super) unsafe extern "C" fn count(error: *const AdbcError) -> c_int {
    // SAFETY: the stable prefix's vendor code must authorize reading private_data.
    unsafe {
        c_int::from(!error.is_null() && (*error).vendor_code == PRIVATE && !(*error).private_data.is_null())
    }
}
pub(super) unsafe extern "C" fn detail(error: *const AdbcError, index: c_int) -> AdbcErrorDetail {
    // SAFETY: authorized private_data is driver-owned and remains live until error release.
    unsafe {
        if count(error) == 0 || index != 0 {
            return AdbcErrorDetail::default();
        }
        let details = &*(*error).private_data.cast::<Details>();
        AdbcErrorDetail {
            key: c"duckdb:error_type".as_ptr(),
            value: details.value.as_bytes().as_ptr(),
            value_length: details.value.as_bytes().len(),
        }
    }
}
fn type_name(kind: DuckdbErrorType) -> &'static [u8] {
    match kind {
        0 => b"Invalid",
        1 => b"OutOfRange",
        2 => b"Conversion",
        3 => b"UnknownType",
        4 => b"Decimal",
        5 => b"MismatchType",
        6 => b"DivideByZero",
        7 => b"ObjectSize",
        8 => b"InvalidType",
        9 => b"Serialization",
        10 => b"Transaction",
        11 => b"NotImplemented",
        12 => b"Expression",
        13 => b"Catalog",
        14 => b"Parser",
        15 => b"Planner",
        16 => b"Scheduler",
        17 => b"Executor",
        18 => b"Constraint",
        19 => b"Index",
        20 => b"Stat",
        21 => b"Connection",
        22 => b"Syntax",
        23 => b"Settings",
        24 => b"Binder",
        25 => b"Network",
        26 => b"Optimizer",
        27 => b"NullPointer",
        28 => b"IO",
        29 => b"Interrupt",
        30 => b"Fatal",
        31 => b"Internal",
        32 => b"InvalidInput",
        33 => b"OutOfMemory",
        34 => b"Permission",
        35 => b"ParameterNotResolved",
        36 => b"ParameterNotAllowed",
        37 => b"Dependency",
        38 => b"HTTP",
        39 => b"MissingExtension",
        40 => b"Autoload",
        41 => b"Sequence",
        42 => b"InvalidConfiguration",
        _ => b"Unknown",
    }
}

#[cfg(feature = "adbc-test-api")]
pub(super) unsafe fn release_known(error: *mut AdbcError) {
    // SAFETY: the test helper releases only the driver's recognized detail callbacks.
    unsafe {
        if let Some(release) = (*error).release
            && (std::ptr::fn_addr_eq(release, release_owned as unsafe extern "C" fn(*mut AdbcError))
                || std::ptr::fn_addr_eq(release, release_stream as unsafe extern "C" fn(*mut AdbcError)))
        {
            release(error);
        }
    }
}
