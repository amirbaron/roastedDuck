//! Row loops of the JSON read functions (DuckDB's `json_executors.hpp`): parse each input
//! document once per row, follow the path(s), and hand the value to the function, which
//! appends its result to a column builder.

use std::borrow::Cow;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::{FunctionData, FunctionState};
use rustdb_types::{
    DataChunk, ListEntry, LogicalType, NativeType, StringHeap, StringT, Unified, ValidityMask, Vector,
    VectorKind,
};

use crate::json_common::{JsonPathType, get, get_unsafe, get_wildcard_path, read_document};
use crate::json_functions::{JsonReadFunctionData, JsonReadManyFunctionData};
use crate::yyjson::{Doc, JsonArena, ValId};

/// The per-call parse arena of a JSON function (`JSONFunctionLocalState`), created on first
/// use and reset per chunk.
pub(crate) fn local_arena(state: &mut FunctionState) -> &mut JsonArena {
    let local = state.local.get_or_insert_with(|| Box::new(JsonArena::default()));
    if !local.is::<JsonArena>() {
        *local = Box::new(JsonArena::default());
    }
    match local.downcast_mut::<JsonArena>() {
        Some(arena) => arena,
        None => unreachable!("the local state was just made a JsonArena"),
    }
}

/// `vector` with sequences expanded, so that unified access works.
pub(crate) fn addressable(vector: &Vector, count: usize) -> Cow<'_, Vector> {
    if vector.kind() == VectorKind::Sequence {
        Cow::Owned(vector.flattened(count))
    } else {
        Cow::Borrowed(vector)
    }
}

/// Row access to a VARCHAR/JSON column.
pub(crate) struct StrInput<'a> {
    vector: &'a Vector,
    unified: Unified<'a, StringT>,
}

impl<'a> StrInput<'a> {
    pub(crate) fn new(vector: &'a Vector) -> Self {
        Self {
            vector,
            unified: vector.unified::<StringT>(),
        }
    }

    /// The bytes of row `row`, or `None` if NULL.
    #[inline]
    pub(crate) fn get(&self, row: usize) -> Option<&'a [u8]> {
        let slot = self.unified.slot(row);
        if !self.unified.validity.is_valid(slot) {
            return None;
        }
        let value: &'a StringT = &self.unified.values[slot];
        Some(self.vector.string_value(value))
    }
}

/// Appends values to a column under construction.
pub(crate) trait ColumnBuilder: Default {
    /// Appends a NULL.
    fn push_null(&mut self);
    /// The number of values appended.
    fn len(&self) -> usize;
    /// The finished vector of type `ty`.
    fn finish(self, ty: &LogicalType) -> Result<Vector>;
}

/// Builds a VARCHAR/JSON column.
#[derive(Debug, Default)]
pub(crate) struct StrBuilder {
    values: Vec<StringT>,
    nulls: Vec<usize>,
    heap: StringHeap,
    scratch: Vec<u8>,
}

impl StrBuilder {
    /// Appends `bytes`.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let value = self.heap.add(bytes);
        self.values.push(value);
    }

    /// Appends the bytes written by `write` into a scratch buffer.
    pub(crate) fn push_with(&mut self, write: impl FnOnce(&mut Vec<u8>)) {
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        write(&mut scratch);
        self.push(&scratch);
        self.scratch = scratch;
    }
}

/// Finishes a flat column of native values with NULLs at `nulls`.
fn finish_native<T: NativeType>(ty: &LogicalType, values: &[T], nulls: &[usize]) -> Vector {
    let rows = values.len();
    let mut vector = Vector::flat(ty.clone(), rows.max(1));
    vector.values_mut::<T>()[..rows].copy_from_slice(values);
    set_nulls(vector.validity_mut(), nulls, rows);
    vector
}

fn set_nulls(validity: &mut ValidityMask, nulls: &[usize], rows: usize) {
    for &row in nulls {
        validity.set_invalid(row, rows.max(1));
    }
}

impl ColumnBuilder for StrBuilder {
    fn push_null(&mut self) {
        self.nulls.push(self.values.len());
        self.values.push(StringT::default());
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn finish(self, ty: &LogicalType) -> Result<Vector> {
        let mut vector = finish_native(ty, &self.values, &self.nulls);
        vector.add_heap(self.heap);
        Ok(vector)
    }
}

/// Builds a column of a fixed-width type.
#[derive(Debug)]
pub(crate) struct NativeBuilder<T> {
    values: Vec<T>,
    nulls: Vec<usize>,
}

impl<T> Default for NativeBuilder<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            nulls: Vec::new(),
        }
    }
}

impl<T: NativeType + Default> NativeBuilder<T> {
    pub(crate) fn push(&mut self, value: T) {
        self.values.push(value);
    }
}

impl<T: NativeType + Default> ColumnBuilder for NativeBuilder<T> {
    fn push_null(&mut self) {
        self.nulls.push(self.values.len());
        self.values.push(T::default());
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn finish(self, ty: &LogicalType) -> Result<Vector> {
        Ok(finish_native(ty, &self.values, &self.nulls))
    }
}

/// Builds a LIST column whose elements are built by `B`.
#[derive(Debug, Default)]
pub(crate) struct ListBuilder<B> {
    entries: Vec<ListEntry>,
    nulls: Vec<usize>,
    pub(crate) child: B,
}

impl<B: ColumnBuilder> ListBuilder<B> {
    /// Appends a list whose elements `fill` appends to the child builder.
    pub(crate) fn push_list(&mut self, fill: impl FnOnce(&mut B) -> Result<()>) -> Result<()> {
        let offset = self.child.len();
        fill(&mut self.child)?;
        self.entries
            .push(ListEntry::new(offset, self.child.len() - offset));
        Ok(())
    }
}

impl<B: ColumnBuilder> ColumnBuilder for ListBuilder<B> {
    fn push_null(&mut self) {
        self.nulls.push(self.entries.len());
        self.entries.push(ListEntry::new(self.child.len(), 0));
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn finish(self, ty: &LogicalType) -> Result<Vector> {
        let child_type = ty.child_type().cloned().unwrap_or(LogicalType::SQLNULL);
        let size = self.child.len();
        let child = self.child.finish(&child_type)?;
        let mut vector = finish_native(ty, &self.entries, &self.nulls);
        vector.set_list_child(child, size);
        Ok(vector)
    }
}

/// Whether every argument is a constant vector (`DataChunk::AllConstant`).
pub(crate) fn all_constant(args: &DataChunk) -> bool {
    args.columns().iter().all(Vector::is_constant)
}

/// The number of rows to compute: one if every argument is constant.
fn result_rows(args: &DataChunk) -> (usize, bool) {
    let constant = all_constant(args);
    (if constant { 1 } else { args.len() }, constant)
}

/// Stores the finished column as the function result.
fn store<B: ColumnBuilder>(builder: B, result: &mut Vector, constant: bool) -> Result<()> {
    let ty = result.logical_type().clone();
    *result = builder.finish(&ty)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// What a JSON read function computes from one value: appends exactly one element.
pub(crate) type JsonFn<'f, B> = dyn FnMut(&Doc<'_>, ValId, &mut B) -> Result<()> + 'f;

/// `JSONExecutors::UnaryExecute`: `f` of each document's root.
pub(crate) fn unary_execute<B: ColumnBuilder>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    f: &mut JsonFn<'_, B>,
) -> Result<()> {
    let (rows, constant) = result_rows(args);
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let arena = local_arena(state);
    let mut builder = B::default();
    for row in 0..rows {
        let Some(json) = input.get(row) else {
            builder.push_null();
            continue;
        };
        arena.reset();
        let doc = read_document(arena, json)?;
        f(&arena.doc(), doc.root, &mut builder)?;
    }
    store(builder, result, constant)
}

/// The bind data of a call with one path argument.
fn read_data(state: &FunctionState) -> Option<Arc<dyn FunctionData>> {
    state.bind_data.clone()
}

/// `JSONExecutors::BinaryExecute`: `f` of the value at the path; NULL where it does not
/// exist (unless `null_if_not_found` is off, then `f` sees `None`... as DuckDB, which
/// passes a null value pointer: here `f` is not called and the closure `missing` runs).
pub(crate) fn binary_execute<B: ColumnBuilder>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    f: &mut JsonFn<'_, B>,
    missing: Option<&mut dyn FnMut(&mut B)>,
) -> Result<()> {
    let bind_data = read_data(state);
    let info = bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<JsonReadFunctionData>())
        .cloned()
        .unwrap_or_default();
    let mut missing = missing;
    let (rows, constant) = result_rows(args);
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let arena = local_arena(state);
    if info.constant {
        if info.path_type == JsonPathType::Wildcard {
            // A list of every match per row.
            let mut builder = ListBuilder::<B>::default();
            let mut vals = Vec::new();
            for row in 0..rows {
                let Some(json) = input.get(row) else {
                    builder.push_null();
                    continue;
                };
                arena.reset();
                let doc = read_document(arena, json)?;
                let view = arena.doc();
                vals.clear();
                get_wildcard_path(&view, doc.root, &info.path, &mut vals);
                builder.push_list(|child| {
                    for &val in &vals {
                        f(&view, val, child)?;
                    }
                    Ok(())
                })?;
            }
            return store(builder, result, constant);
        }
        let mut builder = B::default();
        for row in 0..rows {
            let Some(json) = input.get(row) else {
                builder.push_null();
                continue;
            };
            arena.reset();
            let doc = read_document(arena, json)?;
            let view = arena.doc();
            match get_unsafe(&view, doc.root, &info.path) {
                Some(val) => f(&view, val, &mut builder)?,
                None => match missing.as_mut() {
                    Some(missing) => missing(&mut builder),
                    None => builder.push_null(),
                },
            }
        }
        return store(builder, result, constant);
    }
    // The path is a column (or a NULL constant).
    let path_column = args.column(1);
    let integral = path_column.logical_type().is_integral();
    let paths: Cow<'_, Vector> = if integral {
        Cow::Owned(cast_to_varchar(path_column, rows)?)
    } else {
        addressable(path_column, rows)
    };
    let path_input = StrInput::new(&paths);
    let mut builder = B::default();
    for row in 0..rows {
        let (Some(json), Some(path)) = (input.get(row), path_input.get(row)) else {
            builder.push_null();
            continue;
        };
        arena.reset();
        let doc = read_document(arena, json)?;
        let view = arena.doc();
        match get(&view, doc.root, path, integral)? {
            Some(val) => f(&view, val, &mut builder)?,
            None => match missing.as_mut() {
                Some(missing) => missing(&mut builder),
                None => builder.push_null(),
            },
        }
    }
    store(builder, result, constant)
}

/// The integral path column as text (`VectorOperations::DefaultCast` to VARCHAR).
fn cast_to_varchar(vector: &Vector, rows: usize) -> Result<Vector> {
    let mut out = StrBuilder::default();
    let flat = vector.flattened(rows);
    for row in 0..rows {
        let value = flat.get_value(row);
        if value.is_null() {
            out.push_null();
        } else {
            out.push(value.to_varchar()?.as_bytes());
        }
    }
    out.finish(&LogicalType::VARCHAR)
}

/// `JSONExecutors::ExecuteMany`: a list with `f` of the value at each of the paths.
pub(crate) fn execute_many<B: ColumnBuilder>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    f: &mut JsonFn<'_, B>,
    missing: Option<&mut dyn FnMut(&mut B)>,
) -> Result<()> {
    let bind_data = read_data(state);
    let paths: Vec<Vec<u8>> = bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<JsonReadManyFunctionData>())
        .map(|data| data.paths.clone())
        .unwrap_or_default();
    let mut missing = missing;
    let (rows, constant) = result_rows(args);
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let arena = local_arena(state);
    let mut builder = ListBuilder::<B>::default();
    for row in 0..rows {
        let Some(json) = input.get(row) else {
            builder.push_null();
            continue;
        };
        arena.reset();
        let doc = read_document(arena, json)?;
        let view = arena.doc();
        builder.push_list(|child| {
            for path in &paths {
                match get_unsafe(&view, doc.root, path) {
                    Some(val) => f(&view, val, child)?,
                    None => match missing.as_mut() {
                        Some(missing) => missing(child),
                        None => child.push_null(),
                    },
                }
            }
            Ok(())
        })?;
    }
    store(builder, result, constant)
}
