//! Bounded native-chunk conversion into owned, writable `NumPy` columns and Pandas frames.
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PySlice, PyTuple};
use roastedduck::DataChunk;
use roastedduck_common::{ByteString, Error, byte_format};
use roastedduck_types::{LogicalType, LogicalTypeId as L, Value};
use std::collections::HashMap;

/// Tensor clients use the same owned `NumPy` columns as the frozen client. Fetch
/// precedes importing the optional library, including its consumption semantics.
pub(super) fn tensors(py: Python<'_>, columns: Py<PyAny>, backend: &str) -> PyResult<Py<PyAny>> {
    let converter = if backend == "torch" {
        "from_numpy"
    } else {
        "convert_to_tensor"
    };
    let convert = py.import(backend)?.getattr(converter)?;
    let result = columns.bind(py).cast::<PyDict>()?;
    let entries = result
        .iter()
        .map(|(key, value)| (key.unbind(), value.unbind()))
        .collect::<Vec<_>>();
    for (key, value) in entries {
        result.set_item(key, convert.call1((value,))?)?;
    }
    Ok(columns)
}

pub(super) fn tensor_module(
    name: &str,
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let py = args.py();
    let signature = "(*, connection: duckdb.DuckDBPyConnection = None) -> dict";
    let bad = || crate::error::arguments(py, name, signature, None, args, kwargs);
    if !args.is_empty()
        || kwargs.is_some_and(|kwargs| {
            kwargs
                .iter()
                .any(|(key, _)| key.extract::<String>().is_ok_and(|key| key != "connection"))
        })
    {
        return Err(bad()?);
    }
    let connection = kwargs
        .map(|kwargs| kwargs.get_item("connection"))
        .transpose()?
        .flatten();
    let owner = if let Some(owner) = connection.filter(|owner| !owner.is_none()) {
        if !owner.is_instance_of::<crate::connection::PyConnection>() {
            return Err(bad()?);
        }
        owner
    } else {
        py.import("duckdb")?.call_method0("default_connection")?
    };
    Ok(owner.call_method0(name)?.unbind())
}

pub(super) struct Metadata {
    pub names: Vec<ByteString>,
    pub types: Vec<LogicalType>,
    pub timezone: String,
    pub consumed: bool,
    pub materialized: bool,
    pub materialized_empty: bool,
    pub arrow: roastedduck_arrow::ClientProperties,
}

fn dtype(kind: &LogicalType) -> roastedduck_common::Result<&'static str> {
    Ok(match kind.id() {
        L::Boolean => "bool",
        L::TinyInt => "int8",
        L::SmallInt => "int16",
        L::Integer | L::SqlNull => "int32",
        L::BigInt => "int64",
        L::UTinyInt => "uint8",
        L::USmallInt => "uint16",
        L::UInteger => "uint32",
        L::UBigInt => "uint64",
        L::Float => "float32",
        L::Double | L::HugeInt | L::Decimal => "float64",
        L::Timestamp | L::TimestampTz | L::Date => "datetime64[us]",
        L::TimestampNs => "datetime64[ns]",
        L::TimestampMs => "datetime64[ms]",
        L::TimestampSec => "datetime64[s]",
        L::Interval => "timedelta64[us]",
        L::Enum => match kind.enum_values().unwrap_or_default().len() {
            0..=127 => "int8",
            128..=32767 => "int16",
            _ => "int32",
        },
        L::Time
        | L::TimeNs
        | L::TimeTz
        | L::Varchar
        | L::Bit
        | L::Blob
        | L::List
        | L::Array
        | L::Map
        | L::Struct
        | L::Union
        | L::Uuid
        | L::Variant
        | L::Geometry => "object",
        _ => {
            return Err(Error::not_implemented(byte_format!(
                "Unsupported type \"{}\" for DuckDB -> NumPy conversion",
                kind.to_bytes()
            )));
        }
    })
}

struct Column {
    kind: LogicalType,
    dtype: &'static str,
    data: Py<PyAny>,
    mask: Option<Py<PyAny>>,
    capacity: usize,
    count: usize,
}
fn resize(py: Python<'_>, array: &Bound<'_, PyAny>, size: usize) -> PyResult<()> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("refcheck", false)?;
    array.call_method("resize", ((size,),), Some(&kwargs))?;
    Ok(())
}
impl Column {
    fn new(py: Python<'_>, kind: LogicalType) -> PyResult<Self> {
        let dtype = dtype(&kind).map_err(|e| error::native(py, &e))?;
        let data = py.import("numpy")?.call_method1("empty", (0, dtype))?.unbind();
        Ok(Self {
            kind,
            dtype,
            data,
            mask: None,
            capacity: 0,
            count: 0,
        })
    }
    fn append(&mut self, py: Python<'_>, values: &[Value], timezone: &str, pandas: bool) -> PyResult<()> {
        // C++ initializes TIME_TZ object arrays but its append dispatcher rejects
        // this type. Empty streams never call it; materialized empty chunks do.
        if self.kind.id() == L::TimeTz {
            return Err(error::native(
                py,
                &Error::not_implemented(byte_format!("Unsupported type \"{}\"", self.kind.to_bytes())),
            ));
        }
        let end = self.count + values.len();
        if end > self.capacity {
            self.capacity = end.max(self.capacity.saturating_mul(2));
            resize(py, self.data.bind(py), self.capacity)?;
            if let Some(mask) = &self.mask {
                resize(py, mask.bind(py), self.capacity)?;
            }
        }
        let mask: Vec<u8> = values.iter().map(|v| u8::from(v.is_null())).collect();
        // Only VARCHAR's Pandas converter stores None without a separate null mask.
        let needs_mask =
            self.kind.id() != L::Enum && !(pandas && self.kind.id() == L::Varchar) && mask.contains(&1);
        if needs_mask && self.mask.is_none() {
            self.mask = Some(
                py.import("numpy")?
                    .call_method1("zeros", (self.capacity, "bool"))?
                    .unbind(),
            );
        }
        let slice = PySlice::new(py, self.count as isize, end as isize, 1);
        if let Some(target) = &self.mask {
            let bytes = PyBytes::new(py, &mask);
            let array = py.import("numpy")?.call_method1("frombuffer", (bytes, "bool"))?;
            target.bind(py).set_item(&slice, array)?;
        }
        if self.dtype == "object" {
            for (index, value) in values.iter().enumerate() {
                self.data
                    .bind(py)
                    .set_item(self.count + index, object(py, value, timezone, pandas)?)?;
            }
        } else {
            let mut bytes = Vec::with_capacity(values.len() * 8);
            for value in values {
                self.numeric(value, &mut bytes)
                    .map_err(|e| error::native(py, &e))?;
            }
            let array = py
                .import("numpy")?
                .call_method1("frombuffer", (PyBytes::new(py, &bytes), self.dtype))?;
            self.data.bind(py).set_item(slice, array)?;
        }
        self.count = end;
        Ok(())
    }
    fn append_vector(
        &mut self,
        py: Python<'_>,
        vector: &roastedduck_types::Vector,
        count: usize,
        timezone: &str,
    ) -> PyResult<()> {
        let direct = matches!(
            self.kind.id(),
            L::Boolean
                | L::TinyInt
                | L::SmallInt
                | L::Integer
                | L::BigInt
                | L::UTinyInt
                | L::USmallInt
                | L::UInteger
                | L::UBigInt
                | L::Float
                | L::Double
                | L::Timestamp
                | L::TimestampTz
                | L::TimestampNs
                | L::TimestampMs
                | L::TimestampSec
        );
        if !direct {
            let values = (0..count).map(|row| vector.get_value(row)).collect::<Vec<_>>();
            return self.append(py, &values, timezone, false);
        }
        let vector = vector.flattened(count);
        let width = vector.logical_type().physical_type().size();
        let bytes = vector
            .data_bytes()
            .get(..count * width)
            .ok_or_else(|| error::InternalException::new_err("Invalid native numeric buffer"))?;
        resize(py, self.data.bind(py), count)?;
        let array = py
            .import("numpy")?
            .call_method1("frombuffer", (PyBytes::new(py, bytes), self.dtype))?;
        self.data
            .bind(py)
            .set_item(PySlice::new(py, 0, count as isize, 1), array)?;
        if !vector.validity().is_all_valid() {
            let mask = (0..count)
                .map(|row| u8::from(!vector.validity().is_valid(row)))
                .collect::<Vec<_>>();
            self.mask = Some(
                py.import("numpy")?
                    .call_method1("frombuffer", (PyBytes::new(py, &mask), "bool"))?
                    .call_method0("copy")?
                    .unbind(),
            );
        }
        self.count = count;
        self.capacity = count;
        Ok(())
    }
    fn numeric(&self, value: &Value, bytes: &mut Vec<u8>) -> roastedduck_common::Result<()> {
        let integer = value.as_i128().unwrap_or_default();
        macro_rules! append {
            ($value:expr) => {
                bytes.extend_from_slice(&$value.to_ne_bytes())
            };
        }
        match self.kind.id() {
            L::Boolean => bytes.push(u8::from(value.as_bool().unwrap_or_default())),
            L::TinyInt | L::UTinyInt => bytes.push(integer as u8),
            L::SmallInt | L::USmallInt => append!(integer as u16),
            L::Integer | L::UInteger | L::SqlNull => append!(integer as u32),
            L::BigInt | L::UBigInt => append!(integer as u64),
            L::Float => append!(value.as_f64().unwrap_or_default() as f32),
            L::Double => append!(value.as_f64().unwrap_or_default()),
            L::HugeInt => append!(integer as f64),
            L::Decimal => {
                let (_, scale) = self.kind.decimal_properties().unwrap_or_default();
                append!(integer as f64 / 10_f64.powi(i32::from(scale)));
            }
            L::Timestamp | L::TimestampTz | L::TimestampNs | L::TimestampMs | L::TimestampSec => {
                append!(value.as_i64().unwrap_or_default());
            }
            L::Date => {
                let days = value.as_i32().unwrap_or_default();
                let micros = i64::from(days).checked_mul(86_400_000_000).ok_or_else(|| {
                    Error::conversion(format!(
                        "Could not convert DATE ({}) to microseconds",
                        roastedduck_types::Date(days)
                    ))
                })?;
                append!(micros);
            }
            L::Interval => {
                let interval = value.as_interval().unwrap_or_default();
                let months = i64::from(interval.months)
                    .checked_mul(2_592_000_000_000)
                    .ok_or_else(|| Error::conversion("Could not convert Month to Microseconds"))?;
                let days = i64::from(interval.days) * 86_400_000_000;
                let micros = interval
                    .micros
                    .checked_add(months)
                    .and_then(|n| n.checked_add(days))
                    .ok_or_else(|| Error::conversion("Could not convert Interval to Microseconds"))?;
                append!(micros);
            }
            L::Enum => {
                let code = if value.is_null() { -1 } else { integer };
                match self.dtype {
                    "int8" => append!(code as i8),
                    "int16" => append!(code as i16),
                    _ => append!(code as i32),
                }
            }
            _ => return Err(Error::internal("Non-numeric NumPy buffer type")),
        }
        Ok(())
    }
    fn finish(self, py: Python<'_>, pandas: bool, retain_empty_enum: bool) -> PyResult<Py<PyAny>> {
        resize(py, self.data.bind(py), self.count)?;
        if self.kind.id() == L::Enum {
            let categories = (if self.count == 0 && !retain_empty_enum {
                &[][..]
            } else {
                self.kind.enum_values().unwrap_or_default()
            })
            .iter()
            .map(|name| conversion::text(py, name))
            .collect::<PyResult<Vec<_>>>()?;
            let pandas_module = py.import("pandas")?;
            let dtype = pandas_module
                .getattr("CategoricalDtype")?
                .call1((PyList::new(py, categories)?, true))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("dtype", dtype)?;
            let categorical = pandas_module.getattr("Categorical")?.call_method(
                "from_codes",
                (self.data.bind(py),),
                Some(&kwargs),
            )?;
            return Ok(if pandas {
                categorical
            } else {
                categorical.call_method0("to_numpy")?
            }
            .unbind());
        }
        if let Some(mask) = self.mask {
            resize(py, mask.bind(py), self.count)?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("mask", mask.bind(py))?;
            kwargs.set_item("copy", false)?;
            return Ok(py
                .import("numpy.ma")?
                .getattr("masked_array")?
                .call((self.data.bind(py),), Some(&kwargs))?
                .unbind());
        }
        Ok(self.data)
    }
}
fn object(py: Python<'_>, value: &Value, timezone: &str, pandas: bool) -> PyResult<Py<PyAny>> {
    if !value.is_null() && matches!(value.logical_type().id(), L::List | L::Array) {
        let kind = value
            .logical_type()
            .child_type()
            .cloned()
            .ok_or_else(|| error::InternalException::new_err("Missing list child type"))?;
        let mut column = Column::new(py, kind)?;
        column.append(py, value.children().unwrap_or_default(), timezone, pandas)?;
        return column.finish(py, false, false);
    }
    if !value.is_null() && value.logical_type().id() == L::Struct {
        // NumPy's STRUCT converter always creates a mapping, including unnamed
        // STRUCTs; row/parameter conversion uses positional tuples for those.
        let result = PyDict::new(py);
        for ((name, _), child) in value
            .logical_type()
            .struct_children()
            .unwrap_or_default()
            .iter()
            .zip(value.children().unwrap_or_default())
        {
            result.set_item(
                conversion::text(py, name)?,
                conversion::to_python(py, child, timezone)?,
            )?;
        }
        return Ok(result.unbind().into_any());
    }
    if !value.is_null() && matches!(value.logical_type().id(), L::Blob | L::Geometry) {
        return Ok(PyByteArray::new(py, value.as_bytes().unwrap_or_default())
            .unbind()
            .into_any());
    }
    if !value.is_null() && value.logical_type().id() == L::Bit {
        return Ok(PyBytes::new(py, value.as_bytes().unwrap_or_default())
            .unbind()
            .into_any());
    }
    conversion::to_python(py, value, timezone)
}
pub(super) fn names(names: &[ByteString]) -> Vec<ByteString> {
    let mut seen = HashMap::<ByteString, u64>::new();
    names
        .iter()
        .map(|name| {
            let mut candidate = name.clone();
            let mut number = *seen.get(&name.to_ascii_lowercase()).unwrap_or(&0);
            while seen.contains_key(&candidate.to_ascii_lowercase()) {
                number += 1;
                candidate = byte_format!("{}_{number}", name);
            }
            seen.insert(name.to_ascii_lowercase(), number);
            seen.entry(candidate.to_ascii_lowercase()).or_insert(0);
            candidate
        })
        .collect()
}
// MapFunction uses raw NumPy arrays: ENUM codes and SQLNULL rejection differ from
// the public fetchnumpy/Pandas conversion. Keep this conversion bounded to one input chunk.
pub(super) fn map_frame(
    py: Python<'_>,
    metadata: &Metadata,
    chunk: Option<&DataChunk>,
) -> PyResult<Py<PyAny>> {
    let result = PyDict::new(py);
    for (index, (name, kind)) in metadata.names.iter().zip(&metadata.types).enumerate() {
        if kind.id() == L::SqlNull {
            return Err(error::native(
                py,
                &Error::not_implemented(byte_format!(
                    "Unsupported type \"{}\" for DuckDB -> NumPy conversion",
                    kind.to_bytes()
                )),
            ));
        }
        let mut column = Column::new(py, kind.clone())?;
        if let Some(chunk) = chunk {
            column.append_vector(py, chunk.column(index), chunk.len(), &metadata.timezone)?;
        }
        let value = if kind.id() == L::Enum {
            resize(py, column.data.bind(py), column.count)?;
            column.data
        } else {
            column.finish(py, false, false)?
        };
        result.set_item(conversion::text(py, name)?, value)?;
    }
    Ok(py
        .import("pandas")?
        .getattr("DataFrame")?
        .call1((result,))?
        .unbind())
}

pub(super) fn fetch(
    py: Python<'_>,
    metadata: &Metadata,
    pandas: bool,
    retain_empty_enum: bool,
    date_as_object: bool,
    max_chunks: Option<usize>,
    mut next: impl FnMut() -> PyResult<Option<DataChunk>>,
) -> PyResult<Py<PyAny>> {
    if metadata.consumed && max_chunks.is_none() {
        return if pandas {
            Ok(py.None())
        } else {
            Err(pyo3::exceptions::PyTypeError::new_err(
                "'NoneType' object is not iterable",
            ))
        };
    }
    let mut columns = metadata
        .types
        .iter()
        .cloned()
        .map(|kind| Column::new(py, kind))
        .collect::<PyResult<Vec<_>>>()?;
    // The typed empty chunk is dispatched before the materialized collection
    // is consumed. A conversion failure must remain repeatable on a retry.
    if metadata.materialized_empty {
        for column in &mut columns {
            column.append(py, &[], &metadata.timezone, pandas)?;
        }
    }
    let limit = if metadata.materialized { None } else { max_chunks };
    let mut chunks = 0;
    while limit.is_none_or(|limit| chunks < limit) {
        let Some(chunk) = next()? else {
            break;
        };
        chunks += 1;
        for (index, column) in columns.iter_mut().enumerate() {
            let values = (0..chunk.len())
                .map(|row| chunk.column(index).get_value(row))
                .collect::<Vec<_>>();
            column.append(py, &values, &metadata.timezone, pandas)?;
        }
    }
    let result = PyDict::new(py);
    let names = names(&metadata.names);
    for (name, column) in names.iter().zip(columns) {
        result.set_item(
            conversion::text(py, name)?,
            column.finish(py, pandas, retain_empty_enum)?,
        )?;
    }
    if !pandas {
        return Ok(result.unbind().into_any());
    }
    let pd = py.import("pandas")?;
    for (key, value) in result.iter() {
        if value.is_instance(&py.import("numpy.ma")?.getattr("MaskedArray")?)? {
            let dtype = value.getattr("dtype")?;
            let name = dtype.str()?.to_str()?.to_owned();
            let nullable = match name.as_str() {
                "bool" => "boolean".to_owned(),
                n if n.starts_with("uint") => n.replacen("uint", "UInt", 1),
                n if n.starts_with("int") => n.replacen("int", "Int", 1),
                _ => name,
            };
            let kwargs = PyDict::new(py);
            kwargs.set_item("dtype", nullable)?;
            let series = pd
                .getattr("Series")?
                .call((value.getattr("data")?,), Some(&kwargs))?;
            series.call_method1("__setitem__", (value.getattr("mask")?, pd.getattr("NA")?))?;
            result.set_item(key, series)?;
        }
    }
    let frame = pd.getattr("DataFrame")?.call_method1("from_dict", (&result,))?;
    for (index, (name, kind)) in names.iter().zip(&metadata.types).enumerate() {
        let name = conversion::text(py, name)?;
        if kind.id() == L::TimestampTz {
            let series = frame
                .get_item(&name)?
                .getattr("dt")?
                .call_method1("tz_localize", ("UTC",))?
                .getattr("dt")?
                .call_method1("tz_convert", (&metadata.timezone,))?;
            replace_column(py, &frame, index, &name, &series)?;
        } else if date_as_object && kind.id() == L::Date {
            let series = frame.get_item(&name)?.getattr("dt")?.getattr("date")?;
            replace_column(py, &frame, index, &name, &series)?;
        }
    }
    Ok(frame.unbind())
}

fn replace_column(
    py: Python<'_>,
    frame: &Bound<'_, PyAny>,
    index: usize,
    name: &Py<PyAny>,
    series: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("columns", name.bind(py))?;
    kwargs.set_item("inplace", true)?;
    frame.call_method("drop", (), Some(&kwargs))?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("allow_duplicates", false)?;
    frame.call_method("insert", (index, name.bind(py), series), Some(&kwargs))?;
    Ok(())
}

pub(super) fn arrow(
    py: Python<'_>,
    metadata: &Metadata,
    rows_per_batch: usize,
    mut next: impl FnMut() -> PyResult<Option<DataChunk>>,
) -> PyResult<Py<PyAny>> {
    if metadata.consumed {
        return Ok(py.None());
    }
    if rows_per_batch == 0 {
        return Err(error::native(
            py,
            &Error::invalid_input("The batch size must be higher than zero"),
        ));
    }
    let arrow = py.import("pyarrow")?;
    let mut schema = roastedduck_arrow::ffi::ArrowSchemaWrapper::default();
    roastedduck_arrow::to_arrow_schema(
        &mut schema.arrow_schema,
        &metadata.types,
        &metadata.names,
        &metadata.arrow,
    )
    .map_err(|e| error::native(py, &e))?;
    // PyArrow consumes the C data interface and clears its release callback. The
    // owning wrappers release any value left behind if an import raises.
    let schema = arrow
        .getattr("Schema")?
        .call_method1("_import_from_c", ((&raw mut schema.arrow_schema) as usize,))?;
    let batches = PyList::empty(py);
    let new_appender = || {
        roastedduck_arrow::ArrowAppender::new(
            metadata.types.clone(),
            rows_per_batch.min(2048),
            metadata.arrow.clone(),
        )
        .map_err(|e| error::native(py, &e))
    };
    let append_batch = |appender: roastedduck_arrow::ArrowAppender| -> PyResult<()> {
        let mut array = roastedduck_arrow::ffi::ArrowArrayWrapper {
            arrow_array: appender.finalize().map_err(|e| error::native(py, &e))?,
        };
        let batch = arrow
            .getattr("RecordBatch")?
            .call_method1("_import_from_c", ((&raw mut array.arrow_array) as usize, &schema))?;
        batches.append(batch)
    };
    let mut appender = new_appender()?;
    let mut count = 0;
    while let Some(chunk) = next()? {
        let mut start = 0;
        while start < chunk.len() {
            let end = chunk.len().min(start + rows_per_batch - count);
            appender
                .append(&chunk, start, end, chunk.len())
                .map_err(|e| error::native(py, &e))?;
            count += end - start;
            start = end;
            if count == rows_per_batch {
                append_batch(appender)?;
                appender = new_appender()?;
                count = 0;
            }
        }
    }
    if count > 0 {
        append_batch(appender)?;
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("schema", schema)?;
    Ok(arrow
        .getattr("Table")?
        .call_method("from_batches", (batches,), Some(&kwargs))?
        .unbind())
}
