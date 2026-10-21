//! DuckDB's `icu-timebucket.cpp`: `time_bucket` over TIMESTAMPTZ, bucketing days and
//! months on the calendar of a time zone (UTC unless one is given).

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::date::date_part::Part;
use rustdb_functions::scalar::executor::{Str, addressable, binary, str_at, ternary};
use rustdb_types::{DataChunk, Interval, LogicalType, Timestamp, Vector};

use crate::icu_dateadd::{add, sub_interval};
use crate::icu_datefunc::{self, BindData, TzCalendar, get_time_unsafe, set_time, set_time_zone};
use crate::icu_datesub::subtract_factory;
use crate::icu_datetrunc::truncation_factory;
use crate::icu_timezone::from_naive;

/// Use 2000-01-03 00:00:00 (Monday) as origin when bucket_width is days, hours, ... for
/// TimescaleDB compatibility. There are 10959 days between 1970-01-01 and 2000-01-03.
const DEFAULT_ORIGIN_MICROS_1: i64 = 10959 * Interval::MICROS_PER_DAY;
/// Use 2000-01-01 as origin when bucket_width is months, years, ... for TimescaleDB
/// compatibility. There are 10957 days between 1970-01-01 and 2000-01-01.
const DEFAULT_ORIGIN_MICROS_2: i64 = 10957 * Interval::MICROS_PER_DAY;

/// `BucketWidthType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BucketWidthType {
    ConvertibleToMicros,
    ConvertibleToDays,
    ConvertibleToMonths,
    Unclassified,
}

/// `ClassifyBucketWidth`.
fn classify_bucket_width(bucket_width: Interval) -> BucketWidthType {
    if bucket_width.months == 0 && bucket_width.days == 0 && bucket_width.micros > 0 {
        BucketWidthType::ConvertibleToMicros
    } else if bucket_width.months == 0 && bucket_width.days >= 0 && bucket_width.micros == 0 {
        BucketWidthType::ConvertibleToDays
    } else if bucket_width.months > 0 && bucket_width.days == 0 && bucket_width.micros == 0 {
        BucketWidthType::ConvertibleToMonths
    } else {
        BucketWidthType::Unclassified
    }
}

/// `ClassifyBucketWidthErrorThrow`.
fn classify_bucket_width_error_throw(bucket_width: Interval) -> Result<BucketWidthType> {
    let period_error = || Error::not_implemented("Period must be greater than 0");
    if bucket_width.months == 0 && bucket_width.days == 0 {
        if bucket_width.micros <= 0 {
            return Err(period_error());
        }
        Ok(BucketWidthType::ConvertibleToMicros)
    } else if bucket_width.months == 0 && bucket_width.micros == 0 {
        if bucket_width.days <= 0 {
            return Err(period_error());
        }
        Ok(BucketWidthType::ConvertibleToDays)
    } else if bucket_width.days == 0 && bucket_width.micros == 0 {
        if bucket_width.months <= 0 {
            return Err(period_error());
        }
        Ok(BucketWidthType::ConvertibleToMonths)
    } else if bucket_width.months == 0 {
        Err(Error::not_implemented("Day intervals cannot have time component"))
    } else {
        Err(Error::not_implemented("Month intervals cannot have day or time component"))
    }
}

/// `SubtractOperatorOverflowCheck` on `int64_t`.
fn subtract_checked(left: i64, right: i64) -> Result<i64> {
    left.checked_sub(right)
        .ok_or_else(|| Error::out_of_range(format!("Overflow in subtraction of INT64 ({left} - {right})!")))
}

/// `WidthConvertibleToMicrosCommon`.
fn width_convertible_to_micros_common(
    bucket_width_micros: i64,
    ts: Timestamp,
    origin: Timestamp,
    calendar: &mut TzCalendar,
) -> Result<Timestamp> {
    if bucket_width_micros == 0 {
        return Err(Error::out_of_range("Can't bucket using zero microseconds"));
    }
    let ts_micros = subtract_checked(ts.0, origin.0)?;
    let mut result_micros = (ts_micros / bucket_width_micros) * bucket_width_micros;
    if ts_micros < 0 && ts_micros % bucket_width_micros != 0 {
        result_micros = subtract_checked(result_micros, bucket_width_micros)?;
    }
    add(calendar, origin, Interval::new(0, 0, result_micros))
}

/// `WidthConvertibleToDaysCommon`.
fn width_convertible_to_days_common(
    bucket_width_days: i32,
    ts: Timestamp,
    origin: Timestamp,
    calendar: &mut TzCalendar,
) -> Result<Timestamp> {
    if bucket_width_days == 0 {
        return Err(Error::out_of_range("Can't bucket using zero days"));
    }
    let sub_days = subtract_factory(Part::Day)?;
    let ts_days = sub_days(&mut calendar.calendar, origin, ts)?;
    let result_days = (ts_days / i64::from(bucket_width_days)) * i64::from(bucket_width_days);
    let result_days =
        i32::try_from(result_days).map_err(|_| Error::out_of_range("Timestamp out of range"))?;
    let mut bucket = add(calendar, origin, Interval::new(0, result_days, 0))?;
    if ts < bucket {
        bucket = add(calendar, bucket, Interval::new(0, -bucket_width_days, 0))?;
    }
    Ok(bucket)
}

/// `WidthConvertibleToMonthsCommon`.
fn width_convertible_to_months_common(
    bucket_width_months: i32,
    ts: Timestamp,
    origin: Timestamp,
    calendar_p: &mut TzCalendar,
) -> Result<Timestamp> {
    if bucket_width_months == 0 {
        return Err(Error::out_of_range("Can't bucket using zero months"));
    }
    let trunc_months = truncation_factory(Part::Month)?;
    let sub_months = subtract_factory(Part::Month)?;

    let calendar = &mut calendar_p.calendar;
    let mut tmp_micros = set_time(calendar, ts);
    trunc_months(calendar, &mut tmp_micros)?;
    let truncated_ts = get_time_unsafe(calendar, tmp_micros)?;

    let mut tmp_micros = set_time(calendar, origin);
    trunc_months(calendar, &mut tmp_micros)?;
    let truncated_origin = get_time_unsafe(calendar, tmp_micros)?;

    let ts_months = sub_months(calendar, truncated_origin, truncated_ts)?;
    let ts_months = i32::try_from(ts_months).map_err(|_| {
        Error::conversion(format!(
            "Type INT64 with value {ts_months} can't be cast because the value is out of range for the destination \
             type INT32"
        ))
    })?;
    let mut result_months = (ts_months / bucket_width_months) * bucket_width_months;
    if ts_months < 0 && ts_months % bucket_width_months != 0 {
        result_months = result_months.checked_sub(bucket_width_months).ok_or_else(|| {
            Error::out_of_range(format!(
                "Overflow in subtraction of INT32 ({result_months} - {bucket_width_months})!"
            ))
        })?;
    }
    add(calendar_p, truncated_origin, Interval::new(result_months, 0, 0))
}

/// The bucket of `ts` for a classified width (the `Width*Operator`s); infinite values
/// stay as they are.
fn bucket(
    kind: BucketWidthType,
    bucket_width: Interval,
    ts: Timestamp,
    origin: Timestamp,
    calendar: &mut TzCalendar,
) -> Result<Timestamp> {
    if !ts.is_finite() {
        return Ok(ts);
    }
    match kind {
        BucketWidthType::ConvertibleToMicros => {
            width_convertible_to_micros_common(bucket_width.micros, ts, origin, calendar)
        }
        BucketWidthType::ConvertibleToDays => width_convertible_to_days_common(bucket_width.days, ts, origin, calendar),
        BucketWidthType::ConvertibleToMonths => {
            width_convertible_to_months_common(bucket_width.months, ts, origin, calendar)
        }
        BucketWidthType::Unclassified => Err(Error::not_implemented(
            "Bucket type not implemented for ICU TIME_BUCKET",
        )),
    }
}

/// The default origin of a width class.
fn default_origin(kind: BucketWidthType) -> Timestamp {
    if kind == BucketWidthType::ConvertibleToMonths {
        Timestamp(DEFAULT_ORIGIN_MICROS_2)
    } else {
        Timestamp(DEFAULT_ORIGIN_MICROS_1)
    }
}

/// `BinaryOperator`: classifies the width (with errors) per row.
fn binary_operator(bucket_width: Interval, ts: Timestamp, calendar: &mut TzCalendar) -> Result<Timestamp> {
    let kind = classify_bucket_width_error_throw(bucket_width)?;
    bucket(kind, bucket_width, ts, default_origin(kind), calendar)
}

/// The `Offset*TernaryOperator`s: buckets `ts - offset`, then adds the offset back.
fn offset_bucket(
    kind: BucketWidthType,
    bucket_width: Interval,
    ts: Timestamp,
    offset: Interval,
    calendar: &mut TzCalendar,
) -> Result<Timestamp> {
    if !ts.is_finite() {
        return Ok(ts);
    }
    let shifted = sub_interval(calendar, ts, offset)?;
    let bucketed = bucket(kind, bucket_width, shifted, default_origin(kind), calendar)?;
    add(calendar, bucketed, offset)
}

/// `OffsetTernaryOperator`.
fn offset_operator(bucket_width: Interval, ts: Timestamp, offset: Interval, calendar: &mut TzCalendar) -> Result<Timestamp> {
    let kind = classify_bucket_width_error_throw(bucket_width)?;
    offset_bucket(kind, bucket_width, ts, offset, calendar)
}

/// `OriginTernaryOperator`: NULL for an infinite origin.
fn origin_operator(
    bucket_width: Interval,
    ts: Timestamp,
    origin: Timestamp,
    calendar: &mut TzCalendar,
) -> Result<Option<Timestamp>> {
    if !origin.is_finite() {
        return Ok(None);
    }
    let kind = classify_bucket_width_error_throw(bucket_width)?;
    bucket(kind, bucket_width, ts, origin, calendar).map(Some)
}

/// `TimeZoneTernaryOperator`: the origin is the default one in time zone `tz`.
fn time_zone_operator(bucket_width: Interval, ts: Timestamp, tz: &str, calendar: &mut TzCalendar) -> Result<Timestamp> {
    set_time_zone(&mut calendar.calendar, tz)?;
    let kind = classify_bucket_width_error_throw(bucket_width)?;
    let origin = from_naive(&mut calendar.calendar, default_origin(kind))?;
    bucket(kind, bucket_width, ts, origin, calendar)
}

/// The calendar of a call, in UTC unless `keep_zone`.
fn calendar_of(state: &FunctionState, keep_zone: bool) -> Result<TzCalendar> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar = TzCalendar::new(&info.calendar, &info.cal_setting);
    if !keep_zone {
        set_time_zone(&mut calendar.calendar, "UTC")?;
    }
    Ok(calendar)
}

/// The constant interval of a constant vector, `None` if NULL.
fn constant_interval(vector: &Vector) -> Option<Interval> {
    vector.unified::<Interval>().get(0)
}

/// `ICUTimeBucketFunction`.
fn time_bucket_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let mut calendar = calendar_of(state, false)?;
    let bucket_width_arg = args.column(0);
    if bucket_width_arg.is_constant() {
        let Some(bucket_width) = constant_interval(bucket_width_arg) else {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        };
        let kind = classify_bucket_width(bucket_width);
        binary::<Interval, Timestamp, Timestamp>(args, result, |bucket_width, ts, out| {
            *out = if kind == BucketWidthType::Unclassified {
                binary_operator(bucket_width, ts, &mut calendar)?
            } else {
                bucket(kind, bucket_width, ts, default_origin(kind), &mut calendar)?
            };
            Ok(true)
        })
    } else {
        binary::<Interval, Timestamp, Timestamp>(args, result, |bucket_width, ts, out| {
            *out = binary_operator(bucket_width, ts, &mut calendar)?;
            Ok(true)
        })
    }
}

/// `ICUTimeBucketOffsetFunction`.
fn time_bucket_offset_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let mut calendar = calendar_of(state, false)?;
    let bucket_width_arg = args.column(0);
    if bucket_width_arg.is_constant() {
        let Some(bucket_width) = constant_interval(bucket_width_arg) else {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        };
        let kind = classify_bucket_width(bucket_width);
        ternary::<Interval, Timestamp, Interval, Timestamp>(args, result, |bucket_width, ts, offset, out| {
            *out = if kind == BucketWidthType::Unclassified {
                offset_operator(bucket_width, ts, offset, &mut calendar)?
            } else {
                offset_bucket(kind, bucket_width, ts, offset, &mut calendar)?
            };
            Ok(true)
        })
    } else {
        ternary::<Interval, Timestamp, Interval, Timestamp>(args, result, |bucket_width, ts, offset, out| {
            *out = offset_operator(bucket_width, ts, offset, &mut calendar)?;
            Ok(true)
        })
    }
}

/// `ICUTimeBucketOriginFunction`.
fn time_bucket_origin_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let mut calendar = calendar_of(state, false)?;
    let bucket_width_arg = args.column(0);
    let origin_arg = args.column(2);
    if bucket_width_arg.is_constant() && origin_arg.is_constant() {
        let origin = origin_arg.unified::<Timestamp>().get(0);
        let (Some(bucket_width), Some(origin)) = (constant_interval(bucket_width_arg), origin) else {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        };
        if !origin.is_finite() {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        }
        let kind = classify_bucket_width(bucket_width);
        ternary::<Interval, Timestamp, Timestamp, Timestamp>(args, result, |bucket_width, ts, origin, out| {
            if kind == BucketWidthType::Unclassified {
                match origin_operator(bucket_width, ts, origin, &mut calendar)? {
                    Some(value) => {
                        *out = value;
                        Ok(true)
                    }
                    None => Ok(false),
                }
            } else {
                *out = bucket(kind, bucket_width, ts, origin, &mut calendar)?;
                Ok(true)
            }
        })
    } else {
        ternary::<Interval, Timestamp, Timestamp, Timestamp>(args, result, |bucket_width, ts, origin, out| {
            match origin_operator(bucket_width, ts, origin, &mut calendar)? {
                Some(value) => {
                    *out = value;
                    Ok(true)
                }
                None => Ok(false),
            }
        })
    }
}

/// `ICUTimeBucketTimeZoneFunction`.
fn time_bucket_time_zone_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let mut calendar = calendar_of(state, true)?;
    let count = args.len();
    let bucket_width_arg = args.column(0);
    let tz_arg = addressable(args.column(2), count);
    if bucket_width_arg.is_constant() && tz_arg.is_constant() {
        let tz = str_at(&tz_arg, 0).map(|tz| String::from_utf8_lossy(tz).into_owned());
        let (Some(bucket_width), Some(tz)) = (constant_interval(bucket_width_arg), tz) else {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        };
        set_time_zone(&mut calendar.calendar, &tz)?;
        let kind = classify_bucket_width(bucket_width);
        if kind == BucketWidthType::Unclassified {
            return ternary::<Interval, Timestamp, Str, Timestamp>(args, result, |bucket_width, ts, tz, out| {
                *out = time_zone_operator(bucket_width, ts, &String::from_utf8_lossy(tz), &mut calendar)?;
                Ok(true)
            });
        }
        let origin = from_naive(&mut calendar.calendar, default_origin(kind))?;
        let columns = DataChunk::new(vec![args.column(0).clone(), args.column(1).clone()], count);
        binary::<Interval, Timestamp, Timestamp>(&columns, result, |bucket_width, ts, out| {
            *out = bucket(kind, bucket_width, ts, origin, &mut calendar)?;
            Ok(true)
        })
    } else {
        ternary::<Interval, Timestamp, Str, Timestamp>(args, result, |bucket_width, ts, tz, out| {
            *out = time_zone_operator(bucket_width, ts, &String::from_utf8_lossy(tz), &mut calendar)?;
            Ok(true)
        })
    }
}

/// `ICUTimeBucket::AddTimeBucketFunction`.
pub(crate) fn time_bucket_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("time_bucket");
    let tstz = || LogicalType::TIMESTAMP_TZ;
    set.add(
        ScalarFunction::new(
            "time_bucket",
            vec![LogicalType::INTERVAL, tstz()],
            tstz(),
            time_bucket_rows,
        )
        .with_bind(icu_datefunc::bind),
    );
    set.add(
        ScalarFunction::new(
            "time_bucket",
            vec![LogicalType::INTERVAL, tstz(), LogicalType::INTERVAL],
            tstz(),
            time_bucket_offset_rows,
        )
        .with_bind(icu_datefunc::bind),
    );
    set.add(
        ScalarFunction::new(
            "time_bucket",
            vec![LogicalType::INTERVAL, tstz(), tstz()],
            tstz(),
            time_bucket_origin_rows,
        )
        .with_bind(icu_datefunc::bind),
    );
    set.add(
        ScalarFunction::new(
            "time_bucket",
            vec![LogicalType::INTERVAL, tstz(), LogicalType::VARCHAR],
            tstz(),
            time_bucket_time_zone_rows,
        )
        .with_bind(icu_datefunc::bind),
    );
    set
}
