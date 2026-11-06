//! Python object boundaries; engine names/queries remain native until Python requires Unicode.
use crate::datatype::PyType;
use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyMemoryView, PyString, PyTuple,
};
use rustdb_function::cast::ValueCast;
use rustdb_types::{Date, Interval, LogicalType, LogicalTypeId as L, Time, Timestamp, Uuid, Value};
pub fn text(py: Python<'_>, bytes: &[u8]) -> PyResult<Py<PyAny>> {
    Ok(PyBytes::new(py, bytes)
        .call_method1("decode", ("utf-8", "strict"))?
        .unbind())
}
fn native_error(py: Python<'_>, error: &rustdb_common::Error) -> PyErr {
    crate::error::native(py, error)
}
fn map_key_hashable(kind: &LogicalType) -> bool {
    match kind.id() {
        L::List | L::Array | L::Map | L::Variant | L::Struct => false,
        L::Union => kind
            .union_members()
            .unwrap_or_default()
            .iter()
            .all(|(_, member)| map_key_hashable(member)),
        _ => true,
    }
}
fn sequence(py: Python<'_>, object: &Bound<'_, PyAny>) -> PyResult<Value> {
    let mut values = Vec::new();
    let mut kind = LogicalType::SQLNULL;
    for object in object.try_iter()? {
        let value = from_python(&object?)?;
        kind = LogicalType::force_max_logical_type(&kind, value.logical_type());
        values.push(value);
    }
    for value in &mut values {
        *value = value.default_cast_as(&kind).map_err(|e| native_error(py, &e))?;
    }
    Ok(Value::list(kind, values))
}
// Python's CastToTarget intentionally resolves duplicate UNION member types.
// SQL CAST retains the engine's ambiguity errors; only Python value boundaries use this.
pub(super) fn cast_target(value: Value, target: &LogicalType) -> rustdb_common::Result<Value> {
    if value.is_null() {
        return Ok(Value::null(target.clone()));
    }
    if value.logical_type() == target {
        return Ok(value);
    }
    if let Some(members) = target.union_members() {
        for (index, (_, kind)) in members.iter().enumerate() {
            if kind == value.logical_type() {
                return Ok(Value::union_value(target.clone(), index as u8, value));
            }
        }
        for (index, (_, kind)) in members.iter().enumerate() {
            if let Ok(candidate) = value.default_cast_as(kind) {
                return Ok(Value::union_value(target.clone(), index as u8, candidate));
            }
        }
        return Err(rustdb_common::Error::conversion(rustdb_common::byte_format!(
            "Could not convert value of type {} to {}",
            value.logical_type().to_bytes(),
            target.to_bytes()
        )));
    }
    if value.logical_type().id() == L::Struct && target.id() == L::Struct {
        let source_fields = value.logical_type().struct_children().unwrap_or_default();
        let values = value.children().unwrap_or_default();
        let mut children = Vec::new();
        for (name, kind) in target.struct_children().unwrap_or_default() {
            let Some(index) = source_fields
                .iter()
                .position(|(field, _)| field.eq_ignore_ascii_case(name))
            else {
                return value.default_cast_as(target);
            };
            let Some(child) = values.get(index) else {
                return value.default_cast_as(target);
            };
            children.push(cast_target(child.clone(), kind)?);
        }
        return Ok(Value::from_children(target.clone(), children));
    }
    value.default_cast_as(target)
}

pub(super) fn to_target(object: &Bound<'_, PyAny>, target: &LogicalType) -> PyResult<Value> {
    let py = object.py();
    if object.is_none() {
        return Ok(Value::null(target.clone()));
    }
    if target.id() == L::Decimal && object.is_instance_of::<PyFloat>() {
        return Err(native_error(
            py,
            &rustdb_common::Error::conversion(rustdb_common::byte_format!(
                "Can't losslessly convert from object of float to type {}",
                target.to_bytes()
            )),
        ));
    }
    if target.id() == L::Bit {
        let bytes = if object.is_instance_of::<PyBytes>() {
            Some(object.extract::<&[u8]>()?.to_vec())
        } else if let Ok(bytes) = object.cast::<PyByteArray>() {
            Some(bytes.to_vec())
        } else {
            None
        };
        if let Some(bytes) = bytes {
            if let Some(&bad) = bytes.iter().find(|&&byte| byte != b'0' && byte != b'1') {
                return Err(native_error(
                    py,
                    &rustdb_common::Error::conversion(rustdb_common::byte_format!(
                        "Invalid character encountered in string -> bit conversion: '{}'",
                        &[bad][..]
                    )),
                ));
            }
            let mut packed = Vec::new();
            rustdb_types::bit::parse_text(&bytes, &mut packed)
                .map_err(|e| native_error(py, &rustdb_common::Error::conversion(e)))?;
            return Ok(Value::bytes(target.clone(), &packed));
        }
    }
    if matches!(target.id(), L::TimestampSec | L::TimestampMs | L::TimestampNs)
        && object.is_instance(&py.import("datetime")?.getattr("datetime")?)?
        && object.getattr("tzinfo")?.is_none()
    {
        return Err(native_error(
            py,
            &rustdb_common::Error::not_implemented(rustdb_common::byte_format!(
                "Conversion from 'datetime' to type {} is not implemented yet",
                target.to_bytes()
            )),
        ));
    }
    if object.is_instance_of::<PyTuple>() && target.id() == L::Struct {
        let fields = target.struct_children().unwrap_or_default();
        if object.len()? != fields.len() {
            return Err(native_error(
                py,
                &rustdb_common::Error::invalid_input(format!(
                    "Tried to create a STRUCT value from a tuple containing {} elements, but the STRUCT consists of {} children",
                    object.len()?,
                    fields.len()
                )),
            ));
        }
        let children = fields
            .iter()
            .enumerate()
            .map(|(i, (_, kind))| to_target(&object.get_item(i)?, kind))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(Value::from_children(target.clone(), children));
    }
    if object.is_instance_of::<PyTuple>() && !matches!(target.id(), L::List | L::Array) {
        return Err(native_error(
            py,
            &rustdb_common::Error::invalid_input(rustdb_common::byte_format!(
                "Can't convert tuple to a Value of type {}",
                target.to_bytes()
            )),
        ));
    }
    if let Ok(dict) = object.cast::<PyDict>() {
        if target.id() == L::Struct {
            let fields = target.struct_children().unwrap_or_default();
            if dict.len() != fields.len() {
                return Err(native_error(
                    py,
                    &rustdb_common::Error::invalid_input(rustdb_common::byte_format!(
                        "We could not convert the object {} to the desired target type ({})",
                        object.str()?.extract::<String>()?.as_bytes(),
                        target.to_bytes()
                    )),
                ));
            }
            let mut children = Vec::new();
            for (name, kind) in fields {
                let mut found = None;
                for (key, value) in dict.iter() {
                    if key
                        .str()?
                        .extract::<String>()?
                        .as_bytes()
                        .eq_ignore_ascii_case(name)
                    {
                        found = Some(value);
                    }
                }
                // C++'s case-insensitive key mapping defaults an absent key to index zero.
                let value = found
                    .or_else(|| dict.iter().next().map(|(_, v)| v))
                    .ok_or_else(|| crate::error::InvalidInputException::new_err("Empty STRUCT value"))?;
                children.push(to_target(&value, kind)?);
            }
            return Ok(Value::from_children(target.clone(), children));
        }
        if let Some((key_type, value_type)) = target.map_key_value() {
            let pairs = if dict.len() == 2 && dict.contains("key")? && dict.contains("value")? {
                let keys = dict
                    .get_item("key")?
                    .ok_or_else(|| crate::error::InternalException::new_err("Missing map keys"))?;
                let values = dict
                    .get_item("value")?
                    .ok_or_else(|| crate::error::InternalException::new_err("Missing map values"))?;
                if keys.is_none() || values.is_none() {
                    return Ok(Value::null(target.clone()));
                }
                if keys.len()? != values.len()? {
                    return Err(crate::error::InvalidInputException::new_err(
                        "Invalid Input Error: Map keys and values must have the same length",
                    ));
                }
                keys.try_iter()?
                    .zip(values.try_iter()?)
                    .map(|(k, v)| Ok((k?, v?)))
                    .collect::<PyResult<Vec<_>>>()?
            } else {
                dict.iter().collect()
            };
            let pairs = pairs
                .into_iter()
                .map(|(key, value)| Ok((to_target(&key, key_type)?, to_target(&value, value_type)?)))
                .collect::<PyResult<Vec<_>>>()?;
            let mut seen = std::collections::HashSet::new();
            for (key, _) in &pairs {
                if key.is_null() {
                    return Err(native_error(
                        py,
                        &rustdb_common::Error::invalid_input("Map keys can not be NULL"),
                    ));
                }
                if !seen.insert(key) {
                    return Err(native_error(
                        py,
                        &rustdb_common::Error::invalid_input("Map keys must be unique"),
                    ));
                }
            }
            return Ok(Value::map(key_type.clone(), value_type.clone(), pairs));
        }
    }
    if object.is_instance_of::<PyList>() || object.is_instance_of::<PyTuple>() {
        let child = target.child_type();
        let mut values = object
            .try_iter()?
            .map(|v| {
                let v = v?;
                child.map_or_else(|| from_python(&v), |child| to_target(&v, child))
            })
            .collect::<PyResult<Vec<_>>>()?;
        // Python's list conversion uses the requested child as a conversion hint,
        // then infers the actual result type/array length from converted elements.
        let kind = values
            .iter()
            .filter(|v| !v.is_null())
            .fold(LogicalType::SQLNULL, |kind, value| {
                LogicalType::force_max_logical_type(&kind, value.logical_type())
            });
        for value in &mut values {
            *value = cast_target(value.clone(), &kind).map_err(|e| native_error(py, &e))?;
        }
        return Ok(if target.id() == L::Array {
            Value::from_children(LogicalType::array(kind, values.len() as u32), values)
        } else {
            Value::list(kind, values)
        });
    }
    cast_target(from_python(object)?, target).map_err(|e| native_error(py, &e))
}

pub fn from_python(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    let py = object.py();
    if object.is_none() {
        return Ok(Value::null(LogicalType::SQLNULL));
    }
    if object.is_instance_of::<PyBool>() {
        return Ok(Value::boolean(object.extract()?));
    }
    if object.is_instance_of::<PyInt>() {
        if let Ok(value) = object.extract::<i64>() {
            return Ok(i32::try_from(value).map_or_else(|_| Value::bigint(value), Value::integer));
        }
        if let Ok(value) = object.extract::<u64>() {
            return Ok(Value::ubigint(value));
        }
        if let Ok(value) = object.extract::<i128>() {
            return Ok(Value::hugeint(value));
        }
        if let Ok(value) = object.extract::<u128>() {
            return Ok(Value::uhugeint(value));
        }
        return Err(crate::error::InvalidInputException::new_err(format!(
            "Invalid Input Error: Python integer too large for 128-bit integer type: {}",
            object.str()?
        )));
    }
    if object.is_instance_of::<PyFloat>() {
        return Ok(Value::double(object.extract()?));
    }
    if object.is_instance_of::<PyString>() {
        return Ok(Value::varchar(object.extract::<String>()?));
    }
    if object.is_instance_of::<PyBytes>() {
        return Ok(Value::blob(object.extract::<&[u8]>()?));
    }
    if let Ok(bytes) = object.cast::<PyByteArray>() {
        return Ok(Value::blob(&bytes.to_vec()));
    }
    if object.is_instance_of::<PyMemoryView>() {
        // Honor the exported shape/strides instead of reading beyond a reversed
        // view's origin as a contiguous raw Py_buffer would do.
        let bytes = object.call_method0("tobytes")?;
        return Ok(Value::blob(bytes.extract::<&[u8]>()?));
    }
    if let Ok(dict) = object.cast::<PyDict>() {
        let mut fields = Vec::new();
        let mut values = Vec::new();
        for (name, value) in dict.iter() {
            let name = name.str()?.to_str()?.to_owned();
            let value = from_python(&value)?;
            fields.push((name.into(), value.logical_type().clone()));
            values.push(value);
        }
        return Ok(Value::from_children(LogicalType::struct_type(fields), values));
    }
    if object.is_instance_of::<PyList>() || object.is_instance_of::<PyTuple>() {
        return sequence(py, object);
    }
    if let (Ok(value), Ok(kind)) = (object.getattr("object"), object.getattr("type")) {
        let kind = PyType {
            native: crate::datatype::from_object(&kind)?,
        };
        if value.is_none() || value.is_instance_of::<PyBool>() {
            return from_python(&value);
        }
        if value.is_instance_of::<PyFloat>() && value.extract::<f64>()?.is_nan() {
            return Ok(Value::null(LogicalType::SQLNULL));
        }
        let datetime = py.import("datetime")?;
        if value.is_instance(&datetime.getattr("datetime")?)? {
            if !value.getattr("tzinfo")?.is_none() || kind.native.id() == L::Timestamp {
                return from_python(&value);
            }
            if !matches!(
                kind.native.id(),
                L::TimestampSec | L::TimestampMs | L::TimestampNs
            ) {
                return Err(native_error(
                    py,
                    &rustdb_common::Error::conversion(rustdb_common::byte_format!(
                        "Could not convert 'datetime' to type {}",
                        kind.native.to_bytes()
                    )),
                ));
            }
        } else if value.is_instance(&py.import("decimal")?.getattr("Decimal")?)?
            || value.is_instance(&datetime.getattr("date")?)?
            || value.is_instance(&datetime.getattr("time")?)?
            || value.is_instance(&datetime.getattr("timedelta")?)?
            || value.is_instance(&py.import("uuid")?.getattr("UUID")?)?
        {
            return from_python(&value);
        }
        return to_target(&value, &kind.native);
    }
    let datetime = py.import("datetime")?;
    if object.is_instance(&datetime.getattr("datetime")?)? {
        let string = object.call_method0("isoformat")?.extract::<String>()?;
        let aware = !object.getattr("tzinfo")?.is_none();
        let (time, _) = Timestamp::parse(string.as_bytes(), aware)
            .map_err(|e| crate::error::ConversionException::new_err(e.message(string.as_bytes())))?;
        return Ok(if aware {
            Value::timestamp_tz(time)
        } else {
            Value::timestamp(time)
        });
    }
    if object.is_instance(&datetime.getattr("date")?)? {
        let string = object.call_method0("isoformat")?.extract::<String>()?;
        return Date::parse(string.as_bytes(), true)
            .map(Value::date)
            .map_err(|e| crate::error::ConversionException::new_err(e.message(string.as_bytes())));
    }
    if object.is_instance(&datetime.getattr("time")?)? {
        let micros = object.getattr("hour")?.extract::<i64>()? * 3_600_000_000
            + object.getattr("minute")?.extract::<i64>()? * 60_000_000
            + object.getattr("second")?.extract::<i64>()? * 1_000_000
            + object.getattr("microsecond")?.extract::<i64>()?;
        let offset = object.call_method0("utcoffset")?;
        if !offset.is_none() {
            let seconds = offset.getattr("days")?.extract::<i32>()? * 86400
                + offset.getattr("seconds")?.extract::<i32>()?;
            return Ok(Value::time_tz(rustdb_types::TimeTz::new(Time(micros), seconds)));
        }
        return Ok(Value::time(Time(micros)));
    }
    if object.is_instance(&datetime.getattr("timedelta")?)? {
        return Ok(Value::interval(Interval {
            months: 0,
            days: object.getattr("days")?.extract()?,
            micros: object.getattr("seconds")?.extract::<i64>()? * 1_000_000
                + object.getattr("microseconds")?.extract::<i64>()?,
        }));
    }
    if object.is_instance(&py.import("decimal")?.getattr("Decimal")?)? {
        return crate::decimal::to_value(object);
    }
    if object.is_instance(&py.import("uuid")?.getattr("UUID")?)? {
        return Ok(Value::uuid(
            Uuid::parse(object.str()?.to_str()?.as_bytes(), true)
                .ok_or_else(|| crate::error::ConversionException::new_err("Invalid UUID"))?,
        ));
    }
    let module = object.get_type().getattr("__module__")?.extract::<String>()?;
    if module.starts_with("numpy") && object.hasattr("item")? {
        if object
            .getattr("ndim")
            .and_then(|value| value.extract::<usize>())
            .unwrap_or(0)
            > 0
        {
            return from_python(&object.call_method0("tolist")?);
        }
        return from_python(&object.call_method0("item")?);
    }
    Err(crate::error::NotImplementedException::new_err(format!(
        "Not implemented Error: Unable to transform python value of type '{}' to DuckDB LogicalType",
        object.get_type().str()?
    )))
}
pub fn to_python(py: Python<'_>, value: &Value, timezone: &str) -> PyResult<Py<PyAny>> {
    if value.is_null() {
        return Ok(py.None());
    }
    let kind = value.logical_type();
    match kind.id() {
        L::Boolean => value.as_bool().into_py_any(py),
        L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::HugeInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt => value.as_i128().into_py_any(py),
        L::UHugeInt => value.as_u128().into_py_any(py),
        L::Float | L::Double => value.as_f64().into_py_any(py),
        L::Decimal => Ok(py
            .import("decimal")?
            .getattr("Decimal")?
            .call1((value.to_varchar().map_err(|e| native_error(py, &e))?,))?
            .unbind()),
        L::Varchar => text(py, value.as_bytes().unwrap_or_default()),
        L::Blob | L::Geometry => Ok(PyBytes::new(py, value.as_bytes().unwrap_or_default())
            .unbind()
            .into_any()),
        L::Bit => value
            .to_varchar()
            .map_err(|e| native_error(py, &e))?
            .into_py_any(py),
        L::Enum => {
            let index = value.as_i128().unwrap_or_default() as usize;
            let name = kind
                .enum_values()
                .and_then(|v| v.get(index))
                .ok_or_else(|| crate::error::InternalException::new_err("Invalid ENUM value"))?;
            text(py, name)
        }
        L::List | L::Array => {
            let values = value
                .children()
                .unwrap_or_default()
                .iter()
                .map(|v| to_python(py, v, timezone))
                .collect::<PyResult<Vec<_>>>()?;
            if kind.id() == L::Array {
                Ok(PyTuple::new(py, values)?.unbind().into_any())
            } else {
                Ok(PyList::new(py, values)?.unbind().into_any())
            }
        }
        L::Struct => {
            if kind.is_unnamed_struct() {
                let values = value
                    .children()
                    .unwrap_or_default()
                    .iter()
                    .map(|v| to_python(py, v, timezone))
                    .collect::<PyResult<Vec<_>>>()?;
                return Ok(PyTuple::new(py, values)?.unbind().into_any());
            }
            let result = PyDict::new(py);
            for ((name, _), value) in kind
                .struct_children()
                .unwrap_or_default()
                .iter()
                .zip(value.children().unwrap_or_default())
            {
                result.set_item(text(py, name)?, to_python(py, value, timezone)?)?;
            }
            Ok(result.unbind().into_any())
        }
        L::Map => {
            let result = PyDict::new(py);
            let hashable = kind.map_key_value().is_some_and(|(key, _)| map_key_hashable(key));
            let keys = PyList::empty(py);
            let values = PyList::empty(py);
            for pair in value.children().unwrap_or_default() {
                let pair = pair.children().unwrap_or_default();
                if pair.len() == 2 {
                    let key = to_python(py, &pair[0], timezone)?;
                    let value = to_python(py, &pair[1], timezone)?;
                    if hashable {
                        result.set_item(key, value)?;
                    } else {
                        keys.append(key)?;
                        values.append(value)?;
                    }
                }
            }
            if !hashable {
                result.set_item("key", keys)?;
                result.set_item("value", values)?;
            }
            Ok(result.unbind().into_any())
        }
        L::Union => {
            let fields = value.children().unwrap_or_default();
            let index = fields.first().and_then(Value::as_i128).unwrap_or_default() as usize + 1;
            fields
                .get(index)
                .ok_or_else(|| crate::error::InternalException::new_err("Invalid UNION tag"))
                .and_then(|v| to_python(py, v, timezone))
        }
        L::Uuid => Ok(py
            .import("uuid")?
            .getattr("UUID")?
            .call1((value.to_varchar().map_err(|e| native_error(py, &e))?,))?
            .unbind()),
        L::Date => {
            let date = value
                .as_date()
                .ok_or_else(|| crate::error::InternalException::new_err("Invalid DATE"))?;
            let class = py.import("datetime")?.getattr("date")?;
            if !date.is_finite() {
                return Ok(class.getattr(if date.0 > 0 { "max" } else { "min" })?.unbind());
            }
            let (y, m, d) = date.to_ymd();
            if !(1..=9999).contains(&y) {
                return value
                    .to_varchar()
                    .map_err(|e| native_error(py, &e))?
                    .into_py_any(py);
            }
            Ok(class.call1((y, m, d))?.unbind())
        }
        L::TimeTz => {
            let raw = value
                .as_i64()
                .ok_or_else(|| crate::error::InternalException::new_err("Invalid TIME WITH TIME ZONE"))?;
            let time = rustdb_types::TimeTz(raw as u64);
            let (h, m, s, micros) = time.time().to_hms_micros();
            let datetime = py.import("datetime")?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("seconds", time.offset())?;
            let offset = datetime.getattr("timedelta")?.call((), Some(&kwargs))?;
            let zone = datetime.getattr("timezone")?.call1((offset,))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("tzinfo", zone)?;
            Ok(datetime
                .getattr("time")?
                .call((h, m, s, micros), Some(&kwargs))?
                .unbind())
        }
        L::Time | L::TimeNs => {
            let mut micros = value.as_i64().unwrap_or_default();
            if kind.id() == L::TimeNs {
                micros /= 1000;
            }
            Ok(py
                .import("datetime")?
                .getattr("time")?
                .call1((
                    micros / 3_600_000_000,
                    (micros / 60_000_000) % 60,
                    (micros / 1_000_000) % 60,
                    micros % 1_000_000,
                ))?
                .unbind())
        }
        L::Timestamp | L::TimestampMs | L::TimestampNs | L::TimestampSec | L::TimestampTz => {
            let raw = value.as_i64().unwrap_or_default();
            let micros = match kind.id() {
                L::TimestampMs => raw.saturating_mul(1000),
                L::TimestampNs => raw / 1000,
                L::TimestampSec => raw.saturating_mul(1_000_000),
                _ => raw,
            };
            let datetime = py.import("datetime")?;
            let class = datetime.getattr("datetime")?;
            if !Timestamp::is_finite_raw(raw) {
                return Ok(class.getattr(if raw > 0 { "max" } else { "min" })?.unbind());
            }
            let timestamp = Timestamp(micros);
            let (y, m, d) = timestamp.to_date().to_ymd();
            if !(1..=9999).contains(&y) {
                return value
                    .to_varchar()
                    .map_err(|e| native_error(py, &e))?
                    .into_py_any(py);
            }
            let t = timestamp.to_time().0;
            let result = class.call1((
                y,
                m,
                d,
                t / 3_600_000_000,
                (t / 60_000_000) % 60,
                (t / 1_000_000) % 60,
                t % 1_000_000,
            ))?;
            if kind.id() == L::TimestampTz {
                let kwargs = PyDict::new(py);
                kwargs.set_item("tzinfo", datetime.getattr("timezone")?.getattr("utc")?)?;
                let result = result.call_method("replace", (), Some(&kwargs))?;
                let zone = py.import("pytz")?.getattr("timezone")?.call1((timezone,))?;
                return Ok(result.call_method1("astimezone", (zone,))?.unbind());
            }
            Ok(result.unbind())
        }
        L::Interval => {
            let interval = value
                .as_interval()
                .ok_or_else(|| crate::error::InternalException::new_err("Invalid INTERVAL"))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("days", i64::from(interval.days) + i64::from(interval.months) * 30)?;
            kwargs.set_item("microseconds", interval.micros)?;
            Ok(py
                .import("datetime")?
                .getattr("timedelta")?
                .call((), Some(&kwargs))?
                .unbind())
        }
        L::Bignum => value
            .to_varchar()
            .map_err(|e| native_error(py, &e))?
            .into_py_any(py),
        L::Variant => {
            let decoded = rustdb_types::variant::VariantData::from_value(value)
                .and_then(|value| value.map(|value| value.to_sql_value(0)).transpose())
                .map_err(|e| native_error(py, &e))?;
            if let Some(decoded) = decoded {
                to_python(py, &decoded, timezone)
            } else {
                Ok(py.None())
            }
        }
        _ => Err(native_error(
            py,
            &rustdb_common::Error::not_implemented(rustdb_common::byte_format!(
                "Unsupported type: \"{}\"",
                kind.to_bytes()
            )),
        )),
    }
}
