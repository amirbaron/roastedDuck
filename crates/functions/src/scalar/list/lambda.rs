//! Lambda functions over lists: `list_transform`, `list_filter` and `list_reduce`
//! (DuckDB's `lambda_functions.cpp`, `list_transform.cpp`, `list_filter.cpp` and
//! `list_reduce.cpp`).
//!
//! The binder binds the lambda body and hands it to the executor, which evaluates it
//! through [`rustdb_function::LambdaExecutor`]: the kernels lay out the lambda's input
//! chunk exactly like DuckDB (index parameter first, then the element(s), then the other
//! arguments: outer lambda parameters and captured columns).

use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    BindInput, FunctionData, FunctionState, LambdaExecutor, NullHandling, ScalarFunction, ScalarFunctionSet,
};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, SelectionVector, Value, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, all_constant};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// DuckDB's `ListLambdaBindData` (the lambda itself lives in the executor).
#[derive(Debug, Clone, Copy)]
struct LambdaData {
    has_index: bool,
    has_initial: bool,
}

impl FunctionData for LambdaData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `LambdaFunctions::DetermineListChildType`.
fn list_child_type(ty: &LogicalType) -> Result<LogicalType> {
    match ty.id() {
        LogicalTypeId::SqlNull | LogicalTypeId::Unknown => Ok(ty.clone()),
        LogicalTypeId::List | LogicalTypeId::Array => ty
            .child_type()
            .or_else(|| ty.array_child().map(|(child, _)| child))
            .cloned()
            .ok_or_else(|| Error::internal("list type without a child type")),
        _ => Err(Error::internal("The first argument must be a list or array type")),
    }
}

/// `LambdaFunctions::BindBinaryChildren`: `(element[, index])`.
fn bind_binary_children(child_types: &[LogicalType], parameter: usize) -> Result<LogicalType> {
    let element = list_child_type(&child_types[0])?;
    match parameter {
        0 => Ok(element),
        1 => Ok(LogicalType::BIGINT),
        _ => Err(Error::binder(
            "This lambda function only supports up to two lambda parameters!",
        )),
    }
}

/// The reduce element type, widened to the initial value's type (`BindReduceChildren`).
fn reduce_child_type(child_types: &[LogicalType]) -> Result<LogicalType> {
    let element = list_child_type(&child_types[0])?;
    match child_types.get(2) {
        Some(initial) if *initial != element => LogicalType::try_max_logical_type(&element, initial)
            .ok_or_else(|| {
                Error::binder(
                    "The initial value type must be the same as the list child type or a common super type",
                )
            }),
        _ => Ok(element),
    }
}

/// `BindReduceChildren`: `(accumulator, element[, index])`.
fn bind_reduce_children(child_types: &[LogicalType], parameter: usize) -> Result<LogicalType> {
    let element = reduce_child_type(child_types)?;
    match parameter {
        0 | 1 => Ok(element),
        2 => Ok(LogicalType::BIGINT),
        _ => Err(Error::binder(
            "This lambda function only supports up to three lambda parameters!",
        )),
    }
}

/// `ListLambdaPrepareBind` and `AddArrayCastToList`: `None` to continue binding.
fn prepare_bind(function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<BindResult>> {
    let ty = &input.argument_types[0];
    match ty.id() {
        LogicalTypeId::SqlNull => {
            function.arguments[0] = LogicalType::SQLNULL;
            function.return_type = LogicalType::SQLNULL;
            return Ok(Some(Ok(Some(Arc::new(LambdaData {
                has_index: false,
                has_initial: false,
            })))));
        }
        LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
        LogicalTypeId::Array => function.arguments[0] = LogicalType::list(list_child_type(ty)?),
        _ => function.arguments[0] = ty.clone(),
    }
    Ok(None)
}

fn lambda_input<'a>(input: &'a BindInput<'_>) -> Result<&'a rustdb_function::LambdaInput> {
    input
        .lambda
        .ok_or_else(|| Error::binder("Invalid lambda expression!"))
}

/// `ListTransformBind`.
fn bind_transform(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let lambda = lambda_input(input)?;
    if let Some(result) = prepare_bind(function, input)? {
        return result;
    }
    function.return_type = LogicalType::list(lambda.return_type.clone());
    Ok(Some(Arc::new(LambdaData {
        has_index: lambda.parameter_count == 2,
        has_initial: false,
    })))
}

/// `ListFilterBind`: the lambda is cast to BOOLEAN.
fn bind_filter(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let lambda = lambda_input(input)?;
    if lambda.return_type != LogicalType::BOOLEAN {
        function.lambda_cast = Some(LogicalType::BOOLEAN);
    }
    if let Some(result) = prepare_bind(function, input)? {
        return result;
    }
    function.return_type = function.arguments[0].clone();
    Ok(Some(Arc::new(LambdaData {
        has_index: lambda.parameter_count == 2,
        has_initial: false,
    })))
}

/// `ListReduceBind`.
fn bind_reduce(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let lambda = lambda_input(input)?;
    if let Some(result) = prepare_bind(function, input)? {
        return result;
    }
    let mut element = list_child_type(&function.arguments[0])?;
    let has_initial = input.argument_types.len() == 3;
    if has_initial {
        let initial = &input.argument_types[2];
        if *initial != element {
            element = LogicalType::try_max_logical_type(&element, initial).ok_or_else(|| {
                Error::binder(
                    "The initial value type must be the same as the list child type or a common super type",
                )
            })?;
            function.arguments[0] = LogicalType::list(element.clone());
            function.arguments[2] = element.clone();
        }
    }
    if !(2..=3).contains(&lambda.parameter_count) {
        return Err(Error::binder(
            "list_reduce expects a function with 2 or 3 arguments",
        ));
    }
    if lambda.return_type != element {
        function.lambda_cast = Some(element.clone());
    }
    function.return_type = element;
    Ok(Some(Arc::new(LambdaData {
        has_index: lambda.parameter_count == 3,
        has_initial,
    })))
}

/// The bind data and lambda of a call.
fn lambda_state(state: &mut FunctionState) -> Result<(LambdaData, &mut Box<dyn LambdaExecutor>)> {
    let data = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<LambdaData>())
        .copied()
        .ok_or_else(|| Error::internal("lambda function without bind data"))?;
    let lambda = state
        .lambda
        .as_mut()
        .ok_or_else(|| Error::internal("lambda function without a lambda"))?;
    Ok((data, lambda))
}

/// The vectors in `parts` (each with its row count), concatenated into one vector of `ty`.
fn concatenate(ty: &LogicalType, parts: &[(Vector, usize)], total: usize) -> Result<Vector> {
    let mut target = Vector::flat(ty.clone(), total.max(1));
    let mut position = 0u32;
    for (part, len) in parts {
        let positions: Vec<u32> = (position..position + *len as u32).collect();
        scatter(part, &mut target, &positions)?;
        position += *len as u32;
    }
    Ok(target)
}

/// Which list lambda function runs (DuckDB's functors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Transform,
    Filter,
}

/// DuckDB's `ExecuteLambda`: evaluates the lambda on batches of up to
/// `STANDARD_VECTOR_SIZE` elements.
fn execute_lambda(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    mode: Mode,
) -> Result<()> {
    let result_type = result.logical_type().clone();
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result_type);
        return Ok(());
    }
    let (data, lambda) = lambda_state(state)?;
    let rows = args.len();
    let lists = addressable(args.column(0), rows);
    let input = ListInput::new(&lists)?;
    let others: Vec<_> = args.columns()[1..]
        .iter()
        .map(|column| addressable(column, rows))
        .collect();
    let child_type = list_child_type(&result_type)?;

    let mut entries = vec![ListEntry::new(0, 0); rows.max(1)];
    let mut nulls = Vec::new();
    let mut parts: Vec<(Vector, usize)> = Vec::new();
    let mut total = 0usize;
    // list_filter bookkeeping: the source length of each row and the cursor.
    let mut lengths: Vec<usize> = Vec::with_capacity(rows);
    let mut filter = FilterCursor::default();

    let mut element_rows: Vec<u32> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
    let mut source_rows: Vec<u32> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
    let mut indexes: Vec<i64> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
    let mut offset = 0usize;

    let mut run_batch = |element_rows: &mut Vec<u32>,
                         source_rows: &mut Vec<u32>,
                         indexes: &mut Vec<i64>,
                         entries: &mut [ListEntry],
                         lengths: &[usize],
                         filter: &mut FilterCursor|
     -> Result<()> {
        let count = element_rows.len();
        let elements = input
            .child
            .slice(&SelectionVector::new(std::mem::take(element_rows)));
        let mut columns = Vec::with_capacity(others.len() + 2);
        if data.has_index {
            let mut index = Vector::flat(LogicalType::BIGINT, count.max(1));
            index.values_mut::<i64>()[..count].copy_from_slice(indexes);
            columns.push(index);
        }
        columns.push(elements.clone());
        let selection = SelectionVector::new(std::mem::take(source_rows));
        for column in &others {
            columns.push(if column.is_constant() {
                column.as_ref().clone()
            } else {
                column.slice(&selection)
            });
        }
        indexes.clear();
        let chunk = DataChunk::new(columns, count);
        let values = lambda.execute(&chunk)?;
        match mode {
            Mode::Transform => {
                parts.push((values.flattened(count), count));
                total += count;
            }
            Mode::Filter => {
                let selected = filter.advance(&values, count, entries, lengths);
                let kept = selected.len();
                parts.push((
                    elements.slice(&SelectionVector::new(selected)).flattened(kept),
                    kept,
                ));
                total += kept;
            }
        }
        Ok(())
    };

    for row in 0..rows {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            lengths.push(0);
            continue;
        };
        match mode {
            Mode::Transform => {
                entries[row] = ListEntry::new(offset, entry.length as usize);
                offset += entry.length as usize;
            }
            Mode::Filter => lengths.push(entry.length as usize),
        }
        for index in 0..entry.length {
            if element_rows.len() == STANDARD_VECTOR_SIZE {
                run_batch(
                    &mut element_rows,
                    &mut source_rows,
                    &mut indexes,
                    &mut entries,
                    &lengths,
                    &mut filter,
                )?;
            }
            element_rows
                .push(u32::try_from(entry.offset + index).map_err(|_| Error::internal("list too large"))?);
            source_rows.push(row as u32);
            indexes.push(index as i64 + 1);
        }
    }
    run_batch(
        &mut element_rows,
        &mut source_rows,
        &mut indexes,
        &mut entries,
        &lengths,
        &mut filter,
    )?;
    if mode == Mode::Filter {
        filter.finish(&mut entries, &lengths);
    }
    let child = concatenate(&child_type, &parts, total)?;
    let mut output = Vector::flat(result_type, rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries[..rows]);
    output.set_list_child(child, total);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    let volatile = state.lambda.as_ref().is_some_and(|lambda| lambda.is_volatile());
    if all_constant(args.columns()) && !volatile {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// The progress of `list_filter` through the source lists (DuckDB's `ListFilterInfo`).
#[derive(Debug, Default)]
struct FilterCursor {
    /// The current row.
    row: usize,
    /// Kept elements of the current row so far.
    length: usize,
    /// The result offset of the current row.
    offset: usize,
    /// Elements of the current row seen so far.
    source_length: usize,
}

impl FilterCursor {
    /// Gives empty rows their entry.
    fn skip_empty(&mut self, entries: &mut [ListEntry], lengths: &[usize]) {
        while self.row < lengths.len() && lengths[self.row] == 0 {
            entries[self.row] = ListEntry::new(self.offset, 0);
            self.row += 1;
        }
    }

    /// `ListFilterFunctor::AppendResult`: the batch positions whose lambda value is true.
    fn advance(
        &mut self,
        values: &Vector,
        count: usize,
        entries: &mut [ListEntry],
        lengths: &[usize],
    ) -> Vec<u32> {
        let truth = values.unified::<bool>();
        let mut selected = Vec::new();
        for position in 0..count {
            self.skip_empty(entries, lengths);
            if truth.get(position) == Some(true) {
                selected.push(position as u32);
                self.length += 1;
            }
            self.source_length += 1;
            if lengths.get(self.row) == Some(&self.source_length) {
                entries[self.row] = ListEntry::new(self.offset, self.length);
                self.offset += self.length;
                self.row += 1;
                self.length = 0;
                self.source_length = 0;
            }
        }
        self.skip_empty(entries, lengths);
        selected
    }

    fn finish(&mut self, entries: &mut [ListEntry], lengths: &[usize]) {
        self.skip_empty(entries, lengths);
    }
}

/// DuckDB's `ListReduceFunction`: folds every list in rounds, one element per round.
#[allow(
    clippy::too_many_lines,
    reason = "a direct port of ReduceExecuteInfo/ExecuteReduce"
)]
fn reduce(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let result_type = result.logical_type().clone();
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result_type);
        return Ok(());
    }
    let (data, lambda) = lambda_state(state)?;
    let rows = args.len();
    let lists = addressable(args.column(0), rows);
    let input = ListInput::new(&lists)?;
    let others: Vec<_> = args.columns()[1..]
        .iter()
        .map(|column| addressable(column, rows))
        .collect();
    let mut output = Vector::flat(result_type.clone(), rows.max(1));
    let mut active = vec![true; rows];
    // The first element of every valid list (the initial accumulator without an initial
    // value).
    let mut first_elements = Vec::new();
    for (row, is_active) in active.iter_mut().enumerate() {
        let Some(entry) = input.get(row) else {
            output.set_null(row);
            *is_active = false;
            continue;
        };
        if entry.length == 0 && !data.has_initial {
            return Err(Error::new(
                ErrorKind::ParameterNotAllowed,
                "Cannot perform list_reduce on an empty input list",
            ));
        }
        first_elements.push(u32::try_from(entry.offset).map_err(|_| Error::internal("list too large"))?);
    }
    let mut left = input.child.slice(&SelectionVector::new(first_elements));
    let initial_offset = usize::from(!data.has_initial);
    let mut loops = 0usize;
    loop {
        let loops_offset = loops + initial_offset;
        let mut right_rows = Vec::new();
        let mut left_rows = Vec::new();
        let mut active_rows = Vec::new();
        let mut valid_row = 0u32;
        for (row, is_active) in active.iter_mut().enumerate() {
            if !*is_active {
                continue;
            }
            let entry = input
                .get(row)
                .ok_or_else(|| Error::internal("active list_reduce row without a list"))?;
            let length = entry.length as usize;
            if length > loops_offset {
                right_rows.push(
                    u32::try_from(entry.offset as usize + loops_offset)
                        .map_err(|_| Error::internal("list too large"))?,
                );
                left_rows.push(valid_row);
                active_rows.push(row as u32);
            } else if length == 0 && data.has_initial && loops_offset == 0 {
                *is_active = false;
                output.set_value(row, &others[0].get_value(row))?;
            } else {
                *is_active = false;
                output.set_value(row, &left.get_value(valid_row as usize))?;
            }
            valid_row += 1;
        }
        if right_rows.is_empty() {
            break;
        }
        let count = right_rows.len();
        let mut columns = Vec::with_capacity(others.len() + 3);
        if data.has_index {
            columns.push(Vector::constant(&Value::bigint(loops_offset as i64 + 1))?);
        }
        let active_selection = SelectionVector::new(active_rows);
        columns.push(input.child.slice(&SelectionVector::new(right_rows)));
        if loops == 0 && data.has_initial {
            columns.push(others[0].slice(&active_selection));
        } else {
            columns.push(left.slice(&SelectionVector::new(left_rows)));
        }
        let skip = usize::from(data.has_initial);
        for i in 0..others.len() - skip {
            // DuckDB checks the constness of column `i` but references column `skip + i`.
            columns.push(if others[i].is_constant() {
                others[skip + i].as_ref().clone()
            } else {
                others[skip + i].slice(&active_selection)
            });
        }
        let chunk = DataChunk::new(columns, count);
        left = lambda.execute(&chunk)?.flattened(count);
        loops += 1;
    }
    let volatile = state.lambda.as_ref().is_some_and(|lambda| lambda.is_volatile());
    if all_constant(args.columns()) && !volatile {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn lambda_function(
    name: &str,
    arguments: Vec<LogicalType>,
    return_type: LogicalType,
    kernel: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
    bind: fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult,
    bind_lambda: rustdb_function::BindLambda,
) -> ScalarFunction {
    let mut function = ScalarFunction::new(name, arguments, return_type, kernel).with_bind(bind);
    function.null_handling = NullHandling::Special;
    function.bind_lambda = Some(bind_lambda);
    function
}

/// `list_transform`, `list_filter` and `list_reduce` with their aliases.
pub fn lambda_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let any_list = LogicalType::list(LogicalType::ANY);
    let lambda = LogicalType::simple(LogicalTypeId::Lambda);
    let mut transform = ScalarFunctionSet::new("list_transform");
    transform.add(lambda_function(
        "list_transform",
        vec![any_list.clone(), lambda.clone()],
        any_list.clone(),
        |args, state, result| execute_lambda(args, state, result, Mode::Transform),
        bind_transform,
        bind_binary_children,
    ));
    let mut filter = ScalarFunctionSet::new("list_filter");
    filter.add(lambda_function(
        "list_filter",
        vec![any_list.clone(), lambda.clone()],
        any_list.clone(),
        |args, state, result| execute_lambda(args, state, result, Mode::Filter),
        bind_filter,
        bind_binary_children,
    ));
    let mut reduce_set = ScalarFunctionSet::new("list_reduce");
    let reduce_function = lambda_function(
        "list_reduce",
        vec![any_list.clone(), lambda],
        LogicalType::ANY,
        reduce,
        bind_reduce,
        bind_reduce_children,
    );
    let mut with_initial = reduce_function.clone();
    with_initial.arguments.push(LogicalType::ANY);
    reduce_set.add(reduce_function);
    reduce_set.add(with_initial);
    vec![
        (
            transform,
            &["array_transform", "list_apply", "array_apply", "apply"],
        ),
        (filter, &["array_filter", "filter"]),
        (reduce_set, &["array_reduce", "reduce"]),
    ]
}
