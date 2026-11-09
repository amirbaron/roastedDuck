//! fsspec adapters over native positional file I/O; no file snapshots or SQL rewriting.
use crate::connection::{PyConnection, lock};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::registered_files::{FileHandle, FileSystem, OpenFlags, Registry};
use std::sync::{Arc, Mutex};
#[derive(Debug)]
struct PythonFs {
    name: String,
    protocols: Vec<ByteString>,
    object: Py<PyAny>,
}
#[derive(Debug)]
struct PythonFile {
    object: Mutex<Py<PyAny>>,
    filesystem: Py<PyAny>,
    path: ByteString,
}
fn native<T>(action: impl for<'py> FnOnce(Python<'py>) -> PyResult<T>) -> Result<T> {
    Python::attach(action).map_err(|e| crate::input::python_error(&e))
}
impl Drop for PythonFile {
    fn drop(&mut self) {
        let _ = Python::try_attach(|py| lock(&self.object).bind(py).call_method0("close").map(|_| ()));
    }
}
impl FileHandle for PythonFile {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        let file = lock(&self.object);
        native(|py| {
            let file = file.bind(py);
            file.call_method1("seek", (offset,))?;
            let bytes = file.call_method1("read", (buffer.len(),))?;
            let bytes = bytes.cast::<PyBytes>()?.as_bytes();
            let n = buffer.len().min(bytes.len());
            buffer[..n].copy_from_slice(&bytes[..n]);
            Ok(n)
        })
    }
    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize> {
        let file = lock(&self.object);
        native(|py| {
            let file = file.bind(py);
            file.call_method1("seek", (offset,))?;
            file.call_method1("write", (PyBytes::new(py, buffer),))?.extract()
        })
    }
    fn size(&self) -> Result<u64> {
        native(|py| {
            self.filesystem
                .bind(py)
                .call_method1("size", (crate::conversion::text(py, &self.path)?,))?
                .extract()
        })
    }
    fn modified(&self) -> Result<i64> {
        native(|py| {
            let date = self
                .filesystem
                .bind(py)
                .call_method1("modified", (crate::conversion::text(py, &self.path)?,))?;
            let seconds = date.call_method0("timestamp")?.extract::<f64>()?;
            Ok((seconds as i64).saturating_mul(1_000_000))
        })
    }
    fn flush(&self) -> Result<()> {
        let file = lock(&self.object);
        native(|py| file.bind(py).call_method0("flush").map(|_| ()))
    }
    fn truncate(&self, size: u64) -> Result<()> {
        let file = lock(&self.object);
        native(|py| file.bind(py).call_method1("truncate", (size,)).map(|_| ()))
    }
}
impl FileSystem for PythonFs {
    fn name(&self) -> &str {
        &self.name
    }
    fn handles(&self, path: &[u8]) -> bool {
        self.protocols.iter().any(|protocol| {
            path.strip_prefix(protocol.as_bytes())
                .is_some_and(|rest| rest.starts_with(b"://"))
        })
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        self.open_with_flags(
            path,
            OpenFlags {
                read: !write,
                write,
                create: write,
                truncate: write,
            },
        )
    }
    fn open_with_flags(&self, path: &[u8], flags: OpenFlags) -> Result<Arc<dyn FileHandle>> {
        native(|py| {
            let python_path = crate::conversion::text(py, path)?;
            let fs = self.object.bind(py);
            let mode = if flags.read {
                // Match the Python filesystem adapter's read/read-write modes.
                if flags.write { "rb+" } else { "rb" }
            } else if flags.truncate
                || flags.create && !fs.call_method1("isfile", (&python_path,))?.extract::<bool>()?
            {
                "wb"
            } else {
                // Existing database/WAL contents must survive non-truncating opens.
                "rb+"
            };
            let file = fs.call_method1("open", (python_path, mode))?;
            Ok(Arc::new(PythonFile {
                object: Mutex::new(file.unbind()),
                filesystem: self.object.clone_ref(py),
                path: path.into(),
            }) as Arc<dyn FileHandle>)
        })
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        native(|py| {
            let fs = self.object.bind(py);
            let names = fs.call_method1("glob", (crate::conversion::text(py, path)?,))?;
            names
                .try_iter()?
                .map(|name| {
                    let path = fs
                        .call_method1("unstrip_protocol", (name?,))?
                        .str()?
                        .extract::<String>()?;
                    Ok(path.into())
                })
                .collect()
        })
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        native(|py| {
            self.object
                .bind(py)
                .call_method1("isfile", (crate::conversion::text(py, path)?,))?
                .extract()
        })
    }
    fn is_dir(&self, path: &[u8]) -> Result<bool> {
        native(|py| {
            self.object
                .bind(py)
                .call_method1("isdir", (crate::conversion::text(py, path)?,))?
                .extract()
        })
    }
    fn mkdir(&self, path: &[u8]) -> Result<()> {
        native(|py| {
            let kwargs = pyo3::types::PyDict::new(py);
            kwargs.set_item("exist_ok", true)?;
            self.object.bind(py).call_method(
                "mkdirs",
                (crate::conversion::text(py, path)?,),
                Some(&kwargs),
            )?;
            Ok(())
        })
    }
    fn remove(&self, path: &[u8]) -> Result<()> {
        native(|py| {
            self.object
                .bind(py)
                .call_method1("rm", (crate::conversion::text(py, path)?,))
                .map(|_| ())
        })
    }
    fn rename(&self, source: &[u8], target: &[u8]) -> Result<()> {
        native(|py| {
            self.object
                .bind(py)
                .call_method1(
                    "mv",
                    (
                        crate::conversion::text(py, source)?,
                        crate::conversion::text(py, target)?,
                    ),
                )
                .map(|_| ())
        })
    }
}
fn registry(owner: &PyConnection, py: Python<'_>) -> PyResult<Arc<Registry>> {
    let cache = py
        .detach(|| {
            lock(&owner.state)
                .connection()?
                .function_client()
                .object_cache()
                .ok_or_else(|| Error::internal("Missing object cache"))
        })
        .map_err(|e| crate::error::native(py, &e))?;
    Ok(Registry::initialize(&cache))
}
pub(super) fn register(owner: &Bound<'_, PyConnection>, filesystem: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = owner.py();
    let fsspec = py.import("fsspec")?;
    if !filesystem.is_instance(&fsspec.getattr("AbstractFileSystem")?)? {
        return Err(crate::error::arguments(
            py,
            "register_filesystem",
            "(self: _duckdb.DuckDBPyConnection, filesystem: fsspec.AbstractFileSystem) -> None",
            Some(owner.as_any()),
            &pyo3::types::PyTuple::new(py, [filesystem])?,
            None,
        )?);
    }
    let registry = registry(owner.get(), py)?;
    let protocol = filesystem.getattr("protocol")?;
    if protocol.is_none()
        || protocol.is_instance_of::<PyString>() && protocol.extract::<String>()? == "abstract"
    {
        return Err(crate::error::native(
            py,
            &Error::invalid_input("Must provide concrete fsspec implementation"),
        ));
    }
    let protocols = if protocol.is_instance_of::<PyString>() {
        vec![protocol.extract::<String>()?.into()]
    } else {
        protocol
            .try_iter()?
            .map(|v| v?.str()?.extract::<String>().map(ByteString::from))
            .collect::<PyResult<Vec<_>>>()?
    };
    let name = protocols
        .first()
        .and_then(|p| p.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if name.is_empty() {
        return Err(crate::error::native(
            py,
            &Error::invalid_input("Must provide concrete fsspec implementation"),
        ));
    }
    let fs = Arc::new(PythonFs {
        name,
        protocols,
        object: filesystem.clone().unbind(),
    });
    py.detach(|| registry.register(fs))
        .map_err(|e| crate::error::native(py, &e))
}
pub(super) fn unregister(owner: &PyConnection, py: Python<'_>, name: &str) -> PyResult<()> {
    let registry = registry(owner, py)?;
    py.detach(|| registry.remove(name));
    Ok(())
}
pub(super) fn names(owner: &PyConnection, py: Python<'_>) -> PyResult<Vec<String>> {
    Ok(registry(owner, py)?.names())
}
