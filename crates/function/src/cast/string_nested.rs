//! VARCHAR -> LIST/ARRAY/STRUCT/MAP (DuckDB's `StringToNestedTypeCast`): the text is split
//! into VARCHAR children (see [`super::split`]), which are then cast to the child types.

use rustdb_common::{Error, Result};
use rustdb_types::{ListEntry, LogicalType, StringT, ValidityMask, Value, Vector, VectorKind};

use super::executor::StringColumn;
use super::nested::{cast_child, missing_child};
use super::split::{MapPart, StructFields, count_list_parts, split_list, split_map, split_struct};
use super::structs::StructCastPlan;
use super::{BoundCastInfo, CastFunctionSet, CastParameters};

/// The rows of a VARCHAR source: `(rows, constant)` plus row access.
struct TextInput<'a> {
    source: &'a Vector,
    strings: &'a [StringT],
    rows: usize,
    constant: bool,
}

impl<'a> TextInput<'a> {
    fn new(source: &'a Vector, count: usize) -> Self {
        let constant = source.kind() == VectorKind::Constant;
        Self {
            source,
            strings: source.values::<StringT>(),
            rows: if constant { 1 } else { count },
            constant,
        }
    }

    /// The text of row `row`, or `None` if it is NULL.
    fn text(&self, row: usize) -> Option<&'a [u8]> {
        let slot = self.source.sel().slot(row);
        self.source
            .validity()
            .is_valid(slot)
            .then(|| self.source.string_value(&self.strings[slot]))
    }

    fn finish(&self, mut result: Vector) -> Vector {
        if self.constant {
            result.make_constant();
        }
        result
    }
}

/// `Type VARCHAR with value '...' can't be cast to the destination type ...`.
fn cannot_cast(text: &[u8], target: &LogicalType) -> String {
    format!(
        "Type VARCHAR with value '{}' can't be cast to the destination type {target}",
        String::from_utf8_lossy(text)
    )
}

/// VARCHAR -> LIST (DuckDB's `VectorStringToList`).
fn string_to_list(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = TextInput::new(source, count);
    let target = result.logical_type().clone();
    let child_type = target.child_type().ok_or_else(|| missing_child(&target))?;
    let mut parts = StringColumn::new();
    let mut scratch = Vec::new();
    let mut entries = Vec::with_capacity(input.rows);
    let mut validity = ValidityMask::all_valid();
    let mut all_converted = true;
    for row in 0..input.rows {
        let offset = parts.len();
        match input.text(row) {
            None => validity.set_invalid(row, input.rows),
            Some(text) => {
                if !split_list(text, &mut scratch, |element| parts.push(element)) {
                    params.assign_error(cannot_cast(text, &target))?;
                    all_converted = false;
                    validity.set_invalid(row, input.rows);
                }
            }
        }
        entries.push(ListEntry::new(offset, parts.len() - offset));
    }
    let total = parts.len();
    let parts = parts.finish();
    let (child, converted) = cast_child(child_cast, &parts, child_type, total, params)?;
    if !converted && params.nullify_parent {
        // A row with an element that failed to convert becomes NULL.
        for (row, entry) in entries.iter().enumerate() {
            let range = entry.offset as usize..(entry.offset + entry.length) as usize;
            if validity.is_valid(row) && range.clone().any(|j| child.is_null(j) && !parts.is_null(j)) {
                validity.set_invalid(row, input.rows);
            }
        }
    }
    let mut out = Vector::flat(target.clone(), input.rows);
    out.values_mut::<ListEntry>()[..input.rows].copy_from_slice(&entries);
    out.set_validity(validity);
    out.set_list_child(child, total);
    *result = input.finish(out);
    Ok(converted && all_converted)
}

/// VARCHAR -> ARRAY (DuckDB's `VectorStringToArray`): every value needs exactly the array
/// size elements.
fn string_to_array(
    child_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = TextInput::new(source, count);
    let target = result.logical_type().clone();
    let (child_type, size) = target.array_child().ok_or_else(|| missing_child(&target))?;
    let size = size as usize;
    let mut validity = ValidityMask::all_valid();
    let mut all_lengths_match = true;
    for row in 0..input.rows {
        let Some(text) = input.text(row) else { continue };
        if count_list_parts(text) != size {
            if all_lengths_match {
                all_lengths_match = false;
                let message = format!(
                    "{}, the size of the array must match the destination type",
                    cannot_cast(text, &target)
                );
                if params.strict {
                    return Err(Error::conversion(message));
                }
                params.assign_error(message)?;
            }
            validity.set_invalid(row, input.rows);
        }
    }
    let mut parts = StringColumn::new();
    let mut scratch = Vec::new();
    let mut all_converted = true;
    for row in 0..input.rows {
        let start = parts.len();
        match input.text(row) {
            Some(text) if validity.is_valid(row) => {
                if !split_list(text, &mut scratch, |element| parts.push(element)) {
                    params.assign_error(cannot_cast(text, &target))?;
                    all_converted = false;
                    validity.set_invalid(row, input.rows);
                }
            }
            _ => validity.set_invalid(row, input.rows),
        }
        // Keep every array at exactly `size` children.
        while parts.len() < start + size {
            parts.push(None);
        }
    }
    let total = input.rows * size;
    let (child, converted) = cast_child(child_cast, &parts.finish(), child_type, total, params)?;
    let mut out = Vector::flat(target.clone(), input.rows);
    out.set_validity(validity);
    out.set_array_child(child);
    *result = input.finish(out);
    Ok(all_lengths_match && converted && all_converted)
}

/// VARCHAR -> STRUCT (DuckDB's `VectorStringToStruct`).
fn string_to_struct(
    plan: &StructCastPlan,
    text_type: &LogicalType,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = TextInput::new(source, count);
    let target = result.logical_type().clone();
    let fields = target.struct_children().ok_or_else(|| missing_child(&target))?;
    let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    let layout = if target.is_unnamed_struct() {
        StructFields::Unnamed(fields.len())
    } else {
        StructFields::Named(&names)
    };
    let mut columns: Vec<StringColumn> = fields.iter().map(|_| StringColumn::nulls(input.rows)).collect();
    let mut scratch = Vec::new();
    let mut validity = ValidityMask::all_valid();
    let mut all_converted = true;
    for row in 0..input.rows {
        let Some(text) = input.text(row) else {
            validity.set_invalid(row, input.rows);
            continue;
        };
        let parsed = split_struct(text, layout, &mut scratch, |field, value| {
            if let Some(column) = columns.get_mut(field) {
                column.set(row, value);
            }
        });
        if !parsed {
            for column in &mut columns {
                column.set(row, None);
            }
            params.assign_error(cannot_cast(text, &target))?;
            all_converted = false;
            validity.set_invalid(row, input.rows);
        }
    }
    let mut text = Vector::flat(text_type.clone(), input.rows);
    if let Some(children) = text.struct_children_mut() {
        for (child, column) in children.iter_mut().zip(columns) {
            *child = column.finish();
        }
    }
    text.set_validity(validity);
    let converted = plan.execute(&text, result, input.rows, params)?;
    if input.constant {
        result.make_constant();
    }
    Ok(converted && all_converted)
}

/// VARCHAR -> MAP (DuckDB's `VectorStringToMap`), including the key checks of
/// `MapConversionVerify`.
fn string_to_map(
    key_cast: &BoundCastInfo,
    value_cast: &BoundCastInfo,
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let input = TextInput::new(source, count);
    let target = result.logical_type().clone();
    let (key_type, value_type) = target.map_key_value().ok_or_else(|| missing_child(&target))?;
    let mut keys = StringColumn::new();
    let mut values = StringColumn::new();
    let mut scratch = Vec::new();
    let mut entries = Vec::with_capacity(input.rows);
    let mut validity = ValidityMask::all_valid();
    let mut all_converted = true;
    for row in 0..input.rows {
        let offset = keys.len();
        if let Some(text) = input.text(row) {
            let parsed = split_map(text, &mut scratch, |part| match part {
                MapPart::Key(key) => {
                    keys.push(key);
                    if key.is_none() {
                        // DuckDB reserves the entry of a NULL key before failing.
                        values.push(None);
                    }
                }
                MapPart::Value(value) => values.push(value),
            });
            if !parsed {
                params.assign_error(cannot_cast(text, &target))?;
                all_converted = false;
                validity.set_invalid(row, input.rows);
            }
        } else {
            validity.set_invalid(row, input.rows);
        }
        while values.len() < keys.len() {
            values.push(None);
        }
        entries.push(ListEntry::new(offset, keys.len() - offset));
    }
    let total = keys.len();
    let (keys, keys_converted) = cast_child(key_cast, &keys.finish(), key_type, total, params)?;
    let (values, values_converted) = cast_child(value_cast, &values.finish(), value_type, total, params)?;
    all_converted &= keys_converted && values_converted;
    if !all_converted {
        // Rows whose keys failed to convert become NULL.
        for (row, entry) in entries.iter().enumerate() {
            if validity.is_valid(row) && entry.range().any(|i| keys.is_null(i)) {
                validity.set_invalid(row, input.rows);
            }
        }
    }
    verify_map_keys(&keys, &entries, &validity)?;
    let entry_type = target.child_type().ok_or_else(|| missing_child(&target))?.clone();
    let mut entry_vector = Vector::flat(entry_type, total);
    if let Some([key_slot, value_slot]) = entry_vector.struct_children_mut() {
        *key_slot = keys;
        *value_slot = values;
    }
    let mut out = Vector::flat(target.clone(), input.rows);
    out.values_mut::<ListEntry>()[..input.rows].copy_from_slice(&entries);
    out.set_validity(validity);
    out.set_list_child(entry_vector, total);
    *result = input.finish(out);
    Ok(all_converted)
}

/// DuckDB's `MapVector::MapConversionVerify`: keys must be non-NULL and unique per map.
fn verify_map_keys(keys: &Vector, entries: &[ListEntry], validity: &ValidityMask) -> Result<()> {
    for (row, entry) in entries.iter().enumerate() {
        if !validity.is_valid(row) {
            continue;
        }
        let mut seen: Vec<Value> = Vec::with_capacity(entry.length as usize);
        for i in entry.range() {
            if keys.is_null(i) {
                return Err(Error::invalid_input("Map keys can not be NULL."));
            }
            let key = keys.get_value(i);
            if seen.contains(&key) {
                return Err(Error::invalid_input("Map keys must be unique."));
            }
            seen.push(key);
        }
    }
    Ok(())
}

/// Casts from VARCHAR to nested types.
pub(super) fn cast_switch(set: &CastFunctionSet, target: &LogicalType) -> Result<Option<BoundCastInfo>> {
    use rustdb_types::LogicalTypeId as L;
    Ok(Some(match target.id() {
        L::List => {
            let child = target.child_type().ok_or_else(|| missing_child(target))?;
            let child_cast = set.get_cast_function(&LogicalType::VARCHAR, child)?;
            BoundCastInfo::new(move |source, result, count, params| {
                string_to_list(&child_cast, source, result, count, params)
            })
        }
        L::Array => {
            let (child, _) = target.array_child().ok_or_else(|| missing_child(target))?;
            let child_cast = set.get_cast_function(&LogicalType::VARCHAR, child)?;
            BoundCastInfo::new(move |source, result, count, params| {
                string_to_array(&child_cast, source, result, count, params)
            })
        }
        L::Struct => {
            let fields = target.struct_children().ok_or_else(|| missing_child(target))?;
            let text_type = LogicalType::struct_type(
                fields
                    .iter()
                    .map(|(name, _)| (name.clone(), LogicalType::VARCHAR))
                    .collect(),
            );
            let plan = StructCastPlan::bind(set, &text_type, target)?;
            BoundCastInfo::new(move |source, result, count, params| {
                string_to_struct(&plan, &text_type, source, result, count, params)
            })
        }
        L::Map => {
            let (key, value) = target.map_key_value().ok_or_else(|| missing_child(target))?;
            let key_cast = set.get_cast_function(&LogicalType::VARCHAR, key)?;
            let value_cast = set.get_cast_function(&LogicalType::VARCHAR, value)?;
            BoundCastInfo::new(move |source, result, count, params| {
                string_to_map(&key_cast, &value_cast, source, result, count, params)
            })
        }
        _ => return Ok(None),
    }))
}
