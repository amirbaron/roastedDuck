//! DuckDB's `icu-strptime.cpp`: `strptime` / `try_strptime` with time zone names (patched
//! into the core functions), `strftime(TIMESTAMPTZ, format)`, and the casts between VARCHAR
//! and TIMESTAMP WITH TIME ZONE / TIME WITH TIME ZONE.

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::executor::{RowError, addressable, cast_rows, cast_rows_to_string};
use roastedduck_function::cast::{BoundCastInfo, CastFunctionSet};
use roastedduck_function::{
    BindInput, FunctionData, FunctionState, ScalarBind, ScalarFunction, ScalarFunctionSet,
};
use roastedduck_functions::scalar::date::strftime_format::{ParseResult, Spec, StrTimeFormat};
use roastedduck_functions::scalar::executor::{Str, StrOut, binary, str_at, unary};
use roastedduck_types::{
    DataChunk, Date, LogicalType, LogicalTypeId, StringT, Time, TimeTz, Timestamp, Value, Vector,
};

use crate::icu_datefunc::{
    self, BindData, CalendarPtr, MICROS_PER_MSEC, extract_field, extract_offset_seconds, get_time, set_time,
    set_time_zone, set_time_zone_message, try_get_time, try_set_time_zone,
};
use crate::icu_helpers::get_components;
use crate::icu_timezone::from_naive;
use crate::icu4c::calendar::{Calendar, field};

// ---------------------------------------------------------------------- strptime

/// `ICUStrptime::ICUStrptimeBindData`.
#[derive(Debug)]
struct IcuStrptimeBindData {
    data: BindData,
    formats: Vec<StrTimeFormat>,
}

impl FunctionData for IcuStrptimeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.formats.len() == other.formats.len()
                && self
                    .formats
                    .iter()
                    .zip(&other.formats)
                    .all(|(a, b)| a.format_specifier == b.format_specifier)
        })
    }
}

/// `ICUStrptime::ToMicros`: sets the parsed parts in the calendar's time zone (or at the
/// parsed UTC offset) and returns the µs below the ms.
fn to_micros(calendar: &mut Calendar, parsed: &ParseResult, format: &StrTimeFormat) -> u64 {
    // Get the parts in the current time zone
    let mut micros = parsed.micros() as u64;
    calendar.set(field::EXTENDED_YEAR, parsed.data[0]); // strptime doesn't understand eras
    calendar.set(field::MONTH, parsed.data[1] - 1);
    calendar.set(field::DATE, parsed.data[2]);
    calendar.set(field::HOUR_OF_DAY, parsed.data[3]);
    calendar.set(field::MINUTE, parsed.data[4]);
    calendar.set(field::SECOND, parsed.data[5]);
    calendar.set(field::MILLISECOND, (micros / MICROS_PER_MSEC as u64) as i32);
    micros %= MICROS_PER_MSEC as u64;

    // The offset fields override the TZ setting, so they have to be cleared for every row,
    // or a row without an offset would inherit the one installed by an earlier row.
    calendar.clear_field(field::ZONE_OFFSET);
    calendar.clear_field(field::DST_OFFSET);
    if format.has_specifier(Spec::UtcOffset) {
        calendar.set(field::ZONE_OFFSET, parsed.data[7] * 1000);
    }
    micros
}

/// `ICUStrptime::Parse`.
fn parse_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &IcuStrptimeBindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    let fmt_arg = args.column(1);
    if fmt_arg.is_null(0) {
        *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
        return Ok(());
    }
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    unary::<Str, Timestamp>(&strings, result, |input, out| {
        let mut last = None;
        for format in &info.formats {
            let parsed = format.parse_text(input, false);
            if parsed.not_implemented {
                return Err(Error::not_implemented("Unsupported specifier for strptime"));
            }
            if parsed.ok {
                if parsed.special.is_some() {
                    *out = parsed.to_timestamp()?;
                    return Ok(true);
                }
                // Set TZ first. The calendar is shared by all the rows, so a row without a
                // zone has to restore the bound one.
                if parsed.tz.is_empty() {
                    calendar.set_time_zone(info.data.calendar.time_zone().clone());
                } else {
                    set_time_zone(&mut calendar, &parsed.tz)?;
                }
                let micros = to_micros(&mut calendar, &parsed, format);
                *out = get_time(&mut calendar, micros)?;
                return Ok(true);
            }
            last = Some(parsed);
        }
        let message = last
            .map(|parsed| parsed.format_error(input, &info.formats[0].format_specifier))
            .unwrap_or_default();
        Err(Error::invalid_input(message))
    })
}

/// `ICUStrptime::TryParse`.
fn try_parse_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &IcuStrptimeBindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    let fmt_arg = args.column(1);
    if fmt_arg.is_constant() && fmt_arg.is_null(0) {
        *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
        return Ok(());
    }
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    unary::<Str, Timestamp>(&strings, result, |input, out| {
        for format in &info.formats {
            let parsed = format.parse_text(input, false);
            if parsed.not_implemented {
                return Err(Error::not_implemented("Unsupported specifier for strptime"));
            }
            if !parsed.ok {
                continue;
            }
            if parsed.special.is_some() {
                *out = parsed.to_timestamp()?;
                return Ok(true);
            }
            let tz = if parsed.tz.is_empty() {
                info.data.tz_setting.as_str()
            } else {
                parsed.tz.as_str()
            };
            if try_set_time_zone(&mut calendar, tz) {
                let micros = to_micros(&mut calendar, &parsed, format);
                if let Some(value) = try_get_time(&mut calendar, micros) {
                    *out = value;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    })
}

/// Parses a `strptime` format (`StrTimeFormat::ParseFormatSpecifier` with the binder's
/// error).
fn parse_format(format_string: &str) -> Result<StrTimeFormat> {
    StrTimeFormat::parse(format_string).map_err(|error| {
        Error::invalid_input(format!(
            "Failed to parse format specifier {format_string}: {error}"
        ))
    })
}

/// `ICUStrptime::StrpTimeBindFunction`: formats with time zone names (or, in lists, UTC
/// offsets) parse with ICU into TIMESTAMPTZ; the others use `bind_strptime`.
fn strptime_bind(
    bind_strptime: ScalarBind,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    move |function, input| {
        if input
            .argument_types
            .get(1)
            .is_some_and(|ty| ty.id() == LogicalTypeId::Unknown)
        {
            return bind_strptime(function, input);
        }
        let Some(Some(format_value)) = input.constants.get(1) else {
            return Err(Error::invalid_input("strptime format must be a constant"));
        };
        let try_parse = &*function.name == b"try_strptime";
        let mut icu_formats = None;
        if format_value.is_null() {
            // falls back
        } else if format_value.logical_type().id() == LogicalTypeId::Varchar {
            let format = parse_format(&format_value.to_varchar()?)?;
            // If we have a time zone, we should use ICU for parsing and return a TSTZ
            // instead.
            if format.has_specifier(Spec::TzName) {
                icu_formats = Some(vec![format]);
            }
        } else if *format_value.logical_type() == LogicalType::list(LogicalType::VARCHAR) {
            let children = format_value.children().unwrap_or_default();
            if children.is_empty() {
                return Err(Error::invalid_input("strptime format list must not be empty"));
            }
            let mut formats = Vec::with_capacity(children.len());
            let mut has_tz = false;
            for child in children {
                let text = if child.is_null() {
                    "NULL".to_owned()
                } else {
                    child.to_varchar()?
                };
                let format = parse_format(&text)?;
                // If any format has UTC offsets or names, then we have to produce TSTZ
                has_tz =
                    has_tz || format.has_specifier(Spec::TzName) || format.has_specifier(Spec::UtcOffset);
                formats.push(format);
            }
            if has_tz {
                icu_formats = Some(formats);
            }
        }
        if let Some(formats) = icu_formats {
            function.kernel = if try_parse {
                Arc::new(try_parse_rows)
            } else {
                Arc::new(parse_rows)
            };
            function.return_type = LogicalType::TIMESTAMP_TZ;
            return Ok(Some(Arc::new(IcuStrptimeBindData {
                data: BindData::from_client(input.client),
                formats,
            })));
        }
        // Fall back to faster, non-TZ parsing
        bind_strptime(function, input)
    }
}

/// `ICUStrptime::TailPatch`: replaces the bind of the `(VARCHAR, VARCHAR)` and
/// `(VARCHAR, VARCHAR[])` overloads of `strptime` / `try_strptime`.
pub fn tail_patch_strptime(set: &mut ScalarFunctionSet) -> Result<()> {
    for format_type in [LogicalType::VARCHAR, LogicalType::list(LogicalType::VARCHAR)] {
        let types = [LogicalType::VARCHAR, format_type];
        let function = set
            .functions
            .iter_mut()
            .find(|function| function.arguments == types)
            .ok_or_else(|| Error::internal("ICU - Function for TailPatch not found"))?;
        let bind_strptime = function
            .bind
            .clone()
            .ok_or_else(|| Error::internal("ICU - Function for TailPatch has no bind"))?;
        function.bind = Some(Arc::new(strptime_bind(bind_strptime)));
    }
    Ok(())
}

// ---------------------------------------------------------------------- VARCHAR casts

/// `ICUStrptime::VarcharToTimestampTZ`.
fn varchar_to_timestamp_tz(info: &BindData) -> BoundCastInfo {
    let info = info.clone();
    BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let strings = source.values::<StringT>();
        cast_rows::<Timestamp>(&source, result, count, params, |slot| {
            let input = source.string_value(&strings[slot]);
            let parsed = match Timestamp::parse_parts(input, true) {
                Ok(parsed) => parsed,
                Err(roastedduck_types::TimestampParseError::Range) => {
                    return Err(RowError::Invalid(Timestamp::range_error(input)));
                }
                Err(_) => return Err(RowError::Invalid(Timestamp::format_error(input))),
            };
            if parsed.has_offset {
                return Ok(parsed.timestamp);
            }
            // Convert parts to a TZ (default or parsed) if no offset was provided. Change
            // TZ if one was provided.
            let mut error = None;
            if !parsed.time_zone.is_empty() {
                let tz = String::from_utf8_lossy(parsed.time_zone);
                error = set_time_zone_message(&mut calendar, &tz);
            }
            // Now get the parts in the given time zone
            let value = from_naive(&mut calendar, parsed.timestamp).map_err(RowError::Fatal)?;
            match error {
                Some(message) => Err(RowError::Invalid(message)),
                None => Ok(value),
            }
        })
    })
}

/// `ICUStrptime::VarcharToTimeTZ`.
fn varchar_to_time_tz(info: &BindData) -> BoundCastInfo {
    let info = info.clone();
    BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let strings = source.values::<StringT>();
        cast_rows::<TimeTz>(&source, result, count, params, |slot| {
            let input = source.string_value(&strings[slot]);
            let Some((value, has_offset)) = TimeTz::parse_with_offset_flag(input, false) else {
                return Err(RowError::Invalid(Time::conversion_error(input)));
            };
            if has_offset {
                return Ok(value);
            }
            // Extract the offset from the calendar and apply it to the offset +00 time we
            // parsed.
            let offset = extract_offset_seconds(&mut calendar).map_err(RowError::Fatal)?;
            Ok(TimeTz::new(value.time(), offset))
        })
    })
}

/// `ICUStrptime::BindCastFromVarchar`.
fn bind_cast_from_varchar(
    casts: &CastFunctionSet,
    _source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let Some(client) = casts.client() else {
        return Ok(None);
    };
    let info = BindData::from_client(client);
    match target.id() {
        LogicalTypeId::TimestampTz => Ok(Some(varchar_to_timestamp_tz(&info))),
        LogicalTypeId::TimeTz => Ok(Some(varchar_to_time_tz(&info))),
        _ => Err(Error::internal(
            "Unsupported type for VARCHAR to TIME/TIMESTAMPTZ cast.",
        )),
    }
}

// ---------------------------------------------------------------------- strftime

/// `TimeToStringCast::Format`: `HH:MM:SS[.ffffff]` without trailing zeros.
fn write_time(out: &mut Vec<u8>, hour: i32, minute: i32, second: i32, micros: i32) {
    let two = |out: &mut Vec<u8>, value: i32| {
        out.push(b'0' + (value / 10) as u8);
        out.push(b'0' + (value % 10) as u8);
    };
    two(out, hour);
    out.push(b':');
    two(out, minute);
    out.push(b':');
    two(out, second);
    if micros != 0 {
        out.push(b'.');
        let digits = format!("{micros:06}");
        out.extend_from_slice(digits.trim_end_matches('0').as_bytes());
    }
}

/// `DateToStringCast::Format` of the parts: years padded to 4 digits, ` (BC)` for years
/// before 1.
fn write_date(out: &mut Vec<u8>, year: i32, month: i32, day: i32) {
    let bc = year <= 0;
    let year = if bc { 1 - i64::from(year) } else { i64::from(year) };
    out.extend_from_slice(format!("{year:04}-{month:02}-{day:02}").as_bytes());
    if bc {
        out.extend_from_slice(b" (BC)");
    }
}

/// `Time::ToUTCOffset`: `±HH[:MM]`.
fn write_utc_offset(out: &mut Vec<u8>, hour_offset: i32, minute_offset: i32) {
    let micros = i64::from(hour_offset * 60 + minute_offset) * 60_000_000;
    out.push(if micros < 0 { b'-' } else { b'+' });
    let minutes = micros.abs() / 60_000_000;
    let (hour, min) = (minutes / 60, minutes % 60);
    out.extend_from_slice(format!("{hour:02}").as_bytes());
    if min != 0 {
        out.extend_from_slice(format!(":{min:02}").as_bytes());
    }
}

/// `ICUStrftime::CastOperation`: DuckDB's TIMESTAMPTZ text in the calendar's time zone.
pub(crate) fn cast_operation(calendar: &mut Calendar, input: Timestamp, out: &mut Vec<u8>) -> Result<()> {
    // Infinity is always formatted the same way
    if !input.is_finite() {
        out.extend_from_slice(input.to_string().as_bytes());
        return Ok(());
    }
    // decompose the timestamp
    let ts_data = get_components(input, calendar)?;
    let offset = extract_offset_seconds(calendar)? / 60;
    let hour_offset = offset / 60;
    let minute_offset = offset % 60;

    write_date(out, ts_data.year, ts_data.month, ts_data.day);
    out.push(b' ');
    write_time(
        out,
        ts_data.hour,
        ts_data.minute,
        ts_data.second,
        ts_data.microsecond,
    );
    write_utc_offset(out, hour_offset, minute_offset);
    Ok(())
}

/// `ICUStrftime::BindCastToVarchar`.
#[allow(clippy::unnecessary_wraps)] // a cast bind callback
fn bind_cast_to_varchar(
    casts: &CastFunctionSet,
    _source: &LogicalType,
    _target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let Some(client) = casts.client() else {
        return Ok(None);
    };
    let info = BindData::from_client(client);
    Ok(Some(BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let values = source.values::<Timestamp>();
        cast_rows_to_string(&source, result, count, params, |slot, out| {
            cast_operation(&mut calendar, values[slot], out).map_err(RowError::Fatal)
        })
    })))
}

/// Registers the casts of `icu-strptime.cpp` (`ICUStrptime::AddCasts`,
/// `ICUStrftime::AddCasts`).
pub(crate) fn add_casts(casts: &mut CastFunctionSet) {
    casts.register_bind_function(
        LogicalType::VARCHAR,
        LogicalType::TIMESTAMP_TZ,
        Arc::new(bind_cast_from_varchar),
        -1,
    );
    casts.register_bind_function(
        LogicalType::VARCHAR,
        LogicalType::TIME_TZ,
        Arc::new(bind_cast_from_varchar),
        -1,
    );
    casts.register_bind_function(
        LogicalType::TIMESTAMP_TZ,
        LogicalType::VARCHAR,
        Arc::new(bind_cast_to_varchar),
        -1,
    );
}

/// `ICUStrftime::Operation`: `input` formatted in the calendar's time zone.
fn strftime_operation(
    calendar: &mut Calendar,
    input: Timestamp,
    tz_name: &str,
    format: &StrTimeFormat,
    out: &mut Vec<u8>,
) -> Result<()> {
    // Infinity is always formatted the same way
    if !input.is_finite() {
        out.extend_from_slice(input.to_string().as_bytes());
        return Ok(());
    }
    // Get the parts in the given time zone
    let micros = set_time(calendar, input);
    let mut data = [0_i32; 8];
    data[0] = extract_field(calendar, field::EXTENDED_YEAR)?; // strftime doesn't understand eras.
    data[1] = extract_field(calendar, field::MONTH)? + 1;
    data[2] = extract_field(calendar, field::DATE)?;
    data[3] = extract_field(calendar, field::HOUR_OF_DAY)?;
    data[4] = extract_field(calendar, field::MINUTE)?;
    data[5] = extract_field(calendar, field::SECOND)?;
    data[6] = extract_field(calendar, field::MILLISECOND)? * MICROS_PER_MSEC as i32 + micros as i32;
    data[7] = extract_offset_seconds(calendar)?;

    let date = Date::from_ymd(data[0], data[1], data[2]).ok_or_else(|| {
        Error::conversion(format!("Date out of range: {}-{}-{}", data[0], data[1], data[2]))
    })?;
    // `FormatString` works in nanoseconds.
    data[6] *= 1000;
    format.format_with_tz_name(date, &data, Some(tz_name), out);
    Ok(())
}

/// `ICUStrftime::ParseFormatSpecifier`.
fn parse_strftime_format(format: &[u8]) -> Result<StrTimeFormat> {
    let text = String::from_utf8_lossy(format);
    StrTimeFormat::parse(&text)
        .map_err(|error| Error::invalid_input(format!("Failed to parse format specifier {text}: {error}")))
}

/// `ICUStrftime::ICUStrftimeFunction`.
fn strftime_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.calendar.clone();
    let tz_name = info.tz_setting.clone();
    let fmt_arg = args.column(1);
    if fmt_arg.is_constant() {
        // Common case of constant part.
        let Some(format) = str_at(fmt_arg, 0) else {
            *result = Vector::constant_null(LogicalType::VARCHAR);
            return Ok(());
        };
        let format = parse_strftime_format(format)?;
        let values = DataChunk::new(vec![args.column(0).clone()], args.len());
        unary::<Timestamp, StrOut>(&values, result, |input, out| {
            strftime_operation(&mut calendar, input, &tz_name, &format, out)?;
            Ok(true)
        })
    } else {
        binary::<Timestamp, Str, StrOut>(args, result, |input, format, out| {
            if input.is_finite() {
                let format = parse_strftime_format(format)?;
                strftime_operation(&mut calendar, input, &tz_name, &format, out)?;
            } else {
                out.extend_from_slice(input.to_string().as_bytes());
            }
            Ok(true)
        })
    }
}

/// `strftime(TIMESTAMPTZ, VARCHAR)` (`ICUStrftime::AddBinaryTimestampFunction`).
pub(crate) fn strftime_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("strftime");
    set.add(
        ScalarFunction::new(
            "strftime",
            vec![LogicalType::TIMESTAMP_TZ, LogicalType::VARCHAR],
            LogicalType::VARCHAR,
            strftime_rows,
        )
        .with_bind(icu_datefunc::bind),
    );
    set
}

/// A placeholder to keep `Value` imported for the constant folding helpers.
#[allow(dead_code)]
fn null_timestamp_tz() -> Value {
    Value::null(LogicalType::TIMESTAMP_TZ)
}
