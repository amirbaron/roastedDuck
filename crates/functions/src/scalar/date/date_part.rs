//! `date_part`/`datepart`/`extract` and the named part functions (`year`, `month`, ...,
//! `epoch`, `epoch_ms`, `julian`, `last_day`, `monthname`, `dayname`) — DuckDB's
//! `date_part.cpp` and `date_part_specifier.cpp`.

use std::any::Any;
use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{
    DataChunk, Date, Interval, LogicalType, LogicalTypeId, NativeType, Time, TimeNs, TimeTz, Timestamp,
    Vector,
};

use super::calendar;
use crate::scalar::executor::{Str, StrOut, addressable, binary, unary};
use crate::scalar::structs::build_struct;

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

const MONTHS_PER_YEAR: i64 = 12;
const MONTHS_PER_QUARTER: i64 = 3;
const MONTHS_PER_DECADE: i64 = 120;
const MONTHS_PER_CENTURY: i64 = 1_200;
const MONTHS_PER_MILLENNIUM: i64 = 12_000;

/// A date part (DuckDB's `DatePartSpecifier`), in DuckDB's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    Year,
    Month,
    Day,
    Decade,
    Century,
    Millennium,
    Microseconds,
    Milliseconds,
    Second,
    Minute,
    Hour,
    Dow,
    IsoDow,
    Week,
    IsoYear,
    Quarter,
    Doy,
    YearWeek,
    Era,
    Timezone,
    TimezoneHour,
    TimezoneMinute,
    Epoch,
    JulianDay,
}

impl Part {
    /// DuckDB's `TryGetDatePartSpecifier`.
    pub(crate) fn try_parse(specifier: &str) -> Option<Self> {
        Some(match specifier.to_lowercase().as_str() {
            "year" | "yr" | "y" | "years" | "yrs" => Self::Year,
            "month" | "mon" | "months" | "mons" => Self::Month,
            "day" | "days" | "d" | "dayofmonth" => Self::Day,
            "decade" | "dec" | "decades" | "decs" => Self::Decade,
            "century" | "cent" | "centuries" | "c" => Self::Century,
            "millennium" | "mil" | "millenniums" | "millennia" | "mils" | "millenium" => Self::Millennium,
            "microseconds" | "microsecond" | "us" | "usec" | "usecs" | "usecond" | "useconds" => {
                Self::Microseconds
            }
            "milliseconds" | "millisecond" | "ms" | "msec" | "msecs" | "msecond" | "mseconds" => {
                Self::Milliseconds
            }
            "second" | "sec" | "seconds" | "secs" | "s" => Self::Second,
            "minute" | "min" | "minutes" | "mins" | "m" => Self::Minute,
            "hour" | "hr" | "hours" | "hrs" | "h" => Self::Hour,
            "epoch" => Self::Epoch,
            "dow" | "dayofweek" | "weekday" => Self::Dow,
            "isodow" => Self::IsoDow,
            "week" | "weeks" | "w" | "weekofyear" => Self::Week,
            "doy" | "dayofyear" => Self::Doy,
            "quarter" | "quarters" => Self::Quarter,
            "yearweek" => Self::YearWeek,
            "isoyear" => Self::IsoYear,
            "era" => Self::Era,
            "timezone" => Self::Timezone,
            "timezone_hour" => Self::TimezoneHour,
            "timezone_minute" => Self::TimezoneMinute,
            "julian" | "jd" => Self::JulianDay,
            _ => return None,
        })
    }

    /// DuckDB's `GetDatePartSpecifier`.
    pub(crate) fn parse(specifier: &str) -> Result<Self> {
        Self::try_parse(specifier)
            .ok_or_else(|| Error::conversion(format!("extract specifier \"{specifier}\" not recognized")))
    }

    /// Whether the part is a BIGINT (the others are DOUBLE).
    pub(crate) const fn is_bigint(self) -> bool {
        !matches!(self, Self::Epoch | Self::JulianDay)
    }

    /// The operator's name in DuckDB's "units not recognized" errors.
    const fn unit_name(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::Decade => "decade",
            Self::Century => "century",
            Self::Millennium => "millennium",
            Self::Quarter => "quarter",
            Self::Dow => "dow",
            Self::IsoDow => "isodow",
            Self::Doy => "doy",
            Self::Week => "week",
            Self::IsoYear => "isoyear",
            Self::YearWeek => "yearweek",
            Self::Era => "era",
            Self::Timezone => "timezone",
            Self::TimezoneHour => "timezone_hour",
            Self::TimezoneMinute => "timezone_minute",
            Self::Microseconds => "microseconds",
            Self::Milliseconds => "milliseconds",
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Epoch => "epoch",
            Self::JulianDay => "julian",
        }
    }
}

fn not_implemented(message: String) -> Error {
    Error::new(ErrorKind::NotImplemented, message)
}

fn decade(year: i64) -> i64 {
    year / 10
}

fn century(year: i64) -> i64 {
    if year > 0 {
        (year - 1) / 100 + 1
    } else {
        year / 100 - 1
    }
}

fn millennium(year: i64) -> i64 {
    if year > 0 {
        (year - 1) / 1000 + 1
    } else {
        year / 1000 - 1
    }
}

fn year_week(year: i64, week: i64) -> i64 {
    year * 100 + if year > 0 { week } else { -week }
}

/// A value whose parts can be extracted.
pub(crate) trait Temporal: NativeType {
    /// Infinite dates and timestamps have no parts (the result is NULL).
    fn is_finite_value(self) -> bool {
        true
    }
    /// A BIGINT part (DuckDB's `*Operator::Operation<T, int64_t>`).
    fn bigint_part(self, part: Part) -> Result<i64>;
    /// `epoch(value)` in seconds.
    fn to_epoch(self) -> Result<f64>;
    /// `julian(value)`; only dates and timestamps have one.
    fn to_julian(self) -> Result<f64> {
        Err(not_implemented(
            "Specifier type not implemented for DATEPART".to_owned(),
        ))
    }
    /// Microseconds since the epoch (`epoch_us`).
    fn to_epoch_us(self) -> Result<i64>;
    /// Milliseconds since the epoch (`epoch_ms`).
    fn to_epoch_ms(self) -> Result<i64>;
    /// Nanoseconds since the epoch (`epoch_ns`).
    fn to_epoch_ns(self) -> Result<i64>;
    /// The nanoseconds within the minute (`nanosecond`).
    fn to_nanosecond(self) -> Result<i64> {
        Ok(self.bigint_part(Part::Microseconds)? * Interval::NANOS_PER_MICRO)
    }
}

/// The parts of a date (time parts are 0, zone parts are an error).
fn date_part(date: Date, part: Part) -> Result<i64> {
    let (year, month, day) = date.to_ymd();
    let (year, month, day) = (i64::from(year), i64::from(month), i64::from(day));
    let days = i64::from(date.0);
    Ok(match part {
        Part::Year => year,
        Part::Month => month,
        Part::Day => day,
        Part::Decade => decade(year),
        Part::Century => century(year),
        Part::Millennium => millennium(year),
        Part::Quarter => (month - 1) / MONTHS_PER_QUARTER + 1,
        Part::Dow => calendar::day_of_week(days),
        Part::IsoDow => calendar::iso_day_of_week(days),
        Part::Doy => calendar::day_of_year(date),
        Part::Week => i64::from(calendar::iso_year_week(date).1),
        Part::IsoYear => i64::from(calendar::iso_year_week(date).0),
        Part::YearWeek => {
            let (year, week) = calendar::iso_year_week(date);
            year_week(i64::from(year), i64::from(week))
        }
        Part::Era => i64::from(year > 0),
        Part::Microseconds | Part::Milliseconds | Part::Second | Part::Minute | Part::Hour => 0,
        Part::Timezone | Part::TimezoneHour | Part::TimezoneMinute => {
            return Err(not_implemented(format!(
                "\"date\" units \"{}\" not recognized",
                part.unit_name()
            )));
        }
        Part::Epoch | Part::JulianDay => {
            return Err(not_implemented(
                "Specifier type not implemented for DATEPART".to_owned(),
            ));
        }
    })
}

/// The time parts of a time of day in microseconds; date parts are an error.
fn time_part(micros: i64, part: Part) -> Result<i64> {
    Ok(match part {
        Part::Microseconds => micros % Interval::MICROS_PER_MINUTE,
        Part::Milliseconds => micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_MSEC,
        Part::Second => micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_SEC,
        Part::Minute => micros % Interval::MICROS_PER_HOUR / Interval::MICROS_PER_MINUTE,
        Part::Hour => micros / Interval::MICROS_PER_HOUR,
        Part::Timezone | Part::TimezoneHour | Part::TimezoneMinute => 0,
        Part::Epoch | Part::JulianDay => {
            return Err(not_implemented(
                "Specifier type not implemented for DATEPART".to_owned(),
            ));
        }
        _ => {
            return Err(not_implemented(format!(
                "\"time\" units \"{}\" not recognized",
                part.unit_name()
            )));
        }
    })
}

fn date_conversion_error(date: Date, unit: &str) -> Error {
    Error::conversion(format!("Could not convert DATE ({date}) to {unit}"))
}

impl Temporal for Date {
    fn is_finite_value(self) -> bool {
        self.is_finite()
    }
    fn bigint_part(self, part: Part) -> Result<i64> {
        date_part(self, part)
    }
    fn to_epoch(self) -> Result<f64> {
        Ok((i64::from(self.0) * Interval::SECS_PER_DAY) as f64)
    }
    fn to_julian(self) -> Result<f64> {
        Ok(calendar::julian_day(self) as f64)
    }
    fn to_epoch_us(self) -> Result<i64> {
        i64::from(self.0)
            .checked_mul(Interval::MICROS_PER_DAY)
            .ok_or_else(|| date_conversion_error(self, "microseconds"))
    }
    fn to_epoch_ms(self) -> Result<i64> {
        i64::from(self.0)
            .checked_mul(Interval::MICROS_PER_DAY / Interval::MICROS_PER_MSEC)
            .ok_or_else(|| date_conversion_error(self, "milliseconds"))
    }
    fn to_epoch_ns(self) -> Result<i64> {
        i64::from(self.0)
            .checked_mul(Interval::MICROS_PER_DAY * 1000)
            .ok_or_else(|| date_conversion_error(self, "nanoseconds"))
    }
}

impl Temporal for Timestamp {
    fn is_finite_value(self) -> bool {
        self.is_finite()
    }
    fn bigint_part(self, part: Part) -> Result<i64> {
        match part {
            Part::Microseconds | Part::Milliseconds | Part::Second | Part::Minute | Part::Hour => {
                time_part(self.to_time().0, part)
            }
            Part::Timezone | Part::TimezoneHour | Part::TimezoneMinute => Ok(0),
            _ => date_part(self.to_date(), part),
        }
    }
    fn to_epoch(self) -> Result<f64> {
        Ok(self.0 as f64 / Interval::MICROS_PER_SEC as f64)
    }
    fn to_julian(self) -> Result<f64> {
        let mut result = self.to_time().0 as f64;
        result /= Interval::MICROS_PER_DAY as f64;
        result += calendar::julian_day(self.to_date()) as f64;
        Ok(result)
    }
    fn to_epoch_us(self) -> Result<i64> {
        Ok(self.0)
    }
    fn to_epoch_ms(self) -> Result<i64> {
        Ok(self.0 / Interval::MICROS_PER_MSEC)
    }
    fn to_epoch_ns(self) -> Result<i64> {
        self.0
            .checked_mul(Interval::NANOS_PER_MICRO)
            .ok_or_else(|| Error::conversion("Could not convert Timestamp(US) to Timestamp(NS)"))
    }
}

fn interval_unit_error(part: Part) -> Error {
    not_implemented(format!("interval units \"{}\" not recognized", part.unit_name()))
}

/// DuckDB's `Interval::GetMicro`.
pub(crate) fn interval_micros(interval: Interval) -> Result<i64> {
    let month = i64::from(interval.months)
        .checked_mul(Interval::MICROS_PER_DAY * Interval::DAYS_PER_MONTH)
        .ok_or_else(|| Error::conversion("Could not convert Month to Microseconds"))?;
    let day = i64::from(interval.days)
        .checked_mul(Interval::MICROS_PER_DAY)
        .ok_or_else(|| Error::conversion("Could not convert Day to Microseconds"))?;
    interval
        .micros
        .checked_add(month)
        .and_then(|total| total.checked_add(day))
        .ok_or_else(|| Error::conversion("Could not convert Interval to Microseconds"))
}

impl Temporal for Interval {
    fn bigint_part(self, part: Part) -> Result<i64> {
        let months = i64::from(self.months);
        Ok(match part {
            Part::Year => months / MONTHS_PER_YEAR,
            Part::Month => months % MONTHS_PER_YEAR,
            Part::Day => i64::from(self.days),
            Part::Decade => months / MONTHS_PER_DECADE,
            Part::Century => months / MONTHS_PER_CENTURY,
            Part::Millennium => months / MONTHS_PER_MILLENNIUM,
            Part::Quarter => months % MONTHS_PER_YEAR / MONTHS_PER_QUARTER + 1,
            Part::Dow | Part::IsoDow | Part::Doy | Part::Week | Part::IsoYear | Part::Era => {
                return Err(interval_unit_error(part));
            }
            // DuckDB computes the year, then fails on the week.
            Part::YearWeek => return Err(interval_unit_error(Part::Week)),
            Part::Microseconds => self.micros % Interval::MICROS_PER_MINUTE,
            Part::Milliseconds => self.micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_MSEC,
            Part::Second => self.micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_SEC,
            Part::Minute => self.micros % Interval::MICROS_PER_HOUR / Interval::MICROS_PER_MINUTE,
            Part::Hour => self.micros / Interval::MICROS_PER_HOUR,
            Part::Timezone | Part::TimezoneHour | Part::TimezoneMinute => {
                return Err(not_implemented(format!(
                    "\"interval\" units \"{}\" not recognized",
                    part.unit_name()
                )));
            }
            Part::Epoch | Part::JulianDay => {
                return Err(not_implemented(
                    "Specifier type not implemented for DATEPART".to_owned(),
                ));
            }
        })
    }
    fn to_epoch(self) -> Result<f64> {
        let years = i64::from(self.months) / MONTHS_PER_YEAR;
        let mut days = 365 * years;
        days += Interval::DAYS_PER_MONTH * (i64::from(self.months) % MONTHS_PER_YEAR);
        days += i64::from(self.days);
        let mut epoch = days * Interval::SECS_PER_DAY;
        epoch += years * (Interval::SECS_PER_DAY / 4);
        Ok(epoch as f64 + self.micros as f64 / Interval::MICROS_PER_SEC as f64)
    }
    fn to_epoch_us(self) -> Result<i64> {
        interval_micros(self)
    }
    fn to_epoch_ms(self) -> Result<i64> {
        let error = || Error::conversion("Could not convert Interval to Milliseconds");
        let month = i64::from(self.months)
            .checked_mul(Interval::MICROS_PER_DAY * Interval::DAYS_PER_MONTH / 1000)
            .ok_or_else(error)?;
        let day = i64::from(self.days)
            .checked_mul(Interval::MICROS_PER_DAY / 1000)
            .ok_or_else(error)?;
        (self.micros / 1000)
            .checked_add(month)
            .and_then(|total| total.checked_add(day))
            .ok_or_else(error)
    }
    fn to_epoch_ns(self) -> Result<i64> {
        interval_micros(self)?
            .checked_mul(Interval::NANOS_PER_MICRO)
            .ok_or_else(|| Error::conversion("Could not convert Interval to Nanoseconds"))
    }
}

impl Temporal for Time {
    fn bigint_part(self, part: Part) -> Result<i64> {
        time_part(self.0, part)
    }
    fn to_epoch(self) -> Result<f64> {
        Ok(self.0 as f64 / Interval::MICROS_PER_SEC as f64)
    }
    fn to_epoch_us(self) -> Result<i64> {
        Ok(self.0)
    }
    fn to_epoch_ms(self) -> Result<i64> {
        Ok(self.0 / Interval::MICROS_PER_MSEC)
    }
    fn to_epoch_ns(self) -> Result<i64> {
        Ok(self.0 * Interval::NANOS_PER_MICRO)
    }
}

/// `dtime_ns_t::time()`: the microseconds of a `TIME_NS`, truncated (the TIME cast rounds).
fn time_ns_micros(time: TimeNs) -> Time {
    Time(time.0 / Interval::NANOS_PER_MICRO)
}

impl Temporal for TimeNs {
    fn bigint_part(self, part: Part) -> Result<i64> {
        time_part(time_ns_micros(self).0, part)
    }
    fn to_epoch(self) -> Result<f64> {
        time_ns_micros(self).to_epoch()
    }
    fn to_epoch_us(self) -> Result<i64> {
        Ok(time_ns_micros(self).0)
    }
    fn to_epoch_ms(self) -> Result<i64> {
        Err(Error::conversion("Unimplemented type for cast (INT64 -> INT64)"))
    }
    fn to_epoch_ns(self) -> Result<i64> {
        Ok(self.0)
    }
    fn to_nanosecond(self) -> Result<i64> {
        Ok(self.0 % (Interval::NANOS_PER_SEC * 60))
    }
}

impl Temporal for TimeTz {
    fn bigint_part(self, part: Part) -> Result<i64> {
        let offset = i64::from(self.offset());
        match part {
            Part::Timezone => Ok(offset),
            Part::TimezoneHour => Ok(offset / Interval::SECS_PER_HOUR),
            Part::TimezoneMinute => Ok((offset / Interval::SECS_PER_MINUTE) % 60),
            _ => time_part(self.time().0, part),
        }
    }
    fn to_epoch(self) -> Result<f64> {
        self.time().to_epoch()
    }
    fn to_epoch_us(self) -> Result<i64> {
        Ok(self.time().0)
    }
    fn to_epoch_ms(self) -> Result<i64> {
        self.time().to_epoch_ms()
    }
    fn to_epoch_ns(self) -> Result<i64> {
        self.time().to_epoch_ns()
    }
}

// ---------------------------------------------------------------------------------------
// Kernels
// ---------------------------------------------------------------------------------------

/// `op(value)` per row; infinite dates and timestamps give NULL (DuckDB's `PartOperator`).
fn part_kernel<T: Temporal, R: NativeType>(
    op: fn(T) -> Result<R>,
) -> impl Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync + 'static {
    move |args, _, result| {
        unary::<T, R>(args, result, |value, out| {
            if !value.is_finite_value() {
                return Ok(false);
            }
            *out = op(value)?;
            Ok(true)
        })
    }
}

fn overload<T: Temporal, R: NativeType>(
    name: &str,
    argument: LogicalType,
    return_type: LogicalType,
    op: fn(T) -> Result<R>,
) -> ScalarFunction {
    ScalarFunction::new(name, vec![argument], return_type, part_kernel::<T, R>(op))
}

/// A part function over DATE, TIMESTAMP and INTERVAL (DuckDB's `GetDatePartFunction`).
fn date_part_set(name: &str, part: Part) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    macro_rules! add {
        ($ty:ty, $logical:expr) => {
            set.add(ScalarFunction::new(
                name,
                vec![$logical],
                LogicalType::BIGINT,
                move |args, _, result| {
                    unary::<$ty, i64>(args, result, |value, out| {
                        if !value.is_finite_value() {
                            return Ok(false);
                        }
                        *out = value.bigint_part(part)?;
                        Ok(true)
                    })
                },
            ));
        };
    }
    add!(Date, LogicalType::DATE);
    add!(Timestamp, LogicalType::TIMESTAMP);
    add!(Interval, LogicalType::INTERVAL);
    set
}

/// A part function over every temporal type (DuckDB's `GetTimePartFunction`).
fn time_part_set<R: NativeType>(name: &str, return_type: LogicalType, op: TimeOps<R>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(overload::<Date, R>(
        name,
        LogicalType::DATE,
        return_type.clone(),
        op.date,
    ));
    set.add(overload::<Timestamp, R>(
        name,
        LogicalType::TIMESTAMP,
        return_type.clone(),
        op.timestamp,
    ));
    set.add(overload::<Interval, R>(
        name,
        LogicalType::INTERVAL,
        return_type.clone(),
        op.interval,
    ));
    set.add(overload::<Time, R>(
        name,
        LogicalType::TIME,
        return_type.clone(),
        op.time,
    ));
    set.add(overload::<TimeNs, R>(
        name,
        LogicalType::TIME_NS,
        return_type.clone(),
        op.time_ns,
    ));
    set.add(overload::<TimeTz, R>(
        name,
        LogicalType::TIME_TZ,
        return_type,
        op.time_tz,
    ));
    set
}

/// One operation per temporal type.
#[derive(Clone, Copy)]
struct TimeOps<R> {
    date: fn(Date) -> Result<R>,
    timestamp: fn(Timestamp) -> Result<R>,
    interval: fn(Interval) -> Result<R>,
    time: fn(Time) -> Result<R>,
    time_ns: fn(TimeNs) -> Result<R>,
    time_tz: fn(TimeTz) -> Result<R>,
}

macro_rules! time_ops {
    ($method:ident $(, $arg:expr)?) => {
        TimeOps {
            date: |value| value.$method($($arg)?),
            timestamp: |value| value.$method($($arg)?),
            interval: |value| value.$method($($arg)?),
            time: |value| value.$method($($arg)?),
            time_ns: |value| value.$method($($arg)?),
            time_tz: |value| value.$method($($arg)?),
        }
    };
}

/// `date_part(specifier, value)` per row (DuckDB's `DatePartFunction`).
fn date_part_rows<T: Temporal>(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let mut cached: Option<(Vec<u8>, Part)> = None;
    binary::<Str, T, i64>(args, result, |specifier, value, out| {
        if !value.is_finite_value() {
            return Ok(false);
        }
        let part = match &cached {
            Some((text, part)) if text.as_slice() == specifier => *part,
            _ => {
                let part = Part::parse(&String::from_utf8_lossy(specifier))?;
                cached = Some((specifier.to_vec(), part));
                part
            }
        };
        *out = value.bigint_part(part)?;
        Ok(true)
    })
}

/// The value column alone, for kernels that ignore a bound constant first argument.
fn second_column(args: &DataChunk) -> DataChunk {
    DataChunk::new(vec![args.column(1).clone()], args.len())
}

fn bind_date_part<T: Temporal>(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let Some(Some(value)) = input.constants.first() else {
        return Ok(None);
    };
    if input.argument_types[0].id() == LogicalTypeId::Unknown {
        return Ok(None);
    }
    let part = Part::parse(&crate::values::as_text(value)?)?;
    let ty = input.argument_types[1].id();
    match part {
        Part::JulianDay => {
            function.return_type = LogicalType::DOUBLE;
            function.kernel = match ty {
                LogicalTypeId::Timestamp
                | LogicalTypeId::TimestampSec
                | LogicalTypeId::TimestampMs
                | LogicalTypeId::TimestampNs => {
                    let kernel = part_kernel::<Timestamp, f64>(<Timestamp as Temporal>::to_julian);
                    Arc::new(move |args, state, result| kernel(&second_column(args), state, result))
                }
                LogicalTypeId::Date => {
                    let kernel = part_kernel::<Date, f64>(<Date as Temporal>::to_julian);
                    Arc::new(move |args, state, result| kernel(&second_column(args), state, result))
                }
                _ => return Err(Error::binder("julian can only take DATE or TIMESTAMP arguments")),
            };
        }
        Part::Epoch => {
            function.return_type = LogicalType::DOUBLE;
            let kernel: rustdb_function::ScalarKernel = match ty {
                LogicalTypeId::Timestamp
                | LogicalTypeId::TimestampSec
                | LogicalTypeId::TimestampMs
                | LogicalTypeId::TimestampNs => {
                    Arc::new(part_kernel::<Timestamp, f64>(<Timestamp as Temporal>::to_epoch))
                }
                LogicalTypeId::Date => Arc::new(part_kernel::<Date, f64>(<Date as Temporal>::to_epoch)),
                LogicalTypeId::Interval => {
                    Arc::new(part_kernel::<Interval, f64>(<Interval as Temporal>::to_epoch))
                }
                LogicalTypeId::Time => Arc::new(part_kernel::<Time, f64>(<Time as Temporal>::to_epoch)),
                LogicalTypeId::TimeNs => Arc::new(part_kernel::<TimeNs, f64>(<TimeNs as Temporal>::to_epoch)),
                LogicalTypeId::TimeTz => Arc::new(part_kernel::<TimeTz, f64>(<TimeTz as Temporal>::to_epoch)),
                _ => return Err(Error::binder("epoch can only take temporal arguments")),
            };
            function.kernel =
                Arc::new(move |args, state, result| kernel(&second_column(args), state, result));
        }
        _ => {
            simplified_function_exists(part, &input.argument_types[1])?;
            function.kernel = Arc::new(move |args, _, result| {
                unary::<T, i64>(&second_column(args), result, |value, out| {
                    if !value.is_finite_value() {
                        return Ok(false);
                    }
                    *out = value.bigint_part(part)?;
                    Ok(true)
                })
            });
        }
    }
    Ok(None)
}

/// DuckDB's `DatePartSimplificationRule` rewrites `date_part('dow', x)` with a constant
/// specifier into `dayofweek(x)`; binding that call fails for types the named function
/// does not take (`extract(dow FROM TIME)`), with the usual no-match error.
fn simplified_function_exists(part: Part, ty: &LogicalType) -> Result<()> {
    let name = match part {
        Part::Year => "year",
        Part::Month => "month",
        Part::Day => "day",
        Part::Decade => "decade",
        Part::Century => "century",
        Part::Millennium => "millennium",
        Part::Quarter => "quarter",
        Part::Week => "week",
        Part::YearWeek => "yearweek",
        Part::Dow => "dayofweek",
        Part::IsoDow => "isodow",
        Part::Doy => "dayofyear",
        Part::Microseconds => "microsecond",
        Part::Milliseconds => "millisecond",
        Part::Second => "second",
        Part::Minute => "minute",
        Part::Hour => "hour",
        _ => return Ok(()),
    };
    let Some((set, _)) = named_part_functions()
        .into_iter()
        .find(|(set, _)| &*set.name == name)
    else {
        return Ok(());
    };
    let matches = set.functions.iter().any(|function| {
        function.arguments.len() == 1
            && (function.arguments[0] == *ty
                || rustdb_types::implicit_cast_cost(ty, &function.arguments[0]) >= 0)
    });
    if matches {
        return Ok(());
    }
    let mut candidates = String::new();
    for function in &set.functions {
        let arguments: Vec<String> = function.arguments.iter().map(ToString::to_string).collect();
        let _ = writeln!(
            candidates,
            "\t{name}({}) -> {}",
            arguments.join(", "),
            function.return_type
        );
    }
    Err(Error::binder(format!(
        "No function matches the given name and argument types '{name}({ty})'. You might need to add explicit \
         type casts.\n\tCandidate functions:\n{candidates}"
    )))
}

/// The name DuckDB's `EnumUtil::ToString(LogicalTypeId)` gives a temporal type.
fn type_id_name(id: LogicalTypeId) -> &'static str {
    match id {
        LogicalTypeId::Date => "DATE",
        LogicalTypeId::Timestamp => "TIMESTAMP",
        LogicalTypeId::TimestampTz => "TIMESTAMP WITH TIME ZONE",
        LogicalTypeId::Time => "TIME",
        LogicalTypeId::TimeNs => "TIME_NS",
        LogicalTypeId::TimeTz => "TIME WITH TIME ZONE",
        LogicalTypeId::Interval => "INTERVAL",
        _ => "INVALID",
    }
}

/// DuckDB's `GetDateTypePartSpecifier`: the parts each type supports in the STRUCT form.
fn struct_part(specifier: &str, ty: LogicalTypeId) -> Result<Part> {
    let part = Part::parse(specifier)?;
    let allowed = match ty {
        LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz => true,
        LogicalTypeId::Date => matches!(
            part,
            Part::Year
                | Part::Month
                | Part::Day
                | Part::Decade
                | Part::Century
                | Part::Millennium
                | Part::Dow
                | Part::IsoDow
                | Part::IsoYear
                | Part::Week
                | Part::Quarter
                | Part::Doy
                | Part::YearWeek
                | Part::Era
                | Part::Epoch
                | Part::JulianDay
        ),
        LogicalTypeId::Time | LogicalTypeId::TimeNs | LogicalTypeId::TimeTz => matches!(
            part,
            Part::Microseconds
                | Part::Milliseconds
                | Part::Second
                | Part::Minute
                | Part::Hour
                | Part::Epoch
                | Part::Timezone
                | Part::TimezoneHour
                | Part::TimezoneMinute
        ),
        LogicalTypeId::Interval => matches!(
            part,
            Part::Year
                | Part::Month
                | Part::Day
                | Part::Decade
                | Part::Century
                | Part::Quarter
                | Part::Millennium
                | Part::Microseconds
                | Part::Milliseconds
                | Part::Second
                | Part::Minute
                | Part::Hour
                | Part::Epoch
        ),
        _ => false,
    };
    if allowed {
        Ok(part)
    } else {
        Err(not_implemented(format!(
            "\"{}\" units \"{specifier}\" not recognized",
            type_id_name(ty)
        )))
    }
}

/// The parts of a STRUCT `date_part` call.
#[derive(Debug)]
struct StructParts(Vec<Part>);

impl FunctionData for StructParts {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `StructDatePart::Bind`.
fn bind_struct_date_part(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    if input.argument_types[0].id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let not_constant = || {
        Error::binder(format!(
            "{} can only take constant lists of part names",
            function.name
        ))
    };
    let Some(Some(list)) = input.constants.first() else {
        return Err(not_constant());
    };
    if list.logical_type().id() != LogicalTypeId::List {
        return Err(not_constant());
    }
    let children = list.children().unwrap_or_default();
    if children.is_empty() {
        return Err(Error::binder(format!(
            "{} requires non-empty lists of part names",
            function.name
        )));
    }
    let ty = input.argument_types[1].id();
    let mut names: Vec<String> = Vec::with_capacity(children.len());
    let mut parts = Vec::with_capacity(children.len());
    let mut fields = Vec::with_capacity(children.len());
    for child in children {
        if child.is_null() {
            return Err(Error::binder(format!(
                "NULL struct entry name in {}",
                function.name
            )));
        }
        let name = crate::values::as_text(child)?;
        let part = struct_part(&name, ty)?;
        if names.iter().any(|existing| existing.eq_ignore_ascii_case(&name)) {
            return Err(Error::binder(format!(
                "Duplicate struct entry name \"{name}\" in {}",
                function.name
            )));
        }
        let field_type = if part.is_bigint() {
            LogicalType::BIGINT
        } else {
            LogicalType::DOUBLE
        };
        fields.push((name.clone(), field_type));
        names.push(name);
        parts.push(part);
    }
    function.return_type = LogicalType::struct_type(fields);
    function.kernel = match ty {
        LogicalTypeId::Date => Arc::new(struct_kernel::<Date>),
        LogicalTypeId::Timestamp => Arc::new(struct_kernel::<Timestamp>),
        LogicalTypeId::Time => Arc::new(struct_kernel::<Time>),
        LogicalTypeId::TimeNs => Arc::new(struct_kernel::<TimeNs>),
        LogicalTypeId::Interval => Arc::new(struct_kernel::<Interval>),
        LogicalTypeId::TimeTz => Arc::new(struct_kernel::<TimeTz>),
        _ => return Err(Error::internal("date_part STRUCT variant on a non-temporal type")),
    };
    Ok(Some(Arc::new(StructParts(parts))))
}

/// DuckDB's `StructDatePart::Function`: NULL input gives a NULL struct, infinite input a
/// struct of NULLs.
fn struct_kernel<T: Temporal>(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let parts = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<StructParts>())
        .map(|data| data.0.clone())
        .ok_or_else(|| Error::internal("date_part STRUCT variant without bind data"))?;
    let count = args.len();
    let input = addressable(args.column(1), count);
    let constant = input.is_constant();
    let rows = if constant { 1 } else { count };
    let values = input.unified::<T>();
    let mut bigints: Vec<Vec<Option<i64>>> = vec![Vec::with_capacity(rows); parts.len()];
    let mut doubles: Vec<Vec<Option<f64>>> = vec![Vec::with_capacity(rows); parts.len()];
    let mut nulls = Vec::new();
    for row in 0..rows {
        let value = values.get(row);
        if value.is_none() {
            nulls.push(row);
        }
        let value = value.filter(|value| value.is_finite_value());
        for (index, part) in parts.iter().enumerate() {
            match (value, part) {
                (None, _) => {
                    bigints[index].push(None);
                    doubles[index].push(None);
                }
                (Some(value), Part::Epoch) => doubles[index].push(Some(value.to_epoch()?)),
                (Some(value), Part::JulianDay) => doubles[index].push(Some(value.to_julian()?)),
                (Some(value), part) => bigints[index].push(Some(value.bigint_part(*part)?)),
            }
        }
    }
    let mut fields = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let mut field = if part.is_bigint() {
            let mut vector = Vector::flat(LogicalType::BIGINT, rows.max(1));
            write_column(&mut vector, &bigints[index]);
            vector
        } else {
            let mut vector = Vector::flat(LogicalType::DOUBLE, rows.max(1));
            write_column(&mut vector, &doubles[index]);
            vector
        };
        if rows == 0 {
            field.reset_flat(0);
        }
        fields.push(field);
    }
    let mut output = build_struct(result.logical_type().clone(), fields, rows, false)?;
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn write_column<T: NativeType>(vector: &mut Vector, values: &[Option<T>]) {
    vector.reset_flat(values.len());
    let mut nulls = Vec::new();
    {
        let data = vector.values_mut::<T>();
        for (row, value) in values.iter().enumerate() {
            match value {
                Some(value) => data[row] = *value,
                None => nulls.push(row),
            }
        }
    }
    let validity = vector.validity_mut();
    for row in nulls {
        validity.set_invalid(row, values.len());
    }
}

/// `date_part`/`datepart` (DuckDB's `DatePartFun`).
pub fn date_part_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("date_part");
    macro_rules! add {
        ($ty:ty, $logical:expr) => {
            set.add(
                ScalarFunction::new(
                    "date_part",
                    vec![LogicalType::VARCHAR, $logical],
                    LogicalType::BIGINT,
                    |args, _, result| date_part_rows::<$ty>(args, result),
                )
                .with_bind(bind_date_part::<$ty>),
            );
        };
    }
    add!(Date, LogicalType::DATE);
    add!(Timestamp, LogicalType::TIMESTAMP);
    add!(Time, LogicalType::TIME);
    add!(TimeNs, LogicalType::TIME_NS);
    add!(Interval, LogicalType::INTERVAL);
    add!(TimeTz, LogicalType::TIME_TZ);
    let parts = LogicalType::list(LogicalType::VARCHAR);
    for ty in [
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::TIME,
        LogicalType::TIME_NS,
        LogicalType::INTERVAL,
        LogicalType::TIME_TZ,
    ] {
        set.add(
            ScalarFunction::new(
                "date_part",
                vec![parts.clone(), ty],
                LogicalType::struct_type(Vec::new()),
                |_, _, _| Err(Error::internal("date_part used without being bound")),
            )
            .with_bind(bind_struct_date_part),
        );
    }
    set
}

/// `last_day(date)`: the last day of the month (DuckDB's `LastDayOperator`).
#[allow(clippy::unnecessary_wraps, reason = "the signature of a fallible kernel")]
fn last_day(date: Date) -> Result<Date> {
    let (mut year, mut month, _) = date.to_ymd();
    year += month / 12;
    month %= 12;
    month += 1;
    Ok(Date((calendar::days_from_ymd(year, month, 1) - 1) as i32))
}

fn name_set(name: &str, names: &'static [&'static str], index: fn(Date) -> usize) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::DATE],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<Date, StrOut>(args, result, |date, out| {
                if !date.is_finite() {
                    return Ok(false);
                }
                out.extend_from_slice(names[index(date)].as_bytes());
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::TIMESTAMP],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<Timestamp, StrOut>(args, result, |timestamp, out| {
                if !timestamp.is_finite() {
                    return Ok(false);
                }
                out.extend_from_slice(names[index(timestamp.to_date())].as_bytes());
                Ok(true)
            })
        },
    ));
    set
}

/// `epoch_ms(BIGINT)`/`make_timestamp_ms`: milliseconds since the epoch to a TIMESTAMP.
fn timestamp_from_millis() -> ScalarFunction {
    ScalarFunction::new(
        "epoch_ms",
        vec![LogicalType::BIGINT],
        LogicalType::TIMESTAMP,
        |args, _, result| {
            unary::<i64, Timestamp>(args, result, |millis, out| {
                *out =
                    Timestamp(millis.checked_mul(Interval::MICROS_PER_MSEC).ok_or_else(|| {
                        Error::conversion("Could not convert Timestamp(MS) to Timestamp(US)")
                    })?);
                Ok(true)
            })
        },
    )
}

fn renamed(mut function: ScalarFunction, name: &str) -> ScalarFunction {
    function.name = Arc::from(name);
    function
}

/// Every named date part function, with aliases.
pub fn named_part_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let mut sets: Vec<(ScalarFunctionSet, &'static [&'static str])> = vec![
        (date_part_functions(), &["datepart"]),
        (date_part_set("year", Part::Year), &[]),
        (date_part_set("month", Part::Month), &[]),
        (date_part_set("day", Part::Day), &[]),
        (date_part_set("decade", Part::Decade), &[]),
        (date_part_set("century", Part::Century), &[]),
        (date_part_set("millennium", Part::Millennium), &[]),
        (date_part_set("quarter", Part::Quarter), &[]),
        (date_part_set("dayofweek", Part::Dow), &[]),
        (date_part_set("isodow", Part::IsoDow), &[]),
        (date_part_set("dayofyear", Part::Doy), &[]),
        (date_part_set("week", Part::Week), &[]),
        (date_part_set("isoyear", Part::IsoYear), &[]),
        (date_part_set("era", Part::Era), &[]),
        (date_part_set("timezone_hour", Part::TimezoneHour), &[]),
        (date_part_set("timezone_minute", Part::TimezoneMinute), &[]),
        (date_part_set("yearweek", Part::YearWeek), &[]),
        (date_part_set("dayofmonth", Part::Day), &[]),
        (date_part_set("weekday", Part::Dow), &[]),
        (date_part_set("weekofyear", Part::Week), &[]),
        (
            time_part_set("epoch", LogicalType::DOUBLE, time_ops!(to_epoch)),
            &[],
        ),
        (
            time_part_set(
                "microsecond",
                LogicalType::BIGINT,
                time_ops!(bigint_part, Part::Microseconds),
            ),
            &[],
        ),
        (
            time_part_set(
                "millisecond",
                LogicalType::BIGINT,
                time_ops!(bigint_part, Part::Milliseconds),
            ),
            &[],
        ),
        (
            time_part_set(
                "second",
                LogicalType::BIGINT,
                time_ops!(bigint_part, Part::Second),
            ),
            &[],
        ),
        (
            time_part_set(
                "minute",
                LogicalType::BIGINT,
                time_ops!(bigint_part, Part::Minute),
            ),
            &[],
        ),
        (
            time_part_set("hour", LogicalType::BIGINT, time_ops!(bigint_part, Part::Hour)),
            &[],
        ),
    ];
    let mut timezone = date_part_set("timezone", Part::Timezone);
    timezone.add(ScalarFunction::new(
        "timezone",
        vec![LogicalType::INTERVAL, LogicalType::TIME_TZ],
        LogicalType::TIME_TZ,
        |args, _, result| {
            binary::<Interval, TimeTz, TimeTz>(args, result, |interval, time_tz, out| {
                // DuckDB's `TimezoneOperator`: the time moves to UTC, then by the interval.
                let utc = crate::temporal::time_tz_normalized_micros(time_tz);
                let (time, _) = crate::temporal::time_add_interval(Time(utc), interval);
                *out = TimeTz::new(time, (interval.micros / Interval::MICROS_PER_SEC) as i32);
                Ok(true)
            })
        },
    ));
    sets.push((timezone, &[]));

    let mut epoch_ns = time_part_set("epoch_ns", LogicalType::BIGINT, time_ops!(to_epoch_ns));
    epoch_ns.add(overload::<Timestamp, i64>(
        "epoch_ns",
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BIGINT,
        <Timestamp as Temporal>::to_epoch_ns,
    ));
    epoch_ns.add(ScalarFunction::new(
        "epoch_ns",
        vec![LogicalType::TIMESTAMP_NS],
        LogicalType::BIGINT,
        |args, _, result| {
            unary::<i64, i64>(args, result, |nanos, out| {
                *out = nanos;
                Ok(true)
            })
        },
    ));
    sets.push((epoch_ns, &[]));
    let mut epoch_us = time_part_set("epoch_us", LogicalType::BIGINT, time_ops!(to_epoch_us));
    epoch_us.add(overload::<Timestamp, i64>(
        "epoch_us",
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BIGINT,
        <Timestamp as Temporal>::to_epoch_us,
    ));
    sets.push((epoch_us, &[]));
    let mut epoch_ms = time_part_set("epoch_ms", LogicalType::BIGINT, time_ops!(to_epoch_ms));
    epoch_ms.add(overload::<Timestamp, i64>(
        "epoch_ms",
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BIGINT,
        <Timestamp as Temporal>::to_epoch_ms,
    ));
    epoch_ms.add(timestamp_from_millis());
    sets.push((epoch_ms, &[]));
    let mut make_timestamp_ms = ScalarFunctionSet::new("make_timestamp_ms");
    make_timestamp_ms.add(renamed(timestamp_from_millis(), "make_timestamp_ms"));
    sets.push((make_timestamp_ms, &[]));

    let mut nanosecond = time_part_set("nanosecond", LogicalType::BIGINT, time_ops!(to_nanosecond));
    nanosecond.add(ScalarFunction::new(
        "nanosecond",
        vec![LogicalType::TIMESTAMP_NS],
        LogicalType::BIGINT,
        |args, _, result| {
            unary::<i64, i64>(args, result, |nanos, out| {
                if !Timestamp(nanos).is_finite() {
                    return Err(Error::conversion("Can't get nanoseconds of infinite TIMESTAMP"));
                }
                let micros = nanos.div_euclid(Interval::NANOS_PER_MICRO);
                let rest = nanos.rem_euclid(Interval::NANOS_PER_MICRO);
                let time = Timestamp(micros).to_time();
                *out = (time.0 % Interval::MICROS_PER_MINUTE) * Interval::NANOS_PER_MICRO + rest;
                Ok(true)
            })
        },
    ));
    nanosecond.add(overload::<Timestamp, i64>(
        "nanosecond",
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BIGINT,
        <Timestamp as Temporal>::to_nanosecond,
    ));
    sets.push((nanosecond, &[]));

    let mut last_day_set = ScalarFunctionSet::new("last_day");
    last_day_set.add(overload::<Date, Date>(
        "last_day",
        LogicalType::DATE,
        LogicalType::DATE,
        last_day,
    ));
    last_day_set.add(overload::<Timestamp, Date>(
        "last_day",
        LogicalType::TIMESTAMP,
        LogicalType::DATE,
        |ts| last_day(ts.to_date()),
    ));
    sets.push((last_day_set, &[]));
    sets.push((
        name_set("monthname", &calendar::MONTH_NAMES, |date| {
            (date.to_ymd().1 - 1) as usize
        }),
        &[],
    ));
    sets.push((
        name_set("dayname", &calendar::DAY_NAMES, |date| {
            calendar::day_of_week(i64::from(date.0)) as usize
        }),
        &[],
    ));
    let mut julian = ScalarFunctionSet::new("julian");
    julian.add(overload::<Date, f64>(
        "julian",
        LogicalType::DATE,
        LogicalType::DOUBLE,
        <Date as Temporal>::to_julian,
    ));
    julian.add(overload::<Timestamp, f64>(
        "julian",
        LogicalType::TIMESTAMP,
        LogicalType::DOUBLE,
        <Timestamp as Temporal>::to_julian,
    ));
    sets.push((julian, &[]));
    sets
}
