//! Python scalar functions execute in native expression kernels; Arrow batches use C data ownership.
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyTuple};
use roastedduck_arrow::{
    ClientProperties,
    ffi::{ArrowArrayWrapper, ArrowSchemaWrapper},
};
use roastedduck_common::{Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::{NullHandling, Stability};
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, SelectionVector, Value, Vector};

#[derive(Debug)]
struct ArrowProperties(ClientProperties);
impl roastedduck_function::FunctionData for ArrowProperties {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
const NULL_ERROR: &str = "\nThe returned result contained NULL values, but the 'null_handling' was set to DEFAULT.\nIf you want more control over NULL values then 'null_handling' should be set to SPECIAL.\n\nWith DEFAULT all rows containing NULL have been filtered from the UDFs input.\nThose rows are automatically set to NULL in the final result.\nThe UDF is not expected to return NULL values.\n\t";
fn invalid(py: Python<'_>, text: impl Into<String>) -> PyErr {
    error::native(py, &Error::invalid_input(text.into()))
}
// `map_err` transfers the Python exception, releasing it after constructing the native error.
#[allow(clippy::needless_pass_by_value)]
fn boundary(error: PyErr) -> Error {
    Python::attach(|py| {
        if error.is_instance_of::<crate::error::InvalidInputException>(py)
            || error.is_instance_of::<crate::error::ConversionException>(py)
            || error.is_instance_of::<crate::error::NotImplementedException>(py)
        {
            crate::pandas::python_error(&error)
        } else {
            Error::invalid_input(error.to_string())
        }
    })
}
#[allow(clippy::needless_pass_by_value)]
fn callback(error: PyErr) -> Error {
    let detail = crate::input::python_error(&error);
    Error::invalid_input(format!(
        "Python exception occurred while executing the UDF: {}",
        detail.raw_message()
    ))
}
pub(super) fn option(value: Option<&Bound<'_, PyAny>>, key: &str, names: [&str; 2]) -> PyResult<bool> {
    let Some(value) = value else { return Ok(false) };
    if let Ok(number) = value.extract::<i64>() {
        if number == 0 || number == 1 {
            return Ok(number == 1);
        }
    } else if let Ok(text) = value.extract::<String>() {
        for (index, name) in names.iter().enumerate() {
            if text.eq_ignore_ascii_case(name) {
                return Ok(index == 1);
            }
        }
    }
    Err(invalid(
        value.py(),
        format!("'{}' is not a recognized type for '{key}'", value.str()?),
    ))
}
pub(super) fn annotation(object: &Bound<'_, PyAny>) -> PyResult<LogicalType> {
    let py = object.py();
    let builtins = py.import("builtins")?;
    for (name, ty) in [
        ("int", LogicalType::BIGINT),
        ("float", LogicalType::DOUBLE),
        ("str", LogicalType::VARCHAR),
        ("bool", LogicalType::BOOLEAN),
        ("bytes", LogicalType::BLOB),
    ] {
        if object.is(&builtins.getattr(name)?) {
            return Ok(ty);
        }
    }
    let datetime = py.import("datetime")?;
    for (name, ty) in [
        ("datetime", LogicalType::TIMESTAMP),
        ("date", LogicalType::DATE),
        ("time", LogicalType::TIME),
        ("timedelta", LogicalType::INTERVAL),
    ] {
        if object.is(&datetime.getattr(name)?) {
            return Ok(ty);
        }
    }
    if object.is(&py.import("uuid")?.getattr("UUID")?) {
        return Ok(LogicalType::UUID);
    }
    let typing = py.import("typing")?;
    let origin = typing.call_method1("get_origin", (object,))?;
    if !origin.is_none() {
        let args = typing.call_method1("get_args", (object,))?;
        if origin.is(&builtins.getattr("list")?) {
            return Ok(LogicalType::list(annotation(&args.get_item(0)?)?));
        }
        if origin.is(&builtins.getattr("dict")?) {
            return Ok(LogicalType::map(
                annotation(&args.get_item(0)?)?,
                annotation(&args.get_item(1)?)?,
            ));
        }
    }
    crate::kind(py, object)
}

// These independent flags are the frozen Python create_function keyword contract.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(super) fn function(
    py: Python<'_>,
    name: &[u8],
    callable: &Bound<'_, PyAny>,
    parameters: Option<&Bound<'_, PyAny>>,
    return_type: Option<&Bound<'_, PyAny>>,
    arrow: bool,
    special: bool,
    return_null: bool,
    side_effects: bool,
    properties: ClientProperties,
) -> PyResult<ScalarFunctionSet> {
    if !callable.is_callable() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "function must be callable",
        ));
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("eval_str", true)?;
    let signature = py
        .import("inspect")?
        .getattr("signature")?
        .call((callable,), Some(&kwargs))?;
    let params = signature.getattr("parameters")?.call_method0("values")?;
    let mut arguments = Vec::new();
    let mut varargs = None;
    for parameter in params.try_iter()? {
        let parameter = parameter?;
        let inferred = annotation(&parameter.getattr("annotation")?);
        if inferred.is_err() && parameter.getattr("kind")?.str()?.to_str()? == "VAR_POSITIONAL" {
            varargs = Some(LogicalType::ANY);
        }
        arguments.push(inferred.unwrap_or(LogicalType::ANY));
    }
    if let Some(parameters) = parameters.filter(|p| !p.is_none()) {
        let list = parameters.cast::<PyList>().map_err(|_| {
            invalid(
                py,
                "Either leave 'parameters' empty, or provide a list of DuckDBPyType objects",
            )
        })?;
        if list.len() != arguments.len() {
            return Err(invalid(
                py,
                format!(
                    "{} types provided, but the provided function takes {} parameters",
                    list.len(),
                    arguments.len()
                ),
            ));
        }
        arguments = list.iter().map(|p| annotation(&p)).collect::<PyResult<_>>()?;
    }
    let result_type = if let Some(ret) = return_type.filter(|r| !r.is_none()) {
        annotation(ret)?
    } else {
        annotation(&signature.getattr("return_annotation")?)
            .map_err(|_| invalid(py, "Could not infer the return type, please set it explicitly"))?
    };
    // NumPy initializes its C API on the registering Python thread, before workers run.
    py.import("numpy._core.multiarray")
        .or_else(|_| py.import("numpy.core.multiarray"))?;
    let callable = callable.clone().unbind();
    let ret = result_type.clone();
    let mut function = ScalarFunction::new(name, arguments, result_type, move |input, state, output| {
        *output = Vector::flat(ret.clone(), input.len());
        Python::attach(|py| {
            if arrow {
                execute_arrow(
                    py,
                    callable.bind(py),
                    input,
                    output,
                    &ret,
                    state
                        .bind_data
                        .as_ref()
                        .and_then(|d| d.as_any().downcast_ref::<ArrowProperties>())
                        .map_or(&properties, |d| &d.0),
                    special,
                    return_null,
                )
            } else {
                execute_native(
                    py,
                    callable.bind(py),
                    input,
                    output,
                    &ret,
                    &properties,
                    special,
                    return_null,
                )
            }
        })
    });
    if arrow {
        function.bind = Some(std::sync::Arc::new(|_, input| {
            let mut properties = ClientProperties::default();
            if let Some(settings) = input.client.settings() {
                let setting = |name: &str| settings.current_setting(name).ok();
                let enabled = |name: &str| setting(name).and_then(|v| v.as_bool()).unwrap_or(false);
                if let Some(zone) = setting("TimeZone").and_then(|v| v.to_varchar().ok()) {
                    properties.time_zone = zone;
                }
                if enabled("arrow_large_buffer_size") {
                    properties.arrow_offset_size = roastedduck_arrow::ArrowOffsetSize::Large;
                }
                properties.arrow_use_list_view = enabled("arrow_output_list_view");
                properties.produce_arrow_string_view = enabled("produce_arrow_string_view");
                properties.arrow_lossless_conversion = enabled("arrow_lossless_conversion");
                if let Some(version) = setting("arrow_output_version")
                    .and_then(|v| v.to_varchar().ok())
                    .and_then(|v| roastedduck_arrow::ArrowFormatVersion::from_setting(&v))
                {
                    properties.arrow_output_version = version;
                }
            }
            Ok(Some(std::sync::Arc::new(ArrowProperties(properties))))
        }));
    }
    function.varargs = varargs;
    function.null_handling = if special {
        NullHandling::Special
    } else {
        NullHandling::Default
    };
    function.stability = if side_effects {
        Stability::Volatile
    } else {
        Stability::Consistent
    };
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    Ok(set)
}
fn return_value(object: &Bound<'_, PyAny>, ret: &LogicalType) -> PyResult<Value> {
    // TransformPythonObject writes primitive returns directly to the declared type.
    // Avoid constructing and executing an intermediate INTEGER -> BIGINT vector cast.
    if object.is_none() {
        return Ok(Value::null(ret.clone()));
    }
    if ret.id() == LogicalTypeId::Union
        && object.is_instance_of::<PyInt>()
        && !object.is_instance_of::<PyBool>()
    {
        // Vector conversion starts with BIGINT, unlike value/parameter conversion's
        // sniffed INTEGER. That distinction determines the first exact UNION member.
        if let Ok(value) = object.extract::<i64>() {
            return Ok(Value::bigint(value));
        }
        if let Ok(value) = object.extract::<u64>() {
            return Ok(Value::ubigint(value));
        }
    }
    if object.is_instance_of::<PyInt>() {
        return match ret.id() {
            LogicalTypeId::TinyInt => object.extract().map(Value::tinyint),
            LogicalTypeId::SmallInt => object.extract().map(Value::smallint),
            LogicalTypeId::Integer => object.extract().map(Value::integer),
            LogicalTypeId::BigInt => object.extract().map(Value::bigint),
            LogicalTypeId::HugeInt => object.extract().map(Value::hugeint),
            LogicalTypeId::UTinyInt => object.extract().map(Value::utinyint),
            LogicalTypeId::USmallInt => object.extract().map(Value::usmallint),
            LogicalTypeId::UInteger => object.extract().map(Value::uinteger),
            LogicalTypeId::UBigInt => object.extract().map(Value::ubigint),
            LogicalTypeId::UHugeInt => object.extract().map(Value::uhugeint),
            LogicalTypeId::Double => object.extract().map(Value::double),
            LogicalTypeId::Float => object.extract::<f64>().map(|v| Value::float(v as f32)),
            LogicalTypeId::Boolean if object.is_instance_of::<PyBool>() => {
                object.extract().map(Value::boolean)
            }
            _ => conversion::from_python(object),
        };
    }
    if object.is_instance_of::<PyFloat>() {
        match ret.id() {
            LogicalTypeId::Double => return object.extract().map(Value::double),
            LogicalTypeId::Float => return object.extract::<f64>().map(|v| Value::float(v as f32)),
            _ => {}
        }
    }
    let is_pandas_null = object.get_type().name()?.to_str()? == "NAType"
        && object
            .get_type()
            .getattr("__module__")?
            .extract::<String>()?
            .starts_with("pandas");
    if is_pandas_null {
        Ok(Value::null(ret.clone()))
    } else if ret.is_nested() {
        conversion::to_target(object, ret)
    } else {
        conversion::from_python(object)
    }
}
#[allow(clippy::too_many_arguments)]
fn execute_native(
    py: Python<'_>,
    callable: &Bound<'_, PyAny>,
    input: &DataChunk,
    output: &mut Vector,
    ret: &LogicalType,
    properties: &ClientProperties,
    special: bool,
    return_null: bool,
) -> Result<()> {
    let mut values = Vec::with_capacity(input.column_count());
    let mut arguments = Vec::with_capacity(input.column_count());
    for row in 0..input.len() {
        values.clear();
        values.extend(input.columns().iter().map(|c| c.get_value(row)));
        if !special && values.iter().any(Value::is_null) {
            output.set_value(row, &Value::null(ret.clone()))?;
            continue;
        }
        for value in &values {
            arguments.push(conversion::to_python(py, value, &properties.time_zone).map_err(boundary)?);
        }
        let args = PyTuple::new(py, arguments.drain(..)).map_err(boundary)?;
        let object = match callable.call1(args) {
            Ok(value) => value,
            Err(_) if return_null => {
                output.set_value(row, &Value::null(ret.clone()))?;
                continue;
            }
            Err(e) => return Err(callback(e)),
        };
        if object.is_none() && !special {
            return Err(Error::invalid_input(NULL_ERROR));
        }
        if object.is_instance_of::<PyBool>() && ret.id() != LogicalTypeId::Boolean {
            return Err(Error::new_bytes(roastedduck_common::ErrorKind::MismatchType, roastedduck_common::byte_format!("Type BOOLEAN does not match with {}. Python Conversion Failure: Expected a value of type %s, but got a value of type boolean",ret.to_bytes()).into_bytes()));
        }
        let value = return_value(&object, ret).map_err(boundary)?;
        let value = if value.logical_type() == ret {
            value
        } else {
            conversion::cast_target(value, ret)?
        };
        output.set_value(row, &value)?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn execute_arrow(
    py: Python<'_>,
    callable: &Bound<'_, PyAny>,
    input: &DataChunk,
    output: &mut Vector,
    ret: &LogicalType,
    properties: &ClientProperties,
    special: bool,
    return_null: bool,
) -> Result<()> {
    let rows = (0..input.len())
        .filter(|&row| special || input.columns().iter().all(|c| !c.get_value(row).is_null()))
        .collect::<Vec<_>>();
    for row in 0..input.len() {
        output.set_value(row, &Value::null(ret.clone()))?;
    }
    let selection = SelectionVector::new(rows.iter().map(|&r| r as u32).collect::<Vec<_>>());
    let chunk = DataChunk::new(
        input.columns().iter().map(|v| v.slice(&selection)).collect(),
        rows.len(),
    );
    let arrow = py.import("pyarrow").map_err(boundary)?;
    let types = chunk
        .columns()
        .iter()
        .map(|v| v.logical_type().clone())
        .collect::<Vec<_>>();
    let names = (0..types.len())
        .map(|i| format!("c{i}").into())
        .collect::<Vec<_>>();
    let mut schema = ArrowSchemaWrapper::default();
    roastedduck_arrow::to_arrow_schema(&mut schema.arrow_schema, &types, &names, properties)?;
    let mut array = ArrowArrayWrapper {
        arrow_array: roastedduck_arrow::to_arrow_array(&chunk, properties)?,
    };
    let batch = arrow
        .getattr("RecordBatch")
        .and_then(|c| {
            c.call_method1(
                "_import_from_c",
                (
                    (&raw mut array.arrow_array) as usize,
                    (&raw mut schema.arrow_schema) as usize,
                ),
            )
        })
        .map_err(boundary)?;
    let table = arrow
        .getattr("Table")
        .and_then(|c| c.call_method1("from_batches", (PyList::new(py, [batch])?,)))
        .map_err(boundary)?;
    let args = PyTuple::new(
        py,
        table
            .getattr("columns")
            .map_err(boundary)?
            .try_iter()
            .map_err(boundary)?
            .collect::<PyResult<Vec<_>>>()
            .map_err(boundary)?,
    )
    .map_err(boundary)?;
    let (object, exception) = match callable.call1(args) {
        Ok(value) => (value, false),
        Err(_) if return_null => (
            arrow.call_method1("nulls", (rows.len(),)).map_err(boundary)?,
            true,
        ),
        Err(e) => return Err(callback(e)),
    };
    let table = if object
        .is_instance(&arrow.getattr("Table").map_err(boundary)?)
        .map_err(boundary)?
    {
        object
    } else {
        arrow
            .getattr("Table")
            .and_then(|c| {
                c.call_method1(
                    "from_arrays",
                    (PyList::new(py, [object])?, PyList::new(py, ["c0"])?),
                )
            })
            .map_err(|_| Error::invalid_input("Could not convert the result into an Arrow Table"))?
    };
    let columns = table
        .getattr("num_columns")
        .and_then(|v| v.extract::<usize>())
        .map_err(boundary)?;
    if columns != 1 {
        return Err(Error::invalid_input(format!(
            "The returned table from a pyarrow scalar udf should only contain one column, found {columns}"
        )));
    }
    let count = table
        .getattr("num_rows")
        .and_then(|v| v.extract::<usize>())
        .map_err(boundary)?;
    if count != rows.len() {
        return Err(Error::invalid_input(format!(
            "Returned pyarrow table should have {} tuples, found {count}",
            rows.len()
        )));
    }
    let mut position = 0;
    for batch in table
        .call_method0("to_batches")
        .map_err(boundary)?
        .try_iter()
        .map_err(boundary)?
    {
        let batch = batch.map_err(boundary)?;
        let mut schema = ArrowSchemaWrapper::default();
        let mut array = ArrowArrayWrapper::default();
        batch
            .call_method1(
                "_export_to_c",
                (
                    (&raw mut array.arrow_array) as usize,
                    (&raw mut schema.arrow_schema) as usize,
                ),
            )
            .map_err(boundary)?;
        let mut imported = roastedduck_arrow::ArrowTableSchema::default();
        // SAFETY: PyArrow exported a live struct array and matching schema. Both wrappers
        // release on errors; data_chunk_from_arrow takes array ownership and pins buffers.
        let chunk = unsafe {
            roastedduck_arrow::populate_arrow_table_schema(&mut imported, &schema.arrow_schema)?;
            roastedduck_arrow::data_chunk_from_arrow(&mut array.arrow_array, &imported)?
        };
        for index in 0..chunk.len() {
            let value = chunk.column(0).get_value(index).try_cast(ret)?;
            if !special && !exception && value.is_null() {
                return Err(Error::invalid_input(NULL_ERROR));
            }
            output.set_value(rows[position], &value)?;
            position += 1;
        }
    }
    Ok(())
}
