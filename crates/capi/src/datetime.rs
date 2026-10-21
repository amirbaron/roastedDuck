//! Date and time conversions (DuckDB's `capi/datetime-c.cpp`).
//!
//! DuckDB raises an exception for dates and timestamps that do not exist or are out of
//! range here, which cannot cross the C boundary; these functions return the nearest
//! defined result instead (the epoch for an invalid date, the raw day and time of day for
//! a timestamp whose date does not fit).

use rustdb_types::{Date, Time, TimeTz, Timestamp};

use crate::types::*;

/// `duckdb_date_struct duckdb_from_date(duckdb_date date)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_from_date(date: DuckdbDate) -> DuckdbDateStruct {
    let (year, month, day) = Date(date.days).to_ymd();
    DuckdbDateStruct {
        year,
        month: month as i8,
        day: day as i8,
    }
}

/// `duckdb_date duckdb_to_date(duckdb_date_struct date)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_to_date(date: DuckdbDateStruct) -> DuckdbDate {
    let days = Date::from_ymd(date.year, i32::from(date.month), i32::from(date.day)).map_or(0, |date| date.0);
    DuckdbDate { days }
}

/// `bool duckdb_is_finite_date(duckdb_date date)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_is_finite_date(date: DuckdbDate) -> bool {
    Date(date.days).is_finite()
}

/// `duckdb_time_struct duckdb_from_time(duckdb_time time)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_from_time(time: DuckdbTime) -> DuckdbTimeStruct {
    let (hour, min, sec, micros) = Time(time.micros).to_hms_micros();
    DuckdbTimeStruct {
        hour: hour as i8,
        min: min as i8,
        sec: sec as i8,
        micros,
    }
}

/// `duckdb_time_tz_struct duckdb_from_time_tz(duckdb_time_tz micros)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_from_time_tz(input: DuckdbTimeTz) -> DuckdbTimeTzStruct {
    let time_tz = TimeTz(input.bits);
    DuckdbTimeTzStruct {
        time: duckdb_from_time(DuckdbTime {
            micros: time_tz.time().0,
        }),
        offset: time_tz.offset(),
    }
}

/// `duckdb_time_tz duckdb_create_time_tz(int64_t micros, int32_t offset)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_time_tz(micros: i64, offset: i32) -> DuckdbTimeTz {
    DuckdbTimeTz {
        bits: TimeTz::new(Time(micros), offset).0,
    }
}

/// `duckdb_time duckdb_to_time(duckdb_time_struct time)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_to_time(time: DuckdbTimeStruct) -> DuckdbTime {
    let time = Time::from_hms_micros(
        i32::from(time.hour),
        i32::from(time.min),
        i32::from(time.sec),
        time.micros,
    );
    DuckdbTime { micros: time.0 }
}

/// `duckdb_timestamp_struct duckdb_from_timestamp(duckdb_timestamp ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_from_timestamp(ts: DuckdbTimestamp) -> DuckdbTimestampStruct {
    let timestamp = Timestamp(ts.micros);
    let (date, time) = timestamp
        .to_datetime()
        .unwrap_or((timestamp.to_date(), timestamp.to_time()));
    DuckdbTimestampStruct {
        date: duckdb_from_date(DuckdbDate { days: date.0 }),
        time: duckdb_from_time(DuckdbTime { micros: time.0 }),
    }
}

/// `duckdb_timestamp duckdb_to_timestamp(duckdb_timestamp_struct ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_to_timestamp(ts: DuckdbTimestampStruct) -> DuckdbTimestamp {
    let date = Date(duckdb_to_date(ts.date).days);
    let time = Time(duckdb_to_time(ts.time).micros);
    let micros = Timestamp::from_datetime(date, time).map_or(0, |timestamp| timestamp.0);
    DuckdbTimestamp { micros }
}

/// `bool duckdb_is_finite_timestamp(duckdb_timestamp ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_is_finite_timestamp(ts: DuckdbTimestamp) -> bool {
    Timestamp::is_finite_raw(ts.micros)
}

/// `bool duckdb_is_finite_timestamp_s(duckdb_timestamp_s ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_is_finite_timestamp_s(ts: DuckdbTimestampS) -> bool {
    Timestamp::is_finite_raw(ts.seconds)
}

/// `bool duckdb_is_finite_timestamp_ms(duckdb_timestamp_ms ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_is_finite_timestamp_ms(ts: DuckdbTimestampMs) -> bool {
    Timestamp::is_finite_raw(ts.millis)
}

/// `bool duckdb_is_finite_timestamp_ns(duckdb_timestamp_ns ts)` (capi/datetime-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_is_finite_timestamp_ns(ts: DuckdbTimestampNs) -> bool {
    Timestamp::is_finite_raw(ts.nanos)
}
