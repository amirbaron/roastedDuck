//! `list_select`/`list_where` (DuckDB's `list_select.cpp`), `list_resize`
//! (`list_resize.cpp`), `list_zip` (`list_zip.cpp`) and `list_intersect`
//! (`list_intersect.cpp`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::operator::add_overflow;
use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, all_constant, list_child_type};
use crate::scalar::structs::build_struct;
use crate::signature::template;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// Assembles a LIST vector from entries, NULL rows and a child gathered by `selection`.
fn assemble(
    ty: &LogicalType,
    entries: &[ListEntry],
    nulls: &[usize],
    source: &Vector,
    selection: &[Option<u32>],
    constant: bool,
) -> Result<Vector> {
    let rows = entries.len();
    let size = selection.len();
    let mut child = Vector::flat(list_child_type(ty)?, size.max(1));
    if size > 0 {
        let targets: Vec<u32> = (0..size as u32).collect();
        scatter(&source.select_rows(selection), &mut child, &targets)?;
    }
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

fn nulls_not_allowed() -> Error {
    Error::invalid_input("NULLs are not allowed as list elements in the second input parameter.")
}

/// DuckDB's `ListSelectFunction` for `list_select` (`by_index`) and `list_where`.
fn list_select(args: &DataChunk, result: &mut Vector, by_index: bool) -> Result<()> {
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let lists = addressable(args.column(0), count);
    let selections = addressable(args.column(1), count);
    let input = ListInput::new(&lists)?;
    let selection_lists = ListInput::new(&selections)?;
    if !by_index {
        // DuckDB validates every `list_where` mask before selecting.
        for row in 0..rows {
            if let (Some(_), Some(mask)) = (input.get(row), selection_lists.get(row)) {
                for index in mask.offset..mask.offset + mask.length {
                    if selection_lists.child.is_null(index as usize) {
                        return Err(nulls_not_allowed());
                    }
                }
            }
        }
    }
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut selection = Vec::new();
    for row in 0..rows {
        let (Some(mask), Some(list)) = (selection_lists.get(row), input.get(row)) else {
            nulls.push(row);
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        };
        let start = selection.len();
        for index in 0..mask.length as usize {
            let element = mask.offset as usize + index;
            if selection_lists.child.is_null(element) {
                return Err(nulls_not_allowed());
            }
            let value = selection_lists.child.get_value(element);
            if by_index {
                let position = value.as_i64().unwrap_or(0) - 1;
                if position >= 0 && (position as u64) < list.length {
                    selection.push(Some((list.offset + position as u64) as u32));
                } else {
                    selection.push(None);
                }
            } else if value.as_bool().unwrap_or(false) {
                if (index as u64) < list.length {
                    selection.push(Some((list.offset as usize + index) as u32));
                } else {
                    selection.push(None);
                }
            }
        }
        entries.push(ListEntry::new(start, selection.len() - start));
    }
    *result = assemble(
        result.logical_type(),
        &entries,
        &nulls,
        input.child,
        &selection,
        constant,
    )?;
    Ok(())
}

/// `list_select` and `list_where`, with their aliases.
pub fn list_select_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let element = template("T");
    let list = LogicalType::list(element);
    let mut select = ScalarFunctionSet::new("list_select");
    select.add(ScalarFunction::new(
        "list_select",
        vec![list.clone(), LogicalType::list(LogicalType::BIGINT)],
        list.clone(),
        |args, _, result| list_select(args, result, true),
    ));
    let mut where_set = ScalarFunctionSet::new("list_where");
    where_set.add(ScalarFunction::new(
        "list_where",
        vec![list.clone(), LogicalType::list(LogicalType::BOOLEAN)],
        list,
        |args, _, result| list_select(args, result, false),
    ));
    vec![(select, &["array_select"]), (where_set, &["array_where"])]
}

/// DuckDB's `ListResizeFunction`: truncates or pads (with the default or NULL).
fn list_resize(args: &DataChunk, result: &mut Vector) -> Result<()> {
    // `VectorListBuffer::Reserve`: DuckDB's `MAX_VECTOR_SIZE` (2^37 rows).
    const MAX_VECTOR_SIZE: u64 = 1 << 37;
    let ty = result.logical_type().clone();
    if ty.id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(ty);
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let lists = addressable(args.column(0), count);
    let sizes = addressable(args.column(1), count);
    let defaults = args.columns().get(2).map(|column| addressable(column, count));
    let input = ListInput::new(&lists)?;
    let sizes = sizes.unified::<u64>();
    let mut total: u64 = 0;
    for row in 0..rows {
        if let (Some(_), Some(size)) = (input.get(row), sizes.get(row)) {
            total = total.checked_add(size).ok_or_else(|| add_overflow(total, size))?;
        }
    }
    if total > MAX_VECTOR_SIZE {
        return Err(Error::out_of_range(format!(
            "Cannot resize vector to {total} rows: maximum allowed vector size is 128.0 GiB"
        )));
    }
    let child_type = list_child_type(&ty)?;
    let mut child = Vector::flat(child_type, (total as usize).max(1));
    child.reset_flat(total as usize);
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut offset = 0usize;
    for row in 0..rows {
        let Some(list) = input.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(offset, 0));
            continue;
        };
        let new_size = sizes.get(row).unwrap_or(0) as usize;
        let copy = (list.length as usize).min(new_size);
        entries.push(ListEntry::new(offset, new_size));
        if copy > 0 {
            let sources: Vec<Option<u32>> = (list.offset as usize..list.offset as usize + copy)
                .map(|index| Some(index as u32))
                .collect();
            let targets: Vec<u32> = (offset..offset + copy).map(|index| index as u32).collect();
            scatter(&input.child.select_rows(&sources), &mut child, &targets)?;
        }
        offset += copy;
        if copy < new_size {
            let remaining = new_size - copy;
            let default_row = defaults.as_ref().filter(|defaults| !defaults.is_null(row));
            if let Some(defaults) = default_row {
                let sources = vec![Some(row as u32); remaining];
                let targets: Vec<u32> = (offset..offset + remaining).map(|index| index as u32).collect();
                scatter(&defaults.select_rows(&sources), &mut child, &targets)?;
            } else {
                let validity = child.validity_mut();
                for index in offset..offset + remaining {
                    validity.set_invalid(index, total as usize);
                }
            }
            offset += remaining;
        }
    }
    let mut output = Vector::flat(ty, rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, offset);
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

/// DuckDB's `ListResizeBind`.
fn bind_list_resize(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    function.arguments[1] = LogicalType::UBIGINT;
    let ty = &input.argument_types[0];
    let list = if ty.id() == LogicalTypeId::Array {
        LogicalType::list(list_child_type(ty)?)
    } else {
        ty.clone()
    };
    match list.id() {
        LogicalTypeId::SqlNull => {
            function.arguments[0] = LogicalType::SQLNULL;
            function.return_type = LogicalType::SQLNULL;
            return Ok(None);
        }
        LogicalTypeId::Unknown => {
            function.return_type = list;
            return Ok(None);
        }
        _ => {}
    }
    function.arguments[0] = list.clone();
    if function.arguments.len() == 3 {
        let child = list_child_type(&list)?;
        let default = &input.argument_types[2];
        function.arguments[2] = if *default != child && default.id() != LogicalTypeId::SqlNull {
            child
        } else {
            default.clone()
        };
    }
    function.return_type = list;
    Ok(None)
}

/// `list_resize`/`array_resize`.
pub fn list_resize_functions() -> ScalarFunctionSet {
    let any_list = LogicalType::list(LogicalType::ANY);
    let mut set = ScalarFunctionSet::new("list_resize");
    for arguments in [
        vec![any_list.clone(), LogicalType::ANY],
        vec![any_list.clone(), LogicalType::ANY, LogicalType::ANY],
    ] {
        let mut function =
            ScalarFunction::new("list_resize", arguments, any_list.clone(), |args, _, result| {
                list_resize(args, result)
            })
            .with_bind(bind_list_resize);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

/// DuckDB's `ListZipBind`.
fn bind_list_zip(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let types = input.argument_types;
    let mut size = types.len();
    if size == 0 {
        return Err(Error::binder(rustdb_common::byte_format!(
            "Provide at least one argument to {}",
            function.name
        )));
    }
    if types[size - 1].id() == LogicalTypeId::Boolean {
        size -= 1;
        if size == 0 {
            return Err(Error::binder(rustdb_common::byte_format!(
                "Provide at least one list argument to {}",
                function.name
            )));
        }
    }
    let mut children = Vec::with_capacity(size);
    for (index, ty) in types[..size].iter().enumerate() {
        match ty.id() {
            LogicalTypeId::List | LogicalTypeId::Array => {
                let child = list_child_type(ty)?;
                if ty.id() == LogicalTypeId::Array {
                    // `AddArrayCastToList`.
                    function.cast_argument(index, LogicalType::list(child.clone()));
                }
                children.push((rustdb_common::ByteString::new(), child));
            }
            LogicalTypeId::SqlNull => children.push((rustdb_common::ByteString::new(), LogicalType::SQLNULL)),
            LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
            _ => return Err(Error::binder("Parameter type needs to be List")),
        }
    }
    function.return_type = LogicalType::list(LogicalType::struct_type(children));
    Ok(None)
}

/// DuckDB's `ListZipFunction`.
fn list_zip(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let columns: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let truncate_flag = columns
        .last()
        .is_some_and(|column| column.logical_type().id() == LogicalTypeId::Boolean);
    let list_count = columns.len() - usize::from(truncate_flag);
    let lists: Vec<Option<ListInput<'_>>> = columns[..list_count]
        .iter()
        .map(|column| {
            if column.logical_type().id() == LogicalTypeId::SqlNull {
                Ok(None)
            } else {
                ListInput::new(column).map(Some)
            }
        })
        .collect::<Result<_>>()?;
    let flags = truncate_flag.then(|| columns[list_count].unified::<bool>());
    let mut entries = Vec::with_capacity(rows);
    let mut selections: Vec<Vec<Option<u32>>> = vec![Vec::new(); list_count];
    for row in 0..rows {
        let shortest = flags.as_ref().and_then(|flags| flags.get(row)).unwrap_or(false);
        let mut length: usize = if shortest { i32::MAX as usize } else { 0 };
        for list in &lists {
            let current = match list {
                Some(list) if list.child_size > 0 => list.get(row).map_or(0, |entry| entry.length as usize),
                _ => 0,
            };
            length = if shortest {
                length.min(current)
            } else {
                length.max(current)
            };
        }
        let start = selections.first().map_or(0, Vec::len);
        for (list, selection) in lists.iter().zip(&mut selections) {
            let entry = list.as_ref().and_then(|list| list.get(row));
            let (offset, current) =
                entry.map_or((0, 0), |entry| (entry.offset as usize, entry.length as usize));
            for index in 0..length {
                selection.push((index < current).then_some((offset + index) as u32));
            }
        }
        entries.push(ListEntry::new(start, length));
    }
    let size = entries.iter().map(|entry| entry.length as usize).sum::<usize>();
    let struct_type = list_child_type(result.logical_type())?;
    let field_types: Vec<LogicalType> = struct_type
        .struct_children()
        .map(|children| children.iter().map(|(_, ty)| ty.clone()).collect())
        .unwrap_or_default();
    let mut fields = Vec::with_capacity(list_count);
    for ((list, selection), ty) in lists.iter().zip(&selections).zip(&field_types) {
        let field = match list {
            Some(list) if list.child_size > 0 => {
                let mut field = list.child.select_rows(selection);
                field.flatten(size);
                field
            }
            _ => {
                let mut field = Vector::constant_null(ty.clone());
                field.flatten(size);
                field
            }
        };
        fields.push(field);
    }
    let child = build_struct(struct_type, fields, size, false)?;
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `list_zip`/`array_zip`.
pub fn list_zip_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "list_zip",
        vec![],
        LogicalType::list(LogicalType::simple(LogicalTypeId::Struct)),
        |args, _, result| list_zip(args, result),
    )
    .with_bind(bind_list_zip);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("list_zip");
    set.add(function);
    set
}

/// DuckDB's `ListIntersectFunction`: the elements of the shorter list that occur in the
/// longer one, in the longer list's order, without duplicates.
fn list_intersect(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let ty = result.logical_type().clone();
    if ty.id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(ty);
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let left_lists = addressable(args.column(0), count);
    let right_lists = addressable(args.column(1), count);
    let left = ListInput::new(&left_lists)?;
    let right = ListInput::new(&right_lists)?;
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    let left_keys = create_sort_keys(&[left.child], &modifiers, left.child_size)?;
    let right_keys = create_sort_keys(&[right.child], &modifiers, right.child_size)?;
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut selection = Vec::new();
    for row in 0..rows {
        let start = selection.len();
        let Some(l) = left.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(start, 0));
            continue;
        };
        let Some(r) = right.get(row) else {
            entries.push(ListEntry::new(start, 0));
            continue;
        };
        if l.length == 0 || r.length == 0 {
            entries.push(ListEntry::new(start, 0));
            continue;
        }
        let use_left_for_hash = l.length <= r.length;
        let (hash_list, iter_list) = if use_left_for_hash { (l, r) } else { (r, l) };
        let (hash_input, iter_input) = if use_left_for_hash {
            (&left, &right)
        } else {
            (&right, &left)
        };
        let (hash_keys, iter_keys) = if use_left_for_hash {
            (&left_keys, &right_keys)
        } else {
            (&right_keys, &left_keys)
        };
        let mut set: HashSet<&[u8]> = HashSet::new();
        let mut key_to_index: HashMap<&[u8], usize> = HashMap::new();
        for index in hash_list.offset as usize..(hash_list.offset + hash_list.length) as usize {
            if hash_input.child.is_null(index) {
                continue;
            }
            let key = hash_keys.key(index);
            set.insert(key);
            if use_left_for_hash {
                key_to_index.insert(key, index);
            }
        }
        let mut emitted: HashSet<&[u8]> = HashSet::new();
        for index in iter_list.offset as usize..(iter_list.offset + iter_list.length) as usize {
            if iter_input.child.is_null(index) {
                continue;
            }
            let key = iter_keys.key(index);
            if !set.contains(key) || !emitted.insert(key) {
                continue;
            }
            let emit = if use_left_for_hash {
                key_to_index[key]
            } else {
                index
            };
            selection.push(Some(emit as u32));
        }
        entries.push(ListEntry::new(start, selection.len() - start));
    }
    *result = assemble(&ty, &entries, &nulls, left.child, &selection, constant)?;
    Ok(())
}

/// `list_intersect`/`array_intersect`.
pub fn list_intersect_functions() -> ScalarFunctionSet {
    let list = LogicalType::list(template("T"));
    let mut function = ScalarFunction::new(
        "list_intersect",
        vec![list.clone(), list.clone()],
        list,
        |args, _, result| list_intersect(args, result),
    )
    .with_bind(|function, input| {
        for (index, ty) in input.argument_types.iter().enumerate() {
            if ty.id() == LogicalTypeId::Array {
                function.arguments[index] = LogicalType::list(list_child_type(ty)?);
            }
        }
        Ok(None)
    });
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("list_intersect");
    set.add(function);
    set
}
