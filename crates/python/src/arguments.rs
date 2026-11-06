//! Frozen call binding shared by Python APIs whose strings are native byte strings.
use pyo3::exceptions::PyUnicodeEncodeError;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyString, PyTuple};

#[allow(clippy::too_many_arguments)]
pub(super) fn fields<'py, const N: usize>(
    name: &str,
    signature: &str,
    owner: &Bound<'py, PyAny>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
    names: &[&str; N],
    required: usize,
    positional: usize,
) -> PyResult<[Option<Bound<'py, PyAny>>; N]> {
    let bad = || crate::error::arguments(args.py(), name, signature, Some(owner), args, kwargs);
    if args.len() > positional {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            if !key
                .extract::<String>()
                .is_ok_and(|key| names.contains(&key.as_str()))
            {
                return Err(bad()?);
            }
        }
    }
    let mut values = std::array::from_fn(|_| None);
    for (index, name) in names.iter().enumerate() {
        let keyword = kwargs.map(|kwargs| kwargs.get_item(*name)).transpose()?.flatten();
        if index < args.len() {
            if keyword.is_some() {
                return Err(bad()?);
            }
            values[index] = Some(args.get_item(index)?);
        } else if keyword.is_some() || index >= required {
            values[index] = keyword;
        } else {
            return Err(bad()?);
        }
    }
    Ok(values)
}

pub(super) fn string_bytes(value: &Bound<'_, PyAny>) -> PyResult<Option<Vec<u8>>> {
    match crate::type_creation::string_bytes(value) {
        // The C++ string caster rejects unencodable Unicode as an argument type.
        Err(error) if error.is_instance_of::<PyUnicodeEncodeError>(value.py()) => Ok(None),
        result => result,
    }
}

#[pyfunction]
pub(super) fn _string_argument_valid(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    if let Ok(value) = value.cast::<PyString>() {
        return match value.to_str() {
            Ok(_) => Ok(true),
            Err(error) if error.is_instance_of::<PyUnicodeEncodeError>(value.py()) => Ok(false),
            Err(error) => Err(error),
        };
    }
    Ok(value.is_instance_of::<PyBytes>() || value.is_instance_of::<PyByteArray>())
}
