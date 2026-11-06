//! Native table filters translated to real Arrow/Polars producer predicates.
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use rustdb_arrow::{ArrowStreamParameters, TableFilter};
use rustdb_types::compare::CompareOp;
use rustdb_types::{LogicalTypeId as L, Value};

fn arrow_scalar<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    let arrow = py.import("pyarrow")?;
    let kind = value.logical_type();
    let ty = match kind.id() {
        L::Date => Some(arrow.getattr("date32")?.call0()?),
        L::Time => Some(arrow.getattr("time64")?.call1(("us",))?),
        L::Timestamp => Some(arrow.getattr("timestamp")?.call1(("us",))?),
        L::TimestampMs => Some(arrow.getattr("timestamp")?.call1(("ms",))?),
        L::TimestampNs => Some(arrow.getattr("timestamp")?.call1(("ns",))?),
        L::TimestampSec => Some(arrow.getattr("timestamp")?.call1(("s",))?),
        L::TimestampTz => {
            let kwargs = PyDict::new(py);
            kwargs.set_item("tz", "UTC")?;
            Some(arrow.getattr("timestamp")?.call(("us",), Some(&kwargs))?)
        }
        L::Decimal => {
            let (width, scale) = kind
                .decimal_properties()
                .ok_or_else(|| error::InternalException::new_err("Invalid decimal filter"))?;
            Some(arrow.getattr("decimal128")?.call1((width, scale))?)
        }
        L::UTinyInt => Some(arrow.getattr("uint8")?.call0()?),
        L::USmallInt => Some(arrow.getattr("uint16")?.call0()?),
        L::UInteger => Some(arrow.getattr("uint32")?.call0()?),
        L::UBigInt => Some(arrow.getattr("uint64")?.call0()?),
        L::TimeNs => {
            return Err(error::NotImplementedException::new_err(
                "Not implemented Error: Unimplemented type \"TIME_NS\" for Arrow Filter Pushdown",
            ));
        }
        _ => None,
    };
    let object = if matches!(
        kind.id(),
        L::Date | L::Time | L::Timestamp | L::TimestampMs | L::TimestampNs | L::TimestampSec | L::TimestampTz
    ) {
        if let Some(value) = value.as_i64().or_else(|| value.as_i32().map(i64::from)) {
            value.into_pyobject(py)?.into_any()
        } else {
            conversion::to_python(py, value, "UTC")?.into_bound(py)
        }
    } else {
        conversion::to_python(py, value, "UTC")?.into_bound(py)
    };
    let scalar = if let Some(ty) = ty {
        arrow.getattr("scalar")?.call1((object, ty))?
    } else {
        arrow.getattr("scalar")?.call1((object,))?
    };
    py.import("pyarrow.dataset")?.getattr("scalar")?.call1((scalar,))
}
fn field<'py>(py: Python<'py>, path: &[String], polars: bool) -> PyResult<Bound<'py, PyAny>> {
    if polars {
        let mut field = py.import("polars")?.getattr("col")?.call1((&path[0],))?;
        for name in &path[1..] {
            field = field.getattr("struct")?.call_method1("field", (name,))?;
        }
        Ok(field)
    } else {
        py.import("pyarrow.dataset")?
            .getattr("field")?
            .call1((PyTuple::new(py, path)?,))
    }
}
fn expression<'py>(
    py: Python<'py>,
    filter: &TableFilter,
    path: &mut Vec<String>,
    polars: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let result = match filter {
        TableFilter::ConstantComparison { op, constant } => {
            let field = field(py, path, polars)?;
            if matches!(constant.logical_type().id(), L::Float | L::Double)
                && constant.as_f64().is_some_and(f64::is_nan)
            {
                match op {
                    CompareOp::Equal | CompareOp::GreaterThanOrEqual => field.call_method0("is_nan")?,
                    CompareOp::NotEqual | CompareOp::LessThan => {
                        field.call_method0("is_nan")?.call_method0("__invert__")?
                    }
                    CompareOp::GreaterThan | CompareOp::LessThanOrEqual => py
                        .import(if polars { "polars" } else { "pyarrow.dataset" })?
                        .getattr(if polars { "lit" } else { "scalar" })?
                        .call1((*op == CompareOp::LessThanOrEqual,))?,
                }
            } else {
                let scalar = if polars {
                    conversion::to_python(py, constant, "UTC")?.into_bound(py)
                } else {
                    arrow_scalar(py, constant)?
                };
                let op = match op {
                    CompareOp::Equal => "__eq__",
                    CompareOp::NotEqual => "__ne__",
                    CompareOp::LessThan => "__lt__",
                    CompareOp::LessThanOrEqual => "__le__",
                    CompareOp::GreaterThan => "__gt__",
                    CompareOp::GreaterThanOrEqual => "__ge__",
                };
                field.call_method1(op, (scalar,))?
            }
        }
        TableFilter::IsNull => field(py, path, polars)?.call_method0("is_null")?,
        TableFilter::IsNotNull => {
            field(py, path, polars)?.call_method0(if polars { "is_not_null" } else { "is_valid" })?
        }
        TableFilter::And(children) | TableFilter::Or(children) => {
            let is_or = matches!(filter, TableFilter::Or(_));
            let mut result: Option<Bound<'py, PyAny>> = None;
            for child in children {
                let next = expression(py, child, path, polars)?;
                let Some(next) = next else {
                    if is_or {
                        return Ok(None);
                    }
                    continue;
                };
                result = Some(if let Some(result) = result {
                    result.call_method1(if is_or { "__or__" } else { "__and__" }, (next,))?
                } else {
                    next
                });
            }
            return Ok(result);
        }
        TableFilter::StructExtract {
            child_name,
            child_filter,
            ..
        } => {
            path.push(conversion::text(py, child_name)?.bind(py).extract()?);
            let result = expression(py, child_filter, path, polars);
            path.pop();
            return result;
        }
        TableFilter::Optional(child) => {
            return child
                .as_deref()
                .map(|child| expression(py, child, path, polars))
                .transpose()
                .map(Option::flatten);
        }
        TableFilter::SelectivityOptional(child) => return expression(py, &child.child, path, polars),
        TableFilter::In(values) => {
            let list = PyList::empty(py);
            for value in values {
                list.append(conversion::to_python(py, value, "UTC")?)?;
            }
            field(py, path, polars)?.call_method1(if polars { "is_in" } else { "isin" }, (list,))?
        }
        TableFilter::Dynamic(_) => return Ok(None),
        TableFilter::Expression(_) => {
            return Err(error::NotImplementedException::new_err(
                "Expression cannot be pushed into Arrow",
            ));
        }
    };
    Ok(Some(result))
}
pub(super) fn predicate<'py>(
    py: Python<'py>,
    parameters: &ArrowStreamParameters,
    polars: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let mut result: Option<Bound<'py, PyAny>> = None;
    if let Some(filters) = &parameters.filters {
        for (&column, filter) in &filters.filters {
            let name = parameters
                .projected_columns
                .projection_map
                .get(&column)
                .ok_or_else(|| error::InternalException::new_err("Missing projected Arrow filter column"))?;
            let mut path = vec![conversion::text(py, name)?.bind(py).extract()?];
            if let Some(next) = expression(py, filter, &mut path, polars)? {
                result = Some(if let Some(result) = result {
                    result.call_method1("__and__", (next,))?
                } else {
                    next
                });
            }
        }
    }
    Ok(result)
}
