//! Logical types (DuckDB's `capi/logical_types-c.cpp`).
//!
//! A `duckdb_logical_type` points to a heap-allocated [`LogicalType`]; functions that
//! return one hand over a new allocation for `duckdb_destroy_logical_type`, except
//! `duckdb_get_value_type`, which lends the value's own type.

use std::ffi::c_char;

use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, TypeInfo};

use crate::helper::{c_string, logical_type_id_from_c, logical_type_id_to_c, lossy_str};
use crate::types::*;

/// `duckdb_logical_type`
pub type DuckdbLogicalType = *mut LogicalType;

/// `ArrayType::MAX_ARRAY_SIZE`
pub(crate) const MAX_ARRAY_SIZE: Idx = 100_000;

/// Hands `ty` to C.
pub(crate) fn wrap_type(ty: LogicalType) -> DuckdbLogicalType {
    Box::into_raw(Box::new(ty))
}

/// Borrows the type behind a handle.
///
/// # Safety
/// `ty` must be NULL or a live `duckdb_logical_type`.
pub(crate) unsafe fn type_ref<'a>(ty: DuckdbLogicalType) -> Option<&'a LogicalType> {
    // SAFETY: guaranteed by the caller.
    unsafe { ty.as_ref() }
}

/// `type` if its id is `id` (`AssertLogicalTypeId`).
///
/// # Safety
/// As for [`type_ref`].
unsafe fn type_with_id<'a>(ty: DuckdbLogicalType, id: LogicalTypeId) -> Option<&'a LogicalType> {
    // SAFETY: forwarded.
    unsafe { type_ref(ty) }.filter(|ty| ty.id() == id)
}

/// `type` if it is stored as a STRUCT, i.e. a STRUCT or UNION (`AssertInternalType`).
///
/// # Safety
/// As for [`type_ref`].
unsafe fn struct_like<'a>(ty: DuckdbLogicalType) -> Option<&'a LogicalType> {
    // SAFETY: forwarded.
    unsafe { type_ref(ty) }.filter(|ty| ty.physical_type() == PhysicalType::Struct)
}

/// Whether `ty` or a type nested in it has id `id` (`TypeVisitor::Contains`).
pub(crate) fn contains_type(ty: &LogicalType, id: LogicalTypeId) -> bool {
    if ty.id() == id {
        return true;
    }
    match ty.info() {
        Some(TypeInfo::Struct(info)) => info.child_types.iter().any(|(_, child)| contains_type(child, id)),
        Some(TypeInfo::List(info)) => contains_type(&info.child_type, id),
        Some(TypeInfo::Array(info)) => contains_type(&info.child_type, id),
        _ => false,
    }
}

/// Whether `ty` cannot be instantiated: it contains INVALID or ANY.
pub(crate) fn is_unresolved(ty: &LogicalType) -> bool {
    contains_type(ty, LogicalTypeId::Invalid) || contains_type(ty, LogicalTypeId::Any)
}

/// Reads `count` (name, type) pairs from C arrays; `None` if one is NULL.
///
/// # Safety
/// `types` and `names` must point to `count` entries each.
unsafe fn members(
    types: *mut DuckdbLogicalType,
    names: *const *const c_char,
    count: Idx,
) -> Option<Vec<(String, LogicalType)>> {
    (0..count as usize)
        .map(|i| {
            // SAFETY: both arrays hold `count` entries per the caller.
            let (ty, name) = unsafe { (*types.add(i), *names.add(i)) };
            // SAFETY: the entries are live handles and C strings, or NULL.
            let ty = unsafe { type_ref(ty) }?;
            (!name.is_null()).then(|| {
                // SAFETY: `name` is a NUL-terminated string.
                (unsafe { lossy_str(name) }, ty.clone())
            })
        })
        .collect()
}

/// `duckdb_logical_type duckdb_create_logical_type(duckdb_type type)`
/// (capi/logical_types-c.cpp): types with parameters (DECIMAL, ENUM, LIST, STRUCT, MAP,
/// ARRAY, UNION) have their own constructors and give INVALID here.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_logical_type(ty: DuckdbType) -> DuckdbLogicalType {
    let ty = match ty {
        DUCKDB_TYPE_DECIMAL | DUCKDB_TYPE_ENUM | DUCKDB_TYPE_LIST | DUCKDB_TYPE_STRUCT | DUCKDB_TYPE_MAP
        | DUCKDB_TYPE_ARRAY | DUCKDB_TYPE_UNION => DUCKDB_TYPE_INVALID,
        other => other,
    };
    wrap_type(LogicalType::simple(logical_type_id_from_c(ty)))
}

/// `duckdb_logical_type duckdb_create_list_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_list_type(ty: DuckdbLogicalType) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_ref(ty) }.map_or(std::ptr::null_mut(), |child| {
        wrap_type(LogicalType::list(child.clone()))
    })
}

/// `duckdb_logical_type duckdb_create_array_type(duckdb_logical_type type, idx_t array_size)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_array_type(
    ty: DuckdbLogicalType,
    array_size: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    let Some(child) = (unsafe { type_ref(ty) }) else {
        return std::ptr::null_mut();
    };
    if array_size >= MAX_ARRAY_SIZE {
        return std::ptr::null_mut();
    }
    wrap_type(LogicalType::array(child.clone(), array_size as u32))
}

/// `duckdb_logical_type duckdb_create_union_type(duckdb_logical_type *member_types,
/// const char **member_names, idx_t member_count)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_union_type(
    member_types: *mut DuckdbLogicalType,
    member_names: *const *const c_char,
    member_count: Idx,
) -> DuckdbLogicalType {
    if member_types.is_null() || member_names.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: both arrays hold `member_count` entries per the API contract.
    unsafe { members(member_types, member_names, member_count) }.map_or(std::ptr::null_mut(), |members| {
        wrap_type(LogicalType::union_type(members))
    })
}

/// `duckdb_logical_type duckdb_create_struct_type(duckdb_logical_type *member_types,
/// const char **member_names, idx_t member_count)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_struct_type(
    member_types: *mut DuckdbLogicalType,
    member_names: *const *const c_char,
    member_count: Idx,
) -> DuckdbLogicalType {
    if member_types.is_null() || member_names.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: both arrays hold `member_count` entries per the API contract.
    unsafe { members(member_types, member_names, member_count) }.map_or(std::ptr::null_mut(), |members| {
        wrap_type(LogicalType::struct_type(members))
    })
}

/// `duckdb_logical_type duckdb_create_enum_type(const char **member_names, idx_t member_count)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_enum_type(
    member_names: *const *const c_char,
    member_count: Idx,
) -> DuckdbLogicalType {
    if member_names.is_null() {
        return std::ptr::null_mut();
    }
    let names: Option<Vec<String>> = (0..member_count as usize)
        .map(|i| {
            // SAFETY: the array holds `member_count` entries per the API contract.
            let name = unsafe { *member_names.add(i) };
            // SAFETY: a non-NULL entry is a NUL-terminated string.
            (!name.is_null()).then(|| unsafe { lossy_str(name) })
        })
        .collect();
    names.map_or(std::ptr::null_mut(), |names| {
        wrap_type(LogicalType::enum_type(names))
    })
}

/// `duckdb_logical_type duckdb_create_map_type(duckdb_logical_type key_type,
/// duckdb_logical_type value_type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_map_type(
    key_type: DuckdbLogicalType,
    value_type: DuckdbLogicalType,
) -> DuckdbLogicalType {
    // SAFETY: both are NULL or live handles.
    match unsafe { (type_ref(key_type), type_ref(value_type)) } {
        (Some(key), Some(value)) => wrap_type(LogicalType::map(key.clone(), value.clone())),
        _ => std::ptr::null_mut(),
    }
}

/// `duckdb_logical_type duckdb_create_decimal_type(uint8_t width, uint8_t scale)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_decimal_type(width: u8, scale: u8) -> DuckdbLogicalType {
    if !(1..=LogicalType::MAX_DECIMAL_WIDTH).contains(&width) || scale > width {
        return std::ptr::null_mut();
    }
    wrap_type(LogicalType::decimal(width, scale))
}

/// `duckdb_type duckdb_get_type_id(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_type_id(ty: DuckdbLogicalType) -> DuckdbType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_ref(ty) }.map_or(DUCKDB_TYPE_INVALID, |ty| logical_type_id_to_c(ty.id()))
}

/// `void duckdb_destroy_logical_type(duckdb_logical_type *type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_logical_type(ty: *mut DuckdbLogicalType) {
    // SAFETY: `ty` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = ty.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `uint8_t duckdb_decimal_width(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_decimal_width(ty: DuckdbLogicalType) -> u8 {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Decimal) }
        .and_then(LogicalType::decimal_properties)
        .map_or(0, |(width, _)| width)
}

/// `uint8_t duckdb_decimal_scale(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_decimal_scale(ty: DuckdbLogicalType) -> u8 {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Decimal) }
        .and_then(LogicalType::decimal_properties)
        .map_or(0, |(_, scale)| scale)
}

/// `duckdb_type duckdb_decimal_internal_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_decimal_internal_type(ty: DuckdbLogicalType) -> DuckdbType {
    // SAFETY: `ty` is NULL or a live handle.
    match unsafe { type_with_id(ty, LogicalTypeId::Decimal) }.map(LogicalType::physical_type) {
        Some(PhysicalType::Int16) => DUCKDB_TYPE_SMALLINT,
        Some(PhysicalType::Int32) => DUCKDB_TYPE_INTEGER,
        Some(PhysicalType::Int64) => DUCKDB_TYPE_BIGINT,
        Some(PhysicalType::Int128) => DUCKDB_TYPE_HUGEINT,
        _ => DUCKDB_TYPE_INVALID,
    }
}

/// `duckdb_type duckdb_enum_internal_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_enum_internal_type(ty: DuckdbLogicalType) -> DuckdbType {
    // SAFETY: `ty` is NULL or a live handle.
    match unsafe { type_with_id(ty, LogicalTypeId::Enum) }.map(LogicalType::physical_type) {
        Some(PhysicalType::UInt8) => DUCKDB_TYPE_UTINYINT,
        Some(PhysicalType::UInt16) => DUCKDB_TYPE_USMALLINT,
        Some(PhysicalType::UInt32) => DUCKDB_TYPE_UINTEGER,
        _ => DUCKDB_TYPE_INVALID,
    }
}

/// `uint32_t duckdb_enum_dictionary_size(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_enum_dictionary_size(ty: DuckdbLogicalType) -> u32 {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Enum) }
        .and_then(LogicalType::enum_values)
        .map_or(0, |values| values.len() as u32)
}

/// `char *duckdb_enum_dictionary_value(duckdb_logical_type type, idx_t index)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_enum_dictionary_value(ty: DuckdbLogicalType, index: Idx) -> *mut c_char {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Enum) }
        .and_then(LogicalType::enum_values)
        .and_then(|values| values.get(index as usize))
        .map_or(std::ptr::null_mut(), c_string)
}

/// `duckdb_logical_type duckdb_list_type_child_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp): the element type of a LIST, or the `STRUCT(key, value)`
/// entry type of a MAP.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_list_type_child_type(ty: DuckdbLogicalType) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_ref(ty) }
        .filter(|ty| matches!(ty.id(), LogicalTypeId::List | LogicalTypeId::Map))
        .and_then(LogicalType::child_type)
        .map_or(std::ptr::null_mut(), |child| wrap_type(child.clone()))
}

/// `duckdb_logical_type duckdb_array_type_child_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_array_type_child_type(ty: DuckdbLogicalType) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Array) }
        .and_then(LogicalType::array_child)
        .map_or(std::ptr::null_mut(), |(child, _)| wrap_type(child.clone()))
}

/// `idx_t duckdb_array_type_array_size(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_array_type_array_size(ty: DuckdbLogicalType) -> Idx {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Array) }
        .and_then(LogicalType::array_child)
        .map_or(0, |(_, size)| Idx::from(size))
}

/// `duckdb_logical_type duckdb_map_type_key_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_map_type_key_type(ty: DuckdbLogicalType) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Map) }
        .and_then(LogicalType::map_key_value)
        .map_or(std::ptr::null_mut(), |(key, _)| wrap_type(key.clone()))
}

/// `duckdb_logical_type duckdb_map_type_value_type(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_map_type_value_type(ty: DuckdbLogicalType) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Map) }
        .and_then(LogicalType::map_key_value)
        .map_or(std::ptr::null_mut(), |(_, value)| wrap_type(value.clone()))
}

/// `idx_t duckdb_struct_type_child_count(duckdb_logical_type type)`
/// (capi/logical_types-c.cpp): the fields of a STRUCT, or of the struct a UNION is
/// stored as (its tag included).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_struct_type_child_count(ty: DuckdbLogicalType) -> Idx {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { struct_like(ty) }
        .and_then(LogicalType::struct_children)
        .map_or(0, |children| children.len() as Idx)
}

/// `char *duckdb_struct_type_child_name(duckdb_logical_type type, idx_t index)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_struct_type_child_name(ty: DuckdbLogicalType, index: Idx) -> *mut c_char {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { struct_like(ty) }
        .and_then(LogicalType::struct_children)
        .and_then(|children| children.get(index as usize))
        .map_or(std::ptr::null_mut(), |(name, _)| c_string(name))
}

/// `duckdb_logical_type duckdb_struct_type_child_type(duckdb_logical_type type, idx_t index)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_struct_type_child_type(
    ty: DuckdbLogicalType,
    index: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { struct_like(ty) }
        .and_then(LogicalType::struct_children)
        .and_then(|children| children.get(index as usize))
        .map_or(std::ptr::null_mut(), |(_, child)| wrap_type(child.clone()))
}

/// `idx_t duckdb_union_type_member_count(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_union_type_member_count(ty: DuckdbLogicalType) -> Idx {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Union) }
        .and_then(LogicalType::union_members)
        .map_or(0, |members| members.len() as Idx)
}

/// `char *duckdb_union_type_member_name(duckdb_logical_type type, idx_t index)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_union_type_member_name(ty: DuckdbLogicalType, index: Idx) -> *mut c_char {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Union) }
        .and_then(LogicalType::union_members)
        .and_then(|members| members.get(index as usize))
        .map_or(std::ptr::null_mut(), |(name, _)| c_string(name))
}

/// `duckdb_logical_type duckdb_union_type_member_type(duckdb_logical_type type, idx_t index)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_union_type_member_type(
    ty: DuckdbLogicalType,
    index: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_with_id(ty, LogicalTypeId::Union) }
        .and_then(LogicalType::union_members)
        .and_then(|members| members.get(index as usize))
        .map_or(std::ptr::null_mut(), |(_, member)| wrap_type(member.clone()))
}

/// `char *duckdb_geometry_type_get_crs(duckdb_logical_type type)` (capi/logical_types-c.cpp):
/// the coordinate reference system of a GEOMETRY type, NULL if it has none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_geometry_type_get_crs(ty: DuckdbLogicalType) -> *mut c_char {
    // SAFETY: `ty` is NULL or a live handle.
    match unsafe { type_with_id(ty, LogicalTypeId::Geometry) }.and_then(LogicalType::info) {
        Some(TypeInfo::Geo(info)) if !info.crs.definition.is_empty() => c_string(&info.crs.definition),
        _ => std::ptr::null_mut(),
    }
}

/// `char *duckdb_logical_type_get_alias(duckdb_logical_type type)` (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_logical_type_get_alias(ty: DuckdbLogicalType) -> *mut c_char {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_ref(ty) }
        .and_then(LogicalType::alias)
        .map_or(std::ptr::null_mut(), c_string)
}

/// `void duckdb_logical_type_set_alias(duckdb_logical_type type, const char *alias)`
/// (capi/logical_types-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_logical_type_set_alias(ty: DuckdbLogicalType, alias: *const c_char) {
    // SAFETY: `ty` is NULL or a live handle owned by the caller.
    let Some(ty) = (unsafe { ty.as_mut() }) else {
        return;
    };
    if alias.is_null() {
        return;
    }
    // SAFETY: `alias` is a NUL-terminated string.
    let alias = unsafe { lossy_str(alias) };
    *ty = ty.clone().with_alias(alias);
}

/// `duckdb_create_type_info`: reserved by `duckdb_register_logical_type`.
pub type DuckdbCreateTypeInfo = *mut std::ffi::c_void;

/// `duckdb_state duckdb_register_logical_type(duckdb_connection con, duckdb_logical_type
/// type, duckdb_create_type_info info)` (capi/logical_types-c.cpp): creates a type named
/// after the type's alias, in the system catalog; `info` is unused.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_logical_type(
    con: crate::open::DuckdbConnection,
    ty: DuckdbLogicalType,
    _info: DuckdbCreateTypeInfo,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(context), Some(ty)) = (unsafe { (crate::open::connection_ref(con), type_ref(ty)) }) else {
        return DuckdbState::DuckDBError;
    };
    let Some(name) = ty.alias() else {
        return DuckdbState::DuckDBError;
    };
    if is_unresolved(ty) {
        return DuckdbState::DuckDBError;
    }
    DuckdbState::from_ok(crate::helper::catch(|| context.lock().create_type(name, ty.clone())).is_ok())
}
