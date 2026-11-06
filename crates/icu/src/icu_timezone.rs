//! DuckDB's `icu-timezone.cpp`: `pg_timezone_names()`, `timezone(tz, value)` (`AT TIME
//! ZONE`), `current_localtimestamp()` / `current_localtime()`, and the casts between naive
//! timestamps and TIMESTAMP WITH TIME ZONE and to TIME WITH TIME ZONE.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::cast::executor::{RowError, addressable, cast_rows};
use rustdb_function::cast::{BindCastFn, BoundCastInfo, CastFunctionSet, CastParameters};
use rustdb_function::{
    BindInput, FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability,
    TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_functions::scalar::executor::{Str, binary, unary};
use rustdb_functions::temporal::{time_add_interval, time_tz_normalized_micros};
use rustdb_types::{
    DataChunk, Date, Interval, LogicalType, LogicalTypeId, Time, TimeTz, Timestamp, Value, Vector,
    implicit_cast_cost, timestamp as ts,
};

use crate::icu_datefunc::{
    self, BindData, CalendarPtr, MICROS_PER_MSEC, extract_field, extract_offset_seconds, get_time, set_time,
    set_time_zone,
};
use crate::icu4c::calendar::{Calendar, field};
use crate::icu4c::timezone::TimeZone;
use crate::icu4c::zone_names::ZONE_NAMES;

// ---------------------------------------------------------------------- pg_timezone_names

/// `ICUTimeZoneFunction`: every zone with its abbreviation and its offset now.
fn time_zone_rows(output: &mut DataChunk) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64);
    let mut names = Vec::with_capacity(ZONE_NAMES.len());
    let mut abbrevs = Vec::with_capacity(ZONE_NAMES.len());
    let mut offsets = Vec::with_capacity(ZONE_NAMES.len());
    let mut is_dst = Vec::with_capacity(ZONE_NAMES.len());
    for zone in ZONE_NAMES.iter().take(STANDARD_VECTOR_SIZE) {
        // The LONG name is the one we looked up
        names.push(Value::varchar(zone.name));
        // We don't have the zone tree for determining abbreviated names, so the SHORT name
        // is the shortest, lexicographically first equivalent TZ without a slash.
        abbrevs.push(Value::varchar(zone.abbrev));
        let tz = TimeZone::create(zone.name);
        let (raw_offset_ms, dst_offset_ms) = tz.get_offset(now);
        // What PG reports is the total offset for today, which is the ICU total offset
        // (i.e., "raw") plus the DST offset.
        let total = i64::from(raw_offset_ms + dst_offset_ms);
        offsets.push(Value::interval(Interval::from_micros(total * MICROS_PER_MSEC)));
        is_dst.push(Value::boolean(dst_offset_ms != 0));
    }
    let count = names.len();
    output.set_columns(
        vec![
            column_of(&LogicalType::VARCHAR, &names)?,
            column_of(&LogicalType::VARCHAR, &abbrevs)?,
            column_of(&LogicalType::INTERVAL, &offsets)?,
            column_of(&LogicalType::BOOLEAN, &is_dst)?,
        ],
        count,
    );
    Ok(())
}

/// A flat vector holding `values`.
pub(crate) fn column_of(ty: &LogicalType, values: &[Value]) -> Result<Vector> {
    let mut vector = Vector::flat(ty.clone(), values.len());
    for (row, value) in values.iter().enumerate() {
        vector.set_value(row, value)?;
    }
    Ok(vector)
}

/// `pg_timezone_names()`.
pub(crate) fn pg_timezone_names() -> TableFunctionSet {
    let function = TableFunction::new("pg_timezone_names", vec![])
        .with_bind(|_| {
            Ok(TableFunctionBindOutput {
                return_types: vec![
                    LogicalType::VARCHAR,
                    LogicalType::VARCHAR,
                    LogicalType::INTERVAL,
                    LogicalType::BOOLEAN,
                ],
                names: vec![
                    "name".to_owned().into(),
                    "abbrev".into(),
                    "utc_offset".into(),
                    "is_dst".into(),
                ],
                bind_data: None,
            })
        })
        .with_scan(|_, _, state, output| {
            if state.position > 0 {
                output.set_columns(
                    vec![
                        Vector::flat(LogicalType::VARCHAR, 0),
                        Vector::flat(LogicalType::VARCHAR, 0),
                        Vector::flat(LogicalType::INTERVAL, 0),
                        Vector::flat(LogicalType::BOOLEAN, 0),
                    ],
                    0,
                );
                return Ok(false);
            }
            state.position = 1;
            time_zone_rows(output)?;
            Ok(true)
        });
    let mut set = TableFunctionSet::new("pg_timezone_names");
    set.add(function);
    set
}

// ---------------------------------------------------------------------- naive <-> instant

/// `ICUFromNaiveTimestamp::Operation`: the instant of the local `naive` timestamp in the
/// calendar's time zone.
pub(crate) fn from_naive(calendar: &mut Calendar, naive: Timestamp) -> Result<Timestamp> {
    if !naive.is_finite() {
        return Ok(naive);
    }
    // Extract the parts from the "instant"
    let (local_date, local_time) = naive
        .to_datetime()
        .ok_or_else(|| Error::conversion("Date out of range in timestamp conversion"))?;
    let (year, mm, dd) = local_date.to_ymd();
    let (hr, mn, secs, frac) = local_time.to_hms_micros();
    let millis = frac / MICROS_PER_MSEC as i32;
    let micros = (frac % MICROS_PER_MSEC as i32) as u64;

    // Use them to set the time in the time zone
    calendar.set(field::YEAR, year);
    calendar.set(field::MONTH, mm - 1);
    calendar.set(field::DATE, dd);
    calendar.set(field::HOUR_OF_DAY, hr);
    calendar.set(field::MINUTE, mn);
    calendar.set(field::SECOND, secs);
    calendar.set(field::MILLISECOND, millis);

    get_time(calendar, micros)
}

/// `ICUToNaiveTimestamp::Operation`: the local timestamp of `instant` in the calendar's
/// time zone.
pub(crate) fn to_naive(calendar: &mut Calendar, instant: Timestamp) -> Result<Timestamp> {
    if !instant.is_finite() {
        return Ok(instant);
    }
    // Extract the time zone parts
    let mut micros = set_time(calendar, instant) as i32;
    let era = extract_field(calendar, field::ERA)?;
    let year = extract_field(calendar, field::YEAR)?;
    let mm = extract_field(calendar, field::MONTH)? + 1;
    let dd = extract_field(calendar, field::DATE)?;

    let yyyy = if era != 0 { year } else { -year + 1 };
    let local_date = Date::from_ymd(yyyy, mm, dd)
        .ok_or_else(|| Error::conversion("Unable to convert TIMESTAMPTZ to local date"))?;

    let hr = extract_field(calendar, field::HOUR_OF_DAY)?;
    let mn = extract_field(calendar, field::MINUTE)?;
    let secs = extract_field(calendar, field::SECOND)?;
    let millis = extract_field(calendar, field::MILLISECOND)?;

    micros += millis * MICROS_PER_MSEC as i32;
    let local_time = Time::from_hms_micros(hr, mn, secs, micros);

    Timestamp::from_datetime(local_date, local_time)
        .ok_or_else(|| Error::conversion("Unable to convert TIMESTAMPTZ to local TIMESTAMP"))
}

/// The `CastData` of the ICU casts: the bind data of the connection's settings.
fn cast_bind_data(casts: &CastFunctionSet) -> Option<Arc<BindData>> {
    casts
        .client()
        .map(|client| Arc::new(BindData::from_client(client)))
}

/// Whether `disable_timestamptz_casts` is set for the connection.
fn timestamptz_casts_disabled(client: &FunctionClient) -> bool {
    client
        .settings()
        .and_then(|settings| settings.current_setting("disable_timestamptz_casts").ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// The DuckDB name of a type id (`LogicalTypeIdToString`).
pub(crate) fn type_id_name(id: LogicalTypeId) -> &'static str {
    match id {
        LogicalTypeId::Timestamp => "TIMESTAMP",
        LogicalTypeId::TimestampMs => "TIMESTAMP_MS",
        LogicalTypeId::TimestampNs => "TIMESTAMP_NS",
        LogicalTypeId::TimestampSec => "TIMESTAMP_S",
        LogicalTypeId::Date => "DATE",
        LogicalTypeId::TimestampTz => "TIMESTAMP WITH TIME ZONE",
        _ => "UNKNOWN",
    }
}

/// `ICUFromNaiveTimestamp::BindCastFromNaive`.
fn bind_cast_from_naive(
    casts: &CastFunctionSet,
    source: &LogicalType,
    _target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let Some(client) = casts.client() else {
        return Ok(None);
    };
    if timestamptz_casts_disabled(client) {
        return Err(Error::binder(
            "Casting from TIMESTAMP to TIMESTAMP WITH TIME ZONE without an explicit time zone has been disabled  - \
             use \"AT TIME ZONE ...\"",
        ));
    }
    let Some(info) = cast_bind_data(casts) else {
        return Ok(None);
    };
    let to_us: fn(i64) -> Result<Timestamp> = match source.id() {
        LogicalTypeId::Timestamp => |value| Ok(Timestamp(value)),
        LogicalTypeId::TimestampMs => ts::ms_to_us,
        LogicalTypeId::TimestampNs => |value| Ok(ts::ns_to_us(value)),
        LogicalTypeId::TimestampSec => ts::sec_to_us,
        LogicalTypeId::Date => |value| {
            let date = Date(value as i32);
            Timestamp::from_date(date).ok_or_else(|| {
                Error::invalid_input(format!(
                    "Type INT32 with value {date} can't be cast to the destination type INT64"
                ))
            })
        },
        other => {
            return Err(Error::internal(format!(
                "Type {} not handled in BindCastFromNaive",
                type_id_name(other)
            )));
        }
    };
    let is_date = source.id() == LogicalTypeId::Date;
    Ok(Some(BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let raw: Vec<i64> = if is_date {
            source
                .values::<Date>()
                .iter()
                .map(|date| i64::from(date.0))
                .collect()
        } else {
            source.values::<i64>().to_vec()
        };
        cast_rows::<Timestamp>(&source, result, count, params, |slot| {
            let naive = to_us(raw[slot]).map_err(RowError::Fatal)?;
            from_naive(&mut calendar, naive).map_err(RowError::Fatal)
        })
    })))
}

/// `ICUToNaiveTimestamp::BindCastToNaive`.
fn bind_cast_to_naive(
    casts: &CastFunctionSet,
    _source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let Some(client) = casts.client() else {
        return Ok(None);
    };
    if timestamptz_casts_disabled(client) {
        return Err(Error::binder(format!(
            "Casting from TIMESTAMP WITH TIME ZONE to {} without an explicit time zone has been disabled  - use \
             \"AT TIME ZONE ...\"",
            type_id_name(target.id())
        )));
    }
    let Some(info) = cast_bind_data(casts) else {
        return Ok(None);
    };
    let from_us: fn(Timestamp) -> Result<i64> = match target.id() {
        LogicalTypeId::Timestamp => |value| Ok(value.0),
        LogicalTypeId::TimestampMs => |value| Ok(ts::us_to_ms(value)),
        LogicalTypeId::TimestampNs => ts::us_to_ns,
        LogicalTypeId::TimestampSec => |value| Ok(ts::us_to_sec(value)),
        other => {
            return Err(Error::internal(format!(
                "Type {} not handled in BindCastToNaive",
                type_id_name(other)
            )));
        }
    };
    Ok(Some(BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let values = source.values::<Timestamp>();
        cast_rows::<i64>(&source, result, count, params, |slot| {
            let naive = to_naive(&mut calendar, values[slot]).map_err(RowError::Fatal)?;
            from_us(naive).map_err(RowError::Fatal)
        })
    })))
}

// ---------------------------------------------------------------------- TIMETZ

/// `ICUToTimeTZ::Operation`: `timetz` moved to the calendar's offset.
pub(crate) fn to_time_tz_operation(calendar: &mut Calendar, timetz: TimeTz) -> Result<TimeTz> {
    // Normalise to +00:00, add TZ offset, then set offset to TZ
    let time = Time(time_tz_normalized_micros(timetz));
    let offset = extract_offset_seconds(calendar)?;
    let (time, _) = time_add_interval(
        time,
        Interval::new(0, 0, i64::from(offset) * Interval::MICROS_PER_SEC),
    );
    Ok(TimeTz::new(time, offset))
}

/// `ICUToTimeTZ::ToTimeTZ`: the local time and offset of `instant`, `None` if it has none.
pub(crate) fn to_time_tz(calendar: &mut Calendar, instant: Timestamp) -> Result<Option<TimeTz>> {
    if !instant.is_finite() {
        return Ok(None);
    }
    // Time in current TZ
    let mut micros = set_time(calendar, instant) as i32;
    let hour = extract_field(calendar, field::HOUR_OF_DAY)?;
    let minute = extract_field(calendar, field::MINUTE)?;
    let second = extract_field(calendar, field::SECOND)?;
    let millis = extract_field(calendar, field::MILLISECOND)?;
    micros += millis * MICROS_PER_MSEC as i32;
    if !Time::is_valid_hms(hour, minute, second, micros) {
        return Ok(None);
    }
    let time = Time::from_hms_micros(hour, minute, second, micros);

    // Offset in current TZ
    let offset = extract_offset_seconds(calendar)?;
    Ok(Some(TimeTz::new(time, offset)))
}

/// `ICUToTimeTZ::BindCastToTimeTZ`.
#[allow(clippy::unnecessary_wraps)] // a cast bind callback
fn bind_cast_to_time_tz(
    casts: &CastFunctionSet,
    _source: &LogicalType,
    _target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    let Some(info) = cast_bind_data(casts) else {
        return Ok(None);
    };
    Ok(Some(BoundCastInfo::new(move |source, result, count, params| {
        let mut calendar: CalendarPtr = info.calendar.clone();
        let source = addressable(source, count);
        let values = source.values::<Timestamp>();
        cast_rows::<TimeTz>(&source, result, count, params, |slot| {
            to_time_tz(&mut calendar, values[slot])
                .map_err(RowError::Fatal)?
                .ok_or(RowError::Null)
        })
    })))
}

/// Registers the casts of `icu-timezone.cpp` (`ICUFromNaiveTimestamp::AddCasts`,
/// `ICUToNaiveTimestamp::AddCasts`, `ICUToTimeTZ::AddCasts`).
pub(crate) fn add_casts(casts: &mut CastFunctionSet) {
    let from_naive: BindCastFn = Arc::new(bind_cast_from_naive);
    for source in [
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_MS,
        LogicalType::TIMESTAMP_NS,
        LogicalType::TIMESTAMP_S,
        LogicalType::DATE,
    ] {
        let cost = implicit_cast_cost(&source, &LogicalType::TIMESTAMP_TZ);
        casts.register_bind_function(source, LogicalType::TIMESTAMP_TZ, Arc::clone(&from_naive), cost);
    }
    let to_naive: BindCastFn = Arc::new(bind_cast_to_naive);
    for target in [
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_MS,
        LogicalType::TIMESTAMP_NS,
        LogicalType::TIMESTAMP_S,
    ] {
        let cost = implicit_cast_cost(&LogicalType::TIMESTAMP_TZ, &target);
        casts.register_bind_function(LogicalType::TIMESTAMP_TZ, target, Arc::clone(&to_naive), cost);
    }
    let cost = implicit_cast_cost(&LogicalType::TIMESTAMP_TZ, &LogicalType::TIME_TZ);
    casts.register_bind_function(
        LogicalType::TIMESTAMP_TZ,
        LogicalType::TIME_TZ,
        Arc::new(bind_cast_to_time_tz),
        cost,
    );
}

// ---------------------------------------------------------------------- timezone()

/// The operation `timezone(tz, value)` applies in the calendar's (new) zone.
type TimeZoneOp<T> = fn(&mut Calendar, T) -> Result<T>;

/// `ICUTimeZoneFunc::Execute`.
fn time_zone_execute<T: rustdb_types::NativeType + Copy>(
    args: &DataChunk,
    state: &FunctionState,
    result: &mut Vector,
    op: TimeZoneOp<T>,
    is_finite: fn(T) -> bool,
) -> Result<()> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.calendar.clone();

    // Two cases: constant TZ, variable TZ
    let tz_vec = args.column(0);
    if tz_vec.is_constant() {
        if tz_vec.is_null(0) {
            *result = Vector::constant_null(result.logical_type().clone());
            return Ok(());
        }
        let tz_id = String::from_utf8_lossy(
            rustdb_functions::scalar::executor::str_at(tz_vec, 0).unwrap_or_default(),
        )
        .into_owned();
        set_time_zone(&mut calendar, &tz_id)?;
        let columns = DataChunk::new(vec![args.column(1).clone()], args.len());
        unary::<T, T>(&columns, result, |value, out| {
            *out = op(&mut calendar, value)?;
            Ok(true)
        })
    } else {
        binary::<Str, T, T>(args, result, |tz_id, value, out| {
            if is_finite(value) {
                set_time_zone(&mut calendar, &String::from_utf8_lossy(tz_id))?;
                *out = op(&mut calendar, value)?;
            } else {
                *out = value;
            }
            Ok(true)
        })
    }
}

/// `timezone(VARCHAR, TIMESTAMP)`, `timezone(VARCHAR, TIMESTAMPTZ)` and
/// `timezone(VARCHAR, TIMETZ)` (`ICUTimeZoneFunc::AddFunction`).
pub(crate) fn timezone_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("timezone");
    set.add(
        ScalarFunction::new(
            "timezone",
            vec![LogicalType::VARCHAR, LogicalType::TIMESTAMP],
            LogicalType::TIMESTAMP_TZ,
            |args, state, result| {
                time_zone_execute::<Timestamp>(args, state, result, from_naive, Timestamp::is_finite)
            },
        )
        .with_bind(icu_datefunc::bind),
    );
    set.add(
        ScalarFunction::new(
            "timezone",
            vec![LogicalType::VARCHAR, LogicalType::TIMESTAMP_TZ],
            LogicalType::TIMESTAMP,
            |args, state, result| {
                time_zone_execute::<Timestamp>(args, state, result, to_naive, Timestamp::is_finite)
            },
        )
        .with_bind(icu_datefunc::bind),
    );
    set.add(
        ScalarFunction::new(
            "timezone",
            vec![LogicalType::VARCHAR, LogicalType::TIME_TZ],
            LogicalType::TIME_TZ,
            |args, state, result| {
                time_zone_execute::<TimeTz>(args, state, result, to_time_tz_operation, |_| true)
            },
        )
        .with_bind(icu_datefunc::bind),
    );
    set
}

// ---------------------------------------------------------------------- local now

/// `ICULocalTimestampFunc::BindDataNow`: the calendar and the transaction's start.
#[derive(Debug)]
struct BindDataNow {
    data: BindData,
    now: Timestamp,
}

impl FunctionData for BindDataNow {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.now == other.now && self.data.equals(&other.data))
    }
}

/// `ICULocalTimestampFunc::BindNow`.
#[allow(clippy::unnecessary_wraps)] // a bind callback
fn bind_now(_function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    Ok(Some(Arc::new(BindDataNow {
        data: BindData::from_client(input.client),
        now: Timestamp(input.client.transaction_start_micros()),
    })))
}

/// `ICULocalTimestampFunc::GetLocalTimestamp`.
fn local_timestamp(state: &FunctionState) -> Result<Timestamp> {
    let info: &BindDataNow = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    to_naive(&mut calendar, info.now)
}

/// `current_localtimestamp()` and `current_localtime()`.
pub(crate) fn local_now_functions() -> Vec<ScalarFunctionSet> {
    let mut timestamp = ScalarFunction::new(
        "current_localtimestamp",
        vec![],
        LogicalType::TIMESTAMP,
        |_, state, result| {
            let value = local_timestamp(state)?;
            *result = Vector::constant(&Value::timestamp(value))?;
            Ok(())
        },
    )
    .with_bind(bind_now);
    timestamp.stability = Stability::Consistent;
    let mut timestamp_set = ScalarFunctionSet::new("current_localtimestamp");
    timestamp_set.add(timestamp);

    let time = ScalarFunction::new(
        "current_localtime",
        vec![],
        LogicalType::TIME,
        |_, state, result| {
            let local = local_timestamp(state)?;
            *result = Vector::constant(&Value::time(local.to_time()))?;
            Ok(())
        },
    )
    .with_bind(bind_now);
    let mut time_set = ScalarFunctionSet::new("current_localtime");
    time_set.add(time);
    vec![timestamp_set, time_set]
}

/// Unused parameter marker for casts that do not look at their parameters.
#[allow(dead_code)]
fn unused(_: &mut CastParameters) {}
