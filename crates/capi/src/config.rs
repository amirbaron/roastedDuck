//! Database configuration (DuckDB's `capi/config-c.cpp`).

use std::ffi::{CString, c_char};
use std::sync::OnceLock;

use roastedduck::Config;
use roastedduck_types::Value;

use crate::helper::{lossy_str, to_cstring};
use crate::types::DuckdbState;

/// What a `duckdb_config` points to (DuckDB's `DBConfig`).
#[derive(Debug)]
pub struct ConfigWrapper {
    pub(crate) config: Config,
}

/// `duckdb_config`
pub type DuckdbConfig = *mut ConfigWrapper;

/// A configuration that tells the database it is used through the C API
/// (`SetOptionByName("duckdb_api", "capi")`), which shows in `PRAGMA user_agent`.
pub(crate) fn capi_config() -> Config {
    Config::default().with_option("duckdb_api", Value::varchar("capi"))
}

/// The names and descriptions `duckdb_get_config_flag` hands out, as C strings that live
/// as long as the library.
fn config_flags() -> &'static [(CString, CString)] {
    static FLAGS: OnceLock<Vec<(CString, CString)>> = OnceLock::new();
    FLAGS.get_or_init(|| {
        roastedduck::config_flags()
            .into_iter()
            .map(|(name, description)| (to_cstring(name), to_cstring(description)))
            .collect()
    })
}

/// `duckdb_state duckdb_create_config(duckdb_config *out_config)` (capi/config-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_config(out_config: *mut DuckdbConfig) -> DuckdbState {
    if out_config.is_null() {
        return DuckdbState::DuckDBError;
    }
    let wrapper = ConfigWrapper {
        config: capi_config(),
    };
    // SAFETY: checked non-null above.
    unsafe { *out_config = Box::into_raw(Box::new(wrapper)) };
    DuckdbState::DuckDBSuccess
}

/// `size_t duckdb_config_count()` (capi/config-c.cpp): the options, their aliases and the
/// options of known extensions.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_config_count() -> usize {
    config_flags().len()
}

/// `duckdb_state duckdb_get_config_flag(size_t index, const char **out_name,
/// const char **out_description)` (capi/config-c.cpp): the strings live as long as the
/// library; an extension's option is described by the extension's name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_config_flag(
    index: usize,
    out_name: *mut *const c_char,
    out_description: *mut *const c_char,
) -> DuckdbState {
    let Some((name, description)) = config_flags().get(index) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: the outputs are NULL or writable per the API contract.
    unsafe {
        if let Some(out) = out_name.as_mut() {
            *out = name.as_ptr();
        }
        if let Some(out) = out_description.as_mut() {
            *out = description.as_ptr();
        }
    }
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_set_config(duckdb_config config, const char *name, const char *option)`
/// (capi/config-c.cpp): fails for a value the option does not accept; an unknown name is
/// accepted here and fails the open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_set_config(
    config: DuckdbConfig,
    name: *const c_char,
    option: *const c_char,
) -> DuckdbState {
    // SAFETY: `config` is NULL or a live handle.
    let Some(wrapper) = (unsafe { config.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    if name.is_null() || option.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: both are NUL-terminated strings per the API contract.
    let (name, option) = unsafe { (lossy_str(name), lossy_str(option)) };
    DuckdbState::from_ok(
        wrapper
            .config
            .set_option_by_name(&name, Value::varchar(option))
            .is_ok(),
    )
}

/// `void duckdb_destroy_config(duckdb_config *config)` (capi/config-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_config(config: *mut DuckdbConfig) {
    // SAFETY: `config` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = config.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}
