//! Direct Pandas/NumPy scans over retained arrays, without an Arrow conversion or row snapshot.
use crate::connection::lock;
use crate::{conversion, input};
use pyo3::buffer::{Element, PyBuffer};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyCapsule, PyDict, PyFloat, PyList, PySlice, PyString};
use roastedduck_arrow::ffi::{ArrowArray, ArrowArrayWrapper, ArrowSchema};
use roastedduck_arrow::{ArrowTableSchema, data_chunk_from_arrow, populate_arrow_table_schema};
use roastedduck_ast::{ExternalDependency, OrdinalityType, TableFunctionRef, TableRef, TableRefKind};
use roastedduck_common::{ByteString, Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_types::{DataChunk, Interval, LogicalType, LogicalTypeId as L, StringT, Value, Vector};
use std::any::Any;
use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

static IDS: AtomicU64 = AtomicU64::new(1);
static INPUTS: OnceLock<Mutex<HashMap<u64, Weak<Input>>>> = OnceLock::new();

/// A borrowed object-pointer slot in a Python buffer, not an owning Python reference.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct ObjectSlot(*mut pyo3::ffi::PyObject);

// SAFETY: PEP3118's native `O` format contains PyObject pointers. PyBuffer separately
// checks the pointer width/alignment and retains the exporting array. Slots are only
// read with Python attached; object-array writes/refcount changes require the GIL.
unsafe impl Element for ObjectSlot {
    fn is_compatible_format(format: &CStr) -> bool {
        matches!(format.to_bytes(), b"O" | b"@O")
    }
}

fn inputs() -> &'static Mutex<HashMap<u64, Weak<Input>>> {
    INPUTS.get_or_init(Mutex::default)
}
#[derive(Debug)]
struct Input {
    id: u64,
    _original: Py<PyAny>,
    frame: Py<PyAny>,
    pandas: bool,
}
impl Drop for Input {
    fn drop(&mut self) {
        lock(inputs()).remove(&self.id);
    }
}

pub(super) fn is_dataframe(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    let modules = object.py().import("sys")?.getattr("modules")?;
    match modules.get_item("pandas") {
        Ok(module) if !module.is_none() => object.is_instance(&module.getattr("DataFrame")?),
        _ => Ok(false),
    }
}
pub(super) fn is_numpy(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = object.py();
    let Ok(np) = py.import("sys")?.getattr("modules")?.get_item("numpy") else {
        return Ok(false);
    };
    let array = np.getattr("ndarray")?;
    if object.is_instance(&array)? {
        return Ok(matches!(object.getattr("ndim")?.extract::<usize>()?, 1 | 2));
    }
    let values = if object.is_instance_of::<PyDict>() {
        object.call_method0("values")?
    } else if object.is_instance_of::<PyList>() {
        object.clone()
    } else {
        return Ok(false);
    };
    if values.len()? == 0 {
        return Ok(false);
    }
    for item in values.try_iter()? {
        let item = item?;
        if !item.is_instance(&array)? || item.getattr("ndim")?.extract::<usize>()? != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}
fn dataframe_copy<'py>(object: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = object.py();
    let copy = object.call_method1("copy", (false,))?;
    let names = object
        .getattr("columns")?
        .try_iter()?
        .map(|v| v?.str()?.extract::<String>().map(ByteString::from))
        .collect::<PyResult<Vec<_>>>()?;
    if names.is_empty() {
        return Err(crate::error::native(
            py,
            &Error::invalid_input("Need a DataFrame with at least one column"),
        ));
    }
    let names = crate::host::names(&names);
    copy.setattr(
        "columns",
        PyList::new(
            py,
            names
                .iter()
                .map(|n| conversion::text(py, n))
                .collect::<PyResult<Vec<_>>>()?,
        )?,
    )?;
    Ok(copy)
}
fn retain(object: &Bound<'_, PyAny>, frame: Bound<'_, PyAny>, pandas: bool) -> Arc<Input> {
    let owner = Arc::new(Input {
        id: IDS.fetch_add(1, Ordering::Relaxed),
        _original: object.clone().unbind(),
        frame: frame.unbind(),
        pandas,
    });
    lock(inputs()).insert(owner.id, Arc::downgrade(&owner));
    owner
}

pub(super) fn table(object: &Bound<'_, PyAny>) -> PyResult<TableRef> {
    let py = object.py();
    let pandas = is_dataframe(object)?;
    let frame = if pandas {
        let pd = py.import("pandas")?;
        if let Ok(arrow_dtype) = pd.getattr("ArrowDtype") {
            for dtype in object.getattr("dtypes")?.try_iter()? {
                if dtype?.is_instance(&arrow_dtype)? {
                    let table = py
                        .import("pyarrow")?
                        .getattr("Table")?
                        .call_method1("from_pandas", (object,))
                        .map_err(|_| crate::error::InvalidInputException::new_err(
                            "Invalid Input Error: The dataframe could not be converted to a pyarrow.lib.Table, because a Python exception occurred.",
                        ))?;
                    return input::table(&table);
                }
            }
        }
        dataframe_copy(object)?
    } else if object.is_instance_of::<PyDict>() {
        object.clone()
    } else {
        let dict = PyDict::new(py);
        let one_dimensional = object.getattr("ndim").and_then(|n| n.extract::<usize>()).ok() == Some(1);
        if one_dimensional {
            dict.set_item("column0", object)?;
        } else {
            for (index, value) in object.try_iter()?.enumerate() {
                dict.set_item(format!("column{index}"), value?)?;
            }
        }
        dict.into_any()
    };
    let owner = retain(object, frame, pandas);
    let mut table = roastedduck_parser::build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
        function: Box::new(roastedduck_parser::build::function(
            "pandas_scan",
            vec![roastedduck_parser::build::constant(
                Value::ubigint(owner.id).reinterpret(LogicalType::POINTER),
            )],
        )),
        column_name_alias: Vec::new(),
        with_ordinality: OrdinalityType::WithoutOrdinality,
    }));
    table.alias = format!("df_{:016x}", owner.id).into();
    table.external_dependency = ExternalDependency::new(owner);
    Ok(table)
}

fn null(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    if object.is_none() {
        return Ok(true);
    }
    if object.is_instance_of::<PyFloat>() {
        return Ok(object.extract::<f64>()?.is_nan());
    }
    let name = object.get_type().name()?;
    if name.to_str()? == "MaskedConstant"
        && object
            .get_type()
            .getattr("__module__")?
            .extract::<String>()?
            .starts_with("numpy.ma")
    {
        return Ok(true);
    }
    Ok(matches!(name.to_str()?, "NAType" | "NaTType")
        && object
            .get_type()
            .getattr("__module__")?
            .extract::<String>()?
            .starts_with("pandas"))
}
fn object_value(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    object_value_with(object, &conversion::from_python)
}

fn object_value_with<'py, E: From<PyErr>>(
    object: &Bound<'py, PyAny>,
    leaf: &impl Fn(&Bound<'py, PyAny>) -> std::result::Result<Value, E>,
) -> std::result::Result<Value, E> {
    if null(object)? {
        return Ok(Value::null(LogicalType::SQLNULL));
    }
    if let Ok(dict) = object.cast::<PyDict>() {
        if dict.is_empty() {
            return Ok(Value::map(LogicalType::SQLNULL, LogicalType::SQLNULL, Vec::new()));
        }
        if dict.len() == 2 && dict.contains("key")? && dict.contains("value")? {
            let keys = dict
                .get_item("key")?
                .ok_or_else(|| crate::error::InternalException::new_err("Missing map keys"))?;
            let values = dict
                .get_item("value")?
                .ok_or_else(|| crate::error::InternalException::new_err("Missing map values"))?;
            if keys.is_none() && values.is_none() {
                return Ok(Value::null(LogicalType::map(
                    LogicalType::SQLNULL,
                    LogicalType::SQLNULL,
                )));
            }
            if keys.len()? != values.len()? {
                return Err(crate::error::InvalidInputException::new_err(
                    "Map keys and values must be the same length",
                )
                .into());
            }
            let mut key_type = LogicalType::SQLNULL;
            let mut value_type = LogicalType::SQLNULL;
            let mut entries = Vec::new();
            for (key, value) in keys.try_iter()?.zip(values.try_iter()?) {
                let key = object_value_with(&key?, leaf)?;
                let value = object_value_with(&value?, leaf)?;
                key_type = combine(&key_type, key.logical_type());
                value_type = combine(&value_type, value.logical_type());
                entries.push((key, value));
            }
            return Ok(Value::map(key_type, value_type, entries));
        }
        let mut fields = Vec::new();
        let mut values = Vec::new();
        for (key, value) in dict.iter() {
            let name = ByteString::from(key.str()?.extract::<String>()?);
            let value = object_value_with(&value, leaf)?;
            fields.push((name, value.logical_type().clone()));
            values.push(value);
        }
        return Ok(Value::from_children(LogicalType::struct_type(fields), values));
    }
    if object.is_instance_of::<PyList>() || object.is_instance_of::<pyo3::types::PyTuple>() {
        let mut kind = LogicalType::SQLNULL;
        let mut values = Vec::new();
        for value in object.try_iter()? {
            let value = object_value_with(&value?, leaf)?;
            kind = combine(&kind, value.logical_type());
            values.push(value);
        }
        // Types drive conversion from the original Python values during the scan.
        return Ok(Value::from_children(LogicalType::list(kind), values));
    }
    if object.hasattr("dtype")?
        && object.hasattr("tolist")?
        && object.getattr("ndim")?.extract::<usize>()? > 0
    {
        return object_value_with(&object.call_method0("tolist")?, leaf);
    }
    leaf(object)
}
fn combine(left: &LogicalType, right: &LogicalType) -> LogicalType {
    if left.id() == L::SqlNull {
        return right.clone();
    }
    if right.id() == L::SqlNull {
        return left.clone();
    }
    if left.id() == L::List && right.id() == L::List {
        return LogicalType::list(combine(
            left.child_type().unwrap_or(&LogicalType::SQLNULL),
            right.child_type().unwrap_or(&LogicalType::SQLNULL),
        ));
    }
    if let (Some((left_key, left_value)), Some((right_key, right_value))) =
        (left.map_key_value(), right.map_key_value())
    {
        // Pandas UpgradeType recurses through MAP just as through LIST/STRUCT;
        // generic SQL type promotion would merge mismatched struct fields instead.
        return LogicalType::map(combine(left_key, right_key), combine(left_value, right_value));
    }
    if left.id() == L::Struct && right.id() == L::Struct {
        let a = left.struct_children().unwrap_or_default();
        let b = right.struct_children().unwrap_or_default();
        if a.len() == b.len()
            && a.iter().zip(b).all(|((an, a), (bn, b))| {
                let compatible = a.id() == L::SqlNull
                    || b.id() == L::SqlNull
                    || if a.is_nested() || b.is_nested() {
                        a.id() == b.id()
                    } else {
                        true
                    };
                an.eq_ignore_ascii_case(bn) && compatible
            })
        {
            return LogicalType::struct_type(
                a.iter()
                    .zip(b)
                    .map(|((name, a), (_, b))| (name.clone(), combine(a, b)))
                    .collect(),
            );
        }
        let value = a
            .iter()
            .chain(b)
            .fold(LogicalType::SQLNULL, |kind, (_, value)| combine(&kind, value));
        return LogicalType::map(LogicalType::VARCHAR, value);
    }
    if matches!((left.id(), right.id()), (L::Map, L::Struct) | (L::Struct, L::Map)) {
        let (map, structure) = if left.id() == L::Map {
            (left, right)
        } else {
            (right, left)
        };
        let value = structure.struct_children().unwrap_or_default().iter().fold(
            map.map_key_value()
                .map_or(LogicalType::SQLNULL, |(_, v)| v.clone()),
            |kind, (_, v)| combine(&kind, v),
        );
        return LogicalType::map(LogicalType::VARCHAR, value);
    }
    if left.is_nested() != right.is_nested()
        || (left.is_nested() && left.id() != right.id() && (left.id() == L::List || right.id() == L::List))
    {
        return LogicalType::VARCHAR;
    }
    LogicalType::force_max_logical_type(left, right)
}
fn typed_object(object: &Bound<'_, PyAny>, kind: &LogicalType) -> PyResult<Value> {
    let py = object.py();
    if null(object)? {
        return Ok(Value::null(kind.clone()));
    }
    if kind.id() == L::Map
        && let Ok(dict) = object.cast::<PyDict>()
    {
        let (key_type, value_type) = kind
            .map_key_value()
            .ok_or_else(|| crate::error::InternalException::new_err("Invalid map type"))?;
        let mut entries = Vec::new();
        let source = if dict.len() == 2 && dict.contains("key")? && dict.contains("value")? {
            let keys = dict
                .get_item("key")?
                .ok_or_else(|| crate::error::InternalException::new_err("Missing map keys"))?;
            let values = dict
                .get_item("value")?
                .ok_or_else(|| crate::error::InternalException::new_err("Missing map values"))?;
            if keys.is_none() && values.is_none() {
                return Ok(Value::null(kind.clone()));
            }
            keys.try_iter()?
                .zip(values.try_iter()?)
                .map(|(k, v)| Ok((k?, v?)))
                .collect::<PyResult<Vec<_>>>()?
        } else {
            dict.iter().collect::<Vec<_>>()
        };
        let mut seen = std::collections::HashSet::new();
        for (key, value) in source {
            if null(&key)? {
                return Err(crate::error::InvalidInputException::new_err(
                    "Map keys can not be NULL",
                ));
            }
            let key = if key_type.id() == L::Varchar {
                Value::varchar(key.str()?.extract::<String>()?)
            } else {
                typed_object(&key, key_type)?
            };
            if !seen.insert(key.clone()) {
                return Err(crate::error::InvalidInputException::new_err(
                    "Map keys must be unique",
                ));
            }
            entries.push((key, typed_object(&value, value_type)?));
        }
        return Ok(Value::map(key_type.clone(), value_type.clone(), entries));
    }
    if kind.id() == L::Struct
        && let Ok(dict) = object.cast::<PyDict>()
    {
        let mut values = Vec::new();
        for (name, kind) in kind.struct_children().unwrap_or_default() {
            let mut field = Value::null(kind.clone());
            for (key, value) in dict.iter() {
                if key
                    .str()?
                    .extract::<String>()?
                    .as_bytes()
                    .eq_ignore_ascii_case(name)
                {
                    field = typed_object(&value, kind)?;
                    break;
                }
            }
            values.push(field);
        }
        return Ok(Value::from_children(kind.clone(), values));
    }
    if kind.id() == L::List {
        let child = kind
            .child_type()
            .ok_or_else(|| crate::error::InternalException::new_err("Invalid list type"))?;
        let values = object
            .try_iter()
            .map_err(|_| {
                crate::error::InvalidInputException::new_err("Failed to cast value: value is not a list")
            })?
            .map(|v| typed_object(&v?, child))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(Value::list(child.clone(), values));
    }
    object_value(object)?
        .default_cast_as(kind)
        .map_err(|e| crate::error::native(py, &e))
}
pub(super) fn python_error(error: &PyErr) -> Error {
    let converted = Python::attach(|py| {
        let kind = if error.is_instance_of::<crate::error::InvalidInputException>(py) {
            Some(roastedduck_common::ErrorKind::InvalidInput)
        } else if error.is_instance_of::<crate::error::ConversionException>(py) {
            Some(roastedduck_common::ErrorKind::Conversion)
        } else if error.is_instance_of::<crate::error::NotImplementedException>(py) {
            Some(roastedduck_common::ErrorKind::NotImplemented)
        } else {
            None
        }?;
        let message = error.value(py).str().ok()?.extract::<String>().ok()?;
        let prefix = format!("{} Error: ", kind.display_name());
        Some(Error::new(
            kind,
            message.strip_prefix(&prefix).unwrap_or(&message).to_owned(),
        ))
    });
    converted.unwrap_or_else(|| input::python_error(error))
}
#[derive(Debug)]
enum AnalysisError {
    Unsupported(PyErr),
    Decimal(PyErr),
}
impl From<PyErr> for AnalysisError {
    fn from(error: PyErr) -> Self {
        Self::Unsupported(error)
    }
}
fn analyzed_object_value(object: &Bound<'_, PyAny>) -> std::result::Result<Value, AnalysisError> {
    object_value_with(object, &|value| match crate::decimal::inferred_type(value)
        .map_err(AnalysisError::Decimal)?
    {
        Some(kind) => Ok(Value::null(kind)),
        None => conversion::from_python(value).map_err(AnalysisError::Unsupported),
    })
}
fn analyze(array: &Bound<'_, PyAny>, sample: usize) -> PyResult<LogicalType> {
    if sample == 0 {
        return Ok(LogicalType::VARCHAR);
    }
    let rows = array.len()?;
    if rows == 0 {
        return Ok(LogicalType::SQLNULL);
    }
    let step = (rows / sample.min(rows)).max(1);
    let mut kind = LogicalType::SQLNULL;
    for row in (0..rows).step_by(step) {
        let value = match analyzed_object_value(&array.get_item(row)?) {
            Ok(value) => value,
            Err(AnalysisError::Unsupported(_)) => return Ok(LogicalType::VARCHAR),
            Err(AnalysisError::Decimal(error)) => return Err(error),
        };
        kind = combine(&kind, value.logical_type());
    }
    if kind.id() == L::SqlNull && step > 1 {
        for row in 0..rows {
            let object = array.get_item(row)?;
            if !null(&object)? {
                return analyzed_object_value(&object)
                    .map(|value| value.logical_type().clone())
                    .map_err(|error| match error {
                        AnalysisError::Unsupported(error) | AnalysisError::Decimal(error) => error,
                    });
            }
        }
    }
    Ok(kind)
}
#[derive(Debug)]
struct Column {
    array: Py<PyAny>,
    mask: Option<Py<PyAny>>,
    kind: LogicalType,
    format: String,
    unit: String,
    negative_stride: bool,
}
#[derive(Debug)]
struct BoundData {
    owner: Arc<Input>,
    columns: Vec<Column>,
    rows: usize,
}
impl FunctionData for BoundData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn native_type(format: &str) -> Option<LogicalType> {
    Some(match format.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => LogicalType::BOOLEAN,
        "int8" => LogicalType::TINYINT,
        "int16" => LogicalType::SMALLINT,
        "int32" => LogicalType::INTEGER,
        "int64" => LogicalType::BIGINT,
        "uint8" => LogicalType::UTINYINT,
        "uint16" => LogicalType::USMALLINT,
        "uint32" => LogicalType::UINTEGER,
        "uint64" => LogicalType::UBIGINT,
        "float16" | "float32" => LogicalType::FLOAT,
        "float64" => LogicalType::DOUBLE,
        "object" | "string" => LogicalType::VARCHAR,
        _ => return None,
    })
}
fn bind_column(py: Python<'_>, series: Bound<'_, PyAny>, pandas: bool, sample: usize) -> PyResult<Column> {
    let dtype = series.getattr("dtype")?;
    let description = dtype.str()?.extract::<String>()?;
    let mut mask = None;
    let mut unit = String::new();
    let array;
    let kind;
    if description == "category" {
        let categories = series.getattr("cat")?.getattr("categories")?;
        if categories
            .getattr("dtype")?
            .getattr("kind")?
            .extract::<String>()?
            == "O"
        {
            let entries = categories
                .try_iter()?
                .map(|v| v?.extract::<String>().map(ByteString::from))
                .collect::<PyResult<Vec<_>>>()?;
            kind = LogicalType::enum_type_bytes(entries);
            array = series
                .getattr("cat")?
                .getattr("codes")?
                .call_method0("to_numpy")?;
        } else {
            array = series.call_method0("to_numpy")?;
            kind = native_type(&array.getattr("dtype")?.str()?.extract::<String>()?)
                .unwrap_or(LogicalType::VARCHAR);
        }
    } else if pandas {
        // Pandas 3 uses `str` for its default nullable StringDtype. Both names
        // must retain the same Arrow-buffer and Python-object scan paths.
        let string_dtype = description == "str" || description.starts_with("string");
        let extension = series.getattr("array")?;
        if string_dtype
            && dtype
                .getattr("storage")
                .and_then(|storage| storage.extract::<String>())
                .is_ok_and(|storage| storage.starts_with("pyarrow"))
            && extension.hasattr("_pa_array")?
        {
            // Capture the immutable Arrow buffers at each bind, rather than
            // materializing a full NumPy array of Python strings. Keeping this
            // inside the Pandas binder preserves in-place changes observed by
            // subsequent executions of a retained relation.
            return Ok(Column {
                array: extension.getattr("_pa_array")?.unbind(),
                mask: None,
                kind: LogicalType::VARCHAR,
                format: "arrow_string".to_owned(),
                unit: String::new(),
                negative_stride: false,
            });
        }
        if extension.hasattr("_mask")? {
            mask = Some(extension.getattr("_mask")?.unbind());
        }
        if description == "float16" {
            array = series.call_method1("to_numpy", ("float32",))?;
            kind = LogicalType::FLOAT;
        } else if description.starts_with("datetime64[") || description.starts_with("timedelta64[") {
            array = extension.getattr("asi8")?;
            unit = dtype
                .getattr("unit")
                .and_then(|v| v.extract())
                .unwrap_or_else(|_| {
                    description
                        .split('[')
                        .nth(1)
                        .unwrap_or("ns]")
                        .trim_end_matches(']')
                        .to_owned()
                });
            kind = if description.starts_with("timedelta") {
                LogicalType::INTERVAL
            } else if dtype.hasattr("tz")? {
                LogicalType::TIMESTAMP_TZ
            } else {
                timestamp_type(&unit)
            };
        } else {
            array = if string_dtype {
                // ArrowStringArray._data is deprecated and returns Arrow storage,
                // whereas this scanner consumes Python/NumPy object elements.
                series.call_method0("to_numpy")?
            } else if extension.hasattr("_data")? {
                extension.getattr("_data")?
            } else {
                series.call_method0("to_numpy")?
            };
            kind = if description == "object" {
                analyze(&array, sample)?
            } else if string_dtype {
                LogicalType::VARCHAR
            } else {
                native_type(&description).ok_or_else(|| {
                    crate::error::NotImplementedException::new_err(format!(
                        "Data type '{description}' not recognized"
                    ))
                })?
            };
        }
    } else if description.starts_with("datetime64[") || description.starts_with("timedelta64[") {
        let np = py.import("numpy")?;
        unit = np
            .call_method1("datetime_data", (&dtype,))?
            .get_item(0)?
            .extract()?;
        array = series.call_method1("view", ("int64",))?;
        kind = if description.starts_with("timedelta") {
            LogicalType::INTERVAL
        } else {
            timestamp_type(&unit)
        };
    } else if dtype.getattr("kind")?.extract::<String>()? == "U" {
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_inverse", true)?;
        let unique = py
            .import("numpy")?
            .call_method("unique", (&series,), Some(&kwargs))?;
        let entries = unique
            .get_item(0)?
            .try_iter()?
            .map(|v| v?.extract::<String>().map(ByteString::from))
            .collect::<PyResult<Vec<_>>>()?;
        kind = LogicalType::enum_type_bytes(entries);
        array = unique.get_item(1)?.call_method1("reshape", (-1,))?;
    } else {
        array = if description == "float16" {
            series.call_method1("astype", ("float32",))?
        } else {
            series
        };
        kind = if description == "object" {
            analyze(&array, sample)?
        } else {
            native_type(&description).ok_or_else(|| {
                crate::error::NotImplementedException::new_err(format!(
                    "Data type '{description}' not recognized"
                ))
            })?
        };
    }
    let format = array.getattr("dtype")?.getattr("kind")?.extract::<String>()?
        + &array
            .getattr("dtype")?
            .getattr("itemsize")?
            .extract::<usize>()?
            .to_string();
    let negative_stride = array.getattr("strides")?.get_item(0)?.extract::<isize>()? < 0;
    Ok(Column {
        array: array.unbind(),
        mask,
        kind,
        format,
        unit,
        negative_stride,
    })
}
fn timestamp_type(unit: &str) -> LogicalType {
    match unit {
        "s" => LogicalType::TIMESTAMP_S,
        "ms" => LogicalType::TIMESTAMP_MS,
        "us" => LogicalType::TIMESTAMP,
        _ => LogicalType::TIMESTAMP_NS,
    }
}
fn binding(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let id = input
        .inputs
        .first()
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::invalid_input("Missing dataframe identity"))?;
    let owner = lock(inputs())
        .get(&id)
        .and_then(Weak::upgrade)
        .ok_or_else(|| Error::invalid_input("Dataframe input is no longer available"))?;
    let sample = input
        .binder
        .and_then(|b| b.downcast_ref::<roastedduck_planner::ClientContext>())
        .and_then(|c| c.function_client.settings())
        .and_then(|s| s.current_setting("pandas_analyze_sample").ok())
        .and_then(|v| v.as_u64())
        .unwrap_or(1000) as usize;
    bind_owner(owner, sample)
}

fn bind_owner(owner: Arc<Input>, sample: usize) -> Result<TableFunctionBindOutput> {
    let result = Python::attach(|py| {
        let frame = owner.frame.bind(py);
        let keys = if owner.pandas {
            frame.getattr("columns")?
        } else {
            frame.call_method0("keys")?
        };
        let mut names = Vec::new();
        let mut columns = Vec::new();
        let mut rows = None;
        for key in keys.try_iter()? {
            let key = key?;
            let series = frame.get_item(&key)?;
            let count = series.len()?;
            if rows.is_some_and(|rows| rows != count) {
                return Err(crate::error::InvalidInputException::new_err(
                    "Dataframe columns have different lengths",
                ));
            }
            rows = Some(count);
            names.push(ByteString::from(key.str()?.extract::<String>()?));
            columns.push(bind_column(py, series, owner.pandas, sample)?);
        }
        if columns.is_empty() {
            return Err(crate::error::InvalidInputException::new_err(
                "Invalid Input Error: Need a DataFrame with at least one column",
            ));
        }
        let return_types = columns.iter().map(|column| column.kind.clone()).collect();
        Ok::<_, PyErr>(TableFunctionBindOutput {
            names,
            return_types,
            bind_data: Some(Arc::new(BoundData {
                owner,
                columns,
                rows: rows.unwrap_or_default(),
            })),
        })
    })
    .map_err(|error| python_error(&error))?;
    if result
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<BoundData>())
        .is_some_and(|data| data.columns.iter().any(|column| column.negative_stride))
    {
        // The frozen client extracts an unsigned stride and raises this cast error.
        return Err(Error::new(roastedduck_common::ErrorKind::Invalid,
            "Unable to cast Python instance of type <class 'int'> to C++ type '?' (#define PYBIND11_DETAILED_ERROR_MESSAGES or compile in debug mode for details)")
            .with_subtype("python_callback"));
    }
    Ok(result)
}

fn numeric(column: &Column, data: &[u8]) -> Result<Value> {
    macro_rules! read {
        ($ty:ty) => {
            <$ty>::from_ne_bytes(
                data.try_into()
                    .map_err(|_| Error::internal("Invalid NumPy scalar size"))?,
            )
        };
    }
    let value = match column.format.as_str() {
        "b1" => Value::boolean(data[0] != 0),
        "i1" => Value::tinyint(data[0] as i8),
        "i2" => Value::smallint(read!(i16)),
        "i4" => Value::integer(read!(i32)),
        "i8" => Value::bigint(read!(i64)),
        "u1" => Value::utinyint(data[0]),
        "u2" => Value::usmallint(read!(u16)),
        "u4" => Value::uinteger(read!(u32)),
        "u8" => Value::ubigint(read!(u64)),
        "f4" => Value::float(read!(f32)),
        "f8" => Value::double(read!(f64)),
        _ => return Err(Error::not_implemented("Unsupported NumPy buffer format")),
    };
    if !column.unit.is_empty() {
        let raw = value.as_i64().unwrap_or_default();
        if raw == i64::MIN {
            return Ok(Value::null(column.kind.clone()));
        }
        if column.kind.id() == L::TimestampTz || column.kind.id() == L::Interval {
            let micros = match column.unit.as_str() {
                "ns" => raw / 1000,
                "ms" => raw
                    .checked_mul(1000)
                    .ok_or_else(|| Error::conversion("Timestamp overflow"))?,
                "s" => raw
                    .checked_mul(1_000_000)
                    .ok_or_else(|| Error::conversion("Timestamp overflow"))?,
                _ => raw,
            };
            return Ok(if column.kind.id() == L::Interval {
                Value::interval(Interval {
                    months: 0,
                    days: 0,
                    micros,
                })
            } else {
                Value::bigint(micros).reinterpret(column.kind.clone())
            });
        }
        return Ok(value.reinterpret(column.kind.clone()));
    }
    if column.kind.id() == L::Enum {
        let index = value.as_i128().unwrap_or_default();
        return Ok(if index < 0 {
            Value::null(column.kind.clone())
        } else {
            Value::enum_value(column.kind.clone(), index as u32)
        });
    }
    if value.as_f64().is_some_and(f64::is_nan) {
        return Ok(Value::null(column.kind.clone()));
    }
    value.default_cast_as(&column.kind)
}
pub(super) fn scan(
    data: Option<&dyn FunctionData>,
    ids: &[u64],
    state: &mut roastedduck_function::TableScanState,
    output: &mut DataChunk,
) -> Result<bool> {
    let data = data
        .and_then(|d| d.as_any().downcast_ref::<BoundData>())
        .ok_or_else(|| Error::internal("Missing dataframe bind data"))?;
    let start = state.position as usize;
    let end = data.rows.min(start + STANDARD_VECTOR_SIZE);
    if start == end {
        output.reset();
        return Ok(false);
    }
    let columns = Python::attach(|py| {
        let slice = PySlice::new(py, start as isize, end as isize, 1);
        let pandas_na = if data.owner.pandas {
            Some(py.import("pandas")?.getattr("NA")?)
        } else {
            None
        };
        let indices = if ids.is_empty() {
            (0..data.columns.len() as u64).collect::<Vec<_>>()
        } else {
            ids.to_vec()
        };
        let mut result = Vec::new();
        for id in indices {
            if id == u64::MAX {
                let mut vector = Vector::flat(LogicalType::BIGINT, end - start);
                for row in 0..end - start {
                    vector
                        .set_value(row, &Value::bigint((start + row) as i64))
                        .map_err(|e| crate::error::native(py, &e))?;
                }
                result.push(vector);
                continue;
            }
            let column = &data.columns[id as usize];
            if column.format == "arrow_string" {
                result.push(arrow_string_chunk(py, column, start, end - start)?);
                continue;
            }
            let array = column.array.bind(py).get_item(&slice)?;
            let mask = column
                .mask
                .as_ref()
                .map(|m| m.bind(py).get_item(&slice)?.call_method0("tobytes"))
                .transpose()?;
            let mask = mask
                .as_ref()
                .map(|m| m.cast::<PyBytes>().map(pyo3::types::PyBytesMethods::as_bytes))
                .transpose()?;
            let objects = column.format.starts_with('O');
            let bytes = if objects {
                None
            } else {
                Some(array.call_method0("tobytes")?)
            };
            let bytes = bytes
                .as_ref()
                .map(|b| b.cast::<PyBytes>().map(pyo3::types::PyBytesMethods::as_bytes))
                .transpose()?;
            let width = column.format[1..].parse::<usize>().unwrap_or(8);
            let mut vector = Vector::flat(column.kind.clone(), end - start);
            let direct = matches!(
                column.kind.id(),
                L::Boolean
                    | L::TinyInt
                    | L::SmallInt
                    | L::Integer
                    | L::BigInt
                    | L::UTinyInt
                    | L::USmallInt
                    | L::UInteger
                    | L::UBigInt
                    | L::Float
                    | L::Double
                    | L::Timestamp
                    | L::TimestampNs
                    | L::TimestampMs
                    | L::TimestampSec
            );
            if direct
                && let Some(bytes) = bytes
                && vector.data_bytes().len() == bytes.len()
            {
                vector.data_bytes_mut().copy_from_slice(bytes);
                for row in 0..end - start {
                    let invalid = mask.is_some_and(|mask| mask[row] != 0)
                        || match column.format.as_str() {
                            "f4" => f32::from_ne_bytes(bytes[row * 4..(row + 1) * 4].try_into().map_err(
                                |_| crate::error::InternalException::new_err("Invalid FLOAT buffer"),
                            )?)
                            .is_nan(),
                            "f8" => f64::from_ne_bytes(bytes[row * 8..(row + 1) * 8].try_into().map_err(
                                |_| crate::error::InternalException::new_err("Invalid DOUBLE buffer"),
                            )?)
                            .is_nan(),
                            "i8" if !column.unit.is_empty() => {
                                i64::from_ne_bytes(bytes[row * 8..(row + 1) * 8].try_into().map_err(
                                    |_| crate::error::InternalException::new_err("Invalid timestamp buffer"),
                                )?) == i64::MIN
                            }
                            _ => false,
                        };
                    if invalid {
                        vector.set_null(row);
                    }
                }
                result.push(vector);
                continue;
            }
            if objects && column.kind.id() == L::Varchar {
                // Match NumpyScan's direct object-buffer access. Keeping the exporter
                // alive also preserves strided views; other exporters retain the
                // ordinary Python indexing path when they cannot expose this format.
                let numpy_array_type = py.import("numpy")?.getattr("ndarray")?;
                let mut buffer = if array.get_type().as_any().is(&numpy_array_type) {
                    PyBuffer::<ObjectSlot>::get(&array)
                        .ok()
                        .filter(|buffer| buffer.dimensions() == 1 && buffer.shape()[0] == end - start)
                } else {
                    // Subclasses can override the buffer protocol independently of
                    // indexing. Preserve their existing indexing behavior.
                    None
                };
                for row in 0..end - start {
                    if mask.is_some_and(|m| m[row] != 0) {
                        vector.set_null(row);
                        continue;
                    }
                    let object = if let Some(buffer) = &buffer {
                        // SAFETY: the live buffer validates native object slots and
                        // row is within its shape. Read unaligned to support arbitrary
                        // strides. Acquire an owning reference before any Python call
                        // can mutate the array and replace this slot.
                        let slot = unsafe { buffer.get_ptr(&[row]).cast::<ObjectSlot>().read_unaligned() };
                        if slot.0.is_null() {
                            py.None().into_bound(py)
                        } else {
                            // SAFETY: a non-NULL object slot owns a live Python object;
                            // Python is attached and the exporter is retained above.
                            unsafe { Bound::from_borrowed_ptr(py, slot.0) }
                        }
                    } else {
                        array.get_item(row)?
                    };
                    if !object.is_instance_of::<PyString>()
                        && !object.is_none()
                        && !pandas_na.as_ref().is_some_and(|na| object.is(na))
                    {
                        // A custom conversion can call Python and resize/replace the
                        // exporting array. Release the view before such a call, then
                        // use indexing for subsequent rows rather than a stale pointer.
                        buffer = None;
                    }
                    // Copy directly from Python's UTF-8 view into this chunk's
                    // owned string heap, avoiding temporary String/Value copies.
                    let string = if let Ok(string) = object.cast::<PyString>() {
                        string.clone()
                    } else if object.is_none()
                        || pandas_na.as_ref().is_some_and(|na| object.is(na))
                        || null(&object)?
                    {
                        vector.set_null(row);
                        continue;
                    } else {
                        object.str()?
                    };
                    let value = vector.add_string(string.to_str()?.as_bytes());
                    vector.values_mut::<StringT>()[row] = value;
                }
                result.push(vector);
                continue;
            }
            for row in 0..end - start {
                let value = if mask.is_some_and(|m| m[row] != 0) {
                    Value::null(column.kind.clone())
                } else if let Some(bytes) = bytes {
                    numeric(column, &bytes[row * width..(row + 1) * width])
                        .map_err(|e| crate::error::native(py, &e))?
                } else {
                    let object = array.get_item(row)?;
                    if null(&object)? {
                        Value::null(column.kind.clone())
                    } else if column.kind.id() == L::Varchar {
                        Value::varchar(object.str()?.extract::<String>()?)
                    } else {
                        typed_object(&object, &column.kind)?
                    }
                };
                vector
                    .set_value(row, &value)
                    .map_err(|e| crate::error::native(py, &e))?;
            }
            result.push(vector);
        }
        Ok::<_, PyErr>(result)
    })
    .map_err(|error| python_error(&error))?;
    *output = DataChunk::new(columns, end - start);
    state.position = end as u64;
    Ok(true)
}

fn arrow_string_chunk(py: Python<'_>, column: &Column, start: usize, count: usize) -> PyResult<Vector> {
    // combine_chunks copies at most one engine chunk, even when the original
    // Arrow column has many chunks. No full-column Python string snapshot or
    // contiguous Arrow copy is retained.
    let array = column
        .array
        .bind(py)
        .call_method1("slice", (start, count))?
        .call_method0("combine_chunks")?;
    let array = py.import("pyarrow")?.getattr("StructArray")?.call_method1(
        "from_arrays",
        (PyList::new(py, [array])?, PyList::new(py, ["x"])?),
    )?;
    let exported = array.call_method0("__arrow_c_array__")?;
    let schema = exported.get_item(0)?.cast_into::<PyCapsule>()?;
    let schema_pointer = schema
        .pointer_checked(Some(c"arrow_schema"))?
        .cast::<ArrowSchema>();
    let capsule = exported.get_item(1)?.cast_into::<PyCapsule>()?;
    let array_pointer = capsule
        .pointer_checked(Some(c"arrow_array"))?
        .cast::<ArrowArray>();
    let mut table = ArrowTableSchema::default();
    // SAFETY: these named capsules were exported together by the live PyArrow
    // StructArray above, with one UTF-8 child. The schema capsule stays alive
    // throughout parsing; ownership of the data capsule transfers only once.
    let mut owned = unsafe {
        populate_arrow_table_schema(&mut table, schema_pointer.as_ref())
            .map_err(|error| crate::error::native(py, &error))?;
        ArrowArrayWrapper {
            arrow_array: std::ptr::replace(array_pointer.as_ptr(), ArrowArray::empty()),
        }
    };
    // SAFETY: the schema and array originate from the same StructArray. The
    // converter takes ownership and keeps referenced buffers alive in its vector.
    let chunk = unsafe { data_chunk_from_arrow(&mut owned.arrow_array, &table) }
        .map_err(|error| crate::error::native(py, &error))?;
    if chunk.len() != count
        || chunk.columns().len() != 1
        || chunk.column(0).logical_type() != &LogicalType::VARCHAR
    {
        return Err(crate::error::InternalException::new_err(
            "Unexpected Arrow string chunk shape",
        ));
    }
    Ok(chunk.column(0).clone())
}

pub(super) fn functions() -> TableFunctionSet {
    let mut function = TableFunction::new("pandas_scan", vec![LogicalType::POINTER])
        .with_bind(binding)
        .with_scan(scan);
    function.projection_pushdown = true;
    function.projected_output = true;
    function.serialize = Some(Arc::new(|_, _, _| {
        Err(Error::not_implemented("PandasScan function cannot be serialized"))
    }));
    function.cardinality = Some(Arc::new(|data| {
        data.and_then(|d| d.as_any().downcast_ref::<BoundData>())
            .map(|d| d.rows as u64)
    }));
    let mut set = TableFunctionSet::new("pandas_scan");
    set.add(function);
    set
}

pub(super) fn bind_dataframe(frame: &Bound<'_, PyAny>, sample: usize) -> Result<TableFunctionBindOutput> {
    let copy = dataframe_copy(frame).map_err(|e| python_error(&e))?;
    bind_owner(retain(frame, copy, true), sample)
}
pub(super) fn row_count(data: &dyn FunctionData) -> Result<usize> {
    data.as_any()
        .downcast_ref::<BoundData>()
        .map(|d| d.rows)
        .ok_or_else(|| Error::internal("Missing dataframe bind data"))
}
