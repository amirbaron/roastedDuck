//! MAP functions (DuckDB's `core_functions/scalar/map`): `map`, `cardinality`,
//! `map_entries`, `map_keys`, `map_values`, `map_extract`/`element_at`,
//! `map_extract_value`, `map_from_entries` and `map_concat`.

use std::collections::HashSet;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use roastedduck_row::sort::{NullOrder, OrderModifiers, OrderType, SortKeyBuffer, create_sort_keys};
use roastedduck_types::kernels::scatter;
use roastedduck_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::list::contains_or_position::list_search;
use crate::scalar::nested::{ListInput, all_constant, list_child_type, struct_field};
use crate::scalar::structs::build_struct;
use crate::signature::template;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// DuckDB's `MapVector::EvalMapInvalidReason` messages.
fn duplicate_key() -> Error {
    Error::invalid_input("Map keys must be unique.")
}

fn null_key() -> Error {
    Error::invalid_input("Map keys can not be NULL.")
}

fn not_aligned() -> Error {
    Error::invalid_input("The map key list does not align with the map value list.")
}

fn equality_keys(vector: &Vector, count: usize) -> Result<SortKeyBuffer> {
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    create_sort_keys(&[vector], &modifiers, count)
}

/// The key and value types of a MAP type.
fn map_key_value(ty: &LogicalType) -> Result<(LogicalType, LogicalType)> {
    ty.map_key_value()
        .map(|(key, value)| (key.clone(), value.clone()))
        .ok_or_else(|| Error::internal(format!("expected a MAP type, got {ty}")))
}

/// Builds a MAP (or LIST of STRUCT) vector from entries and its key/value children.
fn build_map(
    ty: &LogicalType,
    entries: &[ListEntry],
    nulls: &[usize],
    keys: Vector,
    values: Vector,
    size: usize,
    constant: bool,
) -> Result<Vector> {
    let rows = entries.len();
    let child_type = list_child_type(ty)?;
    let child = build_struct(child_type, vec![keys, values], size, false)?;
    let mut output = Vector::flat(ty.clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(entries);
    output.set_list_child(child, size);
    let validity = output.validity_mut();
    for &row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    Ok(output)
}

/// DuckDB's `MapFunction`: `map()` or `map(keys, values)`.
fn map_function(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let ty = result.logical_type().clone();
    if args.column_count() == 0 {
        let (key_type, value_type) = map_key_value(&ty)?;
        let mut output = build_map(
            &ty,
            &[ListEntry::new(0, 0)],
            &[],
            Vector::flat(key_type, 1),
            Vector::flat(value_type, 1),
            0,
            false,
        )?;
        output.make_constant();
        *result = output;
        return Ok(());
    }
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull
        || args.column(1).logical_type().id() == LogicalTypeId::SqlNull
    {
        *result = Vector::constant_null(ty);
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let keys = addressable(args.column(0), count);
    let values = addressable(args.column(1), count);
    let key_lists = ListInput::new(&keys)?;
    let value_lists = ListInput::new(&values)?;
    let key_keys = equality_keys(key_lists.child, key_lists.child_size)?;
    let mut key_rows = Vec::new();
    let mut value_rows = Vec::new();
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    for row in 0..rows {
        let (Some(key_entry), Some(value_entry)) = (key_lists.get(row), value_lists.get(row)) else {
            nulls.push(row);
            entries.push(ListEntry::new(key_rows.len(), 0));
            continue;
        };
        if key_entry.length != value_entry.length {
            return Err(not_aligned());
        }
        let mut unique: HashSet<&[u8]> = HashSet::new();
        let start = key_rows.len();
        for index in 0..key_entry.length as usize {
            let key = key_entry.offset as usize + index;
            if key_lists.child.is_null(key) {
                return Err(null_key());
            }
            if !unique.insert(key_keys.key(key)) {
                return Err(duplicate_key());
            }
            key_rows.push(Some(key as u32));
            value_rows.push(Some((value_entry.offset as usize + index) as u32));
        }
        entries.push(ListEntry::new(start, key_rows.len() - start));
    }
    let size = key_rows.len();
    let mut out_keys = key_lists.child.select_rows(&key_rows);
    out_keys.flatten(size);
    let mut out_values = value_lists.child.select_rows(&value_rows);
    out_values.flatten(size);
    *result = build_map(&ty, &entries, &nulls, out_keys, out_values, size, constant)?;
    Ok(())
}

/// `map`.
pub fn map_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("map");
    set.add(ScalarFunction::new(
        "map",
        vec![],
        LogicalType::map(LogicalType::SQLNULL, LogicalType::SQLNULL),
        |args, _, result| map_function(args, result),
    ));
    let key = template("K");
    let value = template("V");
    let mut function = ScalarFunction::new(
        "map",
        vec![LogicalType::list(key.clone()), LogicalType::list(value.clone())],
        LogicalType::map(key, value),
        |args, _, result| map_function(args, result),
    );
    function.null_handling = NullHandling::Special;
    set.add(function);
    set
}

/// `cardinality(map)`: the number of entries.
pub fn cardinality_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "cardinality",
        vec![LogicalType::ANY],
        LogicalType::UBIGINT,
        |args, _, result| {
            crate::scalar::executor::unary::<ListEntry, u64>(args, result, |entry, out| {
                *out = entry.length;
                Ok(true)
            })
        },
    )
    .with_bind(|function, input| -> BindResult {
        if input.argument_types.len() != 1 {
            return Err(Error::binder("Cardinality must have exactly one arguments"));
        }
        if input.argument_types[0].id() != LogicalTypeId::Map {
            return Err(Error::binder("Cardinality can only operate on MAPs"));
        }
        function.arguments[0] = input.argument_types[0].clone();
        function.varargs = None;
        function.return_type = LogicalType::UBIGINT;
        Ok(None)
    });
    function.varargs = Some(LogicalType::ANY);
    let mut set = ScalarFunctionSet::new("cardinality");
    set.add(function);
    set
}

/// The map's entries list re-typed as `ty`, its entry structs included (DuckDB's
/// `MapUtil::ReinterpretMap`).
fn reinterpret(map: &Vector, ty: &LogicalType) -> Result<Vector> {
    let child_type = list_child_type(ty)?;
    let mut output = map.reinterpret(ty.clone())?;
    if let Some(child) = output.list_child_mut() {
        *child = child.reinterpret(child_type)?;
    }
    Ok(output)
}

fn map_entries_function(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let map = args.column(0);
    if map.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    *result = reinterpret(map, result.logical_type())?;
    Ok(())
}

/// `map_keys`/`map_values`: the list of one of the entry fields.
fn map_field_list(args: &DataChunk, result: &mut Vector, field: usize) -> Result<()> {
    let map = args.column(0);
    if map.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    let count = args.len();
    let maps = addressable(map, count);
    let constant = maps.is_constant();
    let rows = if constant { 1 } else { count };
    let input = ListInput::new(&maps)?;
    let child = struct_field(input.child, field, input.child_size)?;
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    for row in 0..rows {
        if let Some(entry) = input.get(row) {
            entries.push(entry);
        } else {
            nulls.push(row);
            entries.push(ListEntry::new(0, 0));
        }
    }
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, input.child_size);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn map_signature() -> (LogicalType, LogicalType, LogicalType) {
    let key = template("K");
    let value = template("V");
    (LogicalType::map(key.clone(), value.clone()), key, value)
}

/// `map_entries`, `map_keys` and `map_values`.
pub fn map_unpack_functions() -> Vec<ScalarFunctionSet> {
    let (map, key, value) = map_signature();
    let row = LogicalType::struct_type(vec![
        ("key".to_owned().into(), key.clone()),
        ("value".to_owned().into(), value.clone()),
    ]);
    let mut sets = Vec::new();
    for (name, return_type, field) in [
        ("map_entries", LogicalType::list(row), None),
        ("map_keys", LogicalType::list(key), Some(0)),
        ("map_values", LogicalType::list(value), Some(1)),
    ] {
        let mut function = ScalarFunction::new(
            name,
            vec![map.clone()],
            return_type,
            move |args, _, result| match field {
                Some(field) => map_field_list(args, result, field),
                None => map_entries_function(args, result),
            },
        );
        function.null_handling = NullHandling::Special;
        let mut set = ScalarFunctionSet::new(name);
        set.add(function);
        sets.push(set);
    }
    sets
}

/// The 1-based positions of the searched keys (DuckDB's `ListSearchOp<int32_t>` on keys).
fn key_positions(maps: &Vector, target: &Vector, count: usize) -> Result<Vector> {
    let (child, child_size) = maps
        .list_child()
        .map(|(child, size)| (child.clone(), size))
        .ok_or_else(|| Error::internal("map function on a non-map vector"))?;
    let keys = struct_field(&child, 0, child_size)?;
    let mut positions = Vector::flat(LogicalType::INTEGER, count.max(1));
    list_search(
        maps,
        &keys,
        child_size,
        target,
        count,
        true,
        false,
        &mut positions,
    )?;
    Ok(positions)
}

/// `map_extract(map, key)`: a list with the value of the key, or an empty list.
fn map_extract_list(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let maps = addressable(args.column(0), count);
    let target = addressable(args.column(1), count);
    let positions = key_positions(&maps, &target, count)?;
    let input = ListInput::new(&maps)?;
    let values = struct_field(input.child, 1, input.child_size)?;
    let mut entries = Vec::with_capacity(count);
    let mut nulls = Vec::new();
    let mut selection = Vec::new();
    for row in 0..count {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        };
        if positions.is_null(row) {
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        }
        let position = positions.get_value(row).as_i32().unwrap_or(1);
        entries.push(ListEntry::new(selection.len(), 1));
        selection.push(Some((entry.offset as usize + position as usize - 1) as u32));
    }
    let size = selection.len();
    let child_type = list_child_type(result.logical_type())?;
    let mut child = Vector::flat(child_type, size.max(1));
    if size > 0 {
        let positions_out: Vec<u32> = (0..size as u32).collect();
        scatter(&values.select_rows(&selection), &mut child, &positions_out)?;
    }
    let mut output = Vector::flat(result.logical_type().clone(), count.max(1));
    output.values_mut::<ListEntry>()[..count].copy_from_slice(&entries);
    output.set_list_child(child, size);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    if count == 1 {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `map_extract_value(map, key)`: the value of the key, or NULL.
fn map_extract_value(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let maps = addressable(args.column(0), count);
    let target = addressable(args.column(1), count);
    let positions = key_positions(&maps, &target, count)?;
    let input = ListInput::new(&maps)?;
    let values = struct_field(input.child, 1, input.child_size)?;
    let selection: Vec<Option<u32>> = (0..count)
        .map(|row| {
            let entry = input.get(row)?;
            if positions.is_null(row) {
                return None;
            }
            let position = positions.get_value(row).as_i32()?;
            Some((entry.offset as usize + position as usize - 1) as u32)
        })
        .collect();
    let mut output = values.select_rows(&selection);
    if count == 1 {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `map_extract`/`element_at` and `map_extract_value`.
pub fn map_extract_functions() -> (ScalarFunctionSet, ScalarFunctionSet) {
    let (map, key, value) = map_signature();
    let mut extract = ScalarFunction::new(
        "map_extract",
        vec![map.clone(), key.clone()],
        LogicalType::list(value.clone()),
        |args, _, result| map_extract_list(args, result),
    );
    extract.null_handling = NullHandling::Special;
    let mut extract_set = ScalarFunctionSet::new("map_extract");
    extract_set.add(extract);
    let mut extract_value =
        ScalarFunction::new("map_extract_value", vec![map, key], value, |args, _, result| {
            map_extract_value(args, result)
        });
    extract_value.null_handling = NullHandling::Special;
    let mut value_set = ScalarFunctionSet::new("map_extract_value");
    value_set.add(extract_value);
    (extract_set, value_set)
}

/// Checks every map row for NULL and duplicate keys (DuckDB's `MapConversionVerify`).
fn verify_map(map: &Vector, count: usize) -> Result<()> {
    let input = ListInput::new(map)?;
    let keys = struct_field(input.child, 0, input.child_size)?;
    let key_keys = equality_keys(&keys, input.child_size)?;
    for row in 0..count {
        let Some(entry) = input.get(row) else {
            continue;
        };
        let mut unique: HashSet<&[u8]> = HashSet::new();
        for key in entry.offset as usize..(entry.offset + entry.length) as usize {
            if keys.is_null(key) {
                return Err(null_key());
            }
            if !unique.insert(key_keys.key(key)) {
                return Err(duplicate_key());
            }
        }
    }
    Ok(())
}

/// `map_from_entries(list of {key, value})`.
pub fn map_from_entries_functions() -> ScalarFunctionSet {
    let key = template("K");
    let value = template("V");
    let row = LogicalType::struct_type(vec![
        (String::new().into(), key.clone()),
        (String::new().into(), value.clone()),
    ]);
    let function = ScalarFunction::new(
        "map_from_entries",
        vec![LogicalType::list(row)],
        LogicalType::map(key, value),
        |args, _, result| {
            let count = args.len();
            let lists = addressable(args.column(0), count);
            let constant = lists.is_constant();
            let map = reinterpret(&lists, result.logical_type())?;
            verify_map(&map, if constant { 1 } else { count })?;
            *result = map;
            Ok(())
        },
    );
    let mut set = ScalarFunctionSet::new("map_from_entries");
    set.add(function);
    set
}

/// DuckDB's `MapConcatBind`.
fn bind_map_concat(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let types = input.argument_types;
    if types.len() < 2 {
        return Err(Error::invalid_input(
            "The provided amount of arguments is incorrect, please provide 2 or more maps",
        ));
    }
    let mut expected = LogicalType::SQLNULL;
    let mut is_null = true;
    for ty in types {
        match ty.id() {
            LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
            LogicalTypeId::SqlNull => continue,
            LogicalTypeId::Map => {}
            _ => return Err(Error::invalid_input("MAP_CONCAT only takes map arguments")),
        }
        is_null = false;
        let (key, value) = map_key_value(ty)?;
        if key.id() == LogicalTypeId::SqlNull && value.id() == LogicalTypeId::SqlNull {
            continue;
        }
        if expected.id() == LogicalTypeId::SqlNull {
            expected = ty.clone();
        } else if *ty != expected {
            return Err(Error::invalid_input(format!(
                "'value' type of map differs between arguments, expected '{expected}', found '{ty}' instead"
            )));
        }
    }
    if expected.id() == LogicalTypeId::SqlNull && !is_null {
        expected = LogicalType::map(LogicalType::SQLNULL, LogicalType::SQLNULL);
    }
    function.arguments = types.to_vec();
    function.varargs = None;
    function.return_type = expected;
    Ok(None)
}

/// DuckDB's `MapConcatFunction`: later maps win for duplicate keys, which keep the
/// position of their first occurrence.
fn map_concat(args: &DataChunk, result: &mut Vector) -> Result<()> {
    struct MapColumn<'a> {
        input: ListInput<'a>,
        keys: Vector,
        values: Vector,
        key_bytes: SortKeyBuffer,
    }
    let ty = result.logical_type().clone();
    if ty.id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(ty);
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let maps: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let mut columns: Vec<Option<MapColumn<'_>>> = Vec::with_capacity(maps.len());
    for map in &maps {
        if map.logical_type().id() == LogicalTypeId::SqlNull {
            columns.push(None);
            continue;
        }
        let input = ListInput::new(map)?;
        let keys = struct_field(input.child, 0, input.child_size)?;
        let values = struct_field(input.child, 1, input.child_size)?;
        let key_bytes = equality_keys(&keys, input.child_size)?;
        columns.push(Some(MapColumn {
            input,
            keys,
            values,
            key_bytes,
        }));
    }
    let (key_type, value_type) = map_key_value(&ty)?;
    let mut out_keys = Vec::new();
    let mut out_values = Vec::new();
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    for row in 0..rows {
        let mut order: Vec<(Vec<u8>, usize, usize)> = Vec::new();
        let mut all_null = true;
        for (map_index, column) in columns.iter().enumerate() {
            let Some(column) = column else { continue };
            let Some(entry) = column.input.get(row) else {
                continue;
            };
            all_null = false;
            for key in entry.offset as usize..(entry.offset + entry.length) as usize {
                let bytes = column.key_bytes.key(key).to_vec();
                match order.iter_mut().find(|(existing, _, _)| *existing == bytes) {
                    Some(mapping) => {
                        mapping.1 = map_index;
                        mapping.2 = key;
                    }
                    None => order.push((bytes, map_index, key)),
                }
            }
        }
        let start = out_keys.len();
        if all_null {
            nulls.push(row);
            entries.push(ListEntry::new(start, 0));
            continue;
        }
        for (_, map_index, key) in &order {
            if let Some(column) = &columns[*map_index] {
                out_keys.push(column.keys.get_value(*key));
                out_values.push(column.values.get_value(*key));
            }
        }
        entries.push(ListEntry::new(start, out_keys.len() - start));
    }
    let size = out_keys.len();
    let mut keys = Vector::flat(key_type, size.max(1));
    let mut values = Vector::flat(value_type, size.max(1));
    for (index, (key, value)) in out_keys.iter().zip(&out_values).enumerate() {
        keys.set_value(index, key)?;
        values.set_value(index, value)?;
    }
    *result = build_map(&ty, &entries, &nulls, keys, values, size, constant)?;
    Ok(())
}

/// `map_concat(map, ...)`.
pub fn map_concat_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "map_concat",
        vec![],
        LogicalType::simple(LogicalTypeId::List),
        |args, _, result| map_concat(args, result),
    )
    .with_bind(bind_map_concat);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("map_concat");
    set.add(function);
    set
}
