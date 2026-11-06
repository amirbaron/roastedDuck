//! Frozen relational expression generation; execution stays in the native planner.
use crate::error;
use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt, PyList};
use rustdb_ast::ParsedExpressionKind;
use rustdb_common::{Error, ErrorKind};
#[allow(clippy::too_many_arguments)]
pub(super) fn expressions(
    py: Python<'_>,
    function: &str,
    columns: &str,
    parameters: &str,
    projected: &str,
    window: &str,
    ignore_nulls: bool,
) -> PyResult<String> {
    let columns = if function == "count" && columns.is_empty() {
        "*"
    } else {
        columns
    };
    let input = columns.split(',').filter(|c| !c.is_empty()).collect::<Vec<_>>();
    let suffix = if ignore_nulls { " ignore nulls) " } else { ") " };
    let mut result = if projected.is_empty() {
        String::new()
    } else {
        format!("{projected}, ")
    };
    if input.is_empty() && !parameters.is_empty() {
        return Ok(result + function + "(" + parameters + suffix + window);
    }
    for (index, column) in input.iter().enumerate() {
        let column = column.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
        let parsed = match crate::expression::parse_list(py, column) {
            Ok(parsed) => parsed,
            Err(e) if e.is_instance_of::<error::ParserException>(py) => {
                let quoted = rustdb_ast::keyword::write_quoted(column, '"');
                let parsed = crate::expression::parse_list(py, &quoted)?;
                if parsed.len() != 1 || !matches!(parsed[0].kind, ParsedExpressionKind::ColumnRef(_)) {
                    return Err(error::native(
                        py,
                        &Error::parser(format!("Invalid column expression: {column}")),
                    ));
                }
                parsed
            }
            Err(e) => return Err(e),
        };
        if parsed.len() != 1 {
            return Err(error::native(
                py,
                &Error::parser(format!("Invalid column expression: {column}")),
            ));
        }
        let rendered = String::from_utf8(parsed[0].to_sql_bytes().into_bytes())
            .map_err(|_| pyo3::exceptions::PyUnicodeError::new_err("Invalid UTF-8 in expression"))?;
        result.push_str(function);
        result.push('(');
        result.push_str(&rendered);
        if !parameters.is_empty() {
            result.push(',');
            result.push_str(parameters);
        }
        result.push_str(suffix);
        result.push_str(window);
        if index + 1 < input.len() {
            result.push(',');
        }
    }
    Ok(result)
}
pub(super) fn quantile(py: Python<'_>, q: &Bound<'_, PyAny>) -> PyResult<String> {
    if q.is_instance_of::<PyFloat>() {
        return Ok(format!("{:.6}", q.extract::<f32>()?));
    }
    if let Ok(list) = q.cast::<PyList>() {
        return Ok(format!(
            "[{}]",
            list.iter()
                .map(|q| q.extract::<f64>().map(|v| format!("{v:.6}")))
                .collect::<PyResult<Vec<_>>>()?
                .join(",")
        ));
    }
    Err(error::native(
        py,
        &Error::new(ErrorKind::InvalidType, "Unsupported type for quantile"),
    ))
}
pub(super) fn bitstring(
    py: Python<'_>,
    min: Option<&Bound<'_, PyAny>>,
    max: Option<&Bound<'_, PyAny>>,
) -> PyResult<String> {
    let min = min.filter(|v| !v.is_none());
    let max = max.filter(|v| !v.is_none());
    match (min, max) {
        (None, None) => Ok(String::new()),
        (Some(min), Some(max)) => {
            if !min.is_instance_of::<PyInt>() || !max.is_instance_of::<PyInt>() {
                return Err(error::native(
                    py,
                    &Error::new(ErrorKind::InvalidType, "min and max must be of type int"),
                ));
            }
            Ok(format!("{},{}", min.extract::<i32>()?, max.extract::<i32>()?))
        }
        _ => Err(error::native(
            py,
            &Error::invalid_input("Both min and max values must be set"),
        )),
    }
}
