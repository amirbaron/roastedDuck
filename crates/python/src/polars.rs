//! Polars adapters over native Arrow output and retained native relation sources.
use pyo3::prelude::*;
use pyo3::types::PyDict;

pub(super) fn kind(object: &Bound<'_, PyAny>) -> PyResult<Option<bool>> {
    let Ok(module) = object.py().import("sys")?.getattr("modules")?.get_item("polars") else {
        return Ok(None);
    };
    if object.is_instance(&module.getattr("DataFrame")?)? {
        return Ok(Some(false));
    }
    if object.is_instance(&module.getattr("LazyFrame")?)? {
        return Ok(Some(true));
    }
    Ok(None)
}
pub(super) fn empty_frame<'py>(object: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = object.py();
    let kwargs = PyDict::new(py);
    kwargs.set_item("schema", object.call_method0("collect_schema")?)?;
    py.import("polars")?.getattr("DataFrame")?.call((), Some(&kwargs))
}
pub(super) fn from_arrow(py: Python<'_>, table: Py<PyAny>) -> PyResult<Py<PyAny>> {
    if table.is_none(py) {
        return Ok(py.import("polars")?.getattr("DataFrame")?.call0()?.unbind());
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("rechunk", false)?;
    Ok(py
        .import("polars")?
        .getattr("from_arrow")?
        .call((table,), Some(&kwargs))?
        .unbind())
}

pub(super) fn arguments(
    owner: &Bound<'_, PyAny>,
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    relation: bool,
) -> PyResult<(usize, bool)> {
    let py = owner.py();
    let batch = if relation { "batch_size" } else { "rows_per_batch" };
    let signature = format!(
        "(self: _duckdb.DuckDBPy{}, {batch}: typing.SupportsInt | typing.SupportsIndex = 1000000, *, lazy: bool = False) -> duckdb::PolarsDataFrame",
        if relation { "Relation" } else { "Connection" }
    );
    let bad = || crate::error::arguments(py, "pl", &signature, Some(owner), args, kwargs);
    if args.len() > 1 {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if key != batch && key != "lazy" {
                return Err(bad()?);
            }
        }
    }
    let keyword = kwargs.map(|kw| kw.get_item(batch)).transpose()?.flatten();
    if args.len() == 1 && keyword.is_some() {
        return Err(bad()?);
    }
    let value = if args.len() == 1 {
        Some(args.get_item(0)?)
    } else {
        keyword
    };
    let batch = if let Some(value) = value {
        match value.extract::<usize>() {
            Ok(v) => v,
            Err(_) => return Err(bad()?),
        }
    } else {
        1_000_000
    };
    let value = kwargs.map(|kw| kw.get_item("lazy")).transpose()?.flatten();
    let lazy = if let Some(value) = value {
        if value.is_none() {
            false
        } else {
            // SAFETY: numeric acceptance and truth conversion use the live object under the GIL.
            if unsafe { pyo3::ffi::PyNumber_Check(value.as_ptr()) } == 0 {
                return Err(bad()?);
            }
            value.is_truthy()?
        }
    } else {
        false
    };
    Ok((batch, lazy))
}

/// The frozen lazy connection adapter has a result, without a relation query tree.
#[pyclass(frozen, module = "_duckdb")]
struct ResultSource {
    reader: std::sync::Mutex<Option<Py<PyAny>>>,
}
#[pymethods]
impl ResultSource {
    #[staticmethod]
    fn project(_columns: &Bound<'_, PyAny>) -> Option<Py<PyAny>> {
        None
    }
    #[staticmethod]
    fn filter(py: Python<'_>, _expression: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Err(crate::error::native(
            py,
            &roastedduck_common::Error::internal("Attempted to dereference shared_ptr that is NULL!"),
        ))
    }
    #[staticmethod]
    #[pyo3(signature=(_limit,_offset=0))]
    fn limit(py: Python<'_>, _limit: i64, _offset: i64) -> PyResult<Py<PyAny>> {
        Err(crate::error::native(
            py,
            &roastedduck_common::Error::internal("Attempted to dereference shared_ptr that is NULL!"),
        ))
    }
    #[pyo3(signature=(_batch_size=1_000_000))]
    fn to_arrow_reader(&self, _batch_size: usize) -> Option<Py<PyAny>> {
        crate::connection::lock(&self.reader).take()
    }
}
pub(super) fn from_reader(py: Python<'_>, reader: Py<PyAny>) -> PyResult<Py<PyAny>> {
    let arrow = py.import("pyarrow")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("schema", reader.bind(py).getattr("schema")?)?;
    let empty = arrow.getattr("Table")?.call_method(
        "from_batches",
        (pyo3::types::PyList::empty(py),),
        Some(&kwargs),
    )?;
    let schema = py
        .import("polars")?
        .getattr("DataFrame")?
        .call1((empty,))?
        .getattr("schema")?;
    let source = Py::new(
        py,
        ResultSource {
            reader: std::sync::Mutex::new(Some(reader)),
        },
    )?;
    Ok(py
        .import("duckdb.polars_io")?
        .getattr("duckdb_source")?
        .call1((source, schema))?
        .unbind())
}
