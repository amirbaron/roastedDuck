//! `json_each` and `json_tree` (DuckDB's `json_table_in_out.cpp`, after SQLite): a row per
//! child of the value at a path, or per value nested in it, with its key, JSON, type,
//! atom, id, parent, full key and path.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::json_common::{JsonPathType, get_unsafe, read_document, val_type_to_string};
use crate::json_executors::{ColumnBuilder, NativeBuilder, StrBuilder, StrInput};
use crate::json_functions::check_path;
use crate::json_type;
use crate::yyjson::{JsonArena, ValId, tag, write_val};

/// Which function (`JSONTableInOutType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InOutType {
    Each,
    Tree,
}

/// The virtual columns (`GetJSONTableInOutVirtualColumns`): the input document, the
/// path the walk starts at, the empty column and a row number per input row.
const JSON_COLUMN: u64 = 1 << 63;
const ROOT_COLUMN: u64 = (1 << 63) + 1;
const EMPTY_COLUMN: u64 = u64::MAX - 1;
const ROW_ID_COLUMN: u64 = u64::MAX;

fn virtual_columns(_: Option<&dyn FunctionData>) -> Vec<(u64, rustdb_common::ByteString, LogicalType)> {
    vec![
        (JSON_COLUMN, "json".to_owned().into(), json_type()),
        (ROOT_COLUMN, "root".to_owned().into(), LogicalType::VARCHAR),
        (EMPTY_COLUMN, String::new().into(), LogicalType::BOOLEAN),
        (ROW_ID_COLUMN, "rowid".to_owned().into(), LogicalType::BIGINT),
    ]
}

/// The bind data: which function, and the input row of a call with constant arguments.
#[derive(Debug)]
struct InOutData {
    kind: InOutType,
    constant_row: Option<Vec<Value>>,
}

impl FunctionData for InOutData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `JSONTableInOutBind`.
fn in_out_bind(kind: InOutType, input: &TableFunctionBindInput<'_>) -> TableFunctionBindOutput {
    let schema = [
        ("key", LogicalType::VARCHAR),
        ("value", json_type()),
        ("type", LogicalType::VARCHAR),
        ("atom", json_type()),
        ("id", LogicalType::UBIGINT),
        ("parent", LogicalType::UBIGINT),
        ("fullkey", LogicalType::VARCHAR),
        ("path", LogicalType::VARCHAR),
    ];
    TableFunctionBindOutput {
        return_types: schema.iter().map(|(_, ty)| ty.clone()).collect(),
        names: schema.iter().map(|(name, _)| (*name).into()).collect(),
        bind_data: Some(Arc::new(InOutData {
            kind,
            constant_row: (!input.inputs.is_empty()).then(|| input.inputs.to_vec()),
        })),
    }
}

/// The state of a call with constant arguments: its input row and the walk.
#[derive(Debug, Default)]
struct ConstantScan {
    input: Option<DataChunk>,
    walk: TableScanState,
}

/// The scan of a call with constant arguments: the in-out function over its input row.
fn constant_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    scan_state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<InOutData>())
        .ok_or_else(|| Error::internal("json_each scan without bind data"))?;
    let state = scan_state
        .local
        .get_or_insert_with(|| Box::new(ConstantScan::default()))
        .downcast_mut::<ConstantScan>()
        .ok_or_else(|| Error::internal("json_each scan state of another function"))?;
    if state.input.is_none() {
        let columns = data
            .constant_row
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(Vector::constant)
            .collect::<Result<Vec<_>>>()?;
        state.input = Some(DataChunk::new(columns, 1));
        state.walk.column_ids = column_ids.to_vec();
    }
    let Some(input) = state.input.as_ref() else {
        return Ok(false);
    };
    let result = in_out_function(bind_data, &mut state.walk, input, chunk)?;
    Ok(result == InOutResult::HaveMoreOutput && !chunk.is_empty())
}

/// A container being walked (`JSONTableInOutRecursionNode`).
#[derive(Debug)]
struct RecursionNode {
    path: String,
    parent: ValId,
    child_index: usize,
}

/// A thread's progress through its input (`JSONTableInOutLocalState`).
#[derive(Debug, Default)]
struct InOutState {
    arena: JsonArena,
    /// The path the walk starts at.
    path: String,
    root: ValId,
    initialized: bool,
    row: usize,
    nodes: Vec<RecursionNode>,
    /// Rows produced for the current input row (the `rowid` column).
    total_count: i64,
}

impl InOutState {
    /// `GetPath`.
    fn current_path(&self) -> String {
        let mut result = self.path.clone();
        for node in &self.nodes {
            result.push_str(&node.path);
        }
        result
    }
}

/// The output columns under construction (`JSONTableInOutResult`).
#[derive(Default)]
struct Output {
    key: StrBuilder,
    value: StrBuilder,
    ty: StrBuilder,
    atom: StrBuilder,
    id: NativeBuilder<u64>,
    parent: NativeBuilder<u64>,
    fullkey: StrBuilder,
    path: StrBuilder,
    count: usize,
}

impl Output {
    /// `AddRow`: a row for `val` (the child `key` of an object, if any).
    fn add_row(&mut self, kind: InOutType, state: &InOutState, key: Option<ValId>, val: ValId) {
        let doc = state.arena.doc();
        let array_element = state.nodes.last().is_some_and(|node| doc.is_arr(node.parent));
        let child_index = state.nodes.last().map_or(0, |node| node.child_index);
        match key {
            Some(key) => self.key.push(doc.str(key)),
            None if array_element => self.key.push(child_index.to_string().as_bytes()),
            None => self.key.push_null(),
        }
        self.value.push_with(|buf| write_val(buf, &doc, val));
        self.ty.push(val_type_to_string(&doc, val).as_bytes());
        match doc.tag(val) {
            tag::NULL | tag::ARR | tag::OBJ => self.atom.push_null(),
            _ => self.atom.push_with(|buf| write_val(buf, &doc, val)),
        }
        self.id.push((val - state.root_doc()) as u64);
        match state.nodes.last() {
            Some(node) if kind == InOutType::Tree => {
                self.parent.push((node.parent - state.root_doc()) as u64);
            }
            _ => self.parent.push_null(),
        }
        let path = state.current_path();
        let fullkey = match key {
            Some(key) => format!("{path}.{}", String::from_utf8_lossy(doc.str(key))),
            None if array_element => format!("{path}[{child_index}]"),
            None => path.clone(),
        };
        self.fullkey.push(fullkey.as_bytes());
        self.path.push(path.as_bytes());
        self.count += 1;
    }

    /// The columns `column_ids` asks for (all eight when empty), virtual ones included;
    /// `row` describes the input row the rows came from. Other ids (the ordinality the
    /// operator adds) are not the function's.
    fn finish(self, column_ids: &[u64], row: &RowColumns) -> Result<Vec<Vector>> {
        let count = self.count;
        let columns = vec![
            self.key.finish(&LogicalType::VARCHAR)?,
            self.value.finish(&json_type())?,
            self.ty.finish(&LogicalType::VARCHAR)?,
            self.atom.finish(&json_type())?,
            self.id.finish(&LogicalType::UBIGINT)?,
            self.parent.finish(&LogicalType::UBIGINT)?,
            self.fullkey.finish(&LogicalType::VARCHAR)?,
            self.path.finish(&LogicalType::VARCHAR)?,
        ];
        if column_ids.is_empty() {
            return Ok(columns);
        }
        let mut result = Vec::with_capacity(column_ids.len());
        for &column in column_ids {
            match column {
                JSON_COLUMN => result.push(Vector::constant(&row.json)?.reinterpret(json_type())?),
                ROOT_COLUMN => result.push(Vector::constant(&Value::varchar(&row.root))?),
                EMPTY_COLUMN => result.push(Vector::constant(&Value::null(LogicalType::BOOLEAN))?),
                ROW_ID_COLUMN => {
                    let mut rowid = Vector::flat(LogicalType::BIGINT, count.max(1));
                    for (offset, value) in rowid.values_mut::<i64>()[..count].iter_mut().enumerate() {
                        *value = row.first_rowid + offset as i64;
                    }
                    result.push(rowid);
                }
                _ => {
                    if let Some(vector) = usize::try_from(column).ok().and_then(|index| columns.get(index)) {
                        result.push(vector.clone());
                    }
                }
            }
        }
        Ok(result)
    }
}

/// The constant virtual columns of the rows of one input row.
struct RowColumns {
    json: Value,
    root: String,
    first_rowid: i64,
}

impl InOutState {
    /// The slot of the document's root (ids are relative to it).
    fn root_doc(&self) -> ValId {
        self.root
    }
}

/// The casts used to bring constant arguments to JSON / VARCHAR.
fn casts() -> &'static CastFunctionSet {
    static CASTS: OnceLock<CastFunctionSet> = OnceLock::new();
    CASTS.get_or_init(|| {
        let mut casts = CastFunctionSet::new();
        crate::json_casts::register_json_casts(&mut casts);
        casts
    })
}

/// The input column as text: JSON / VARCHAR as is, other types through their cast.
fn text_column(column: &Vector, rows: usize, target: &LogicalType) -> Result<Vector> {
    if column.logical_type().id() == LogicalTypeId::Varchar {
        return Ok(column.flattened(rows.max(1)));
    }
    cast_vector(casts(), column, target, rows.max(1), &mut CastParameters::new())
}

/// `InitializeLocalState`: parses the row's document and starts the walk at the path.
fn initialize_row(
    kind: InOutType,
    state: &mut InOutState,
    input: &DataChunk,
    row: usize,
    out: &mut Output,
) -> Result<()> {
    state.nodes.clear();
    state.arena.reset();
    state.total_count = 0;
    let mut path_value = Value::varchar("$");
    if input.column_count() > 1 {
        let paths = text_column(input.column(1), input.len(), &LogicalType::VARCHAR)?;
        let path = StrInput::new(&paths).get(row);
        let Some(path) = path else {
            return Ok(());
        };
        path_value = Value::bytes(LogicalType::VARCHAR, path);
    }
    let (path, path_type) = check_path(&path_value)?;
    if path_type == JsonPathType::Wildcard {
        return Err(Error::binder(
            "Wildcard JSON path not supported in json_each/json_tree",
        ));
    }
    if path.first() != Some(&b'$') {
        return Err(Error::binder(
            "JSON path must start with '$' for json_each/json_tree",
        ));
    }
    state.path = String::from_utf8_lossy(&path).into_owned();
    let documents = text_column(input.column(0), input.len(), &json_type())?;
    let Some(json) = StrInput::new(&documents).get(row) else {
        return Ok(());
    };
    let doc = read_document(&mut state.arena, json)?;
    state.root = doc.root;
    let view = state.arena.doc();
    let Some(start) = get_unsafe(&view, doc.root, &path) else {
        return Ok(());
    };
    let is_container = view.is_ctn(start);
    if !is_container || kind == InOutType::Tree {
        out.add_row(kind, state, None, start);
    }
    if is_container {
        state.nodes.push(RecursionNode {
            path: String::new(),
            parent: start,
            child_index: 0,
        });
    }
    Ok(())
}

/// Walks the current document depth first until the output is full (the loop of
/// `JSONTableInOutFunction`).
fn walk(kind: InOutType, state: &mut InOutState, out: &mut Output) {
    while out.count != STANDARD_VECTOR_SIZE {
        let Some(node) = state.nodes.last() else {
            return;
        };
        let doc = state.arena.doc();
        let parent = node.parent;
        let start = node.child_index;
        let children: Vec<(Option<ValId>, ValId)> = if doc.is_arr(parent) {
            doc.arr_iter(parent).map(|value| (None, value)).collect()
        } else {
            doc.obj_iter(parent)
                .map(|(key, value)| (Some(key), value))
                .collect()
        };
        let mut finished = true;
        for (idx, &(key, value)) in children.iter().enumerate().skip(start) {
            out.add_row(kind, state, key, value);
            if let Some(node) = state.nodes.last_mut() {
                node.child_index += 1;
            }
            if kind == InOutType::Tree && doc.is_ctn(value) {
                let path = match key {
                    Some(key) => format!(".{}", String::from_utf8_lossy(doc.str(key))),
                    None => format!("[{idx}]"),
                };
                state.nodes.push(RecursionNode {
                    path,
                    parent: value,
                    child_index: 0,
                });
                finished = false;
                break;
            }
            if out.count == STANDARD_VECTOR_SIZE {
                finished = idx + 1 == children.len();
                break;
            }
        }
        if finished {
            state.nodes.pop();
        }
    }
}

/// `JSONTableInOutFunction`.
fn in_out_function(
    bind_data: Option<&dyn FunctionData>,
    scan_state: &mut TableScanState,
    input: &DataChunk,
    output: &mut DataChunk,
) -> Result<InOutResult> {
    let kind = bind_data
        .and_then(|data| data.as_any().downcast_ref::<InOutData>())
        .map_or(InOutType::Each, |data| data.kind);
    let state = scan_state
        .local
        .get_or_insert_with(|| Box::new(InOutState::default()))
        .downcast_mut::<InOutState>()
        .ok_or_else(|| Error::internal("json_each state of another function"))?;
    let column_ids = &scan_state.column_ids;
    let mut out = Output::default();
    loop {
        if state.row >= input.len() {
            state.row = 0;
            state.initialized = false;
            let row = RowColumns {
                json: Value::null(json_type()),
                root: String::new(),
                first_rowid: 0,
            };
            output.set_columns(out.finish(column_ids, &row)?, 0);
            return Ok(InOutResult::NeedMoreInput);
        }
        if !state.initialized {
            initialize_row(kind, state, input, state.row, &mut out)?;
            state.initialized = true;
        }
        walk(kind, state, &mut out);
        // An output chunk holds the rows of one input row.
        let row = RowColumns {
            json: input.column(0).get_value(state.row),
            root: state.path.clone(),
            first_rowid: state.total_count,
        };
        state.total_count += out.count as i64;
        if state.nodes.is_empty() {
            // This row is done; the next call continues with the next row.
            state.row += 1;
            state.initialized = false;
        }
        if out.count > 0 {
            let count = out.count;
            output.set_columns(out.finish(column_ids, &row)?, count);
            return Ok(InOutResult::HaveMoreOutput);
        }
    }
}

/// `GetJSONTableInOutFunction`.
fn in_out_table_function(name: &str, kind: InOutType, input: LogicalType, with_path: bool) -> TableFunction {
    let mut arguments = vec![input];
    if with_path {
        arguments.push(LogicalType::VARCHAR);
    }
    let mut function = TableFunction::new(name, arguments)
        .with_bind(move |input: &TableFunctionBindInput<'_>| Ok(in_out_bind(kind, input)))
        .with_scan(constant_scan)
        .with_in_out(in_out_function);
    // The engine's in-out operator expects every column (no projection pushdown).
    function.in_out = true;
    function.get_virtual_columns = Some(Arc::new(virtual_columns));
    // The scan of a constant call produces one column per column id.
    function.projected_output = true;
    function
}

/// `json_each` and `json_tree`.
pub(crate) fn in_out_functions() -> Vec<TableFunctionSet> {
    [("json_each", InOutType::Each), ("json_tree", InOutType::Tree)]
        .into_iter()
        .map(|(name, kind)| {
            let mut set = TableFunctionSet::new(name);
            for input in [LogicalType::VARCHAR, json_type()] {
                set.add(in_out_table_function(name, kind, input.clone(), false));
                set.add(in_out_table_function(name, kind, input, true));
            }
            set
        })
        .collect()
}
