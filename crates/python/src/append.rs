//! Frozen dataframe append: retain the registered scan and insert natively.
use crate::connection::{PyConnection, lock};
use crate::error;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use roastedduck_common::ByteString;

struct Arguments<'py> {
    name: ByteString,
    frame: Bound<'py, PyAny>,
    by_name: bool,
}
fn arguments<'py>(
    py: Python<'py>,
    owner: Option<&Bound<'py, PyConnection>>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Arguments<'py>> {
    let signature = if owner.is_some() {
        "(self: _duckdb.DuckDBPyConnection, table_name: str, df: pandas.DataFrame, *, by_name: bool = False) -> _duckdb.DuckDBPyConnection"
    } else {
        "(table_name: str, df: pandas.DataFrame, *, by_name: bool = False, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection"
    };
    let bad = || error::arguments(py, "append", signature, owner.map(Bound::as_any), args, kwargs);
    if args.len() > 2 {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if !(matches!(key.as_str(), "table_name" | "df" | "by_name")
                || owner.is_none() && key == "connection")
            {
                return Err(bad()?);
            }
        }
    }
    let field = |index, name| -> PyResult<Option<Bound<'py, PyAny>>> {
        let keyword = kwargs.map(|kwargs| kwargs.get_item(name)).transpose()?.flatten();
        if args.len() > index {
            if keyword.is_some() {
                return Err(bad()?);
            }
            args.get_item(index).map(Some)
        } else {
            Ok(keyword)
        }
    };
    let Some(name) = field(0, "table_name")? else {
        return Err(bad()?);
    };
    let name = if let Ok(bytes) = name.cast::<PyBytes>() {
        ByteString::from(bytes.as_bytes())
    } else if let Ok(name) = name.extract::<String>() {
        name.into()
    } else {
        return Err(bad()?);
    };
    let Some(frame) = field(1, "df")? else {
        return Err(bad()?);
    };
    if !crate::pandas::is_dataframe(&frame)? {
        return Err(bad()?);
    }
    let by_name = kwargs
        .map(|kwargs| kwargs.get_item("by_name"))
        .transpose()?
        .flatten();
    let by_name = if let Some(value) = by_name {
        if value.is_none() {
            false
        } else {
            // SAFETY: numeric conversion examines a live Python object under the GIL.
            if unsafe { pyo3::ffi::PyNumber_Check(value.as_ptr()) } == 0 {
                return Err(bad()?);
            }
            value.is_truthy()?
        }
    } else {
        false
    };
    if owner.is_none()
        && let Some(connection) = kwargs
            .map(|kwargs| kwargs.get_item("connection"))
            .transpose()?
            .flatten()
        && !connection.is_none()
        && connection.cast::<PyConnection>().is_err()
    {
        return Err(bad()?);
    }
    Ok(Arguments { name, frame, by_name })
}
fn execute<'py>(
    owner: Bound<'py, PyConnection>,
    args: &Arguments<'py>,
) -> PyResult<Bound<'py, PyConnection>> {
    let py = owner.py();
    // C++ keeps this registration, even if the insert fails. Preserve its snapshot,
    // replacement policy and ownership rather than collecting dataframe rows.
    let registration = PyTuple::new(
        py,
        ["__append_df".into_pyobject(py)?.into_any(), args.frame.clone()],
    )?;
    PyConnection::register(owner.clone(), &registration, None)?;
    let mut columns = ByteString::new();
    if args.by_name {
        columns.push(b'(');
        for (index, name) in args.frame.getattr("columns")?.try_iter()?.enumerate() {
            if index > 0 {
                columns.push_bytes(", ");
            }
            let name = name?.str()?.extract::<String>()?;
            columns.push_bytes(roastedduck_ast::keyword::write_optionally_quoted_bytes(
                name.as_bytes(),
            ));
        }
        columns.push(b')');
    }
    let name = roastedduck_ast::keyword::write_optionally_quoted_bytes(&args.name);
    let sql = roastedduck_common::byte_format!("INSERT INTO {name} {columns} SELECT * FROM __append_df");
    let state = std::sync::Arc::clone(&owner.get().state);
    py.detach(|| lock(&state).execute(&sql, None))
        .map_err(|e| error::native(py, &e))?;
    Ok(owner)
}
pub(super) fn call<'py>(
    owner: Bound<'py, PyConnection>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyConnection>> {
    let values = arguments(owner.py(), Some(&owner), args, kwargs)?;
    execute(owner, &values)
}
#[pyfunction(name="_append", signature=(*args, **kwargs))]
pub(super) fn default<'py>(
    py: Python<'py>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyConnection>> {
    let values = arguments(py, None, args, kwargs)?;
    let owner = kwargs
        .map(|kwargs| kwargs.get_item("connection"))
        .transpose()?
        .flatten()
        .filter(|value| !value.is_none());
    let owner = if let Some(owner) = owner {
        owner.cast_into::<PyConnection>()?
    } else {
        py.import("duckdb")?
            .getattr("default_connection")?
            .call0()?
            .cast_into::<PyConnection>()?
    };
    execute(owner, &values)
}
