//! `strftime`, `strptime` and `try_strptime` (DuckDB's `strftime.cpp`).

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{
    BindInput, FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet,
};
use roastedduck_types::{DataChunk, Date, Interval, LogicalType, LogicalTypeId, Timestamp, Value, Vector};

use super::strftime_format::{Spec, StrTimeFormat};
use crate::scalar::executor::{Str, StrOut, unary};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// The bound format of a `strftime` call (`None` for a NULL format).
#[derive(Debug)]
struct StrfTimeBindData(Option<StrTimeFormat>);

impl FunctionData for StrfTimeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `StrfTimeBindFunction`.
fn bind_strftime(format_index: usize) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult {
    move |_, input| {
        if input.argument_types[format_index].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let Some(Some(value)) = input.constants.get(format_index) else {
            return Err(Error::invalid_input("strftime format must be a constant"));
        };
        if value.is_null() {
            return Ok(Some(Arc::new(StrfTimeBindData(None))));
        }
        let text = crate::values::as_text(value)?;
        let format = StrTimeFormat::parse(&text).map_err(|error| {
            Error::invalid_input(format!("Failed to parse format specifier {text}: {error}"))
        })?;
        Ok(Some(Arc::new(StrfTimeBindData(Some(format)))))
    }
}

fn strftime_format(state: &FunctionState) -> Result<Option<&StrTimeFormat>> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<StrfTimeBindData>())
        .map(|data| data.0.as_ref())
        .ok_or_else(|| Error::internal("strftime without bind data"))
}

fn date_data(date: Date, micros: i64, nanos: i32) -> [i32; 8] {
    let (year, month, day) = date.to_ymd();
    let hour = micros / Interval::MICROS_PER_HOUR;
    let minute = micros % Interval::MICROS_PER_HOUR / Interval::MICROS_PER_MINUTE;
    let second = micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_SEC;
    let fraction = micros % Interval::MICROS_PER_SEC;
    [
        year,
        month,
        day,
        hour as i32,
        minute as i32,
        second as i32,
        (fraction * 1000) as i32 + nanos,
        0,
    ]
}

/// A temporal value `strftime` formats.
trait Formattable: roastedduck_types::NativeType {
    /// The date and time parts, or the text of an infinite value.
    fn parts(self) -> std::result::Result<(Date, [i32; 8]), String>;
}

impl Formattable for Date {
    fn parts(self) -> std::result::Result<(Date, [i32; 8]), String> {
        if !self.is_finite() {
            return Err(self.to_string());
        }
        Ok((self, date_data(self, 0, 0)))
    }
}

impl Formattable for Timestamp {
    fn parts(self) -> std::result::Result<(Date, [i32; 8]), String> {
        if !self.is_finite() {
            return Err(self.to_string());
        }
        let date = self.to_date();
        Ok((date, date_data(date, self.to_time().0, 0)))
    }
}

/// `TIMESTAMP_NS` values (nanoseconds since the epoch).
impl Formattable for i64 {
    fn parts(self) -> std::result::Result<(Date, [i32; 8]), String> {
        let timestamp = Timestamp(self);
        if !timestamp.is_finite() {
            return Err(timestamp.to_string());
        }
        // DuckDB's `Timestamp::Convert(timestamp_ns_t, ...)`.
        let negative = i64::from(self < 0);
        let micros = (self + negative) / Interval::NANOS_PER_MICRO - negative;
        let date = Timestamp(micros).to_date();
        let day_nanos = i64::from(date.0) * Interval::MICROS_PER_DAY * Interval::NANOS_PER_MICRO;
        let rest = self - day_nanos;
        let time_micros = rest / Interval::NANOS_PER_MICRO;
        let nanos = (rest % Interval::NANOS_PER_MICRO) as i32;
        Ok((date, date_data(date, time_micros, nanos)))
    }
}

fn strftime_rows<T: Formattable>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    value_index: usize,
) -> Result<()> {
    let Some(format) = strftime_format(state)? else {
        *result = Vector::constant_null(LogicalType::VARCHAR);
        return Ok(());
    };
    let values = DataChunk::new(vec![args.column(value_index).clone()], args.len());
    unary::<T, StrOut>(&values, result, |value, out| {
        match value.parts() {
            Ok((date, data)) => format.format(date, &data, out),
            Err(text) => out.extend_from_slice(text.as_bytes()),
        }
        Ok(true)
    })
}

/// `strftime(value, format)` and `strftime(format, value)`.
pub fn strftime_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("strftime");
    macro_rules! add {
        ($ty:ty, $logical:expr) => {
            set.add(
                ScalarFunction::new(
                    "strftime",
                    vec![$logical, LogicalType::VARCHAR],
                    LogicalType::VARCHAR,
                    |args, state, result| strftime_rows::<$ty>(args, state, result, 0),
                )
                .with_bind(bind_strftime(1)),
            );
        };
    }
    macro_rules! add_reversed {
        ($ty:ty, $logical:expr) => {
            set.add(
                ScalarFunction::new(
                    "strftime",
                    vec![LogicalType::VARCHAR, $logical],
                    LogicalType::VARCHAR,
                    |args, state, result| strftime_rows::<$ty>(args, state, result, 1),
                )
                .with_bind(bind_strftime(0)),
            );
        };
    }
    add!(Date, LogicalType::DATE);
    add!(Timestamp, LogicalType::TIMESTAMP);
    add!(i64, LogicalType::TIMESTAMP_NS);
    add_reversed!(Date, LogicalType::DATE);
    add_reversed!(Timestamp, LogicalType::TIMESTAMP);
    add_reversed!(i64, LogicalType::TIMESTAMP_NS);
    add!(Timestamp, LogicalType::TIMESTAMP_TZ);
    add_reversed!(Timestamp, LogicalType::TIMESTAMP_TZ);
    set
}

/// The bound formats of a `strptime` call (empty for a NULL format).
#[derive(Debug)]
struct StrpTimeBindData {
    formats: Vec<StrTimeFormat>,
    nanos: bool,
}

impl FunctionData for StrpTimeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn parse_strptime_format(text: &str) -> Result<StrTimeFormat> {
    StrTimeFormat::parse(text)
        .map_err(|error| Error::invalid_input(format!("Failed to parse format specifier {text}: {error}")))
}

/// DuckDB's `StrpTimeFunction::Bind`.
fn bind_strptime(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    if input.argument_types[1].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(Some(value)) = input.constants.get(1) else {
        return Err(Error::invalid_input("strptime format must be a constant"));
    };
    let formats: Vec<StrTimeFormat> = if value.is_null() {
        Vec::new()
    } else if value.logical_type().id() == LogicalTypeId::Varchar {
        vec![parse_strptime_format(&crate::values::as_text(value)?)?]
    } else if *value.logical_type() == LogicalType::list(LogicalType::VARCHAR) {
        let children = value.children().unwrap_or_default();
        if children.is_empty() {
            return Err(Error::invalid_input("strptime format list must not be empty"));
        }
        children
            .iter()
            .map(|child| parse_strptime_format(&value_text(child)?))
            .collect::<Result<_>>()?
    } else {
        return Err(Error::invalid_input("strptime format must be a string"));
    };
    let has_offset = formats.iter().any(|format| format.has_specifier(Spec::UtcOffset));
    let has_nanos = formats
        .iter()
        .any(|format| format.has_specifier(Spec::NanosecondPadded));
    let mut nanos = false;
    if has_offset {
        function.return_type = LogicalType::TIMESTAMP_TZ;
    } else if has_nanos {
        function.return_type = LogicalType::TIMESTAMP_NS;
        nanos = true;
    }
    Ok(Some(Arc::new(StrpTimeBindData { formats, nanos })))
}

fn value_text(value: &Value) -> Result<String> {
    if value.is_null() {
        return Ok("NULL".to_owned());
    }
    crate::values::as_text(value)
}

fn strptime_rows(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    try_parse: bool,
) -> Result<()> {
    let data = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<StrpTimeBindData>())
        .ok_or_else(|| Error::internal("strptime without bind data"))?;
    let format_column = args.column(1);
    let format_null = if try_parse {
        format_column.is_constant() && format_column.is_null(0)
    } else {
        format_column.is_null(0)
    };
    if format_null || data.formats.is_empty() {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    let nanos = data.nanos;
    let parse = |input: &[u8]| -> Result<Option<i64>> {
        let mut last = None;
        for format in &data.formats {
            let parsed = format.parse_text(input, false);
            if parsed.not_implemented {
                return Err(Error::not_implemented("Unsupported specifier for strptime"));
            }
            if parsed.ok {
                if try_parse {
                    let value = if nanos {
                        parsed.try_to_timestamp_ns()
                    } else {
                        parsed.try_to_timestamp().map(|timestamp| timestamp.0)
                    };
                    if value.is_some() {
                        return Ok(value);
                    }
                    last = Some(parsed);
                    continue;
                }
                return Ok(Some(if nanos {
                    parsed.to_timestamp_ns()?
                } else {
                    parsed.to_timestamp()?.0
                }));
            }
            last = Some(parsed);
        }
        if try_parse {
            return Ok(None);
        }
        let message = last
            .map(|parsed| parsed.format_error(input, &data.formats[0].format_specifier))
            .unwrap_or_default();
        Err(Error::invalid_input(message))
    };
    unary::<Str, i64>(&strings, result, |input, out| match parse(input)? {
        Some(value) => {
            *out = value;
            Ok(true)
        }
        None => Ok(false),
    })
}

fn strptime_set(name: &str, try_parse: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for format_type in [LogicalType::VARCHAR, LogicalType::list(LogicalType::VARCHAR)] {
        let mut function = ScalarFunction::new(
            name,
            vec![LogicalType::VARCHAR, format_type],
            LogicalType::TIMESTAMP,
            move |args, state, result| strptime_rows(args, state, result, try_parse),
        )
        .with_bind(bind_strptime);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

/// `strptime` and `try_strptime`.
pub fn strptime_functions() -> Vec<ScalarFunctionSet> {
    vec![
        strptime_set("strptime", false),
        strptime_set("try_strptime", true),
    ]
}
