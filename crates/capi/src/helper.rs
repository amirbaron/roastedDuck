//! Type and enum conversions between the engine and `duckdb.h`, memory handed to C, and
//! the `string_t` helpers (DuckDB's `capi/helper-c.cpp`).

use std::ffi::{CStr, CString, c_char, c_void};

use rustdb_ast::StatementType;
use rustdb_common::{Error, ErrorKind};
use rustdb_types::{LogicalTypeId, StringT};

use crate::error_data::{DuckdbErrorData, duckdb_create_error_data};
use crate::types::*;

/// `LogicalTypeIdFromC`
pub(crate) fn logical_type_id_from_c(ty: DuckdbType) -> LogicalTypeId {
    use LogicalTypeId as L;
    match ty {
        DUCKDB_TYPE_BOOLEAN => L::Boolean,
        DUCKDB_TYPE_TINYINT => L::TinyInt,
        DUCKDB_TYPE_SMALLINT => L::SmallInt,
        DUCKDB_TYPE_INTEGER => L::Integer,
        DUCKDB_TYPE_BIGINT => L::BigInt,
        DUCKDB_TYPE_UTINYINT => L::UTinyInt,
        DUCKDB_TYPE_USMALLINT => L::USmallInt,
        DUCKDB_TYPE_UINTEGER => L::UInteger,
        DUCKDB_TYPE_UBIGINT => L::UBigInt,
        DUCKDB_TYPE_FLOAT => L::Float,
        DUCKDB_TYPE_DOUBLE => L::Double,
        DUCKDB_TYPE_TIMESTAMP => L::Timestamp,
        DUCKDB_TYPE_DATE => L::Date,
        DUCKDB_TYPE_TIME => L::Time,
        DUCKDB_TYPE_INTERVAL => L::Interval,
        DUCKDB_TYPE_HUGEINT => L::HugeInt,
        DUCKDB_TYPE_UHUGEINT => L::UHugeInt,
        DUCKDB_TYPE_VARCHAR => L::Varchar,
        DUCKDB_TYPE_BLOB => L::Blob,
        DUCKDB_TYPE_DECIMAL => L::Decimal,
        DUCKDB_TYPE_TIMESTAMP_S => L::TimestampSec,
        DUCKDB_TYPE_TIMESTAMP_MS => L::TimestampMs,
        DUCKDB_TYPE_TIMESTAMP_NS => L::TimestampNs,
        DUCKDB_TYPE_ENUM => L::Enum,
        DUCKDB_TYPE_LIST => L::List,
        DUCKDB_TYPE_STRUCT => L::Struct,
        DUCKDB_TYPE_MAP => L::Map,
        DUCKDB_TYPE_ARRAY => L::Array,
        DUCKDB_TYPE_UUID => L::Uuid,
        DUCKDB_TYPE_UNION => L::Union,
        DUCKDB_TYPE_BIT => L::Bit,
        DUCKDB_TYPE_TIME_TZ => L::TimeTz,
        DUCKDB_TYPE_TIMESTAMP_TZ => L::TimestampTz,
        DUCKDB_TYPE_ANY => L::Any,
        DUCKDB_TYPE_BIGNUM => L::Bignum,
        DUCKDB_TYPE_SQLNULL => L::SqlNull,
        DUCKDB_TYPE_STRING_LITERAL => L::StringLiteral,
        DUCKDB_TYPE_INTEGER_LITERAL => L::IntegerLiteral,
        DUCKDB_TYPE_TIME_NS => L::TimeNs,
        DUCKDB_TYPE_GEOMETRY => L::Geometry,
        DUCKDB_TYPE_VARIANT => L::Variant,
        _ => L::Invalid,
    }
}

/// `LogicalTypeIdToC`
pub(crate) fn logical_type_id_to_c(id: LogicalTypeId) -> DuckdbType {
    use LogicalTypeId as L;
    match id {
        L::Boolean => DUCKDB_TYPE_BOOLEAN,
        L::TinyInt => DUCKDB_TYPE_TINYINT,
        L::SmallInt => DUCKDB_TYPE_SMALLINT,
        L::Integer => DUCKDB_TYPE_INTEGER,
        L::BigInt => DUCKDB_TYPE_BIGINT,
        L::UTinyInt => DUCKDB_TYPE_UTINYINT,
        L::USmallInt => DUCKDB_TYPE_USMALLINT,
        L::UInteger => DUCKDB_TYPE_UINTEGER,
        L::UBigInt => DUCKDB_TYPE_UBIGINT,
        L::HugeInt => DUCKDB_TYPE_HUGEINT,
        L::UHugeInt => DUCKDB_TYPE_UHUGEINT,
        L::Float => DUCKDB_TYPE_FLOAT,
        L::Double => DUCKDB_TYPE_DOUBLE,
        L::Timestamp => DUCKDB_TYPE_TIMESTAMP,
        L::TimestampTz => DUCKDB_TYPE_TIMESTAMP_TZ,
        L::TimestampSec => DUCKDB_TYPE_TIMESTAMP_S,
        L::TimestampMs => DUCKDB_TYPE_TIMESTAMP_MS,
        L::TimestampNs => DUCKDB_TYPE_TIMESTAMP_NS,
        L::Date => DUCKDB_TYPE_DATE,
        L::Time => DUCKDB_TYPE_TIME,
        L::TimeTz => DUCKDB_TYPE_TIME_TZ,
        L::Varchar => DUCKDB_TYPE_VARCHAR,
        L::Blob => DUCKDB_TYPE_BLOB,
        L::Bit => DUCKDB_TYPE_BIT,
        L::Bignum => DUCKDB_TYPE_BIGNUM,
        L::Interval => DUCKDB_TYPE_INTERVAL,
        L::Decimal => DUCKDB_TYPE_DECIMAL,
        L::Enum => DUCKDB_TYPE_ENUM,
        L::List => DUCKDB_TYPE_LIST,
        L::Struct => DUCKDB_TYPE_STRUCT,
        L::Map => DUCKDB_TYPE_MAP,
        L::Union => DUCKDB_TYPE_UNION,
        L::Uuid => DUCKDB_TYPE_UUID,
        L::Array => DUCKDB_TYPE_ARRAY,
        L::Any => DUCKDB_TYPE_ANY,
        L::SqlNull => DUCKDB_TYPE_SQLNULL,
        L::StringLiteral => DUCKDB_TYPE_STRING_LITERAL,
        L::IntegerLiteral => DUCKDB_TYPE_INTEGER_LITERAL,
        L::TimeNs => DUCKDB_TYPE_TIME_NS,
        L::Geometry => DUCKDB_TYPE_GEOMETRY,
        L::Variant => DUCKDB_TYPE_VARIANT,
        _ => DUCKDB_TYPE_INVALID,
    }
}

/// `GetCTypeSize`: the width of one value in the deprecated materialised result, 0 for
/// the types it does not materialise (nested and complex types).
pub(crate) fn c_type_size(ty: DuckdbType) -> usize {
    match ty {
        DUCKDB_TYPE_BOOLEAN | DUCKDB_TYPE_TINYINT | DUCKDB_TYPE_UTINYINT => 1,
        DUCKDB_TYPE_SMALLINT | DUCKDB_TYPE_USMALLINT => 2,
        DUCKDB_TYPE_INTEGER | DUCKDB_TYPE_UINTEGER | DUCKDB_TYPE_FLOAT | DUCKDB_TYPE_DATE => 4,
        DUCKDB_TYPE_BIGINT
        | DUCKDB_TYPE_UBIGINT
        | DUCKDB_TYPE_DOUBLE
        | DUCKDB_TYPE_TIME
        | DUCKDB_TYPE_TIME_NS
        | DUCKDB_TYPE_TIMESTAMP
        | DUCKDB_TYPE_TIMESTAMP_TZ
        | DUCKDB_TYPE_TIMESTAMP_S
        | DUCKDB_TYPE_TIMESTAMP_MS
        | DUCKDB_TYPE_TIMESTAMP_NS => 8,
        DUCKDB_TYPE_VARCHAR => size_of::<*const c_char>(),
        DUCKDB_TYPE_UHUGEINT | DUCKDB_TYPE_HUGEINT | DUCKDB_TYPE_UUID | DUCKDB_TYPE_BLOB
        | DUCKDB_TYPE_INTERVAL | DUCKDB_TYPE_DECIMAL => 16,
        _ => 0,
    }
}

/// `StatementTypeToC`
pub(crate) fn statement_type_to_c(ty: StatementType) -> DuckdbStatementType {
    use StatementType as S;
    match ty {
        S::InvalidStatement => DUCKDB_STATEMENT_TYPE_INVALID,
        S::SelectStatement => 1,
        S::InsertStatement => 2,
        S::UpdateStatement => 3,
        S::ExplainStatement => 4,
        S::DeleteStatement => 5,
        S::PrepareStatement => 6,
        S::CreateStatement => 7,
        S::ExecuteStatement => 8,
        S::AlterStatement => 9,
        S::TransactionStatement => 10,
        S::CopyStatement => 11,
        S::AnalyzeStatement => 12,
        S::VariableSetStatement => 13,
        S::CreateFuncStatement => 14,
        S::DropStatement => 15,
        S::ExportStatement => 16,
        S::PragmaStatement => 17,
        S::VacuumStatement => 18,
        S::CallStatement => 19,
        S::SetStatement => 20,
        S::LoadStatement => 21,
        S::RelationStatement => 22,
        S::ExtensionStatement => 23,
        S::LogicalPlanStatement => 24,
        S::AttachStatement => 25,
        S::DetachStatement => 26,
        S::MultiStatement => 27,
        S::CopyDatabaseStatement => 28,
        S::UpdateExtensionsStatement => 29,
        S::MergeIntoStatement => 30,
    }
}

/// `ErrorTypeToC`: `duckdb_error_type` numbers the error types like DuckDB's
/// `ExceptionType`, which [`ErrorKind`] mirrors.
pub(crate) const fn error_type_to_c(kind: ErrorKind) -> DuckdbErrorType {
    kind as DuckdbErrorType
}

/// `ErrorTypeFromC`
pub(crate) fn error_type_from_c(ty: DuckdbErrorType) -> ErrorKind {
    use ErrorKind as K;
    const KINDS: [ErrorKind; 43] = [
        K::Invalid,
        K::OutOfRange,
        K::Conversion,
        K::UnknownType,
        K::Decimal,
        K::MismatchType,
        K::DivideByZero,
        K::ObjectSize,
        K::InvalidType,
        K::Serialization,
        K::Transaction,
        K::NotImplemented,
        K::Expression,
        K::Catalog,
        K::Parser,
        K::Planner,
        K::Scheduler,
        K::Executor,
        K::Constraint,
        K::Index,
        K::Stat,
        K::Connection,
        K::Syntax,
        K::Settings,
        K::Binder,
        K::Network,
        K::Optimizer,
        K::NullPointer,
        K::Io,
        K::Interrupt,
        K::Fatal,
        K::Internal,
        K::InvalidInput,
        K::OutOfMemory,
        K::Permission,
        K::ParameterNotResolved,
        K::ParameterNotAllowed,
        K::Dependency,
        K::Http,
        K::MissingExtension,
        K::Autoload,
        K::Sequence,
        K::InvalidConfiguration,
    ];
    KINDS.get(ty as usize).copied().unwrap_or(K::Invalid)
}

/// Reads an optional C string; NULL and invalid UTF-8 are reported as `None`.
///
/// # Safety
/// `ptr` must be NULL or point to a NUL-terminated string that outlives `'a`.
pub(crate) unsafe fn opt_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Reads a C string, replacing invalid UTF-8 (DuckDB stores such text as given; the
/// engine's strings are Rust strings).
///
/// # Safety
/// `ptr` must point to a NUL-terminated string.
pub(crate) unsafe fn lossy_str(ptr: *const c_char) -> String {
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

/// Reads `len` bytes at `ptr` (NULL with a zero length is the empty slice).
///
/// # Safety
/// `ptr` must be NULL with `len == 0`, or point to `len` readable bytes outliving `'a`.
pub(crate) unsafe fn bytes<'a>(ptr: *const c_void, len: Idx) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: guaranteed by the caller.
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len as usize) }
}

/// Copies `bytes` into a buffer from `duckdb_malloc`, followed by a NUL byte.
pub(crate) fn malloc_bytes(bytes: &[u8], nul_terminated: bool) -> *mut u8 {
    let len = bytes.len() + usize::from(nul_terminated);
    // SAFETY: `malloc` accepts any size; the result is checked below.
    let ptr = unsafe { libc::malloc(len.max(1)) }.cast::<u8>();
    if ptr.is_null() {
        return ptr;
    }
    // SAFETY: `ptr` has room for `len >= bytes.len()` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        if nul_terminated {
            *ptr.add(bytes.len()) = 0;
        }
    }
    ptr
}

/// `text` as a C string kept by this library; like `std::string::c_str()`, C readers see
/// it up to an interior NUL byte.
pub(crate) fn to_cstring(text: impl AsRef<[u8]>) -> CString {
    let text = text.as_ref();
    let end = text.iter().position(|&byte| byte == 0).unwrap_or(text.len());
    CString::new(&text[..end]).unwrap_or_default()
}

/// Copies `text` into a NUL-terminated buffer the caller frees with `duckdb_free` (the
/// `strdup` of DuckDB's C API). Interior NUL bytes are kept; C readers stop at the first.
pub(crate) fn c_string(text: impl AsRef<[u8]>) -> *mut c_char {
    malloc_bytes(text.as_ref(), true).cast()
}

/// Runs an engine call that may panic, turning a panic into an internal error like the
/// `catch (...)` blocks of DuckDB's C API.
pub(crate) fn catch<T>(call: impl FnOnce() -> rustdb::Result<T>) -> rustdb::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        Err(Error::internal(message))
    })
}

/// `void *duckdb_malloc(size_t size)` (capi/helper-c.cpp): `malloc`, so callers may
/// release the memory with either `duckdb_free` or `free`.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_malloc(size: usize) -> *mut c_void {
    // SAFETY: `malloc` accepts any size.
    unsafe { libc::malloc(size) }
}

/// `void duckdb_free(void *ptr)` (capi/helper-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_free(ptr: *mut c_void) {
    // SAFETY: `ptr` is NULL or was allocated by `malloc` (`duckdb_malloc` and every
    // buffer this library hands out).
    unsafe { libc::free(ptr) }
}

/// `idx_t duckdb_vector_size()` (capi/helper-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_vector_size() -> Idx {
    rustdb_types::STANDARD_VECTOR_SIZE as Idx
}

/// `bool duckdb_string_is_inlined(duckdb_string_t string)` (capi/helper-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_string_is_inlined(string: DuckdbStringT) -> bool {
    string.is_inlined()
}

/// `uint32_t duckdb_string_t_length(duckdb_string_t string)` (capi/helper-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_string_t_length(string: DuckdbStringT) -> u32 {
    string.len() as u32
}

/// `const char *duckdb_string_t_data(duckdb_string_t *string)` (capi/helper-c.cpp): the
/// inlined bytes inside `*string`, or the pointer it holds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_string_t_data(string: *mut DuckdbStringT) -> *const c_char {
    // SAFETY: `string` points to a `duckdb_string_t` per the API contract; the bytes of
    // an inlined string are inside it, those of a long one wherever its pointer points.
    unsafe { StringT::data_ptr(string) }.cast()
}

/// `duckdb_error_data duckdb_valid_utf8_check(const char *str, idx_t len)`
/// (capi/helper-c.cpp): NULL for valid UTF-8, otherwise an `INVALID_INPUT` error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_valid_utf8_check(str: *const c_char, len: Idx) -> DuckdbErrorData {
    // SAFETY: `str` points to `len` bytes per the API contract.
    let text = unsafe { bytes(str.cast(), len) };
    if std::str::from_utf8(text).is_ok() {
        return std::ptr::null_mut();
    }
    // SAFETY: the message is a NUL-terminated literal.
    unsafe {
        duckdb_create_error_data(
            DUCKDB_ERROR_INVALID_INPUT,
            c"invalid Unicode detected, str must be valid UTF-8".as_ptr(),
        )
    }
}
