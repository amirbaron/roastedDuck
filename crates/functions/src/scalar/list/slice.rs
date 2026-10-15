//! `list_slice`/`array_slice` and `x[a:b:c]` (DuckDB's `array_slice.cpp`) and `flatten`
//! (`flatten.cpp`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{
    BindInput, FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet,
};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, StringT, Unified, Vector};

use crate::scalar::executor::{ScalarOutput, StrOut, addressable};
use crate::scalar::nested::{ListInput, all_constant, list_child_type};
use crate::scalar::string::substring::{substring_ascii, substring_unicode};
use crate::scalar::string::utf8::char_count;
use crate::signature::template;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// Which slice bounds were given as the empty list (`x[:b]`, `x[a:]`).
#[derive(Debug, Clone, Copy)]
struct SliceBindData {
    begin_is_empty: bool,
    end_is_empty: bool,
}

impl FunctionData for SliceBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `CalculateSliceLength`.
fn slice_length(begin: i64, end: i64, step: i64, step_valid: bool) -> Result<u64> {
    let step = step.unsigned_abs();
    if step == 0 && step_valid {
        return Err(Error::invalid_input("Slice step cannot be zero"));
    }
    let span = (end - begin) as u64;
    if step == 1 {
        return Ok(span);
    }
    if step >= span {
        return Ok(1);
    }
    if step != 0 && span % step != 0 {
        return Ok(span / step + 1);
    }
    Ok(span.checked_div(step).unwrap_or(0))
}

/// DuckDB's `ClampIndex`.
fn clamp_index(index: &mut i64, length: i64, is_min: bool) {
    if *index < 0 {
        if !is_min {
            *index += 1;
        }
        *index += length;
    } else if *index > length {
        *index = length;
    }
}

/// DuckDB's `ClampSlice`: 1-based (negative: from the end) bounds to a 0-based range.
fn clamp_slice(length: i64, begin: &mut i64, end: &mut i64) {
    if *begin != 0 && *begin != i64::MIN {
        *begin -= 1;
    }
    let mut is_min = false;
    if *begin == i64::MIN {
        *begin += 1;
        is_min = true;
    }
    if *begin < 0 && -*begin > length && *end < 0 && *end < -length {
        *begin = 0;
        *end = 0;
        return;
    }
    if *begin < 0 && -*begin > length {
        *begin = 0;
    }
    clamp_index(begin, length, is_min);
    clamp_index(end, length, false);
    *end = (*end).max(*begin);
}

/// The bounds of one row after DuckDB's step swap and clamping.
fn bounds(length: i64, begin: i64, end: i64, step: i64, empty: SliceBindData) -> (i64, i64) {
    let (mut begin, mut end) = (begin, end);
    if step < 0 {
        std::mem::swap(&mut begin, &mut end);
        if empty.end_is_empty {
            begin = 0;
        }
        if empty.begin_is_empty {
            end = length;
        }
    }
    clamp_slice(length, &mut begin, &mut end);
    (begin, end)
}

fn bind_data(state: &FunctionState) -> Result<SliceBindData> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<SliceBindData>())
        .copied()
        .ok_or_else(|| Error::internal("array_slice without bind data"))
}

/// The bound columns of one call: begin, end and optional step, read as BIGINT.
struct Bounds<'a> {
    begin: Option<Unified<'a, i64>>,
    end: Option<Unified<'a, i64>>,
    step: Option<Unified<'a, i64>>,
    columns: &'a [std::borrow::Cow<'a, Vector>],
}

impl Bounds<'_> {
    /// `(begin, end, step)` of a row, `None` if any given value is NULL.
    fn get(&self, row: usize, length: i64, empty: SliceBindData) -> Option<(i64, i64, i64)> {
        let begin = match &self.begin {
            Some(values) => values.get(row)?,
            None => {
                if self.columns[1].is_null(row) {
                    return None;
                }
                0
            }
        };
        let end = match &self.end {
            Some(values) => values.get(row)?,
            None => {
                if self.columns[2].is_null(row) {
                    return None;
                }
                length
            }
        };
        let step = match &self.step {
            Some(values) => values.get(row)?,
            None => 1,
        };
        let begin = if empty.begin_is_empty { 0 } else { begin };
        let end = if empty.end_is_empty { length } else { end };
        Some((begin, end, step))
    }
}

/// DuckDB's `ArraySliceFunction`.
fn array_slice(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let empty = bind_data(state)?;
    let ty = result.logical_type().clone();
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull || ty.id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(ty);
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let columns: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let has_step = columns.len() == 4;
    let bound = |index: usize, is_empty: bool| (!is_empty).then(|| columns[index].unified::<i64>());
    let bounds = Bounds {
        begin: bound(1, empty.begin_is_empty),
        end: bound(2, empty.end_is_empty),
        step: has_step.then(|| columns[3].unified::<i64>()),
        columns: &columns,
    };
    match ty.id() {
        LogicalTypeId::List => {
            slice_lists(&columns[0], &bounds, rows, constant, has_step, empty, &ty, result)
        }
        LogicalTypeId::Varchar | LogicalTypeId::Blob => {
            let blob = ty.id() == LogicalTypeId::Blob;
            let strings = columns[0].unified::<StringT>();
            StrOut::write(result, rows, |row, out| {
                let Some(value) = strings.get(row) else {
                    return Ok(false);
                };
                let input = columns[0].string_value(&value);
                let length = if blob { input.len() } else { char_count(input) } as i64;
                let Some((begin, end, step)) = bounds.get(row, length, empty) else {
                    return Ok(false);
                };
                let (begin, end) = bounds_for_row(length, begin, end, step, empty);
                let sliced = if blob {
                    substring_ascii(input, begin + 1, end - begin)?
                } else {
                    substring_unicode(input, begin + 1, end - begin)?
                };
                out.extend_from_slice(sliced);
                Ok(true)
            })?;
            if constant {
                result.make_constant();
            }
            Ok(())
        }
        _ => Err(Error::not_implemented("Specifier type not implemented")),
    }
}

fn bounds_for_row(length: i64, begin: i64, end: i64, step: i64, empty: SliceBindData) -> (i64, i64) {
    bounds(length, begin, end, step, empty)
}

#[allow(clippy::too_many_arguments)]
fn slice_lists(
    lists: &Vector,
    bounds: &Bounds<'_>,
    rows: usize,
    constant: bool,
    has_step: bool,
    empty: SliceBindData,
    ty: &LogicalType,
    result: &mut Vector,
) -> Result<()> {
    let input = ListInput::new(lists)?;
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut selection: Vec<Option<u32>> = Vec::new();
    // Without a step (or with a constant step of 1) the result references the input's
    // child; otherwise its elements are gathered (DuckDB's `SliceValueWithSteps`).
    let mut reference = !has_step;
    for row in 0..rows {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(0, 0));
            continue;
        };
        let length = entry.length as i64;
        let Some((begin, end, step)) = bounds.get(row, length, empty) else {
            nulls.push(row);
            entries.push(ListEntry::new(0, 0));
            continue;
        };
        let (begin, end) = bounds_for_row(length, begin, end, step, empty);
        if constant && step == 1 {
            reference = true;
        }
        if reference {
            entries.push(ListEntry::new(
                entry.offset as usize + begin as usize,
                (end - begin) as usize,
            ));
            continue;
        }
        if end - begin == 0 {
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        }
        let length = slice_length(begin, end, step, true)? as usize;
        let start = selection.len();
        let mut child = if step < 0 {
            entry.offset as i64 + end - 1
        } else {
            entry.offset as i64 + begin
        };
        for _ in 0..length {
            selection.push(Some(child as u32));
            child += step;
        }
        entries.push(ListEntry::new(start, length));
    }
    let (child, size) = if reference {
        (input.child.clone(), input.child_size)
    } else {
        let size = selection.len();
        let mut child = input.child.select_rows(&selection);
        child.flatten(size);
        (child, size)
    };
    let mut output = Vector::flat(ty.clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
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

/// DuckDB's `CheckIfParamIsEmpty`: an empty list constant marks an omitted bound.
fn param_is_empty(input: &BindInput<'_>, index: usize) -> Result<bool> {
    if input.argument_types[index].id() != LogicalTypeId::List {
        return Ok(false);
    }
    let empty = input.constants[index]
        .as_ref()
        .is_some_and(|value| !value.is_null() && value.children().is_some_and(<[_]>::is_empty));
    if !empty {
        return Err(Error::binder(
            "The upper and lower bounds of the slice must be a BIGINT",
        ));
    }
    Ok(true)
}

/// DuckDB's `ArraySliceBind`.
fn bind_slice(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let ty = &input.argument_types[0];
    match ty.id() {
        LogicalTypeId::Array => {
            let list = LogicalType::list(list_child_type(ty)?);
            function.arguments[0] = list.clone();
            function.return_type = list;
        }
        LogicalTypeId::List => {
            function.arguments[0] = ty.clone();
            function.return_type = ty.clone();
        }
        LogicalTypeId::Blob | LogicalTypeId::Varchar => {
            if function.arguments.len() == 4 {
                return Err(Error::new(
                    ErrorKind::NotImplemented,
                    "Slice with steps has not been implemented for string types, you can consider rewriting your \
                     query as follows:\n SELECT array_to_string((str_split(string, '')[begin:end:step], '');",
                ));
            }
            function.arguments[0] = ty.clone();
            function.return_type = ty.clone();
            for index in 1..3 {
                if input.argument_types[index].id() != LogicalTypeId::List {
                    function.arguments[index] = LogicalType::BIGINT;
                }
            }
        }
        LogicalTypeId::SqlNull | LogicalTypeId::Unknown => {
            function.arguments[0] = LogicalType::simple(LogicalTypeId::Unknown);
            function.return_type = LogicalType::SQLNULL;
        }
        _ => {
            return Err(Error::binder(
                "ARRAY_SLICE can only operate on LISTs and VARCHARs",
            ));
        }
    }
    let begin_is_empty = param_is_empty(input, 1)?;
    if !begin_is_empty {
        function.arguments[1] = LogicalType::BIGINT;
    }
    let end_is_empty = param_is_empty(input, 2)?;
    if !end_is_empty {
        function.arguments[2] = LogicalType::BIGINT;
    }
    Ok(Some(Arc::new(SliceBindData {
        begin_is_empty,
        end_is_empty,
    })))
}

/// `list_slice`/`array_slice`.
pub fn list_slice_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("list_slice");
    let mut arguments = vec![LogicalType::ANY; 3];
    for with_step in [false, true] {
        if with_step {
            arguments.push(LogicalType::BIGINT);
        }
        let mut function =
            ScalarFunction::new("list_slice", arguments.clone(), LogicalType::ANY, array_slice)
                .with_bind(bind_slice);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

/// DuckDB's `ListFlattenFunction`: concatenates the non-NULL inner lists.
fn flatten(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let outer = args.column(0);
    if outer.logical_type().id() == LogicalTypeId::SqlNull {
        *result = outer.clone();
        return Ok(());
    }
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let outer = addressable(outer, count);
    let outer_lists = ListInput::new(&outer)?;
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    if outer_lists.child.logical_type().id() == LogicalTypeId::SqlNull {
        for row in 0..rows {
            if outer_lists.get(row).is_none() {
                nulls.push(row);
            }
            entries.push(ListEntry::new(0, 0));
        }
        let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
        output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
        let child_type = list_child_type(result.logical_type())?;
        output.set_list_child(Vector::flat(child_type, 1), 0);
        let validity = output.validity_mut();
        for row in nulls {
            validity.set_invalid(row, rows);
        }
        if constant {
            output.make_constant();
        }
        *result = output;
        return Ok(());
    }
    let inner_lists = ListInput::new(outer_lists.child)?;
    let mut selection = Vec::new();
    for row in 0..rows {
        let Some(outer_entry) = outer_lists.get(row) else {
            nulls.push(row);
            entries.push(ListEntry::new(selection.len(), 0));
            continue;
        };
        let start = selection.len();
        for inner in outer_entry.offset as usize..(outer_entry.offset + outer_entry.length) as usize {
            let Some(inner_entry) = inner_lists.get(inner) else {
                continue;
            };
            selection.extend(
                (inner_entry.offset as usize..(inner_entry.offset + inner_entry.length) as usize)
                    .map(|item| Some(item as u32)),
            );
        }
        entries.push(ListEntry::new(start, selection.len() - start));
    }
    let size = selection.len();
    let mut child = inner_lists.child.select_rows(&selection);
    child.flatten(size);
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
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

/// `flatten(list of lists)`.
pub fn flatten_functions() -> ScalarFunctionSet {
    let element = template("T");
    let mut set = ScalarFunctionSet::new("flatten");
    set.add(ScalarFunction::new(
        "flatten",
        vec![LogicalType::list(LogicalType::list(element.clone()))],
        LogicalType::list(element),
        |args, _, result| flatten(args, result),
    ));
    set
}
