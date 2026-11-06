//! Catalog lookups from callbacks (DuckDB's `capi/catalog-c.cpp`).
//!
//! The catalogs are reached through the binder's context of the connection's open
//! transaction ([`FunctionClient::transaction_context`]), so they are only available
//! while the connection runs a statement, as DuckDB requires an active transaction.

use std::ffi::{CString, c_char};
use std::sync::Arc;

use rustdb_catalog::{Catalog, CatalogEntry, CatalogType};
use rustdb_function::FunctionClient;
use rustdb_planner::ClientContext as BinderContext;

use crate::helper::{opt_str, to_cstring};
use crate::open::{DuckdbClientContext, client_context_ref};

/// `duckdb_catalog_entry_type`
pub type DuckdbCatalogEntryType = u32;

pub const DUCKDB_CATALOG_ENTRY_TYPE_INVALID: DuckdbCatalogEntryType = 0;
pub const DUCKDB_CATALOG_ENTRY_TYPE_TABLE: DuckdbCatalogEntryType = 1;
pub const DUCKDB_CATALOG_ENTRY_TYPE_SCHEMA: DuckdbCatalogEntryType = 2;
pub const DUCKDB_CATALOG_ENTRY_TYPE_VIEW: DuckdbCatalogEntryType = 3;
pub const DUCKDB_CATALOG_ENTRY_TYPE_INDEX: DuckdbCatalogEntryType = 4;
pub const DUCKDB_CATALOG_ENTRY_TYPE_PREPARED_STATEMENT: DuckdbCatalogEntryType = 5;
pub const DUCKDB_CATALOG_ENTRY_TYPE_SEQUENCE: DuckdbCatalogEntryType = 6;
pub const DUCKDB_CATALOG_ENTRY_TYPE_COLLATION: DuckdbCatalogEntryType = 7;
pub const DUCKDB_CATALOG_ENTRY_TYPE_TYPE: DuckdbCatalogEntryType = 8;
pub const DUCKDB_CATALOG_ENTRY_TYPE_DATABASE: DuckdbCatalogEntryType = 9;

/// What a `duckdb_catalog` points to (DuckDB's `CCatalogWrapper`).
#[derive(Debug)]
pub struct CCatalogWrapper {
    catalog: Arc<Catalog>,
}

/// `duckdb_catalog`
pub type DuckdbCatalog = *mut CCatalogWrapper;

/// What a `duckdb_catalog_entry` points to (DuckDB's `CCatalogEntryWrapper`).
#[derive(Debug)]
pub struct CCatalogEntryWrapper {
    entry: Arc<CatalogEntry>,
    /// The entry's name, kept alive for `duckdb_catalog_entry_get_name`.
    name: CString,
}

/// `duckdb_catalog_entry`
pub type DuckdbCatalogEntry = *mut CCatalogEntryWrapper;

/// `CatalogTypeFromC`; `None` for the types the engine's catalogs do not hold
/// (prepared statements and databases are not catalog entries here).
fn catalog_type_from_c(ty: DuckdbCatalogEntryType) -> Option<CatalogType> {
    Some(match ty {
        DUCKDB_CATALOG_ENTRY_TYPE_TABLE => CatalogType::Table,
        DUCKDB_CATALOG_ENTRY_TYPE_SCHEMA => CatalogType::Schema,
        DUCKDB_CATALOG_ENTRY_TYPE_VIEW => CatalogType::View,
        DUCKDB_CATALOG_ENTRY_TYPE_INDEX => CatalogType::Index,
        DUCKDB_CATALOG_ENTRY_TYPE_SEQUENCE => CatalogType::Sequence,
        DUCKDB_CATALOG_ENTRY_TYPE_COLLATION => CatalogType::Collation,
        DUCKDB_CATALOG_ENTRY_TYPE_TYPE => CatalogType::Type,
        _ => return None,
    })
}

/// `CatalogTypeToC`
fn catalog_type_to_c(ty: CatalogType) -> DuckdbCatalogEntryType {
    match ty {
        CatalogType::Table => DUCKDB_CATALOG_ENTRY_TYPE_TABLE,
        CatalogType::Schema => DUCKDB_CATALOG_ENTRY_TYPE_SCHEMA,
        CatalogType::View => DUCKDB_CATALOG_ENTRY_TYPE_VIEW,
        CatalogType::Index => DUCKDB_CATALOG_ENTRY_TYPE_INDEX,
        CatalogType::Sequence => DUCKDB_CATALOG_ENTRY_TYPE_SEQUENCE,
        CatalogType::Collation => DUCKDB_CATALOG_ENTRY_TYPE_COLLATION,
        CatalogType::Type => DUCKDB_CATALOG_ENTRY_TYPE_TYPE,
        _ => DUCKDB_CATALOG_ENTRY_TYPE_INVALID,
    }
}

/// The binder's context of the connection's open transaction, if one is open.
fn transaction_context(client: &FunctionClient) -> Option<Arc<BinderContext>> {
    client.transaction_context()?.downcast::<BinderContext>().ok()
}

/// `duckdb_catalog duckdb_client_context_get_catalog(duckdb_client_context context,
/// const char *name)` (capi/catalog-c.cpp): the attached database's catalog, NULL
/// outside a transaction or for an unknown name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_client_context_get_catalog(
    context: DuckdbClientContext,
    name: *const c_char,
) -> DuckdbCatalog {
    // SAFETY: `context` is NULL or a live handle; `name` is NULL or a C string.
    let (Some(client), Some(name)) = (unsafe { client_context_ref(context) }, unsafe { opt_str(name) })
    else {
        return std::ptr::null_mut();
    };
    if name.is_empty() {
        return std::ptr::null_mut();
    }
    let Some(binder) = transaction_context(client) else {
        return std::ptr::null_mut();
    };
    binder.catalog(name).map_or(std::ptr::null_mut(), |catalog| {
        Box::into_raw(Box::new(CCatalogWrapper {
            catalog: Arc::clone(catalog),
        }))
    })
}

/// `void duckdb_destroy_catalog(duckdb_catalog *catalog)` (capi/catalog-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_catalog(catalog: *mut DuckdbCatalog) {
    // SAFETY: `catalog` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = catalog.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `const char *duckdb_catalog_get_type_name(duckdb_catalog catalog)` (capi/catalog-c.cpp):
/// the catalogs of the engine are DuckDB catalogs (`DuckCatalog::GetCatalogType`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_catalog_get_type_name(catalog: DuckdbCatalog) -> *const c_char {
    if catalog.is_null() {
        return std::ptr::null();
    }
    c"duckdb".as_ptr()
}

/// `duckdb_catalog_entry duckdb_catalog_get_entry(duckdb_catalog catalog,
/// duckdb_client_context context, duckdb_catalog_entry_type entry_type,
/// const char *schema_name, const char *entry_name)` (capi/catalog-c.cpp): the entry as the
/// connection's transaction sees it, NULL if there is none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_catalog_get_entry(
    catalog: DuckdbCatalog,
    context: DuckdbClientContext,
    entry_type: DuckdbCatalogEntryType,
    schema_name: *const c_char,
    entry_name: *const c_char,
) -> DuckdbCatalogEntry {
    // SAFETY: the handles are NULL or live; the names are NULL or C strings.
    let (Some(catalog), Some(client), Some(schema), Some(name)) = (unsafe {
        (
            catalog.as_ref(),
            client_context_ref(context),
            opt_str(schema_name),
            opt_str(entry_name),
        )
    }) else {
        return std::ptr::null_mut();
    };
    let (Some(ty), Some(binder)) = (catalog_type_from_c(entry_type), transaction_context(client)) else {
        return std::ptr::null_mut();
    };
    catalog
        .catalog
        .get_entry(&binder.transaction, ty, schema, name)
        .map_or(std::ptr::null_mut(), |entry| {
            let name = to_cstring(entry.name());
            Box::into_raw(Box::new(CCatalogEntryWrapper { entry, name }))
        })
}

/// `duckdb_catalog_entry_type duckdb_catalog_entry_get_type(duckdb_catalog_entry entry)`
/// (capi/catalog-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_catalog_entry_get_type(entry: DuckdbCatalogEntry) -> DuckdbCatalogEntryType {
    // SAFETY: `entry` is NULL or a live handle.
    unsafe { entry.as_ref() }.map_or(DUCKDB_CATALOG_ENTRY_TYPE_INVALID, |wrapper| {
        catalog_type_to_c(wrapper.entry.catalog_type())
    })
}

/// `const char *duckdb_catalog_entry_get_name(duckdb_catalog_entry entry)`
/// (capi/catalog-c.cpp): the name lives as long as the handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_catalog_entry_get_name(entry: DuckdbCatalogEntry) -> *const c_char {
    // SAFETY: `entry` is NULL or a live handle.
    unsafe { entry.as_ref() }.map_or(std::ptr::null(), |wrapper| wrapper.name.as_ptr())
}

/// `void duckdb_destroy_catalog_entry(duckdb_catalog_entry *entry)` (capi/catalog-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_catalog_entry(entry: *mut DuckdbCatalogEntry) {
    // SAFETY: `entry` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = entry.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}
