//! Configuration options defined by the client (DuckDB's `capi/config_options-c.cpp`).

use std::ffi::c_char;

use roastedduck_function::cast::ValueCast;
use roastedduck_function::{ExtensionOption, SettingScope};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

use crate::helper::{catch, opt_str};
use crate::logical_types::{DuckdbLogicalType, type_ref};
use crate::open::{DuckdbClientContext, DuckdbConnection, client_context_ref, connection_ref};
use crate::types::*;
use crate::value::{DuckdbValue, value_ref, wrap_value};

/// `duckdb_config_option_scope`
pub type DuckdbConfigOptionScope = u32;

pub const DUCKDB_CONFIG_OPTION_SCOPE_INVALID: DuckdbConfigOptionScope = 0;
pub const DUCKDB_CONFIG_OPTION_SCOPE_LOCAL: DuckdbConfigOptionScope = 1;
pub const DUCKDB_CONFIG_OPTION_SCOPE_SESSION: DuckdbConfigOptionScope = 2;
pub const DUCKDB_CONFIG_OPTION_SCOPE_GLOBAL: DuckdbConfigOptionScope = 3;

/// What a `duckdb_config_option` points to (DuckDB's `CConfigOption`).
#[derive(Debug)]
pub struct CConfigOption {
    name: String,
    ty: LogicalType,
    default_value: Value,
    /// The scope `SET` without one uses: the session (`SESSION` or `LOCAL`) or
    /// `GLOBAL`.
    global_by_default: bool,
    description: String,
}

/// `duckdb_config_option`
pub type DuckdbConfigOption = *mut CConfigOption;

/// The option behind a handle.
///
/// # Safety
/// `option` must be NULL or a live `duckdb_config_option`.
unsafe fn definition<'a>(option: DuckdbConfigOption) -> Option<&'a mut CConfigOption> {
    // SAFETY: guaranteed by the caller.
    unsafe { option.as_mut() }
}

/// `duckdb_config_option duckdb_create_config_option()` (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_config_option() -> DuckdbConfigOption {
    Box::into_raw(Box::new(CConfigOption {
        name: String::new(),
        ty: LogicalType::INVALID,
        default_value: Value::default(),
        global_by_default: false,
        description: String::new(),
    }))
}

/// `void duckdb_destroy_config_option(duckdb_config_option *option)`
/// (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_config_option(option: *mut DuckdbConfigOption) {
    // SAFETY: `option` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = option.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_config_option_set_name(duckdb_config_option option, const char *name)`
/// (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_config_option_set_name(option: DuckdbConfigOption, name: *const c_char) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(option), Some(name)) = unsafe { (definition(option), opt_str(name)) } {
        name.clone_into(&mut option.name);
    }
}

/// `void duckdb_config_option_set_type(duckdb_config_option option, duckdb_logical_type
/// type)` (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_config_option_set_type(option: DuckdbConfigOption, ty: DuckdbLogicalType) {
    // SAFETY: both handles are NULL or live.
    if let (Some(option), Some(ty)) = unsafe { (definition(option), type_ref(ty)) } {
        option.ty = ty.clone();
    }
}

/// `void duckdb_config_option_set_default_value(duckdb_config_option option, duckdb_value
/// default_value)` (capi/config_options-c.cpp): without a type yet, the option takes the
/// value's; otherwise the value is cast to it (and kept as it was if it cannot be).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_config_option_set_default_value(
    option: DuckdbConfigOption,
    default_value: DuckdbValue,
) {
    // SAFETY: both handles are NULL or live.
    let (Some(option), Some(value)) = (unsafe { definition(option) }, unsafe { value_ref(default_value) })
    else {
        return;
    };
    if option.ty.id() == LogicalTypeId::Invalid {
        option.ty = value.logical_type().clone();
        option.default_value = value.clone();
    } else if let Ok(cast) = catch(|| value.default_cast_as(&option.ty)) {
        option.default_value = cast;
    }
}

/// `void duckdb_config_option_set_default_scope(duckdb_config_option option,
/// duckdb_config_option_scope scope)` (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_config_option_set_default_scope(
    option: DuckdbConfigOption,
    scope: DuckdbConfigOptionScope,
) {
    // SAFETY: the handle is NULL or live.
    let Some(option) = (unsafe { definition(option) }) else {
        return;
    };
    match scope {
        DUCKDB_CONFIG_OPTION_SCOPE_LOCAL | DUCKDB_CONFIG_OPTION_SCOPE_SESSION => {
            option.global_by_default = false;
        }
        DUCKDB_CONFIG_OPTION_SCOPE_GLOBAL => option.global_by_default = true,
        _ => {}
    }
}

/// `void duckdb_config_option_set_description(duckdb_config_option option,
/// const char *description)` (capi/config_options-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_config_option_set_description(
    option: DuckdbConfigOption,
    description: *const c_char,
) {
    // SAFETY: the handle is NULL or live; `description` is NULL or a C string.
    if let (Some(option), Some(description)) = unsafe { (definition(option), opt_str(description)) } {
        description.clone_into(&mut option.description);
    }
}

/// `duckdb_state duckdb_register_config_option(duckdb_connection connection,
/// duckdb_config_option option)` (capi/config_options-c.cpp): the option needs a name and
/// a type, and no option of that name may exist. Like DuckDB's, the registration is not
/// transactional.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_config_option(
    connection: DuckdbConnection,
    option: DuckdbConfigOption,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(context), Some(option)) = (unsafe { connection_ref(connection) }, unsafe { option.as_ref() })
    else {
        return DuckdbState::DuckDBError;
    };
    if option.name.is_empty() || option.ty.id() == LogicalTypeId::Invalid {
        return DuckdbState::DuckDBError;
    }
    let extension_option = ExtensionOption {
        name: option.name.clone(),
        description: option.description.clone(),
        input_type: option.ty.clone(),
        default_value: option.default_value.clone(),
        set_function: None,
        global_by_default: option.global_by_default,
    };
    let added = catch(|| Ok(context.lock().database().add_extension_option(extension_option)));
    DuckdbState::from_ok(added.unwrap_or(false))
}

/// `duckdb_value duckdb_client_context_get_config_option(duckdb_client_context context,
/// const char *name, duckdb_config_option_scope *out_scope)` (capi/config_options-c.cpp):
/// the option's current value and scope (`SESSION` for the connection's own value), NULL
/// and `INVALID` for an unknown option.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_client_context_get_config_option(
    context: DuckdbClientContext,
    name: *const c_char,
    out_scope: *mut DuckdbConfigOptionScope,
) -> DuckdbValue {
    // SAFETY: `context` is NULL or a live handle; `name` is NULL or a C string.
    let (Some(client), Some(name)) = (unsafe { client_context_ref(context) }, unsafe { opt_str(name) })
    else {
        return std::ptr::null_mut();
    };
    let setting = client
        .settings()
        .and_then(|settings| settings.current_setting_scope(name));
    let (value, scope) = match setting {
        Some((value, SettingScope::Local)) => (wrap_value(value), DUCKDB_CONFIG_OPTION_SCOPE_SESSION),
        Some((value, SettingScope::Global)) => (wrap_value(value), DUCKDB_CONFIG_OPTION_SCOPE_GLOBAL),
        None => (std::ptr::null_mut(), DUCKDB_CONFIG_OPTION_SCOPE_INVALID),
    };
    if !out_scope.is_null() {
        // SAFETY: checked non-null; writable per the API contract.
        unsafe { *out_scope = scope };
    }
    value
}
