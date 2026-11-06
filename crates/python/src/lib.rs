//! Native DuckDB Python API backed by the Rust engine. Coverage is tracked in docs/python-api-inventory.json.
// Keep the public ADBC initializer in the Python image, sharing the same engine code
// and process state as the Python API instead of loading a second library.
pub use rustdb_capi::duckdb_adbc_init;

mod aggregation;
mod append;
mod arguments;
mod arrow_filter;
mod connection;
mod conversion;
mod datatype;
mod decimal;
mod error;
mod expression;
mod files;
mod filesystem;
mod host;
mod input;
mod json;
mod map;
mod pandas;
mod pending;
mod polars;
mod reader;
mod relation;
mod statement;
mod type_creation;
mod udf;
use datatype::PyType;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use rustdb_types::LogicalType;

#[pyfunction(signature=(*args, **kwargs))]
fn torch(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyAny>> {
    host::tensor_module("torch", args, kwargs)
}
#[pyfunction(signature=(*args, **kwargs))]
fn tf(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyAny>> {
    host::tensor_module("tf", args, kwargs)
}

fn kind(_py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<LogicalType> {
    datatype::from_object(value)
}

fn string_kind(collation: &[u8]) -> PyType {
    if collation.is_empty() {
        return PyType {
            native: LogicalType::VARCHAR,
        };
    }
    PyType {
        native: LogicalType::with_info(
            rustdb_types::LogicalTypeId::Varchar,
            rustdb_types::TypeInfo::String(rustdb_types::StringTypeInfo {
                collation: collation.into(),
            }),
        ),
    }
}

#[pyfunction(signature=(*args, **kwargs))]
fn sqltype(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("sqltype", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn dtype(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("dtype", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn array_type(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("array_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn decimal_type(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("decimal_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn map_type(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("map_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn struct_type(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("struct_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn row_type(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("row_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn union_type(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("union_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn enum_type(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("enum_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn list_type(args: &Bound<'_, pyo3::types::PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<PyType> {
    type_creation::call("list_type", None, args, kwargs)
}

#[pyfunction(signature=(*args, **kwargs))]
fn string_type(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("string_type", None, args, kwargs)
}

#[pyfunction(name="type",signature=(*args, **kwargs))]
fn type_object(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    type_creation::call("type", None, args, kwargs)
}
#[pyfunction]
fn _set_default_connection_error(
    py: Python<'_>,
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    Err(error::arguments(
        py,
        "set_default_connection",
        "(connection: duckdb.DuckDBPyConnection) -> None",
        None,
        args,
        kwargs,
    )?)
}
#[pyfunction]
fn _dataframe_arguments_error(
    py: Python<'_>,
    name: &str,
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: &Bound<'_, PyDict>,
) -> PyResult<()> {
    let signature = match name {
        "df" => {
            "(*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> pandas.DataFrame\n    2. (*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> pandas.DataFrame\n    3. (df: pandas.DataFrame, *, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation"
        }
        "aggregate" => {
            "(df: pandas.DataFrame, aggr_expr: object, group_expr: str = '', *, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation"
        }
        "limit" => {
            "(df: pandas.DataFrame, n: typing.SupportsInt | typing.SupportsIndex, offset: typing.SupportsInt | typing.SupportsIndex = 0, *, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation"
        }
        _ => return Err(error::InternalException::new_err("Unknown dataframe helper")),
    };
    Err(error::arguments(py, name, signature, None, args, Some(kwargs))?)
}
#[pymodule]
fn _duckdb(module: &Bound<'_, PyModule>) -> PyResult<()> {
    rustdb_capi::register_extension_loader();
    let py = module.py();
    error::register(module)?;
    expression::register(module)?;
    datatype::register(module)?;
    statement::register(module)?;
    module.add_class::<connection::PyConnection>()?;
    module.add_function(wrap_pyfunction!(torch, module)?)?;
    module.add_function(wrap_pyfunction!(tf, module)?)?;
    module.add_function(wrap_pyfunction!(_set_default_connection_error, module)?)?;
    module.add_function(wrap_pyfunction!(_dataframe_arguments_error, module)?)?;
    module.add_function(wrap_pyfunction!(arguments::_string_argument_valid, module)?)?;
    module.add_class::<relation::PyRelation>()?;
    let relation_class = module.getattr("DuckDBPyRelation")?;
    relation_class.setattr("__getattr__", relation_class.getattr("_lookup_attribute")?)?;
    relation_class.delattr("_lookup_attribute")?;
    module.add_function(wrap_pyfunction!(connection::connect, module)?)?;
    module.add_function(wrap_pyfunction!(append::default, module)?)?;
    module.add_function(wrap_pyfunction!(sqltype, module)?)?;
    module.add_function(wrap_pyfunction!(dtype, module)?)?;
    module.add_function(wrap_pyfunction!(row_type, module)?)?;
    module.add_function(wrap_pyfunction!(enum_type, module)?)?;
    module.add_function(wrap_pyfunction!(type_object, module)?)?;
    module.add_function(wrap_pyfunction!(string_type, module)?)?;
    module.add_function(wrap_pyfunction!(list_type, module)?)?;
    module.add_function(wrap_pyfunction!(array_type, module)?)?;
    module.add_function(wrap_pyfunction!(decimal_type, module)?)?;
    module.add_function(wrap_pyfunction!(map_type, module)?)?;
    module.add_function(wrap_pyfunction!(struct_type, module)?)?;
    module.add_function(wrap_pyfunction!(union_type, module)?)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add("__duckdb_version__", env!("CARGO_PKG_VERSION"))?;
    module.add("__standard_vector_size__", rustdb_types::STANDARD_VECTOR_SIZE)?;
    let version = py.import("sys")?.getattr("version_info")?;
    module.add(
        "__formatted_python_version__",
        format!(
            "{}.{}",
            version.getattr("major")?.extract::<u8>()?,
            version.getattr("minor")?.extract::<u8>()?
        ),
    )?;
    module.add("__interactive__", false)?;
    module.add("__jupyter__", false)?;
    module.add("apilevel", "2.0")?;
    module.add("threadsafety", 1)?;
    module.add("paramstyle", "qmark")?;
    module.add("__rust_engine__", true)?;
    module.add(
        "__implemented_surfaces__",
        PyList::new(
            py,
            [
                "connect",
                "DuckDBPyConnection",
                "DuckDBPyType",
                "DB-API execution/fetch",
                "native conversions",
                "exception hierarchy",
            ],
        )?,
    )?;
    Ok(())
}
