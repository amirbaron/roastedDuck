//! Temporal casts, with expectations taken from DuckDB 1.5.6 (`TimeZone='UTC'`).
#![allow(clippy::unwrap_used)] // Test helpers unwrap like the tests themselves.

use roastedduck_function::cast::{CastFunctionSet, CastParameters, ValueCast};
use roastedduck_types::{LogicalType, Value};

/// `CAST(CAST(text AS source) AS target)` rendered as VARCHAR, or the error message.
fn cast(text: &str, source: &LogicalType, target: &LogicalType, try_cast: bool) -> String {
    let set = CastFunctionSet::new();
    let value = Value::varchar(text)
        .cast_with(&set, source, &mut CastParameters::new())
        .unwrap();
    let mut params = if try_cast {
        CastParameters::try_cast()
    } else {
        CastParameters::new()
    };
    match value.cast_with(&set, target, &mut params) {
        Ok(result) => result.to_varchar().unwrap_or_else(|e| e.to_string()),
        Err(error) => error.to_string(),
    }
}

fn from_text(text: &str, target: &LogicalType) -> String {
    cast(text, &LogicalType::VARCHAR, target, false)
}

#[test]
fn dates_to_timestamps() {
    let date = LogicalType::DATE;
    let error =
        "Conversion Error: Type INT32 with value 5881580-07-10 can't be cast to the destination type INT64";
    assert_eq!(
        cast("5881580-07-10", &date, &LogicalType::TIMESTAMP, false),
        error
    );
    assert_eq!(
        cast("5881580-07-10", &date, &LogicalType::TIMESTAMP, true),
        "NULL"
    );
    assert_eq!(
        cast("5881580-07-10", &date, &LogicalType::TIMESTAMP_NS, false),
        error
    );
    assert_eq!(
        cast("2262-05-01", &date, &LogicalType::TIMESTAMP_NS, false),
        "Conversion Error: Type INT32 with value 2262-05-01 can't be cast to the destination type INT64"
    );
    assert_eq!(
        cast("5881580-07-10", &date, &LogicalType::TIMESTAMP_S, false),
        error
    );
}

#[test]
fn timestamps_to_times() {
    let timestamp = LogicalType::TIMESTAMP;
    let error =
        "Invalid Input Error: Type INT64 with value infinity can't be cast to the destination type INT64";
    assert_eq!(cast("infinity", &timestamp, &LogicalType::TIME, false), error);
    assert_eq!(cast("infinity", &timestamp, &LogicalType::TIME, true), error);
    assert_eq!(cast("infinity", &timestamp, &LogicalType::TIME_TZ, false), error);
    let out_of_ns = "294247-01-10 04:00:54.775806";
    assert_eq!(
        cast(out_of_ns, &timestamp, &LogicalType::TIMESTAMP_NS, true),
        "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)"
    );
    assert_eq!(
        cast("-infinity", &timestamp, &LogicalType::DATE, false),
        "-infinity"
    );
    assert_eq!(
        cast("infinity", &LogicalType::TIMESTAMP_NS, &timestamp, false),
        "infinity"
    );
    assert_eq!(
        cast(
            "2021-01-01 01:02:03.456789",
            &timestamp,
            &LogicalType::TIMESTAMP_TZ,
            false
        ),
        "2021-01-01 01:02:03.456789+00"
    );
    assert_eq!(
        cast(
            "2021-01-01 01:02:03.456",
            &LogicalType::TIMESTAMP_MS,
            &timestamp,
            false
        ),
        "2021-01-01 01:02:03.456"
    );
    assert_eq!(
        cast(
            "2021-01-01 01:02:03",
            &LogicalType::TIMESTAMP_S,
            &LogicalType::VARCHAR,
            false
        ),
        "2021-01-01 01:02:03"
    );
}

#[test]
fn times() {
    assert_eq!(
        cast(
            "23:59:59.999999",
            &LogicalType::TIME,
            &LogicalType::TIME_NS,
            false
        ),
        "23:59:59.999999"
    );
    assert_eq!(
        cast("12:00:00+05:30", &LogicalType::TIME_TZ, &LogicalType::TIME, false),
        "12:00:00"
    );
    assert_eq!(
        cast(
            "12:00:00.123456789",
            &LogicalType::TIME_NS,
            &LogicalType::TIME,
            false
        ),
        "12:00:00.123457"
    );
}

#[test]
fn strings_to_temporals() {
    assert_eq!(
        from_text("x", &LogicalType::TIMESTAMP_NS),
        "Conversion Error: Could not convert string 'x' to INT64"
    );
    assert_eq!(
        from_text("x", &LogicalType::TIMESTAMP_MS),
        "Conversion Error: Could not convert string 'x' to INT64"
    );
    assert_eq!(
        from_text("x", &LogicalType::DATE),
        "Conversion Error: invalid date field format: \"x\", expected format is (YYYY-MM-DD)"
    );
    assert_eq!(
        from_text("2021-13-01", &LogicalType::DATE),
        "Conversion Error: date field value out of range: \"2021-13-01\""
    );
    assert_eq!(
        from_text("x", &LogicalType::TIME),
        "Conversion Error: time field value out of range: \"x\", expected format is ([YYYY-MM-DD ]HH:MM:SS[.MS])"
    );
    assert_eq!(
        from_text("x", &LogicalType::TIMESTAMP),
        "Conversion Error: invalid timestamp field format: \"x\", expected format is (YYYY-MM-DD HH:MM:SS[.US][±HH[:MM[:SS]]| ZONE])"
    );
    assert_eq!(
        from_text("x", &LogicalType::INTERVAL),
        "Conversion Error: Could not convert string 'x' to INTERVAL"
    );
    assert_eq!(
        from_text("1000000000 years", &LogicalType::INTERVAL),
        "Invalid Input Error: Type INT64 with value 12000000000 can't be cast because the value is out of range for the destination type INT32"
    );
    assert_eq!(
        cast(
            "1000000000 years",
            &LogicalType::VARCHAR,
            &LogicalType::INTERVAL,
            true
        ),
        "NULL"
    );
    assert_eq!(
        from_text("1 year 2 days 03:04:05.6", &LogicalType::INTERVAL),
        "1 year 2 days 03:04:05.6"
    );
}
