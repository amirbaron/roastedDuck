//! `time_bucket` (DuckDB's `time_bucket.cpp`): truncation to buckets of a fixed width,
//! counted from an origin (2000-01-03 for micro-based widths, 2000-01 for month widths).

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, Date, Interval, LogicalType, NativeType, Time, Timestamp, Vector};

use super::date_part::interval_micros;
use crate::operator::subtract_overflow;
use crate::scalar::executor::{addressable, binary, ternary};
use crate::temporal::{date_add_interval, interval_invert, timestamp_add_interval, timestamp_from_datetime};

const DEFAULT_ORIGIN_MICROS: i64 = 10_959 * Interval::MICROS_PER_DAY;
const DEFAULT_ORIGIN_MONTHS: i32 = 360;

/// How a bucket width is applied (DuckDB's `BucketWidthType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Width {
    Micros,
    Months,
    Unclassified,
}

fn not_implemented(message: &str) -> Error {
    Error::new(ErrorKind::NotImplemented, message.to_owned())
}

/// DuckDB's `ClassifyBucketWidth` (for a constant width).
fn classify(width: Interval) -> Result<Width> {
    if width.months == 0 && interval_micros(width)? > 0 {
        Ok(Width::Micros)
    } else if width.months > 0 && width.days == 0 && width.micros == 0 {
        Ok(Width::Months)
    } else {
        Ok(Width::Unclassified)
    }
}

/// DuckDB's `ClassifyBucketWidthErrorThrow`.
fn classify_or_error(width: Interval) -> Result<Width> {
    if width.months == 0 {
        if interval_micros(width)? <= 0 {
            return Err(not_implemented("Period must be greater than 0"));
        }
        Ok(Width::Micros)
    } else if width.days == 0 && width.micros == 0 {
        if width.months < 0 {
            return Err(not_implemented("Period must be greater than 0"));
        }
        Ok(Width::Months)
    } else {
        Err(not_implemented(
            "Month intervals cannot have day or time component",
        ))
    }
}

/// A DATE or TIMESTAMP bucketed value.
trait Bucketed: NativeType + PartialEq {
    fn finite(self) -> bool;
    /// `Cast<T, timestamp_t>`.
    fn to_timestamp(self) -> Result<Timestamp>;
    /// `Cast<timestamp_t, T>`.
    fn from_timestamp(timestamp: Timestamp) -> Self;
    /// `Cast<T, date_t>`.
    fn to_date(self) -> Date;
    /// `Cast<date_t, T>`.
    fn from_date(date: Date) -> Result<Self>;
    /// `Interval::Add(T, interval)`.
    fn add(self, interval: Interval) -> Result<Self>;
}

impl Bucketed for Date {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn to_timestamp(self) -> Result<Timestamp> {
        timestamp_from_datetime(self, Time(0))
    }
    fn from_timestamp(timestamp: Timestamp) -> Self {
        timestamp.to_date()
    }
    fn to_date(self) -> Date {
        self
    }
    fn from_date(date: Date) -> Result<Self> {
        Ok(date)
    }
    fn add(self, interval: Interval) -> Result<Self> {
        date_add_interval(self, interval)
    }
}

impl Bucketed for Timestamp {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn to_timestamp(self) -> Result<Timestamp> {
        Ok(self)
    }
    fn from_timestamp(timestamp: Timestamp) -> Self {
        timestamp
    }
    fn to_date(self) -> Date {
        Timestamp::to_date(self)
    }
    fn from_date(date: Date) -> Result<Self> {
        timestamp_from_datetime(date, Time(0))
    }
    fn add(self, interval: Interval) -> Result<Self> {
        timestamp_add_interval(self, interval)
    }
}

/// Months since 1970-01 (DuckDB's `EpochMonths`).
fn epoch_months<T: Bucketed>(value: T) -> i32 {
    let (year, month, _) = value.to_date().to_ymd();
    (year - 1970) * 12 + month - 1
}

/// DuckDB's `WidthConvertibleToMicrosCommon`.
fn bucket_micros(width: i64, value: i64, origin: i64) -> Result<Timestamp> {
    let origin = origin % width;
    let value = value
        .checked_sub(origin)
        .ok_or_else(|| subtract_overflow(value, origin))?;
    let mut result = (value / width) * width;
    if value < 0 && value % width != 0 {
        result = result
            .checked_sub(width)
            .ok_or_else(|| subtract_overflow(result, width))?;
    }
    Ok(Timestamp(result.wrapping_add(origin)))
}

/// DuckDB's `WidthConvertibleToMonthsCommon`.
fn bucket_months(width: i32, value: i32, origin: i32) -> Result<Date> {
    let origin = origin % width;
    let value = value
        .checked_sub(origin)
        .ok_or_else(|| subtract_overflow(value, origin))?;
    let mut result = (value / width) * width;
    if value < 0 && value % width != 0 {
        result = result
            .checked_sub(width)
            .ok_or_else(|| subtract_overflow(result, width))?;
    }
    result = result.wrapping_add(origin);
    let (year, month) = if result < 0 && result % 12 != 0 {
        (1970 + result / 12 - 1, result % 12 + 13)
    } else {
        (1970 + result / 12, result % 12 + 1)
    };
    crate::temporal::date_from_ymd(year, month, 1)
}

/// `time_bucket(width, value)` for a classified width.
fn bucket<T: Bucketed>(width: Interval, value: T, kind: Width) -> Result<T> {
    if !value.finite() {
        return Ok(value);
    }
    match kind {
        Width::Micros => Ok(T::from_timestamp(bucket_micros(
            interval_micros(width)?,
            value.to_timestamp()?.0,
            DEFAULT_ORIGIN_MICROS,
        )?)),
        Width::Months => T::from_date(bucket_months(
            width.months,
            epoch_months(value),
            DEFAULT_ORIGIN_MONTHS,
        )?),
        Width::Unclassified => bucket(width, value, classify_or_error(width)?),
    }
}

/// `time_bucket(width, value, offset)`.
fn bucket_offset<T: Bucketed>(width: Interval, value: T, offset: Interval, kind: Width) -> Result<T> {
    if !value.finite() {
        return Ok(value);
    }
    match kind {
        Width::Micros => {
            let shifted = timestamp_add_interval(value.to_timestamp()?, interval_invert(offset))?;
            let bucketed = bucket_micros(interval_micros(width)?, shifted.0, DEFAULT_ORIGIN_MICROS)?;
            Ok(T::from_timestamp(timestamp_add_interval(bucketed, offset)?))
        }
        Width::Months => {
            let months = epoch_months(value.add(interval_invert(offset))?);
            T::from_date(bucket_months(width.months, months, DEFAULT_ORIGIN_MONTHS)?)?.add(offset)
        }
        Width::Unclassified => bucket_offset(width, value, offset, classify_or_error(width)?),
    }
}

/// `time_bucket(width, value, origin)`; an infinite origin gives NULL.
fn bucket_origin<T: Bucketed>(width: Interval, value: T, origin: T, kind: Width) -> Result<Option<T>> {
    if !origin.finite() {
        return Ok(None);
    }
    if !value.finite() {
        return Ok(Some(value));
    }
    Ok(Some(match kind {
        Width::Micros => T::from_timestamp(bucket_micros(
            interval_micros(width)?,
            value.to_timestamp()?.0,
            origin.to_timestamp()?.0,
        )?),
        Width::Months => T::from_date(bucket_months(
            width.months,
            epoch_months(value),
            epoch_months(origin),
        )?)?,
        Width::Unclassified => return bucket_origin(width, value, origin, classify_or_error(width)?),
    }))
}

/// The width classification for the whole chunk: constant widths are classified once,
/// others per row with errors.
fn chunk_width(args: &DataChunk) -> Result<Option<Width>> {
    let width = args.column(0);
    if !width.is_constant() || width.is_null(0) {
        return Ok(Some(Width::Unclassified));
    }
    let values = addressable(width, 1);
    let Some(value) = values.unified::<Interval>().get(0) else {
        return Ok(None);
    };
    classify(value).map(Some)
}

fn time_bucket_rows<T: Bucketed>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let Some(kind) = chunk_width(args)? else {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    };
    match args.column_count() {
        2 => binary::<Interval, T, T>(args, result, |width, value, out| {
            *out = bucket(width, value, kind)?;
            Ok(true)
        }),
        _ => Err(Error::internal("time_bucket arity")),
    }
}

fn time_bucket_offset_rows<T: Bucketed>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let Some(kind) = chunk_width(args)? else {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    };
    ternary::<Interval, T, Interval, T>(args, result, |width, value, offset, out| {
        *out = bucket_offset(width, value, offset, kind)?;
        Ok(true)
    })
}

fn time_bucket_origin_rows<T: Bucketed>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let both_constant = args.column(0).is_constant() && args.column(2).is_constant();
    let kind = if both_constant {
        let origins = addressable(args.column(2), 1);
        let origin_null = origins
            .unified::<T>()
            .get(0)
            .is_none_or(|origin| !origin.finite());
        if args.column(0).is_null(0) || origin_null {
            *result = Vector::constant_null(result.logical_type().clone());
            return Ok(());
        }
        chunk_width(args)?.unwrap_or(Width::Unclassified)
    } else {
        Width::Unclassified
    };
    ternary::<Interval, T, T, T>(args, result, |width, value, origin, out| {
        match bucket_origin(width, value, origin, kind)? {
            Some(value) => {
                *out = value;
                Ok(true)
            }
            None => Ok(false),
        }
    })
}

/// `time_bucket`.
pub fn time_bucket_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("time_bucket");
    macro_rules! add {
        ($ty:ty, $logical:expr) => {
            set.add(ScalarFunction::new(
                "time_bucket",
                vec![LogicalType::INTERVAL, $logical],
                $logical,
                |args, _, result| time_bucket_rows::<$ty>(args, result),
            ));
        };
    }
    add!(Date, LogicalType::DATE);
    add!(Timestamp, LogicalType::TIMESTAMP);
    macro_rules! add3 {
        ($ty:ty, $logical:expr, $third:expr, $kernel:ident) => {
            set.add(ScalarFunction::new(
                "time_bucket",
                vec![LogicalType::INTERVAL, $logical, $third],
                $logical,
                |args, _, result| $kernel::<$ty>(args, result),
            ));
        };
    }
    add3!(
        Date,
        LogicalType::DATE,
        LogicalType::INTERVAL,
        time_bucket_offset_rows
    );
    add3!(
        Timestamp,
        LogicalType::TIMESTAMP,
        LogicalType::INTERVAL,
        time_bucket_offset_rows
    );
    add3!(
        Date,
        LogicalType::DATE,
        LogicalType::DATE,
        time_bucket_origin_rows
    );
    add3!(
        Timestamp,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP,
        time_bucket_origin_rows
    );
    set
}
