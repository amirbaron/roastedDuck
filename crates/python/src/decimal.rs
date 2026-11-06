//! Frozen `PyDecimal` value conversion and its distinct Pandas type-inference path.
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyByteArray, PyBytes, PyFloat, PyInt, PyString};
use rustdb_common::Error;
use rustdb_function::cast::ValueCast;
use rustdb_types::{LogicalType, Value};

#[derive(Debug)]
enum Exponent {
    Scale(i32),
    Power(i32),
    Nan,
    Infinity,
}

#[derive(Debug)]
struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    exponent: Exponent,
}

fn cast_error(object: &Bound<'_, PyAny>) -> PyErr {
    object.get_type().repr().and_then(|class| class.extract::<String>()).map_or_else(
        |error| error,
        |class| PyRuntimeError::new_err(format!(
            "Unable to cast Python instance of type {class} to C++ type '?' (#define PYBIND11_DETAILED_ERROR_MESSAGES or compile in debug mode for details)"
        )),
    )
}

impl Decimal {
    fn parse(object: &Bound<'_, PyAny>) -> PyResult<Self> {
        let tuple = object.call_method0("as_tuple")?;
        let exponent = tuple.getattr("exponent")?;
        let exponent = if exponent.is_instance_of::<PyInt>() {
            let value = exponent.extract::<i32>().map_err(|_| cast_error(&exponent))?;
            if value >= 0 {
                Exponent::Power(value)
            } else {
                Exponent::Scale(value.wrapping_neg())
            }
        } else {
            match exponent.extract::<String>().ok().as_deref() {
                Some("n") => Exponent::Nan,
                Some("F") => Exponent::Infinity,
                _ => {
                    return Err(crate::error::native(
                        object.py(),
                        &Error::not_implemented(
                            "Failed to convert decimal.Decimal value, exponent type is unknown",
                        ),
                    ));
                }
            }
        };
        let negative = tuple.getattr("sign")?.extract::<i8>()? != 0;
        let digits = tuple
            .getattr("digits")?
            .try_iter()?
            .map(|digit| digit?.extract::<u8>())
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            negative,
            digits,
            exponent,
        })
    }

    /// `PyDecimal::TryGetType`: inference retains the full signed32bit exponent.
    fn inferred_type(&self) -> LogicalType {
        let mut width = self.digits.len() as i32;
        let scale = match self.exponent {
            Exponent::Nan | Exponent::Infinity => return LogicalType::FLOAT,
            Exponent::Scale(scale) => scale,
            Exponent::Power(scale) => {
                width = width.wrapping_add(scale);
                scale
            }
        };
        if scale > width {
            width = scale.wrapping_add(1);
        }
        if width > 38 {
            LogicalType::DOUBLE
        } else {
            LogicalType::decimal(width as u8, scale as u8)
        }
    }

    fn to_value(&self, object: &Bound<'_, PyAny>) -> PyResult<Value> {
        let mut width = self.digits.len();
        if width > 38 {
            return double(object);
        }
        let (scale, power) = match self.exponent {
            Exponent::Nan => return Ok(Value::float(f32::NAN)),
            Exponent::Infinity => return Ok(Value::float(f32::INFINITY)),
            Exponent::Scale(scale) => (scale as u8, false),
            Exponent::Power(scale) => (scale as u8, true),
        };
        if power {
            width += usize::from(scale);
        } else if usize::from(scale) > width {
            width = usize::from(scale) + 1;
        }
        if width > 38 {
            return double(object);
        }
        let mut coefficient = self.digits.iter().fold(0_i128, |value, &digit| {
            value.wrapping_mul(10).wrapping_add(i128::from(digit))
        });
        if power {
            // The frozen client caches19signed64bit powers, caps the first
            // lookup at18, then multiplies only while power >19.
            let mut multiplier = 10_i64.pow(u32::from(scale.min(18)));
            for _ in 19..scale {
                multiplier = multiplier.wrapping_mul(10);
            }
            coefficient = coefficient.wrapping_mul(i128::from(multiplier));
        }
        if self.negative {
            coefficient = coefficient.wrapping_neg();
        }
        Ok(Value::decimal(coefficient, width as u8, scale))
    }
}

fn double(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    let text = object.str()?;
    Value::varchar(text.to_str()?)
        .default_cast_as(&LogicalType::DOUBLE)
        .map_err(|error| crate::error::native(object.py(), &error))
}

pub(super) fn to_value(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    Decimal::parse(object)?.to_value(object)
}

pub(super) fn inferred_type(object: &Bound<'_, PyAny>) -> PyResult<Option<LogicalType>> {
    if object.is_instance_of::<PyInt>()
        || object.is_instance_of::<PyBool>()
        || object.is_instance_of::<PyFloat>()
        || object.is_instance_of::<PyString>()
        || object.is_instance_of::<PyBytes>()
        || object.is_instance_of::<PyByteArray>()
    {
        return Ok(None);
    }
    if object.is_instance(&object.py().import("decimal")?.getattr("Decimal")?)? {
        Ok(Some(Decimal::parse(object)?.inferred_type()))
    } else {
        Ok(None)
    }
}
