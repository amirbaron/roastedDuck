//! Table descriptions (DuckDB's `capi/table_description-c.cpp`).

use std::ffi::{CString, c_char};

use roastedduck::{ColumnDescription, TableDescription};

use crate::helper::{c_string, catch, opt_bytes, to_cstring};
use crate::logical_types::{DuckdbLogicalType, wrap_type};
use crate::open::{DuckdbConnection, connection_ref};
use crate::types::*;

/// What a `duckdb_table_description` points to (DuckDB's `TableDescriptionWrapper`).
#[derive(Debug, Default)]
pub struct TableDescriptionWrapper {
    description: Option<TableDescription>,
    /// The last error; empty when there is none.
    error: CString,
}

/// `duckdb_table_description`
pub type DuckdbTableDescription = *mut TableDescriptionWrapper;

impl TableDescriptionWrapper {
    fn set_error(&mut self, message: impl AsRef<[u8]>) {
        self.error = to_cstring(message);
    }

    /// Column `index` (`GetTableDescription`), recording an error when it is out of range.
    fn column(&mut self, index: Idx) -> Option<&ColumnDescription> {
        let count = self.description.as_ref()?.columns.len();
        if index >= count as Idx {
            self.set_error(format!(
                "Column index {index} is out of range, table only has {count} columns"
            ));
            return None;
        }
        self.description.as_ref()?.columns.get(index as usize)
    }
}

/// `duckdb_state duckdb_table_description_create(duckdb_connection connection,
/// const char *schema, const char *table, duckdb_table_description *out)`
/// (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_create(
    connection: DuckdbConnection,
    schema: *const c_char,
    table: *const c_char,
    out: *mut DuckdbTableDescription,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_table_description_create_ext(connection, std::ptr::null(), schema, table, out) }
}

/// `duckdb_state duckdb_table_description_create_ext(duckdb_connection connection,
/// const char *catalog, const char *schema, const char *table, duckdb_table_description *out)`
/// (capi/table_description-c.cpp): a NULL catalog is the default database, a NULL schema
/// `main`; the description is handed out even when creating it fails, for its error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_create_ext(
    connection: DuckdbConnection,
    catalog: *const c_char,
    schema: *const c_char,
    table: *const c_char,
    out: *mut DuckdbTableDescription,
) -> DuckdbState {
    if out.is_null() {
        return DuckdbState::DuckDBError;
    }
    let wrapper = Box::into_raw(Box::default());
    // SAFETY: `out` is non-NULL and writable per the API contract; `wrapper` is live.
    let wrapper: &mut TableDescriptionWrapper = unsafe {
        *out = wrapper;
        &mut *wrapper
    };
    // SAFETY: `connection` is NULL or a live handle; the names are NULL or NUL-terminated.
    let (Some(context), Some(table)) = (unsafe { (connection_ref(connection), opt_bytes(table)) }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: as above.
    let (catalog, schema) = unsafe {
        (
            opt_bytes(catalog).unwrap_or(b""),
            opt_bytes(schema).unwrap_or(b"main"),
        )
    };
    match catch(|| context.lock().table_info(catalog, schema, table)) {
        Ok(Some(description)) => {
            wrapper.description = Some(description);
            DuckdbState::DuckDBSuccess
        }
        Ok(None) => {
            wrapper.set_error("No table with that schema+name could be located");
            DuckdbState::DuckDBError
        }
        Err(error) => {
            wrapper.set_error(error.raw_message_bytes());
            DuckdbState::DuckDBError
        }
    }
}

/// `void duckdb_table_description_destroy(duckdb_table_description *table_description)`
/// (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_destroy(table_description: *mut DuckdbTableDescription) {
    // SAFETY: `table_description` is NULL or points to NULL or a live handle.
    unsafe {
        let Some(slot) = table_description.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `const char *duckdb_table_description_error(duckdb_table_description table_description)`
/// (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_error(
    table_description: DuckdbTableDescription,
) -> *const c_char {
    // SAFETY: `table_description` is NULL or a live handle.
    match unsafe { table_description.as_ref() } {
        Some(wrapper) if !wrapper.error.is_empty() => wrapper.error.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// `duckdb_state duckdb_column_has_default(duckdb_table_description table_description,
/// idx_t index, bool *out)` (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_has_default(
    table_description: DuckdbTableDescription,
    index: Idx,
    out: *mut bool,
) -> DuckdbState {
    // SAFETY: `table_description` is NULL or a live handle.
    let Some(wrapper) = (unsafe { table_description.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    let Some(has_default) = wrapper.column(index).map(|column| column.default_value.is_some()) else {
        return DuckdbState::DuckDBError;
    };
    if out.is_null() {
        wrapper.set_error("Please provide a valid (non-null) 'out' variable");
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `out` is non-NULL and writable per the API contract.
    unsafe { *out = has_default };
    DuckdbState::DuckDBSuccess
}

/// `idx_t duckdb_table_description_get_column_count(duckdb_table_description table_description)`
/// (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_get_column_count(
    table_description: DuckdbTableDescription,
) -> Idx {
    // SAFETY: `table_description` is NULL or a live handle.
    unsafe { table_description.as_ref() }
        .and_then(|wrapper| wrapper.description.as_ref())
        .map_or(0, |description| description.columns.len() as Idx)
}

/// `char *duckdb_table_description_get_column_name(duckdb_table_description table_description,
/// idx_t index)` (capi/table_description-c.cpp): the caller frees the name with `duckdb_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_get_column_name(
    table_description: DuckdbTableDescription,
    index: Idx,
) -> *mut c_char {
    // SAFETY: `table_description` is NULL or a live handle.
    unsafe { table_description.as_mut() }
        .and_then(|wrapper| wrapper.column(index))
        .map_or(std::ptr::null_mut(), |column| c_string(&column.name))
}

/// `duckdb_logical_type duckdb_table_description_get_column_type(
/// duckdb_table_description table_description, idx_t index)` (capi/table_description-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_description_get_column_type(
    table_description: DuckdbTableDescription,
    index: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `table_description` is NULL or a live handle.
    unsafe { table_description.as_mut() }
        .and_then(|wrapper| wrapper.column(index))
        .map_or(std::ptr::null_mut(), |column| {
            wrap_type(column.logical_type.clone())
        })
}
