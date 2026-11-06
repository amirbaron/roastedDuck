//! Casts from LIST, ARRAY and MAP (DuckDB's `list_casts.cpp`, `array_casts.cpp` and
//! `map_cast.cpp`), plus helpers shared by every nested cast.
//!
//! Nested kernels cast child vectors as a whole with the bound child casts and rebuild
//! the parent around the converted children. Constant inputs are processed as a single
//! row and produce constant results; dictionary inputs are flattened first.

use std::borrow::Cow;

use rustdb_common::{Error, Result};
use rustdb_types::format::{NestedQuoting, write_nested_string};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, ValidityMask, Vector, VectorKind};

use super::executor::cast_rows_to_string;
use super::{BoundCastInfo, CastFunctionSet, CastParameters};

/// A nested source made addressable by row.
pub(super) struct NestedInput<'a> {
    pub(super) vector: Cow<'a, Vector>,
    /// Rows to process: 1 for constant inputs.
    pub(super) rows: usize,
    /// Whether the result must be a constant vector.
    pub(super) constant: bool,
}

impl<'a> NestedInput<'a> {
    pub(super) fn new(source: &'a Vector, count: usize) -> Self {
        match source.kind() {
            VectorKind::Constant => Self {
                vector: Cow::Borrowed(source),
                rows: 1,
                constant: true,
            },
            VectorKind::Flat => Self {
                vector: Cow::Borrowed(source),
                rows: count,
                constant: false,
            },
            VectorKind::Dictionary | VectorKind::Sequence => Self {
                vector: Cow::Owned(source.flattened(count)),
                rows: count,
                constant: false,
            },
        }
    }

    /// Turns `result` into a constant vector if the input was constant.
    pub(super) fn finish(&self, mut result: Vector) -> Vector {
        if self.constant {
            result.make_constant();
        }
        result
    }
}

pub(super) fn missing_child(ty: &LogicalType) -> Error {
    Error::new_bytes(
        rustdb_common::ErrorKind::Internal,
        rustdb_common::byte_format!("vector of type {ty} has no child vector").into_bytes(),
    )
}

/// Casts `count` rows of `source` into a fresh vector of type `target`.
pub(super) fn cast_child(
    cast: &BoundCastInfo,
    source: &Vector,
    target: &LogicalType,
    count: usize,
    params: &mut CastParameters,
) -> Result<(Vector, bool)> {
    let mut result = Vector::flat(target.clone(), count);
    let converted = cast.execute(source, &mut result, count, params)?;
    Ok((result, converted))
}

/// How a child's text is embedded in its parent's text: nested children verbatim, others
/// quoted when needed.
pub(super) fn child_quoting(child: &LogicalType) -> Option<NestedQuoting> {
    (!child.is_nested()).then_some(NestedQuoting::Value)
}

/// Appends row `row` of the VARCHAR vector `text` as an element of a nested value's text.
pub(super) fn write_element(text: &Vector, row: usize, quoting: Option<NestedQuoting>, out: &mut Vec<u8>) {
    if text.is_null(row) {
        out.extend_from_slice(b"NULL");
        return;
    }
    let bytes = text.string_bytes(row);
    match quoting {
        Some(quoting) => write_nested_string(bytes, quoting, out),
        None => out.extend_from_slice(bytes),
    }
}

/// LIST -> LIST (and MAP -> MAP): the entries are kept, the child vector is cast.
pub(super) fn list_to_list(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = NestedInput::new(source, count);
    let target = result.logical_type().clone();
    let child_type = target.child_type().ok_or_else(|| missing_child(&target))?;
    let (child, size) = input.vector.list_child().ok_or_else(|| missing_child(&target))?;
    let (new_child, converted) = cast_child(child_cast, child, child_type, size, params)?;
    let mut out = Vector::flat(target.clone(), input.rows);
    for (entry, source_entry) in out
        .values_mut::<ListEntry>()
        .iter_mut()
        .zip(input.vector.values::<ListEntry>())
    {
        *entry = *source_entry;
    }
    out.set_validity(input.vector.validity().clone());
    out.set_list_child(new_child, size);
    *result = input.finish(out);
    Ok(converted)
}

/// LIST -> VARCHAR: `[a, 'b,c', NULL]`.
fn list_to_varchar(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let quoting = source
        .logical_type()
        .child_type()
        .map_or(Some(NestedQuoting::Value), child_quoting);
    let mut text = Vector::flat(LogicalType::list(LogicalType::VARCHAR), count);
    list_to_list(child_cast, source, &mut text, count, params)?;
    let (child, _) = text
        .list_child()
        .ok_or_else(|| missing_child(text.logical_type()))?;
    let entries = text.values::<ListEntry>();
    cast_rows_to_string(&text, result, count, params, |slot, out| {
        out.push(b'[');
        for (i, row) in entries[slot].range().enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            write_element(child, row, quoting, out);
        }
        out.push(b']');
        Ok(())
    })
}

/// LIST -> ARRAY: every list must have exactly the array size.
fn list_to_array(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = NestedInput::new(source, count);
    let target = result.logical_type().clone();
    let (child_type, size) = target.array_child().ok_or_else(|| missing_child(&target))?;
    let size = size as usize;
    let (child, _) = input.vector.list_child().ok_or_else(|| missing_child(&target))?;
    let entries = input.vector.values::<ListEntry>();
    let validity = input.vector.validity();
    let mut all_ok = true;
    let mut out_validity = ValidityMask::all_valid();
    // The child rows feeding each array element; `None` for elements of NULL arrays.
    let mut selection = Vec::with_capacity(input.rows * size);
    for (row, entry) in entries.iter().enumerate().take(input.rows) {
        let length = entry.length as usize;
        if !validity.is_valid(row) || length != size {
            if validity.is_valid(row) {
                let message = format!("Cannot cast list with length {length} to array with length {size}");
                if input.constant {
                    params.assign_error(message)?;
                    *result = Vector::constant_null(target.clone());
                    return Ok(false);
                }
                if all_ok {
                    all_ok = false;
                    params.assign_error(message)?;
                }
            }
            out_validity.set_invalid(row, input.rows);
            selection.extend(std::iter::repeat_n(None, size));
        } else {
            selection.extend(entry.range().map(|i| Some(i as u32)));
        }
    }
    let selected = child.select_rows(&selection);
    let (new_child, converted) = cast_child(child_cast, &selected, child_type, selection.len(), params)?;
    let mut out = Vector::flat(target.clone(), input.rows);
    out.set_validity(out_validity);
    out.set_array_child(new_child);
    *result = input.finish(out);
    Ok(all_ok && converted)
}

/// DuckDB's `ListCastSwitch`.
fn from_list(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let source_child = source.child_type().ok_or_else(|| missing_child(source))?;
    Ok(Some(match target.id() {
        LogicalTypeId::List => {
            let child_cast = set.get_cast_function(
                source_child,
                target.child_type().ok_or_else(|| missing_child(target))?,
            )?;
            BoundCastInfo::new(move |source, result, count, params| {
                list_to_list(&child_cast, source, result, count, params)
            })
        }
        LogicalTypeId::Varchar => {
            let child_cast = set.get_cast_function(source_child, &LogicalType::VARCHAR)?;
            BoundCastInfo::new(move |source, result, count, params| {
                list_to_varchar(&child_cast, source, result, count, params)
            })
        }
        LogicalTypeId::Array => {
            let (target_child, _) = target.array_child().ok_or_else(|| missing_child(target))?;
            let child_cast = set.get_cast_function(source_child, target_child)?;
            BoundCastInfo::new(move |source, result, count, params| {
                list_to_array(&child_cast, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}

/// ARRAY -> ARRAY: sizes must match (all rows fail otherwise).
fn array_to_array(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let target = result.logical_type().clone();
    let (Some((_, source_size)), Some((_, target_size))) =
        (source.logical_type().array_child(), target.array_child())
    else {
        return Err(missing_child(&target));
    };
    let mut all_ok = true;
    if source_size != target_size {
        params.assign_error(format!(
            "Cannot cast array of size {source_size} to array of size {target_size}"
        ))?;
        if !params.strict {
            *result = Vector::constant_null(target);
            return Ok(false);
        }
        all_ok = false;
    }
    let input = NestedInput::new(source, count);
    let child = input.vector.array_child().ok_or_else(|| missing_child(&target))?;
    let (child_type, _) = target.array_child().ok_or_else(|| missing_child(&target))?;
    let (new_child, converted) = cast_child(
        child_cast,
        child,
        child_type,
        input.rows * source_size as usize,
        params,
    )?;
    let mut out = Vector::flat(target.clone(), input.rows);
    out.set_validity(input.vector.validity().clone());
    out.set_array_child(new_child);
    *result = input.finish(out);
    Ok(all_ok && converted)
}

/// ARRAY -> LIST: each array becomes a list of the same elements.
fn array_to_list(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let target = result.logical_type().clone();
    let source = source.flattened(count);
    let size = source
        .logical_type()
        .array_child()
        .map_or(0, |(_, size)| size as usize);
    let child = source.array_child().ok_or_else(|| missing_child(&target))?;
    let child_type = target.child_type().ok_or_else(|| missing_child(&target))?;
    let (new_child, converted) = cast_child(child_cast, child, child_type, count * size, params)?;
    let mut out = Vector::flat(target.clone(), count);
    for (row, entry) in out.values_mut::<ListEntry>().iter_mut().enumerate().take(count) {
        *entry = ListEntry::new(row * size, size);
    }
    out.set_validity(source.validity().clone());
    out.set_list_child(new_child, count * size);
    // Like DuckDB, a single row produces a constant vector.
    if count == 1 {
        out.make_constant();
    }
    *result = out;
    Ok(converted)
}

/// ARRAY -> VARCHAR: `[a, b]`; unlike LIST, elements are never quoted.
fn array_to_varchar(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let size = source.logical_type().array_child().map_or(0, |(_, size)| size);
    let mut text = Vector::flat(LogicalType::array(LogicalType::VARCHAR, size), count);
    array_to_array(child_cast, source, &mut text, count, params)?;
    let size = size as usize;
    let child = text
        .array_child()
        .ok_or_else(|| missing_child(text.logical_type()))?;
    cast_rows_to_string(&text, result, count, params, |slot, out| {
        out.push(b'[');
        for i in 0..size {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            write_element(child, slot * size + i, None, out);
        }
        out.push(b']');
        Ok(())
    })
}

/// DuckDB's `ArrayCastSwitch`.
fn from_array(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let (source_child, _) = source.array_child().ok_or_else(|| missing_child(source))?;
    Ok(Some(match target.id() {
        LogicalTypeId::Varchar => {
            let child_cast = set.get_cast_function(source_child, &LogicalType::VARCHAR)?;
            BoundCastInfo::new(move |source, result, count, params| {
                array_to_varchar(&child_cast, source, result, count, params)
            })
        }
        LogicalTypeId::Array => {
            let (target_child, _) = target.array_child().ok_or_else(|| missing_child(target))?;
            let child_cast = set.get_cast_function(source_child, target_child)?;
            BoundCastInfo::new(move |source, result, count, params| {
                array_to_array(&child_cast, source, result, count, params)
            })
        }
        LogicalTypeId::List => {
            let child_cast = set.get_cast_function(
                source_child,
                target.child_type().ok_or_else(|| missing_child(target))?,
            )?;
            BoundCastInfo::new(move |source, result, count, params| {
                array_to_list(&child_cast, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}

/// MAP -> VARCHAR: `{key=value, ...}`.
fn map_to_varchar(
    entry_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let (key_quoting, value_quoting) = match source.logical_type().map_key_value() {
        Some((key, value)) => (child_quoting(key), child_quoting(value)),
        None => return Err(missing_child(source.logical_type())),
    };
    let mut text = Vector::flat(
        LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
        count,
    );
    list_to_list(entry_cast, source, &mut text, count, params)?;
    let (entries_vector, _) = text
        .list_child()
        .ok_or_else(|| missing_child(text.logical_type()))?;
    let Some([keys, values]) = entries_vector.struct_children() else {
        return Err(missing_child(text.logical_type()));
    };
    let entries = text.values::<ListEntry>();
    cast_rows_to_string(&text, result, count, params, |slot, out| {
        out.push(b'{');
        for (i, row) in entries[slot].range().enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            if entries_vector.is_null(row) {
                out.extend_from_slice(b"NULL");
            } else if keys.is_null(row) {
                out.extend_from_slice(b"invalid");
            } else {
                write_element(keys, row, key_quoting, out);
                out.push(b'=');
                write_element(values, row, value_quoting, out);
            }
        }
        out.push(b'}');
        Ok(())
    })
}

/// DuckDB's `MapCastSwitch`.
fn from_map(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let source_entry = source.child_type().ok_or_else(|| missing_child(source))?;
    Ok(Some(match target.id() {
        LogicalTypeId::Map => {
            let target_entry = target.child_type().ok_or_else(|| missing_child(target))?;
            let entry_cast = set.get_cast_function(source_entry, target_entry)?;
            BoundCastInfo::new(move |source, result, count, params| {
                list_to_list(&entry_cast, source, result, count, params)
            })
        }
        LogicalTypeId::Varchar => {
            let text_type = LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR);
            let text_entry = text_type.child_type().ok_or_else(|| missing_child(&text_type))?;
            let entry_cast = set.get_cast_function(source_entry, text_entry)?;
            BoundCastInfo::new(move |source, result, count, params| {
                map_to_varchar(&entry_cast, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}

/// Casts whose source is LIST, ARRAY or MAP.
pub(super) fn cast_switch(
    set: &CastFunctionSet,
    source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    match source.id() {
        LogicalTypeId::List => from_list(set, source, target),
        LogicalTypeId::Array => from_array(set, source, target),
        LogicalTypeId::Map => from_map(set, source, target),
        _ => Ok(None),
    }
}
