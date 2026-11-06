//! Frozen type constructors shared by connection and module APIs.
use crate::{connection::PyConnection, datatype::PyType, error};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PyString, PyTuple};
use rustdb_types::LogicalType;

const TYPE: &str = "_duckdb._sqltypes.DuckDBPyType";
const INT: &str = "typing.SupportsInt | typing.SupportsIndex";

pub(super) fn string_bytes(value: &Bound<'_, PyAny>) -> PyResult<Option<Vec<u8>>> {
    if let Ok(value) = value.cast::<PyString>() {
        return Ok(Some(value.to_str()?.as_bytes().to_vec()));
    }
    if let Ok(value) = value.cast::<PyBytes>() {
        return Ok(Some(value.as_bytes().to_vec()));
    }
    if let Ok(value) = value.cast::<PyByteArray>() {
        return Ok(Some(value.to_vec()));
    }
    Ok(None)
}

pub(super) fn call(
    name: &str,
    owner: Option<&Bound<'_, PyConnection>>,
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyType> {
    let py = args.py();
    let (names, required, parameters): (&[&str], usize, String) = match name {
        "type" | "dtype" | "sqltype" => (&["type_str"], 1, "type_str: str".to_owned()),
        "array_type" => (&["type", "size"], 2, format!("type: {TYPE}, size: {INT}")),
        "decimal_type" => (&["width", "scale"], 2, format!("width: {INT}, scale: {INT}")),
        "enum_type" => (
            &["name", "type", "values"],
            3,
            format!("name: str, type: {TYPE}, values: list"),
        ),
        "list_type" => (&["type"], 1, format!("type: {TYPE}")),
        "map_type" => (&["key", "value"], 2, format!("key: {TYPE}, value: {TYPE}")),
        "struct_type" | "row_type" => (&["fields"], 1, "fields: object".to_owned()),
        "union_type" => (&["members"], 1, "members: object".to_owned()),
        "string_type" => (&["collation"], 0, "collation: str = ''".to_owned()),
        _ => return Err(error::InternalException::new_err("Unknown type constructor")),
    };
    let signature = if owner.is_some() {
        format!("(self: _duckdb.DuckDBPyConnection, {parameters}) -> {TYPE}")
    } else {
        format!("({parameters}, *, connection: duckdb.DuckDBPyConnection = None) -> {TYPE}")
    };
    let bad = || error::arguments(py, name, &signature, owner.map(Bound::as_any), args, kwargs);
    if args.len() > names.len() {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if !(names.contains(&key.as_str()) || owner.is_none() && key == "connection") {
                return Err(bad()?);
            }
        }
    }
    let mut values = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let keyword = kwargs.map(|kwargs| kwargs.get_item(*name)).transpose()?.flatten();
        if args.len() > index {
            if keyword.is_some() {
                return Err(bad()?);
            }
            values.push(Some(args.get_item(index)?));
        } else if keyword.is_some() {
            values.push(keyword);
        } else if index < required {
            return Err(bad()?);
        } else {
            values.push(None);
        }
    }
    let connection = if owner.is_none() {
        kwargs
            .map(|kwargs| kwargs.get_item("connection"))
            .transpose()?
            .flatten()
            .filter(|value| !value.is_none())
    } else {
        None
    };
    let connection = match &connection {
        Some(value) => match value.extract::<PyRef<'_, PyConnection>>() {
            Ok(value) => Some(value),
            Err(_) => return Err(bad()?),
        },
        None => None,
    };
    let get = |index: usize| {
        values
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| error::InternalException::new_err("Missing validated type-constructor argument"))
    };
    let kind = |index| -> PyResult<LogicalType> {
        let value = get(index)?;
        if value.is_none() {
            return Err(bad()?);
        }
        match crate::datatype::from_object(value) {
            Ok(kind) => Ok(kind),
            Err(_) => Err(bad()?),
        }
    };
    let native = match name {
        "type" | "dtype" | "sqltype" => {
            let Some(text) = string_bytes(get(0)?)? else {
                return Err(bad()?);
            };
            if let Some(owner) = owner {
                return owner.get().named_type(py, &text);
            }
            if let Some(connection) = connection {
                return connection.named_type(py, &text);
            }
            let default = py.import("duckdb")?.call_method0("default_connection")?;
            return default
                .extract::<PyRef<'_, PyConnection>>()?
                .named_type(py, &text);
        }
        "list_type" => LogicalType::list(kind(0)?),
        "map_type" => LogicalType::map(kind(0)?, kind(1)?),
        "array_type" => {
            let child = kind(0)?;
            let Ok(size) = get(1)?.extract::<u64>() else {
                return Err(bad()?);
            };
            LogicalType::array(child, size as u32)
        }
        "decimal_type" => {
            let (Ok(width), Ok(scale)) = (get(0)?.extract::<i32>(), get(1)?.extract::<i32>()) else {
                return Err(bad()?);
            };
            let (width, scale) = (width as u8, scale as u8);
            if width > 38 {
                return Err(error::native(
                    py,
                    &rustdb_common::Error::internal(rustdb_common::byte_format!(
                        "Decimal has a width of {width} which is bigger than the maximum supported width of 38"
                    )),
                ));
            }
            LogicalType::decimal(width, scale)
        }
        "string_type" => {
            let collation = if let Some(value) = &values[0] {
                let Some(text) = string_bytes(value)? else {
                    return Err(bad()?);
                };
                text
            } else {
                Vec::new()
            };
            return Ok(crate::string_kind(&collation));
        }
        "enum_type" => {
            if string_bytes(get(0)?)?.is_none() || !get(2)?.is_instance_of::<PyList>() {
                return Err(bad()?);
            }
            if !get(1)?.is_none() {
                let _underlying = kind(1)?;
            }
            return Err(error::NotImplementedException::new_err(
                "Not implemented Error: enum_type creation method is not implemented yet",
            ));
        }
        "struct_type" | "row_type" | "union_type" => {
            if name == "union_type" && get(0)?.is_none() {
                return Err(bad()?);
            }
            let fields = fields(get(0)?)?;
            if fields.is_empty() {
                return Err(error::InvalidInputException::new_err(if name == "union_type" {
                    "Invalid Input Error: Can not create an empty union type!"
                } else {
                    "Invalid Input Error: Can not create an empty struct type!"
                }));
            }
            if name == "union_type" {
                LogicalType::union_type(fields)
            } else {
                LogicalType::struct_type(fields)
            }
        }
        _ => unreachable!("constructor validated"),
    };
    Ok(PyType { native })
}

fn fields(object: &Bound<'_, PyAny>) -> PyResult<Vec<(rustdb_common::ByteString, LogicalType)>> {
    let mut result = Vec::new();
    let convert = |value: &Bound<'_, PyAny>| -> PyResult<LogicalType> {
        crate::datatype::from_object(value).map_err(|_| {
            crate::error::InvalidInputException::new_err(format!(
                "Invalid Input Error: object has to be a list of DuckDBPyType's, not '{}'",
                value.get_type()
            ))
        })
    };
    if let Ok(dict) = object.cast::<PyDict>() {
        for (name, value) in dict.iter() {
            result.push((name.str()?.extract::<String>()?.into(), convert(&value)?));
        }
    } else if let Ok(list) = object.cast::<PyList>() {
        for (index, value) in list.iter().enumerate() {
            result.push((format!("v{}", index + 1).into(), convert(&value)?));
        }
    } else {
        return Err(error::InvalidInputException::new_err(format!(
            "Invalid Input Error: Can not construct a child list from object of type '{}', only dict/list is supported",
            object.get_type()
        )));
    }
    Ok(result)
}
