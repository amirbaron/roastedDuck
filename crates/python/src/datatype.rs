use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};
use rustdb_types::LogicalType;

#[pyclass(module = "_duckdb._sqltypes", name = "DuckDBPyType", frozen)]
#[derive(Clone, Debug)]
pub struct PyType {
    pub native: LogicalType,
}
impl PyType {
    pub fn parse(py: Python<'_>, name: &str) -> PyResult<Self> {
        let owner = py.import("duckdb")?.call_method0("default_connection")?;
        owner
            .extract::<PyRef<'_, crate::connection::PyConnection>>()?
            .named_type(py, name.as_bytes())
    }
}
pub(super) fn from_object(object: &Bound<'_, PyAny>) -> PyResult<LogicalType> {
    let py = object.py();
    if let Ok(kind) = object.extract::<PyRef<'_, PyType>>() {
        return Ok(kind.native.clone());
    }
    if object.is_instance_of::<PyString>() {
        return Ok(PyType::parse(py, &object.extract::<String>()?)?.native);
    }
    if object.is_instance_of::<pyo3::types::PyType>() {
        let builtins = py.import("builtins")?;
        for (name, kind) in [
            ("str", LogicalType::VARCHAR),
            ("int", LogicalType::BIGINT),
            ("bytes", LogicalType::BLOB),
            ("bytearray", LogicalType::BLOB),
            ("float", LogicalType::DOUBLE),
            ("bool", LogicalType::BOOLEAN),
        ] {
            if object.is(&builtins.getattr(name)?) {
                return Ok(kind);
            }
        }
        let instance = object.call0()?;
        if let Ok(dtype) = instance.getattr("dtype") {
            let name = dtype.str()?.extract::<String>()?;
            let native = match name.as_str() {
                "bool" => Some(LogicalType::BOOLEAN),
                "int8" => Some(LogicalType::TINYINT),
                "uint8" => Some(LogicalType::UTINYINT),
                "int16" => Some(LogicalType::SMALLINT),
                "uint16" => Some(LogicalType::USMALLINT),
                "int32" => Some(LogicalType::INTEGER),
                "uint32" => Some(LogicalType::UINTEGER),
                "int64" => Some(LogicalType::BIGINT),
                "uint64" => Some(LogicalType::UBIGINT),
                "float16" | "float32" => Some(LogicalType::FLOAT),
                "float64" => Some(LogicalType::DOUBLE),
                _ => None,
            };
            if let Some(native) = native {
                return Ok(native);
            }
        }
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Could not convert from unknown 'type' to DuckDBPyType",
        ));
    }
    if let Ok(dict) = object.cast::<PyDict>() {
        if dict.is_empty() {
            return Err(crate::error::native(
                py,
                &rustdb_common::Error::invalid_input(
                    "Could not convert empty dictionary to a duckdb STRUCT type",
                ),
            ));
        }
        let fields = dict
            .iter()
            .map(|(name, kind)| Ok((name.str()?.extract::<String>()?.into(), from_object(&kind)?)))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(LogicalType::struct_type(fields));
    }
    let types = py.import("types")?;
    if object.is_instance(&types.getattr("GenericAlias")?)? {
        let origin = object.getattr("__origin__")?;
        let args = object.getattr("__args__")?;
        let builtins = py.import("builtins")?;
        if origin.is(&builtins.getattr("list")?) {
            if args.len()? != 1 {
                return Err(crate::error::native(
                    py,
                    &rustdb_common::Error::not_implemented("Can only create a LIST from a single type"),
                ));
            }
            return Ok(LogicalType::list(from_object(&args.get_item(0)?)?));
        }
        if origin.is(&builtins.getattr("dict")?) {
            if args.len()? != 2 {
                return Err(crate::error::native(
                    py,
                    &rustdb_common::Error::not_implemented(
                        "Can only create a MAP from a dict if args is formed correctly",
                    ),
                ));
            }
            return Ok(LogicalType::map(
                from_object(&args.get_item(0)?)?,
                from_object(&args.get_item(1)?)?,
            ));
        }
        return Err(crate::error::native(
            py,
            &rustdb_common::Error::invalid_input(format!(
                "Could not convert from '{}' to DuckDBPyType",
                origin.str()?
            )),
        ));
    }
    let typing = py.import("typing")?;
    if object.is_instance(&types.getattr("UnionType")?)?
        || typing
            .call_method1("get_origin", (object,))?
            .is(&typing.getattr("Union")?)
    {
        let none_type = py.None().bind(py).get_type();
        let args = object.getattr("__args__")?;
        let mut members = Vec::new();
        for item in args.try_iter()? {
            let item = item?;
            if !item.is(&none_type) {
                members.push(from_object(&item)?);
            }
        }
        if members.len() == 1 {
            return Ok(members.remove(0));
        }
        return Ok(LogicalType::union_type(
            members
                .into_iter()
                .enumerate()
                .map(|(i, kind)| (format!("u{}", i + 1).into(), kind))
                .collect(),
        ));
    }
    Err(crate::error::native(
        py,
        &rustdb_common::Error::not_implemented(format!(
            "Could not convert from object of type '{}' to DuckDBPyType",
            object.get_type().repr()?
        )),
    ))
}

#[pymethods]
impl PyType {
    #[new]
    fn new(name: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            native: from_object(name)?,
        })
    }
    fn __getattr__(&self, _py: Python<'_>, name: &str) -> PyResult<Self> {
        let kind = if let Some(fields) = self.native.struct_children() {
            fields
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name.as_bytes()))
                .map(|(_, kind)| kind)
        } else if self.native.id() == rustdb_types::LogicalTypeId::List && name.eq_ignore_ascii_case("child")
        {
            self.native.child_type()
        } else if let Some((key, value)) = self.native.map_key_value() {
            match name.to_ascii_lowercase().as_str() {
                "key" => Some(key),
                "value" => Some(value),
                _ => {
                    return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                        "Tried to get a child from a map by the name of '{name}', but this type only has 'key' and 'value' children"
                    )));
                }
            }
        } else {
            None
        };
        kind.cloned().map(|native| Self { native }).ok_or_else(|| pyo3::exceptions::PyAttributeError::new_err(format!("Tried to get child type by the name of '{name}', but this type either isn't nested, or it doesn't have a child by that name")))
    }
    fn _lookup_attribute(&self, py: Python<'_>, name: &str) -> PyResult<Self> {
        self.__getattr__(py, name)
    }
    fn __getitem__(&self, py: Python<'_>, name: &str) -> PyResult<Self> {
        self.__getattr__(py, name)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::conversion::text(py, &self.native.to_bytes())
    }
    fn __repr__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.__str__(py)
    }
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        self.__str__(py)?.bind(py).hash()
    }
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = other.py();
        if let Ok(other) = other.extract::<PyRef<'_, Self>>() {
            return (self.native == other.native).into_py_any(py);
        }
        if let Ok(name) = other.extract::<String>() {
            return self
                .native
                .to_bytes()
                .eq_ignore_ascii_case(name.as_bytes())
                .into_py_any(py);
        }
        Ok(py.NotImplemented())
    }
    #[getter]
    fn id(&self) -> String {
        self.native.id().name().to_ascii_lowercase()
    }
    #[getter]
    fn children(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if self.native.id() == rustdb_types::LogicalTypeId::Variant {
            return Err(crate::error::InvalidInputException::new_err(
                "Invalid Input Error: This type is not nested so it doesn't have children",
            ));
        }
        let mut items = Vec::new();
        if let Some((width, scale)) = self.native.decimal_properties() {
            items.push(PyTuple::new(py, ["precision".into_py_any(py)?, width.into_py_any(py)?])?.unbind());
            items.push(PyTuple::new(py, ["scale".into_py_any(py)?, scale.into_py_any(py)?])?.unbind());
        } else if let Some((child, size)) = self.native.array_child() {
            items.push(
                PyTuple::new(
                    py,
                    [
                        "child".into_py_any(py)?,
                        Py::new(
                            py,
                            Self {
                                native: child.clone(),
                            },
                        )?
                        .into_any(),
                    ],
                )?
                .unbind(),
            );
            items.push(PyTuple::new(py, ["size".into_py_any(py)?, size.into_py_any(py)?])?.unbind());
        } else if let Some((key, value)) = self.native.map_key_value() {
            for (name, kind) in [("key", key), ("value", value)] {
                items.push(
                    PyTuple::new(
                        py,
                        [
                            name.into_py_any(py)?,
                            Py::new(py, Self { native: kind.clone() })?.into_any(),
                        ],
                    )?
                    .unbind(),
                );
            }
        } else if let Some(values) = self.native.enum_values() {
            let values = PyList::new(
                py,
                values
                    .iter()
                    .map(|v| crate::conversion::text(py, v))
                    .collect::<PyResult<Vec<_>>>()?,
            )?;
            items.push(PyTuple::new(py, ["values".into_py_any(py)?, values.into_any().unbind()])?.unbind());
        } else if let Some(fields) = self.native.struct_children() {
            for (name, kind) in fields {
                items.push(
                    PyTuple::new(
                        py,
                        [
                            crate::conversion::text(py, name)?,
                            Py::new(py, Self { native: kind.clone() })?.into_any(),
                        ],
                    )?
                    .unbind(),
                );
            }
        } else if let Some(child) = self.native.child_type() {
            items.push(
                PyTuple::new(
                    py,
                    [
                        "child".into_py_any(py)?,
                        Py::new(
                            py,
                            Self {
                                native: child.clone(),
                            },
                        )?
                        .into_any(),
                    ],
                )?
                .unbind(),
            );
        } else {
            return Err(crate::error::InvalidInputException::new_err(
                "Invalid Input Error: This type is not nested so it doesn't have children",
            ));
        }
        Ok(PyList::new(py, items)?.unbind().into_any())
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let child = PyModule::new(py, "_duckdb._sqltypes")?;
    child.add_class::<PyType>()?;
    let class = child.getattr("DuckDBPyType")?;
    class.setattr("__getattr__", class.getattr("_lookup_attribute")?)?;
    class.delattr("_lookup_attribute")?;
    macro_rules! add {($($name:ident),*)=>{$(child.add(stringify!($name),Py::new(py,PyType{native:LogicalType::$name})?)?;)*};}
    add!(
        BOOLEAN,
        TINYINT,
        SMALLINT,
        INTEGER,
        BIGINT,
        UTINYINT,
        USMALLINT,
        UINTEGER,
        UBIGINT,
        HUGEINT,
        UHUGEINT,
        FLOAT,
        DOUBLE,
        VARCHAR,
        BLOB,
        BIT,
        DATE,
        TIME,
        TIME_NS,
        TIME_TZ,
        TIMESTAMP,
        TIMESTAMP_S,
        TIMESTAMP_MS,
        TIMESTAMP_NS,
        TIMESTAMP_TZ,
        INTERVAL,
        UUID,
        SQLNULL
    );
    child.add(
        "_BIGNUM",
        Py::new(
            py,
            PyType {
                native: LogicalType::BIGNUM,
            },
        )?,
    )?;
    child.add(
        "_DECIMAL",
        Py::new(
            py,
            PyType {
                native: LogicalType::decimal(0, 0),
            },
        )?,
    )?;
    child.add(
        "VARIANT",
        Py::new(
            py,
            PyType {
                native: LogicalType::variant(),
            },
        )?,
    )?;
    module.add("_sqltypes", &child)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("_duckdb._sqltypes", &child)?;
    Ok(())
}
