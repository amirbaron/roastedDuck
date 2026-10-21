//! DuckDB's `icu-makedate.cpp`: the TIMESTAMPTZ to DATE cast (`ICUMakeDate`) and
//! `make_timestamptz`.

use std::borrow::Cow;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::cast::executor::{RowError, addressable, cast_rows};
use rustdb_function::cast::numeric::{CastNumber, TryCastTo};
use rustdb_function::cast::{BoundCastInfo, CastFunctionSet};
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::executor::{str_at, unary};
use rustdb_types::{DataChunk, Date, LogicalType, StringT, Timestamp, Vector, VectorKind};

use crate::icu4c::calendar::{Calendar, field};
use crate::icu_datefunc::{self, BindData, CalendarPtr, MICROS_PER_MSEC, extract_field, get_time, set_time, set_time_zone};
use crate::icu_timezone::type_id_name;

/// `ICUMakeDate::Operation`: the local date of `instant` in the calendar's time zone.
pub(crate) fn make_date(calendar: &mut Calendar, instant: Timestamp) -> Result<Date> {
    if !instant.is_finite() {
        return Ok(instant.to_date());
    }
    // Extract the time zone parts
    set_time(calendar, instant);
    let era = extract_field(calendar, field::ERA)?;
    let year = extract_field(calendar, field::YEAR)?;
    let mm = extract_field(calendar, field::MONTH)? + 1;
    let dd = extract_field(calendar, field::DATE)?;

    let yyyy = if era != 0 { year } else { -year + 1 };
    Date::from_ymd(yyyy, mm, dd).ok_or_else(|| Error::conversion("Unable to convert TIMESTAMPTZ to DATE"))
}

/// `ICUMakeDate::ToDate`: the local date of `instant` in the session's time zone.
pub(crate) fn to_date(client: &rustdb_function::FunctionClient, instant: Timestamp) -> Result<Date> {
    let mut data = BindData::from_client(client);
    make_date(&mut data.calendar, instant)
}

/// `ICUMakeDate::BindCastToDate`.
fn bind_cast_to_date(casts: &CastFunctionSet, _source: &LogicalType, _target: &LogicalType) -> Result<Option<BoundCastInfo>> {
    let Some(client) = casts.client() else {
        return Ok(None);
    };
    let disabled = client
        .settings()
        .and_then(|settings| settings.current_setting("disable_timestamptz_casts").ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if disabled {
        return Err(Error::binder(
            "Casting from TIMESTAMP WITH TIME ZONE to DATE without an explicit time zone has been disabled  - use \
             \"AT TIME ZONE ...\"",
        ));
    }
    let info = BindData::from_client(client);
    Ok(Some(BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let values = source.values::<Timestamp>();
        cast_rows::<Date>(&source, result, count, params, |slot| {
            make_date(&mut calendar, values[slot]).map_err(RowError::Fatal)
        })
    })))
}

/// `ICUMakeDate::AddCasts`.
pub(crate) fn add_casts(casts: &mut CastFunctionSet) {
    casts.register_bind_function(LogicalType::TIMESTAMP_TZ, LogicalType::DATE, Arc::new(bind_cast_to_date), -1);
}

/// `Cast::Operation<int64_t, int32_t>`.
fn cast_to_i32(value: i64) -> Result<i32> {
    i32::try_from(value).map_err(|_| {
        Error::invalid_input(format!(
            "Type INT64 with value {value} can't be cast because the value is out of range for the destination type \
             INT32"
        ))
    })
}

/// `ICUMakeTimestampTZFunc::Operation`.
fn make_timestamp_tz(calendar: &mut Calendar, yyyy: i64, mm: i64, dd: i64, hr: i64, mn: i64, ss: f64) -> Result<Timestamp> {
    let year = cast_to_i32(yyyy + i64::from(yyyy < 0))?;
    let month = cast_to_i32(mm.checked_sub(1).ok_or_else(|| {
        Error::out_of_range(format!("Overflow in subtraction of INT64 ({mm} - 1)!"))
    })?)?;
    let day = cast_to_i32(dd)?;
    let hour = cast_to_i32(hr)?;
    let min = cast_to_i32(mn)?;

    let secs: i32 = TryCastTo::<i32>::try_cast_to(ss).ok_or_else(|| {
        Error::invalid_input(format!(
            "Type DOUBLE with value {} can't be cast because the value is out of range for the destination type INT32",
            CastNumber::to_text(ss)
        ))
    })?;
    let mut ss = ss - f64::from(secs);
    ss *= 1000.0;
    let millis = ss as i32;
    let micros = ((ss - f64::from(millis)) * MICROS_PER_MSEC as f64).round() as i64;

    calendar.set(field::YEAR, year);
    calendar.set(field::MONTH, month);
    calendar.set(field::DATE, day);
    calendar.set(field::HOUR_OF_DAY, hour);
    calendar.set(field::MINUTE, min);
    calendar.set(field::SECOND, secs);
    calendar.set(field::MILLISECOND, millis);

    get_time(calendar, micros as u64)
}

/// A column prepared for row access (sequences expanded).
fn prepared(vector: &Vector, count: usize) -> Cow<'_, Vector> {
    if vector.kind() == VectorKind::Sequence {
        Cow::Owned(vector.flattened(count))
    } else {
        Cow::Borrowed(vector)
    }
}

/// `ICUMakeTimestampTZFunc::Execute` (the `SenaryExecutor` / `SeptenaryExecutor` loops).
fn make_timestamp_tz_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.calendar.clone();
    let count = args.len();
    let columns: Vec<Cow<'_, Vector>> = args.columns().iter().map(|column| prepared(column, count)).collect();

    // Three cases: no TZ, constant TZ, variable TZ
    let mut variable_tz = false;
    if columns.len() == 7 {
        let tz_vec = &columns[6];
        if tz_vec.is_constant() {
            let Some(tz_id) = str_at(tz_vec, 0) else {
                *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
                return Ok(());
            };
            set_time_zone(&mut calendar, &String::from_utf8_lossy(tz_id))?;
        } else {
            variable_tz = true;
        }
    }

    let constant = columns.iter().all(|column| column.is_constant());
    let rows = if constant { 1 } else { count };
    let parts: Vec<_> = columns[..5].iter().map(|column| column.unified::<i64>()).collect();
    let seconds = columns[5].unified::<f64>();
    let zones = variable_tz.then(|| columns[6].unified::<StringT>());
    result.reset_flat(rows);
    let mut nulls = Vec::new();
    {
        let out = &mut result.values_mut::<Timestamp>()[..rows];
        for (row, out) in out.iter_mut().enumerate() {
            let values: Option<Vec<i64>> = parts.iter().map(|part| part.get(row)).collect();
            let (Some(values), Some(ss)) = (values, seconds.get(row)) else {
                nulls.push(row);
                continue;
            };
            if let Some(zones) = &zones {
                let Some(tz_id) = zones.get(row) else {
                    nulls.push(row);
                    continue;
                };
                let tz_id = String::from_utf8_lossy(columns[6].string_value(&tz_id)).into_owned();
                set_time_zone(&mut calendar, &tz_id)?;
            }
            *out = make_timestamp_tz(
                &mut calendar,
                values[0],
                values[1],
                values[2],
                values[3],
                values[4],
                ss,
            )?;
        }
    }
    for row in nulls {
        result.validity_mut().set_invalid(row, rows);
    }
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `ICUMakeTimestampTZFunc::FromMicros`.
fn from_micros(args: &DataChunk, _state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary::<i64, Timestamp>(args, result, |micros, out| {
        let value = Timestamp(micros);
        if !value.is_finite() {
            return Err(Error::conversion(format!(
                "Timestamp microseconds out of range: {micros}"
            )));
        }
        *out = value;
        Ok(true)
    })
}

/// `make_timestamptz` (`ICUMakeTimestampTZFunc::AddFunction`).
pub(crate) fn make_timestamptz_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("make_timestamptz");
    let parts = vec![LogicalType::BIGINT; 5];
    let mut senary = parts.clone();
    senary.push(LogicalType::DOUBLE);
    set.add(
        ScalarFunction::new("make_timestamptz", senary.clone(), LogicalType::TIMESTAMP_TZ, make_timestamp_tz_rows)
            .with_bind(icu_datefunc::bind),
    );
    let mut septenary = senary;
    septenary.push(LogicalType::VARCHAR);
    set.add(
        ScalarFunction::new("make_timestamptz", septenary, LogicalType::TIMESTAMP_TZ, make_timestamp_tz_rows)
            .with_bind(icu_datefunc::bind),
    );
    set.add(ScalarFunction::new(
        "make_timestamptz",
        vec![LogicalType::BIGINT],
        LogicalType::TIMESTAMP_TZ,
        from_micros,
    ));
    set
}

/// The name of a cast's source type, for errors.
#[allow(dead_code)]
fn source_name(ty: &LogicalType) -> &'static str {
    type_id_name(ty.id())
}
