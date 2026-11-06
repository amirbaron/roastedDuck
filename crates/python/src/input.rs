//! Native Arrow input factories. Query trees and bound scans own Python producers;
//! the lookup table contains weak references only.
use crate::connection::lock;
use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyList};
use rustdb_arrow::ffi::{ArrowArrayStream, ArrowArrayStreamWrapper, ArrowSchema};
use rustdb_ast::{
    CreateInfoKind, ExternalDependency, OrdinalityType, QueryNodeKind, SelectStatement, SqlStatementKind,
    TableFunctionRef, TableRef, TableRefKind,
};
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunctionBindInput, TableFunctionSet};
use rustdb_types::{LogicalType, Value};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const FUNCTION: &str = "arrow_scan";
const DUMB_FUNCTION: &str = "arrow_scan_dumb";
static IDS: AtomicU64 = AtomicU64::new(1);
static INPUTS: OnceLock<Mutex<HashMap<u64, Weak<ArrowInput>>>> = OnceLock::new();
fn inputs() -> &'static Mutex<HashMap<u64, Weak<ArrowInput>>> {
    INPUTS.get_or_init(Mutex::default)
}

#[derive(Debug)]
struct ArrowInput {
    id: u64,
    object: Py<PyAny>,
}
impl Drop for ArrowInput {
    fn drop(&mut self) {
        lock(inputs()).remove(&self.id);
    }
}
pub(super) fn python_error(error: &PyErr) -> Error {
    let mut message = error.to_string();
    Python::attach(|py| {
        if let Some(traceback) = error.traceback(py) {
            let frames = (|| -> PyResult<String> {
                let mut traceback = traceback.into_any();
                loop {
                    let next = traceback.getattr("tb_next")?;
                    if next.is_none() {
                        break;
                    }
                    traceback = next;
                }
                let mut frame = traceback.getattr("tb_frame")?;
                let mut output = String::from("\n\nAt:\n");
                while !frame.is_none() {
                    let code = frame.getattr("f_code")?;
                    let filename = code.getattr("co_filename")?.extract::<String>()?;
                    let name = code.getattr("co_name")?.extract::<String>()?;
                    let line = frame.getattr("f_lineno")?.extract::<i64>()?;
                    let _ = writeln!(output, "  {filename}({line}): {name}");
                    frame = frame.getattr("f_back")?;
                }
                Ok(output)
            })();
            if let Ok(frames) = frames {
                message.push_str(&frames);
            }
        }
    });
    Error::new(rustdb_common::ErrorKind::Invalid, message).with_subtype("python_callback")
}
impl ArrowInput {
    fn schema(&self, out: &mut ArrowSchema) -> Result<()> {
        let direct = Python::attach(|py| {
            let Some(mut pointer) =
                capsule_stream(self.object.bind(py)).map_err(|error| python_error(&error))?
            else {
                return Ok::<_, Error>(false);
            };
            // SAFETY: the named capsule is held by this input, and the GIL protects its interface.
            let stream = unsafe { pointer.as_mut() };
            if stream.release.is_none() {
                return Err(Error::invalid_input("The ArrowArrayStream was already released"));
            }
            let get_schema = stream
                .get_schema
                .ok_or_else(|| Error::invalid_input("Arrow stream has no get_schema callback"))?;
            // SAFETY: the live capsule interface provides this callback; out is writable.
            if unsafe { get_schema(stream, out) } != 0 {
                return Err(Error::invalid_input("Arrow stream get_schema failed"));
            }
            Ok(true)
        })?;
        if direct {
            return Ok(());
        }
        let exported = Python::attach(|py| {
            let object = self.object.bind(py);
            let schema = if crate::polars::kind(object)? == Some(true) {
                Some(
                    crate::polars::empty_frame(object)?
                        .call_method0("to_arrow")?
                        .getattr("schema")?,
                )
            } else if object.hasattr("__arrow_c_schema__")? {
                Some(object.clone())
            } else if object.hasattr("schema")? {
                let schema = object.getattr("schema")?;
                schema.hasattr("__arrow_c_schema__")?.then_some(schema)
            } else if object.hasattr("projected_schema")? {
                Some(object.getattr("projected_schema")?)
            } else {
                None
            };
            let Some(schema) = schema else {
                return Ok::<_, PyErr>(None);
            };
            let capsule = schema
                .call_method0("__arrow_c_schema__")?
                .cast_into::<PyCapsule>()?;
            let pointer = capsule
                .pointer_checked(Some(c"arrow_schema"))?
                .cast::<ArrowSchema>();
            // SAFETY: the named Arrow schema capsule transfers ownership to us.
            Ok(Some(unsafe {
                std::ptr::replace(pointer.as_ptr(), ArrowSchema::empty())
            }))
        })
        .map_err(|error| python_error(&error))?;
        if let Some(schema) = exported {
            *out = schema;
            return Ok(());
        }
        let mut owned = self.stream()?;
        let stream = &mut owned.arrow_array_stream;
        let get_schema = stream
            .get_schema
            .ok_or_else(|| Error::invalid_input("Arrow stream has no get_schema callback"))?;
        // SAFETY: the cached stream is owned, live, and locked; out is writable.
        let status = unsafe { get_schema(stream, out) };
        if status != 0 {
            return Err(Error::invalid_input("Arrow stream get_schema failed"));
        }
        Ok(())
    }
    fn stream(&self) -> Result<Box<ArrowArrayStreamWrapper>> {
        self.stream_projected(None)
    }
    fn stream_projected(
        &self,
        parameters: Option<&rustdb_arrow::ArrowStreamParameters>,
    ) -> Result<Box<ArrowArrayStreamWrapper>> {
        Python::attach(|py| {
            let object = self.object.bind(py);
            let lazy = crate::polars::kind(object)? == Some(true);
            let reader = if lazy {
                let frame = if let Some(parameters) = parameters {
                    if let Some(predicate) = crate::arrow_filter::predicate(py, parameters, true)? {
                        object.call_method1("filter", (predicate,))?
                    } else {
                        object.clone()
                    }
                } else {
                    object.clone()
                };
                frame.call_method0("collect")?.call_method0("to_arrow")?
            } else if parameters.is_some() && {
                let dataset = py.import("pyarrow.dataset")?;
                object.is_instance(&dataset.getattr("Dataset")?)?
                    || object.is_instance(&dataset.getattr("Scanner")?)?
            } {
                // Keep datasets intact so their scanner can prune fragments/row groups.
                // Turning them into a reader here would only filter after I/O.
                object.clone()
            } else if object.is_instance_of::<PyCapsule>() || object.hasattr("__arrow_c_stream__")? {
                object.clone()
            } else if object.is_instance(&py.import("pyarrow")?.getattr("RecordBatch")?)? {
                py.import("pyarrow")?.getattr("RecordBatchReader")?.call_method1(
                    "from_batches",
                    (object.getattr("schema")?, PyList::new(py, [object])?),
                )?
            } else if object.hasattr("to_reader")? {
                object.call_method0("to_reader")?
            } else {
                object.call_method0("scanner")?.call_method0("to_reader")?
            };
            let reader = if let Some(parameters) = parameters {
                let names = parameters
                    .projected_columns
                    .columns
                    .iter()
                    .map(|name| crate::conversion::text(py, name))
                    .collect::<PyResult<Vec<_>>>()?;
                if lazy {
                    if names.is_empty() {
                        reader
                    } else {
                        reader.call_method1("select", (PyList::new(py, names)?,))?
                    }
                } else {
                    let dataset = py.import("pyarrow.dataset")?;
                    let kwargs = pyo3::types::PyDict::new(py);
                    if !names.is_empty() {
                        kwargs.set_item("columns", PyList::new(py, names)?)?;
                    }
                    if let Some(predicate) = crate::arrow_filter::predicate(py, parameters, false)? {
                        kwargs.set_item("filter", predicate)?;
                    }
                    let scanner = if reader.is_instance(&dataset.getattr("Dataset")?)? {
                        reader.call_method("scanner", (), Some(&kwargs))?
                    } else {
                        let reader = if reader.is_instance(&dataset.getattr("Scanner")?)? {
                            reader.call_method0("to_reader")?
                        } else {
                            py.import("pyarrow")?
                                .getattr("RecordBatchReader")?
                                .call_method1("from_stream", (reader,))?
                        };
                        dataset
                            .getattr("Scanner")?
                            .call_method("from_batches", (reader,), Some(&kwargs))?
                    };
                    scanner.call_method0("to_reader")?
                }
            } else {
                reader
            };
            let capsule = if reader.is_instance_of::<PyCapsule>() {
                reader.cast_into::<PyCapsule>()?
            } else {
                reader
                    .call_method0("__arrow_c_stream__")?
                    .cast_into::<PyCapsule>()?
            };
            let pointer = capsule
                .pointer_checked(Some(c"arrow_array_stream"))?
                .cast::<ArrowArrayStream>();
            // SAFETY: a named Arrow C stream capsule owns this interface. Moving the
            // stream and clearing the capsule's release callback transfers ownership.
            let stream = unsafe { std::ptr::replace(pointer.as_ptr(), ArrowArrayStream::empty()) };
            if stream.release.is_none() {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Arrow stream was already released",
                ));
            }
            Ok::<_, PyErr>(Box::new(ArrowArrayStreamWrapper {
                arrow_array_stream: stream,
            }))
        })
        .map_err(|error| python_error(&error))
    }
}

pub(super) fn select(table: TableRef) -> SelectStatement {
    let mut node = rustdb_parser::build::select_node();
    node.select_list.push(rustdb_parser::build::star());
    *node.from_table = table;
    rustdb_parser::build::select_statement(rustdb_parser::build::query_node(QueryNodeKind::Select(Box::new(
        node,
    ))))
}
pub(super) fn create_view(
    connection: &mut rustdb::Connection,
    source: SelectStatement,
    name: impl AsRef<[u8]>,
    replace: bool,
    temporary: bool,
) -> Result<()> {
    // The name is assigned to the AST, like C++ ViewRelation::CreateView. A raw
    // catalog name may contain NUL, which must not pass through the SQL scanner.
    let sql = rustdb_common::byte_format!(
        "CREATE {} {} VIEW __python_view_name__ AS SELECT 42",
        if replace { "OR REPLACE" } else { "" },
        if temporary { "TEMP" } else { "" }
    );
    let mut statement = rustdb_parser::parse_bytes(&sql)?
        .pop()
        .ok_or_else(|| Error::internal("Missing view statement"))?;
    let SqlStatementKind::Create(info) = &mut statement.kind else {
        return Err(Error::internal("Expected CREATE VIEW"));
    };
    let CreateInfoKind::View(view) = &mut info.kind else {
        return Err(Error::internal("Expected view info"));
    };
    view.view_name = name.as_ref().into();
    *view.query = crate::files::view_source(source);
    info.sql.clear();
    connection.execute_statement(sql, statement)?;
    Ok(())
}
// The original Arrow scanner reads the factory only inside these callbacks. Bound
// data below retains the factory until every execution/scan has finished.
unsafe fn produce(
    factory: usize,
    parameters: &rustdb_arrow::ArrowStreamParameters,
) -> Result<Box<ArrowArrayStreamWrapper>> {
    // SAFETY: factory comes from the retained ArrowInput in the bind callback.
    unsafe { &*(factory as *const ArrowInput) }.stream_projected(Some(parameters))
}
unsafe fn schema(factory: *mut ArrowArrayStream, out: &mut ArrowSchema) -> Result<()> {
    // SAFETY: factory is the same retained ArrowInput, cast to the scanner's ABI type.
    unsafe { &*factory.cast::<ArrowInput>() }.schema(out)
}
#[derive(Debug)]
struct InputOwner {
    _owner: Arc<ArrowInput>,
}
impl FunctionData for InputOwner {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
unsafe fn produce_dumb(
    factory: usize,
    _parameters: &rustdb_arrow::ArrowStreamParameters,
) -> Result<Box<ArrowArrayStreamWrapper>> {
    // SAFETY: the retained owner is the factory that supplied this pointer.
    unsafe { &*(factory as *const ArrowInput) }.stream()
}
pub(super) fn functions() -> Result<Vec<TableFunctionSet>> {
    let mut sets = Vec::new();
    for (native, name, produce) in [
        (
            "arrow_scan",
            FUNCTION,
            produce as rustdb_arrow::StreamFactoryProduce,
        ),
        (
            "arrow_scan_dumb",
            DUMB_FUNCTION,
            produce_dumb as rustdb_arrow::StreamFactoryProduce,
        ),
    ] {
        let mut function = rustdb_arrow::arrow_table_functions()
            .into_iter()
            .find(|set| set.name.as_ref() == native)
            .and_then(|mut set| set.functions.pop())
            .ok_or_else(|| Error::internal("Missing Arrow scanner"))?;
        let bind = function
            .bind
            .take()
            .ok_or_else(|| Error::internal("Missing Arrow bind"))?;
        function.name = name.into();
        function.bind = Some(Arc::new(move |input| {
            // Python sources pass an owned identity and our producer address.
            // Other clients keep the native three-pointer Arrow factory ABI.
            if input.inputs.get(1).and_then(Value::as_u64) != Some(produce as *const () as usize as u64) {
                return bind(input);
            }
            let id = input
                .inputs
                .first()
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::invalid_input("Missing Arrow input identity"))?;
            let owner = lock(inputs())
                .get(&id)
                .and_then(Weak::upgrade)
                .ok_or_else(|| Error::invalid_input("Arrow input is no longer available"))?;
            let pointer = |p: usize| Value::ubigint(p as u64).reinterpret(LogicalType::POINTER);
            let values = [
                pointer(Arc::as_ptr(&owner) as usize),
                pointer(produce as *const () as usize),
                pointer(schema as *const () as usize),
            ];
            let mut output = bind(&TableFunctionBindInput {
                inputs: &values,
                ..*input
            })?;
            let inner = output
                .bind_data
                .take()
                .ok_or_else(|| Error::internal("Missing Arrow bind data"))?;
            output.bind_data = Some(rustdb_arrow::retain_factory_owner(
                inner.as_ref(),
                Arc::new(InputOwner { _owner: owner }),
            )?);
            Ok(output)
        }));
        let mut set = TableFunctionSet::new(name);
        set.add(function);
        sets.push(set);
    }
    Ok(sets)
}

pub(super) fn table_named(object: &Bound<'_, PyAny>, name: impl AsRef<[u8]>) -> PyResult<TableRef> {
    if object
        .extract::<PyRef<'_, crate::relation::PyRelation>>()
        .is_err()
        && !crate::pandas::is_dataframe(object)?
        && !crate::pandas::is_numpy(object)?
        && !accepted(object)?
    {
        return Err(crate::error::native(
            object.py(),
            &Error::invalid_input(rustdb_common::byte_format!(
                "Python Object \"{}\" of type \"{}\" not suitable for replacement scans.\nMake sure that \"{}\" is either a pandas.DataFrame, duckdb.DuckDBPyRelation, pyarrow Table, Dataset, RecordBatchReader, Scanner, or NumPy ndarrays with supported format",
                name.as_ref(),
                object.get_type().name()?.extract::<String>()?.as_bytes(),
                name.as_ref()
            )),
        ));
    }
    table(object)
}
pub(super) fn table(object: &Bound<'_, PyAny>) -> PyResult<TableRef> {
    let py = object.py();
    if let Ok(relation) = object.extract::<PyRef<'_, crate::relation::PyRelation>>() {
        return Ok(relation.replacement(object, b""));
    }
    if crate::pandas::is_dataframe(object)? || crate::pandas::is_numpy(object)? {
        return crate::pandas::table(object);
    }
    if let Some(lazy) = crate::polars::kind(object)? {
        let schema = if lazy {
            object.call_method0("collect_schema")?
        } else {
            object.getattr("schema")?
        };
        if schema.len()? == 0 {
            return Err(crate::error::native(
                py,
                &Error::invalid_input("Provided table/dataframe must have at least one column"),
            ));
        }
        if !lazy {
            return table(&object.call_method0("to_arrow")?);
        }
    }
    if let Some(pointer) = capsule_stream(object)? {
        // SAFETY: capsule_stream checked the interface name and object owns the pointer.
        if unsafe { pointer.as_ref() }.release.is_none() {
            return Err(crate::error::native(
                py,
                &Error::invalid_input("The ArrowArrayStream was already released"),
            ));
        }
    }
    if !accepted(object)? {
        let name = object.get_type().name()?;
        return Err(crate::error::native(
            py,
            &Error::invalid_input(format!(
                "Python Object Type {name} is not an accepted Arrow Object."
            )),
        ));
    }
    let function = if object.is_instance_of::<PyCapsule>() || py.import("pyarrow.dataset").is_err() {
        DUMB_FUNCTION
    } else {
        FUNCTION
    };
    let owner = Arc::new(ArrowInput {
        id: IDS.fetch_add(1, Ordering::Relaxed),
        object: object.clone().unbind(),
    });
    lock(inputs()).insert(owner.id, Arc::downgrade(&owner));
    let mut table = rustdb_parser::build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
        function: Box::new(rustdb_parser::build::function(
            function,
            vec![
                rustdb_parser::build::constant(Value::ubigint(owner.id).reinterpret(LogicalType::POINTER)),
                rustdb_parser::build::constant(
                    Value::ubigint(if function == DUMB_FUNCTION {
                        produce_dumb as *const () as usize
                    } else {
                        produce as *const () as usize
                    } as u64)
                    .reinterpret(LogicalType::POINTER),
                ),
                rustdb_parser::build::constant(
                    Value::ubigint(schema as *const () as usize as u64).reinterpret(LogicalType::POINTER),
                ),
            ],
        )),
        column_name_alias: Vec::new(),
        with_ordinality: OrdinalityType::WithoutOrdinality,
    }));
    table.alias = format!("arrow_object_{:016x}", owner.id).into();
    table.external_dependency = ExternalDependency::new(owner);
    Ok(table)
}

pub(super) fn arrow_table(object: &Bound<'_, PyAny>) -> PyResult<TableRef> {
    if let Ok(ipc) = object.py().import("pyarrow.ipc")
        && object.is_instance(&ipc.getattr("MessageReader")?)?
    {
        return Err(crate::error::Error::new_err(
            "The nanoarrow community extension is needed to read the Arrow IPC protocol. \n You can install it with \"INSTALL nanoarrow FROM community;\". \n Then you can load it with \"LOAD nanoarrow;\"",
        ));
    }
    if !accepted(object)? {
        let name = object.get_type().name()?;
        return Err(crate::error::native(
            object.py(),
            &Error::invalid_input(format!(
                "Python Object Type {name} is not an accepted Arrow Object."
            )),
        ));
    }
    table(object)
}

fn accepted(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    if crate::polars::kind(object)?.is_some() {
        return Ok(true);
    }
    if capsule_stream(object)?.is_some() {
        return Ok(true);
    }
    if object.hasattr("__arrow_c_stream__")? || object.hasattr("scanner")? || object.hasattr("to_reader")? {
        return Ok(true);
    }
    match object.py().import("pyarrow") {
        Ok(module) => object.is_instance(&module.getattr("RecordBatch")?),
        Err(error) if error.is_instance_of::<pyo3::exceptions::PyModuleNotFoundError>(object.py()) => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn capsule_stream(object: &Bound<'_, PyAny>) -> PyResult<Option<NonNull<ArrowArrayStream>>> {
    if let Ok(capsule) = object.cast::<PyCapsule>() {
        let name = match capsule.name()? {
            // SAFETY: copy immediately under the GIL without calling Python;
            // the live capsule cannot have its name changed during this copy.
            Some(name) => unsafe { name.as_cstr() }.to_bytes().to_vec(),
            None => b"NULL".to_vec(),
        };
        if name != b"arrow_array_stream" {
            return Err(crate::error::native(
                object.py(),
                &Error::invalid_input(rustdb_common::byte_format!(
                    "Expected a 'arrow_array_stream' PyCapsule, got: {}",
                    &name[..]
                )),
            ));
        }
        return Ok(Some(capsule.pointer_checked(Some(c"arrow_array_stream"))?.cast()));
    }
    Ok(None)
}

enum Found {
    Missing,
    Table(TableRef),
    Unsupported(String),
}

pub(super) fn replacement_scan() -> rustdb_planner::ReplacementScan {
    rustdb_planner::ReplacementScan(Arc::new(|input| {
        let setting = |name: &str, default: bool| {
            input
                .context
                .function_client
                .settings()
                .and_then(|settings| settings.current_setting(name).ok())
                .and_then(|value| value.as_bool())
                .unwrap_or(default)
        };
        if !setting("python_enable_replacements", true) || !setting("enable_external_access", true) {
            return Ok(None);
        }
        let scan_all = setting("python_scan_all_frames", false);
        let name = Python::attach(|py| crate::conversion::text(py, input.table_name))
            .map_err(|error| crate::error::unicode_decode_error(&error))?;
        let found = Python::attach(|py| -> PyResult<Found> {
            let name_text = name.bind(py).extract::<String>()?;
            let mut frame = py.import("inspect")?.call_method0("currentframe")?;
            let frontend = py.import("duckdb")?.getattr("__file__")?.extract::<String>()?;
            while !frame.is_none() && frame.getattr("f_code")?.getattr("co_filename")?.extract::<String>()? == frontend {
                frame = frame.getattr("f_back")?;
            }
            while !frame.is_none() {
                for scope in ["f_locals", "f_globals"] {
                    let mapping = frame.getattr(scope)?;
                    if mapping.is_none() || !mapping.contains(name.bind(py))? { continue; }
                    let object = mapping.get_item(name.bind(py))?;
                    if object.is_instance(&py.import("types")?.getattr("BuiltinFunctionType")?)? { continue; }
                    if let Ok(relation)=object.extract::<PyRef<'_,crate::relation::PyRelation>>() {
                        if relation.connection_id(py)!=input.context.function_client.connection_id() {
                            return Ok(Found::Unsupported(format!("Python Object \"{name_text}\" of type \"DuckDBPyRelation\" not suitable for replacement scan.\nThe object was created by another Connection and can therefore not be used by this Connection.")));
                        }
                        return Ok(Found::Table(relation.replacement(&object,input.table_name)));
                    }
                    if let Some(lazy) = crate::polars::kind(&object)? {
                        let schema = if lazy { object.call_method0("collect_schema")? } else { object.getattr("schema")? };
                        if schema.len()? == 0 { return Ok(Found::Unsupported("Provided table/dataframe must have at least one column".to_owned())); }
                    }
                    if crate::pandas::is_dataframe(&object)? || crate::pandas::is_numpy(&object)? || accepted(&object)? {
                        if let Some(pointer) = capsule_stream(&object)? {
                            // SAFETY: the checked capsule is held by object and the GIL.
                            if unsafe { pointer.as_ref() }.release.is_none() {
                                return Ok(Found::Unsupported("The ArrowArrayStream was already released".to_owned()));
                            }
                        }
                        let mut table = table(&object)?;
                        table.alias = input.table_name.into();
                        return Ok(Found::Table(table));
                    }
                    let kind = object.get_type().name()?;
                    let filename = frame.getattr("f_code")?.getattr("co_filename")?.extract::<String>()?;
                    let line = frame.getattr("f_lineno")?.extract::<i64>()?;
                    return Ok(Found::Unsupported(format!("Python Object \"{name_text}\" of type \"{kind}\" found on line \"{filename}:{line}\" not suitable for replacement scans.\nMake sure that \"{name_text}\" is either a pandas.DataFrame, duckdb.DuckDBPyRelation, pyarrow Table, Dataset, RecordBatchReader, Scanner, or NumPy ndarrays with supported format")));
                }
                if !scan_all { break; }
                frame = frame.getattr("f_back")?;
            }
            Ok(Found::Missing)
        }).map_err(|error| crate::pandas::python_error(&error))?;
        match found {
            Found::Missing => Ok(None),
            Found::Table(table) => Ok(Some(table)),
            Found::Unsupported(message) => Err(Error::invalid_input(message)),
        }
    }))
}
