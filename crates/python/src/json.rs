//! Frozen JSON relation options over native JSON scans and retained file-like snapshots.
use crate::connection::{PyConnection, lock};
use crate::{conversion, error, files};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyString, PyTuple};
use rustdb_ast::{ExternalDependency, OrdinalityType, TableFunctionRef, TableRefKind};
use rustdb_common::{ByteString, Error};
use rustdb_types::{LogicalType, Value};
use std::fmt::Write as _;
use std::sync::Arc;
const OPTIONS: &[&str] = &[
    "columns",
    "sample_size",
    "maximum_depth",
    "records",
    "format",
    "date_format",
    "timestamp_format",
    "compression",
    "maximum_object_size",
    "ignore_errors",
    "convert_strings_to_integers",
    "field_appearance_threshold",
    "map_inference_threshold",
    "maximum_sample_files",
    "filename",
    "hive_partitioning",
    "union_by_name",
    "hive_types",
    "hive_types_autocast",
];
fn option_error(py: Python<'_>, name: &str, expected: &str, value: &Bound<'_, PyAny>) -> PyResult<PyErr> {
    Ok(error::native(
        py,
        &Error::binder(format!(
            "read_json only accepts '{name}' as {expected}, not '{}'",
            value.get_type().str()?
        )),
    ))
}
#[derive(Debug)]
struct ReadOptions {
    parameters: Vec<(ByteString, Value)>,
    overflow: Option<PyErr>,
}
fn options<'py>(
    owner: &Bound<'py, PyConnection>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<ReadOptions> {
    let py = owner.py();
    let mut overflow = None;
    let get = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(kwargs
            .map(|k| k.get_item(name))
            .transpose()?
            .flatten()
            .filter(|v| !v.is_none()))
    };
    let mut result: Vec<(ByteString, Value)> = Vec::new();
    for name in [
        "filename",
        "hive_types",
        "hive_partitioning",
        "union_by_name",
        "hive_types_autocast",
    ] {
        if let Some(value) = get(name)? {
            if matches!(
                name,
                "hive_partitioning" | "union_by_name" | "hive_types_autocast"
            ) && !value.is_instance_of::<PyBool>()
            {
                return Err(option_error(py, name, "a boolean", &value)?);
            }
            result.push((name.into(), conversion::from_python(&value)?));
        }
    }
    if let Some(columns) = get("columns")? {
        if !files::mapping(&columns)? {
            return Err(error::native(
                py,
                &Error::binder("read_json only accepts 'columns' as a dict[str, str]"),
            ));
        }
        let columns = py.import("builtins")?.getattr("dict")?.call1((columns,))?;
        let columns = columns.cast::<PyDict>()?;
        let mut fields = Vec::new();
        let mut values = Vec::new();
        for (name, kind) in columns.iter() {
            if !name.is_instance_of::<PyString>() {
                return Err(error::native(
                    py,
                    &Error::binder(format!(
                        "The provided column name must be a str, not of type '{}'",
                        name.get_type().str()?
                    )),
                ));
            }
            if !kind.is_instance_of::<PyString>() {
                return Err(error::native(
                    py,
                    &Error::binder(format!(
                        "The provided column type must be a str, not of type '{}'",
                        name.get_type().str()?
                    )),
                ));
            }
            fields.push((name.extract::<String>()?.into(), LogicalType::VARCHAR));
            values.push(Value::varchar(kind.extract::<String>()?));
        }
        result.push((
            "columns".into(),
            Value::from_children(LogicalType::struct_type(fields), values),
        ));
    }
    for name in [
        "records",
        "format",
        "date_format",
        "timestamp_format",
        "compression",
        "sample_size",
        "maximum_depth",
        "maximum_object_size",
        "ignore_errors",
        "convert_strings_to_integers",
        "field_appearance_threshold",
        "map_inference_threshold",
        "maximum_sample_files",
    ] {
        let Some(value) = get(name)? else { continue };
        let val = match name {
            "records" | "format" | "date_format" | "timestamp_format" | "compression" => {
                if !value.is_instance_of::<PyString>() {
                    return Err(option_error(py, name, "a string", &value)?);
                }
                Value::varchar(value.extract::<String>()?)
            }
            "ignore_errors" | "convert_strings_to_integers" => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(option_error(py, name, "a boolean", &value)?);
                }
                Value::boolean(value.extract()?)
            }
            "field_appearance_threshold" => {
                if !value.is_instance_of::<PyFloat>() {
                    return Err(option_error(py, name, "a float", &value)?);
                }
                Value::double(value.extract()?)
            }
            _ => {
                if !value.is_instance_of::<PyInt>() {
                    return Err(option_error(
                        py,
                        name,
                        if name == "maximum_object_size" {
                            "an unsigned integer"
                        } else {
                            "an integer"
                        },
                        &value,
                    )?);
                }
                match name {
                    "sample_size" | "maximum_depth" => {
                        // py::int_'s C-long conversion precedes INTEGER's narrowing.
                        // SAFETY: type checked above, with the GIL held.
                        let number = unsafe { pyo3::ffi::PyLong_AsLong(value.as_ptr()) };
                        if PyErr::occurred(py) {
                            // The frozen wrapper leaves this pending until all later
                            // validation/binding completes. Defer it safely so later
                            // argument and native bind errors retain their priority.
                            overflow = Some(PyErr::fetch(py));
                        }
                        Value::integer(number as i32)
                    }
                    "maximum_object_size" => conversion::to_target(&value, &LogicalType::UINTEGER)?,
                    _ => conversion::to_target(&value, &LogicalType::BIGINT)?,
                }
            }
        };
        result.push((name.into(), val));
    }
    if !result.iter().any(|(key, _)| &key[..] == b"columns") {
        result.push(("auto_detect".into(), Value::boolean(true)));
    }
    Ok(ReadOptions {
        parameters: result,
        overflow,
    })
}
pub(super) fn read(
    owner: Bound<'_, PyConnection>,
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<crate::relation::PyRelation> {
    let py = owner.py();
    let mut signature = String::from("(self: _duckdb.DuckDBPyConnection, path_or_buffer: object, *");
    for name in OPTIONS {
        let _ = write!(
            signature,
            ", {name}: typing.Optional[{}] = None",
            if matches!(*name, "records" | "format") {
                "str"
            } else {
                "object"
            }
        );
    }
    signature.push_str(") -> _duckdb.DuckDBPyRelation");
    let bad = || error::arguments(py, "read_json", &signature, Some(owner.as_any()), args, kwargs);
    if args.len() > 1 {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if key != "path_or_buffer" && !OPTIONS.contains(&key.as_str()) {
                return Err(bad()?);
            }
        }
    }
    let keyword = kwargs
        .map(|kw| kw.get_item("path_or_buffer"))
        .transpose()?
        .flatten();
    if args.len() == 1 && keyword.is_some() {
        return Err(bad()?);
    }
    let path = if args.len() == 1 {
        args.get_item(0)?
    } else if let Some(path) = keyword {
        path
    } else {
        return Err(bad()?);
    };
    for name in ["records", "format"] {
        if let Some(value) = kwargs.map(|kw| kw.get_item(name)).transpose()?.flatten()
            && !value.is_none()
            && !value.is_instance_of::<PyString>()
        {
            return Err(bad()?);
        }
    }
    let state = Arc::clone(&owner.get().state);
    py.detach(|| lock(&state).connection().map(|_| ()))
        .map_err(|e| error::native(py, &e))?;
    let (paths, owners) = files::paths(py, &state, &path)?;
    let ReadOptions { parameters, overflow } = options(&owner, kwargs)?;
    let memory_input = !owners.is_empty();
    let auto = parameters.iter().any(|(key, _)| &key[..] == b"auto_detect");
    let input = if paths.len() == 1 {
        Value::try_varchar(&paths[0])
    } else {
        Ok(Value::list(
            LogicalType::VARCHAR,
            paths
                .iter()
                .map(|p| Value::try_varchar(p))
                .collect::<rustdb_common::Result<_>>()
                .map_err(|e| error::native(py, &e))?,
        ))
    }
    .map_err(|e| error::native(py, &e))?;
    let alias = paths[0]
        .split(|byte| *byte == b'.')
        .next()
        .unwrap_or_default()
        .to_vec()
        .into();
    let mut children = vec![rustdb_parser::build::constant(input)];
    for (name, value) in parameters {
        let mut value = rustdb_parser::build::constant(value);
        value.alias = name;
        children.push(value);
    }
    let mut table = rustdb_parser::build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
        function: Box::new(rustdb_parser::build::function(
            if auto { "read_json_auto" } else { "read_json" },
            children,
        )),
        column_name_alias: Vec::new(),
        with_ordinality: OrdinalityType::WithoutOrdinality,
    }));
    table.external_dependency = ExternalDependency::new(Arc::new(owners));
    let binding = owner.getattr("read_json")?.repr()?.extract::<String>()?;
    let result = crate::relation::PyRelation::from_json_table(owner, table, alias);
    if let Some(cause) = overflow {
        if memory_input {
            if let Err(error) = &result
                && (error.is_instance_of::<error::BinderException>(py)
                    || error.is_instance_of::<error::CatalogException>(py))
            {
                return result;
            }
            // The frozen object's filesystem re-enters Python while the overflow
            // is pending. Its callback boundary reports the original Python error.
            return Err(error::Error::new_err(format!(
                "OverflowError: {}",
                cause.value(py).str()?
            )));
        }
        if result.is_ok() {
            let error = pyo3::exceptions::PySystemError::new_err(format!(
                "{binding} returned a result with an exception set"
            ));
            error.set_cause(py, Some(cause));
            return Err(error);
        }
    }
    result
}
