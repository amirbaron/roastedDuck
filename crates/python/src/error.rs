//! The frozen Python exception hierarchy and native engine error translation.
use pyo3::exceptions::{PyException, PyRuntimeError, PyUnicodeDecodeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyString};
use roastedduck_common::{Error as NativeError, ErrorKind};
use std::fmt::Write as _;
macro_rules! exception {
    ($name:ident,$base:ty) => {
        pyo3::create_exception!(_duckdb, $name, $base);
    };
}
exception!(Warning, PyException);
exception!(Error, PyException);
exception!(DatabaseError, Error);
exception!(DataError, DatabaseError);
exception!(OperationalError, DatabaseError);
exception!(IntegrityError, DatabaseError);
exception!(InternalError, DatabaseError);
exception!(ProgrammingError, DatabaseError);
exception!(NotSupportedError, DatabaseError);
exception!(FatalException, DatabaseError);
exception!(InterruptException, DatabaseError);
exception!(PermissionException, DatabaseError);
exception!(SequenceException, DatabaseError);
exception!(DependencyException, DatabaseError);
exception!(OutOfRangeException, DataError);
exception!(ConversionException, DataError);
exception!(TypeMismatchException, DataError);
exception!(TransactionException, OperationalError);
exception!(OutOfMemoryException, OperationalError);
exception!(ConnectionException, OperationalError);
exception!(SerializationException, OperationalError);
exception!(IOException, OperationalError);
exception!(HTTPException, IOException);
exception!(ConstraintException, IntegrityError);
exception!(InternalException, InternalError);
exception!(ParserException, ProgrammingError);
exception!(SyntaxException, ProgrammingError);
exception!(BinderException, ProgrammingError);
exception!(InvalidInputException, ProgrammingError);
exception!(InvalidTypeException, ProgrammingError);
exception!(CatalogException, ProgrammingError);
exception!(NotImplementedException, NotSupportedError);

/// Copy the native HTTP response metadata, as DuckDB's `PyThrowException` does.
fn http_attributes(py: Python<'_>, value: &Bound<'_, PyAny>, error: &NativeError) -> PyResult<()> {
    let headers = PyDict::new(py);
    for (key, bytes) in error.extra_info() {
        match key {
            "status_code" => {
                let text = crate::conversion::text(py, bytes)?;
                value.setattr("status_code", py.get_type::<PyInt>().call1((text,))?)?;
            }
            "response_body" => value.setattr("body", crate::conversion::text(py, bytes)?)?,
            "reason" => value.setattr("reason", crate::conversion::text(py, bytes)?)?,
            _ => {
                if let Some(name) = key.strip_prefix("header_") {
                    headers.set_item(name, crate::conversion::text(py, bytes)?)?;
                }
            }
        }
    }
    value.setattr("headers", headers)
}

/// Preserve decoder exception arguments through the engine's owned error channel.
/// Hex keeps the metadata valid even when `errors_as_json` round-trips the error.
pub(super) fn unicode_decode_error(error: &PyErr) -> NativeError {
    Python::attach(|py| -> PyResult<NativeError> {
        let value = error.value(py);
        let bytes = value.getattr("object")?.extract::<Vec<u8>>()?;
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(NativeError::new(ErrorKind::Invalid, error.to_string())
            .with_subtype("python_unicode_decode")
            .with_extra_info("python_unicode_object", hex)
            .with_extra_info(
                "python_unicode_encoding",
                value.getattr("encoding")?.extract::<String>()?,
            )
            .with_extra_info(
                "python_unicode_start",
                value.getattr("start")?.extract::<usize>()?.to_string(),
            )
            .with_extra_info(
                "python_unicode_end",
                value.getattr("end")?.extract::<usize>()?.to_string(),
            )
            .with_extra_info(
                "python_unicode_reason",
                value.getattr("reason")?.extract::<String>()?,
            ))
    })
    .unwrap_or_else(|_| crate::input::python_error(error))
}

pub fn native(py: Python<'_>, error: &NativeError) -> PyErr {
    if error.subtype() == Some("python_unicode_decode") {
        let restore = || -> PyResult<PyErr> {
            let info = error.extra_info();
            let get = |key| -> PyResult<&str> {
                let value = info
                    .iter()
                    .find_map(|(name, value)| (*name == key).then_some(*value))
                    .ok_or_else(|| PyRuntimeError::new_err("Missing decoder exception metadata"))?;
                std::str::from_utf8(value)
                    .map_err(|_| PyRuntimeError::new_err("Invalid decoder exception metadata"))
            };
            let hex = get("python_unicode_object")?;
            let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(PyRuntimeError::new_err("Invalid decoder bytes"));
            }
            let bytes = pairs
                .iter()
                .map(|pair| {
                    let pair = std::str::from_utf8(pair)
                        .map_err(|_| PyRuntimeError::new_err("Invalid decoder bytes"))?;
                    u8::from_str_radix(pair, 16).map_err(|_| PyRuntimeError::new_err("Invalid decoder bytes"))
                })
                .collect::<PyResult<Vec<_>>>()?;
            let number = |key| {
                get(key)?
                    .parse::<usize>()
                    .map_err(|_| PyRuntimeError::new_err("Invalid decoder offset"))
            };
            let value = py.get_type::<PyUnicodeDecodeError>().call1((
                get("python_unicode_encoding")?,
                pyo3::types::PyBytes::new(py, &bytes),
                number("python_unicode_start")?,
                number("python_unicode_end")?,
                get("python_unicode_reason")?,
            ))?;
            Ok(PyErr::from_value(value))
        };
        return restore().unwrap_or_else(|error| error);
    }
    if error.subtype() == Some("python_interrupt") {
        return pyo3::exceptions::PyRuntimeError::new_err("Query interrupted");
    }
    if error.subtype() == Some("python_callback") {
        return match crate::conversion::text(py, error.raw_message_bytes()) {
            Ok(message) => Error::new_err((message,)),
            Err(error) => error,
        };
    }
    let kind = error.kind();
    let mut bytes = error.to_bytes();
    // Native JSON errors omit the display prefix; Python's typed exception
    // constructor retains it before the unchanged JSON object.
    if bytes.first() == Some(&b'{') {
        bytes =
            roastedduck_common::byte_format!("{} Error: {}", kind.display_name(), &bytes[..]).into_bytes();
    }
    let message = match crate::conversion::text(py, &bytes) {
        Ok(message) => message,
        Err(error) => return error,
    };
    let class = match kind {
        ErrorKind::OutOfRange => py.get_type::<OutOfRangeException>(),
        ErrorKind::Conversion => py.get_type::<ConversionException>(),
        ErrorKind::MismatchType => py.get_type::<TypeMismatchException>(),
        ErrorKind::Transaction => py.get_type::<TransactionException>(),
        ErrorKind::NotImplemented => py.get_type::<NotImplementedException>(),
        ErrorKind::Catalog => py.get_type::<CatalogException>(),
        ErrorKind::Parser => py.get_type::<ParserException>(),
        ErrorKind::Constraint => py.get_type::<ConstraintException>(),
        ErrorKind::Connection => py.get_type::<ConnectionException>(),
        ErrorKind::Syntax => py.get_type::<SyntaxException>(),
        ErrorKind::Binder => py.get_type::<BinderException>(),
        ErrorKind::Io => py.get_type::<IOException>(),
        ErrorKind::Interrupt => py.get_type::<InterruptException>(),
        ErrorKind::Fatal => py.get_type::<FatalException>(),
        ErrorKind::Internal => py.get_type::<InternalException>(),
        ErrorKind::InvalidInput => py.get_type::<InvalidInputException>(),
        ErrorKind::InvalidType => py.get_type::<InvalidTypeException>(),
        ErrorKind::OutOfMemory => py.get_type::<OutOfMemoryException>(),
        ErrorKind::Permission => py.get_type::<PermissionException>(),
        ErrorKind::Serialization => py.get_type::<SerializationException>(),
        ErrorKind::Sequence => py.get_type::<SequenceException>(),
        ErrorKind::Dependency => py.get_type::<DependencyException>(),
        ErrorKind::Http => py.get_type::<HTTPException>(),
        _ => py.get_type::<PyRuntimeError>(),
    };
    match class.call1((message,)) {
        Ok(value) => {
            if kind == ErrorKind::Http
                && let Err(error) = http_attributes(py, &value, error)
            {
                return error;
            }
            PyErr::from_value(value)
        }
        Err(error) => error,
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    macro_rules! add {($($name:ident),*)=>{$(module.add(stringify!($name),py.get_type::<$name>())?;)*};}
    add!(
        Warning,
        Error,
        DatabaseError,
        DataError,
        OperationalError,
        IntegrityError,
        InternalError,
        ProgrammingError,
        NotSupportedError,
        FatalException,
        InterruptException,
        PermissionException,
        SequenceException,
        DependencyException,
        OutOfRangeException,
        ConversionException,
        TypeMismatchException,
        TransactionException,
        OutOfMemoryException,
        ConnectionException,
        SerializationException,
        IOException,
        HTTPException,
        ConstraintException,
        InternalException,
        ParserException,
        SyntaxException,
        BinderException,
        InvalidInputException,
        InvalidTypeException,
        CatalogException,
        NotImplementedException
    );
    let http = py.get_type::<HTTPException>();
    let string = py.get_type::<PyString>();
    let headers = py
        .import("typing")?
        .getattr("Dict")?
        .get_item((&string, &string))?;
    let annotations = PyDict::new(py);
    annotations.set_item("status_code", py.get_type::<PyInt>())?;
    annotations.set_item("body", &string)?;
    annotations.set_item("reason", &string)?;
    annotations.set_item("headers", headers)?;
    http.setattr("__annotations__", annotations)?;
    http.setattr(
        "__doc__",
        "Thrown when an error occurs in the httpfs extension, or whilst downloading an extension.",
    )?;
    Ok(())
}

pub(super) fn deprecated(py: Python<'_>, old: &str, replacement: &str) -> PyResult<()> {
    py.import("warnings")?.call_method1(
        "warn",
        (
            format!("{old}() is deprecated, use {replacement}() instead."),
            py.get_type::<pyo3::exceptions::PyDeprecationWarning>(),
        ),
    )?;
    Ok(())
}

/// The frozen binding's call diagnostic, with the actual positional/keyword invocation.
pub(super) fn arguments(
    _py: Python<'_>,
    name: &str,
    signature: &str,
    owner: Option<&Bound<'_, PyAny>>,
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
) -> PyResult<PyErr> {
    let argument_repr = |value: &Bound<'_, PyAny>| {
        value
            .repr()
            .and_then(|value| value.extract::<String>())
            .unwrap_or_else(|_| "<repr raised Error>".to_owned())
    };
    let mut values = Vec::new();
    if let Some(owner) = owner {
        values.push(argument_repr(owner));
    }
    for value in args.iter() {
        values.push(argument_repr(&value));
    }
    let mut message = format!(
        "{name}(): incompatible function arguments. The following argument types are supported:\n    1. {signature}\n\nInvoked with: {}",
        values.join(", ")
    );
    if let Some(kwargs) = kwargs.filter(|v| !v.is_empty()) {
        let mut named = Vec::new();
        for (key, value) in kwargs.iter() {
            named.push(format!("{}={}", key.str()?, argument_repr(&value)));
        }
        message.push_str(if values.is_empty() {
            "kwargs: "
        } else {
            "; kwargs: "
        });
        message.push_str(&named.join(", "));
    }
    Ok(pyo3::exceptions::PyTypeError::new_err(message))
}
