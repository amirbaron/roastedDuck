//! `date_diff`/`datediff` (DuckDB's `date_diff.cpp`), `date_sub`/`datesub`
//! (`date_sub.cpp`) and the two-argument `age` (`age.cpp`).

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, Date, Interval, LogicalType, NativeType, Time, Timestamp, Vector};

use super::date_part::Part;
use crate::operator::subtract_overflow;
use crate::scalar::executor::{Str, binary, ternary};
use crate::temporal::timestamp_from_datetime;

/// The units `date_diff` and `date_sub` count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Year,
    Month,
    Day,
    Decade,
    Century,
    Millennium,
    Quarter,
    Week,
    IsoYear,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
}

impl Unit {
    /// The unit of a specifier; `function` names the function in the error.
    fn from_part(part: Part, function: &str) -> Result<Self> {
        Ok(match part {
            Part::Year => Self::Year,
            Part::Month => Self::Month,
            Part::Day | Part::Dow | Part::IsoDow | Part::Doy | Part::JulianDay => Self::Day,
            Part::Decade => Self::Decade,
            Part::Century => Self::Century,
            Part::Millennium => Self::Millennium,
            Part::Quarter => Self::Quarter,
            Part::Week | Part::YearWeek => Self::Week,
            Part::IsoYear => Self::IsoYear,
            Part::Microseconds => Self::Microsecond,
            Part::Milliseconds => Self::Millisecond,
            Part::Second | Part::Epoch => Self::Second,
            Part::Minute => Self::Minute,
            Part::Hour => Self::Hour,
            _ => {
                return Err(Error::new(
                    ErrorKind::NotImplemented,
                    format!("Specifier type not implemented for {function}"),
                ));
            }
        })
    }

    const fn time_unit_name(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::Decade => "decade",
            Self::Century => "century",
            Self::Millennium => "millennium",
            Self::Quarter => "quarter",
            Self::Week => "week",
            Self::IsoYear => "isoyear",
            Self::Microsecond => "microseconds",
            Self::Millisecond => "milliseconds",
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
        }
    }
}

fn time_unit_error(unit: Unit) -> Error {
    Error::new(
        ErrorKind::NotImplemented,
        format!("\"time\" units \"{}\" not recognized", unit.time_unit_name()),
    )
}

/// A value `date_diff`/`date_sub` work on.
trait Difference: NativeType {
    fn finite(self) -> bool;
    /// DuckDB's `DateDiff::*Operator`.
    fn diff(start: Self, end: Self, unit: Unit) -> Result<i64>;
    /// DuckDB's `DateSub::*Operator`.
    fn sub(start: Self, end: Self, unit: Unit) -> Result<i64>;
}

fn subtract(end: i64, start: i64) -> Result<i64> {
    end.checked_sub(start)
        .ok_or_else(|| subtract_overflow(end, start))
}

/// `(value + (value < 0)) / units - (value < 0)`: floor division (DuckDB's `Truncate`).
fn floor_truncate(value: i64, units: i64) -> i64 {
    let negative = i64::from(value < 0);
    (value + negative) / units - negative
}

fn date_epoch_micros(date: Date) -> Result<i64> {
    i64::from(date.0)
        .checked_mul(Interval::MICROS_PER_DAY)
        .ok_or_else(|| Error::conversion(format!("Could not convert DATE ({date}) to microseconds")))
}

/// The calendar parts of a date diff.
fn diff_dates(start: Date, end: Date, unit: Unit) -> Result<i64> {
    let (start_year, start_month, _) = start.to_ymd();
    let (end_year, end_month, _) = end.to_ymd();
    let (sy, ey) = (i64::from(start_year), i64::from(end_year));
    let start_months = sy * 12 + i64::from(start_month) - 1;
    let end_months = ey * 12 + i64::from(end_month) - 1;
    Ok(match unit {
        Unit::Year => ey - sy,
        Unit::Month => end_months - start_months,
        Unit::Day => i64::from(end.0) - i64::from(start.0),
        Unit::Decade => ey / 10 - sy / 10,
        Unit::Century => ey / 100 - sy / 100,
        Unit::Millennium => ey / 1000 - sy / 1000,
        Unit::Quarter => end_months / 3 - start_months / 3,
        Unit::Week => (i64::from(end.0) - i64::from(start.0)) / 7,
        Unit::IsoYear => {
            i64::from(super::calendar::iso_year_week(end).0)
                - i64::from(super::calendar::iso_year_week(start).0)
        }
        Unit::Microsecond => subtract(date_epoch_micros(end)?, date_epoch_micros(start)?)?,
        Unit::Millisecond => {
            date_epoch_micros(end)? / Interval::MICROS_PER_MSEC
                - date_epoch_micros(start)? / Interval::MICROS_PER_MSEC
        }
        Unit::Second => (i64::from(end.0) - i64::from(start.0)) * Interval::SECS_PER_DAY,
        Unit::Minute => {
            i64::from(end.0) * Interval::SECS_PER_DAY / Interval::SECS_PER_MINUTE
                - i64::from(start.0) * Interval::SECS_PER_DAY / Interval::SECS_PER_MINUTE
        }
        Unit::Hour => {
            i64::from(end.0) * Interval::SECS_PER_DAY / Interval::SECS_PER_HOUR
                - i64::from(start.0) * Interval::SECS_PER_DAY / Interval::SECS_PER_HOUR
        }
    })
}

/// Microseconds between two timestamps (DuckDB's `DateSub::SubtractMicros`).
fn subtract_micros(start: Timestamp, end: Timestamp) -> Result<i64> {
    subtract(end.0, start.0)
}

/// The month difference of `date_sub` (DuckDB's `DateSub::MonthOperator`): an end at
/// month end matches a start at or after the same month end.
fn sub_months(start: Timestamp, end: Timestamp) -> Result<i64> {
    if start > end {
        return Ok(-sub_months(end, start)?);
    }
    let mut start = start;
    let end_date = end.to_date();
    let end_time = end.to_time();
    let (year, month, day) = end_date.to_ymd();
    let end_days = Date::month_days(year, month).unwrap_or(31);
    if end_days == day {
        let start_date = start.to_date();
        let start_time = start.to_time();
        let (year, month, day) = start_date.to_ymd();
        if day > end_days || (day == end_days && start_time < end_time) {
            let clamped = Date(super::calendar::days_from_ymd(year, month, end_days) as i32);
            start = timestamp_from_datetime(clamped, start_time)?;
        }
    }
    Ok(i64::from(age(end, start).months))
}

impl Difference for Timestamp {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn diff(start: Self, end: Self, unit: Unit) -> Result<i64> {
        match unit {
            Unit::Microsecond => subtract(end.0, start.0),
            Unit::Millisecond => Ok(floor_truncate(end.0, Interval::MICROS_PER_MSEC)
                - floor_truncate(start.0, Interval::MICROS_PER_MSEC)),
            Unit::Second => Ok(floor_truncate(end.0, Interval::MICROS_PER_SEC)
                - floor_truncate(start.0, Interval::MICROS_PER_SEC)),
            Unit::Minute => Ok(floor_truncate(end.0, Interval::MICROS_PER_MINUTE)
                - floor_truncate(start.0, Interval::MICROS_PER_MINUTE)),
            Unit::Hour => Ok(floor_truncate(end.0, Interval::MICROS_PER_HOUR)
                - floor_truncate(start.0, Interval::MICROS_PER_HOUR)),
            _ => diff_dates(start.to_date(), end.to_date(), unit),
        }
    }
    fn sub(start: Self, end: Self, unit: Unit) -> Result<i64> {
        Ok(match unit {
            Unit::Year | Unit::IsoYear => sub_months(start, end)? / 12,
            Unit::Month => sub_months(start, end)?,
            Unit::Quarter => sub_months(start, end)? / 3,
            Unit::Decade => sub_months(start, end)? / 120,
            Unit::Century => sub_months(start, end)? / 1_200,
            Unit::Millennium => sub_months(start, end)? / 12_000,
            Unit::Day => subtract_micros(start, end)? / Interval::MICROS_PER_DAY,
            Unit::Week => subtract_micros(start, end)? / (Interval::MICROS_PER_DAY * 7),
            Unit::Microsecond => subtract_micros(start, end)?,
            Unit::Millisecond => subtract_micros(start, end)? / Interval::MICROS_PER_MSEC,
            Unit::Second => subtract_micros(start, end)? / Interval::MICROS_PER_SEC,
            Unit::Minute => subtract_micros(start, end)? / Interval::MICROS_PER_MINUTE,
            Unit::Hour => subtract_micros(start, end)? / Interval::MICROS_PER_HOUR,
        })
    }
}

impl Difference for Date {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn diff(start: Self, end: Self, unit: Unit) -> Result<i64> {
        diff_dates(start, end, unit)
    }
    fn sub(start: Self, end: Self, unit: Unit) -> Result<i64> {
        let start = timestamp_from_datetime(start, Time(0))?;
        let end = timestamp_from_datetime(end, Time(0))?;
        Timestamp::sub(start, end, unit)
    }
}

impl Difference for Time {
    fn finite(self) -> bool {
        true
    }
    fn diff(start: Self, end: Self, unit: Unit) -> Result<i64> {
        let (start, end) = (start.0, end.0);
        Ok(match unit {
            Unit::Microsecond => end - start,
            Unit::Millisecond => end / Interval::MICROS_PER_MSEC - start / Interval::MICROS_PER_MSEC,
            Unit::Second => end / Interval::MICROS_PER_SEC - start / Interval::MICROS_PER_SEC,
            Unit::Minute => end / Interval::MICROS_PER_MINUTE - start / Interval::MICROS_PER_MINUTE,
            Unit::Hour => end / Interval::MICROS_PER_HOUR - start / Interval::MICROS_PER_HOUR,
            unit => return Err(time_unit_error(unit)),
        })
    }
    fn sub(start: Self, end: Self, unit: Unit) -> Result<i64> {
        let delta = end.0 - start.0;
        Ok(match unit {
            Unit::Microsecond => delta,
            Unit::Millisecond => delta / Interval::MICROS_PER_MSEC,
            Unit::Second => delta / Interval::MICROS_PER_SEC,
            Unit::Minute => delta / Interval::MICROS_PER_MINUTE,
            Unit::Hour => delta / Interval::MICROS_PER_HOUR,
            // DuckDB maps `isoyear` to the year operator in `date_sub`.
            Unit::IsoYear => return Err(time_unit_error(Unit::Year)),
            unit => return Err(time_unit_error(unit)),
        })
    }
}

/// DuckDB's `DateDiffFunction`/`DateSubFunction`: a constant unit is parsed once.
fn difference_rows<T: Difference>(
    args: &DataChunk,
    result: &mut Vector,
    function: &'static str,
    op: fn(T, T, Unit) -> Result<i64>,
) -> Result<()> {
    let units = args.column(0);
    if units.is_constant() {
        if units.is_null(0) {
            *result = Vector::constant_null(LogicalType::BIGINT);
            return Ok(());
        }
        let specifier = crate::values::as_text(&units.get_value(0))?;
        let unit = Unit::from_part(Part::parse(&specifier)?, function)?;
        let values = DataChunk::new(vec![args.column(1).clone(), args.column(2).clone()], args.len());
        return binary::<T, T, i64>(&values, result, |start, end, out| {
            if !start.finite() || !end.finite() {
                return Ok(false);
            }
            *out = op(start, end, unit)?;
            Ok(true)
        });
    }
    ternary::<Str, T, T, i64>(args, result, |specifier, start, end, out| {
        if !start.finite() || !end.finite() {
            return Ok(false);
        }
        let unit = Unit::from_part(Part::parse(&String::from_utf8_lossy(specifier))?, function)?;
        *out = op(start, end, unit)?;
        Ok(true)
    })
}

fn difference_set(name: &str, function: &'static str, sub: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    macro_rules! add {
        ($ty:ty, $logical:expr) => {
            set.add(ScalarFunction::new(
                name,
                vec![LogicalType::VARCHAR, $logical, $logical],
                LogicalType::BIGINT,
                move |args, _, result| {
                    if sub {
                        difference_rows::<$ty>(args, result, function, <$ty as Difference>::sub)
                    } else {
                        difference_rows::<$ty>(args, result, function, <$ty as Difference>::diff)
                    }
                },
            ));
        };
    }
    add!(Date, LogicalType::DATE);
    add!(Timestamp, LogicalType::TIMESTAMP);
    add!(Time, LogicalType::TIME);
    set
}

/// `date_diff`.
pub fn date_diff_functions() -> ScalarFunctionSet {
    difference_set("date_diff", "DATEDIFF", false)
}

/// `date_sub`.
pub fn date_sub_functions() -> ScalarFunctionSet {
    difference_set("date_sub", "DATESUB", true)
}

const NORMAL_DAYS: [i64; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const LEAP_DAYS: [i64; 13] = [0, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// DuckDB's `Interval::GetAge(timestamp_1, timestamp_2)`: the symbolic difference.
pub(crate) fn age(first: Timestamp, second: Timestamp) -> Interval {
    let components = |timestamp: Timestamp| {
        let (year, month, day) = timestamp.to_date().to_ymd();
        let micros = timestamp.to_time().0;
        (
            i64::from(year),
            i64::from(month),
            i64::from(day),
            micros / Interval::MICROS_PER_HOUR,
            micros % Interval::MICROS_PER_HOUR / Interval::MICROS_PER_MINUTE,
            micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_SEC,
            micros % Interval::MICROS_PER_SEC,
        )
    };
    let one = components(first);
    let two = components(second);
    let negative = first < second;
    let sign = if negative { -1 } else { 1 };
    let mut year = (one.0 - two.0) * sign;
    let mut month = (one.1 - two.1) * sign;
    let mut day = (one.2 - two.2) * sign;
    let mut hour = (one.3 - two.3) * sign;
    let mut minute = (one.4 - two.4) * sign;
    let mut second = (one.5 - two.5) * sign;
    let mut micros = (one.6 - two.6) * sign;
    while micros < 0 {
        micros += Interval::MICROS_PER_SEC;
        second -= 1;
    }
    while second < 0 {
        second += 60;
        minute -= 1;
    }
    while minute < 0 {
        minute += 60;
        hour -= 1;
    }
    while hour < 0 {
        hour += 24;
        day -= 1;
    }
    while day < 0 {
        let (reference_year, reference_month) = if negative { (one.0, one.1) } else { (two.0, two.1) };
        let leap = Date::is_leap_year(reference_year as i32);
        day += if leap { LEAP_DAYS } else { NORMAL_DAYS }[reference_month as usize];
        month -= 1;
    }
    while month < 0 {
        month += 12;
        year -= 1;
    }
    let (year, month, day, hour, minute, second, micros) = (
        year * sign,
        month * sign,
        day * sign,
        hour * sign,
        minute * sign,
        second * sign,
        micros * sign,
    );
    Interval::new(
        (year * 12 + month) as i32,
        day as i32,
        ((hour * 60 + minute) * 60 + second) * Interval::MICROS_PER_SEC + micros,
    )
}

/// The connection whose transaction start `age(timestamp)` uses.
#[derive(Debug)]
struct ClientData(std::sync::Arc<rustdb_function::FunctionClient>);

impl rustdb_function::FunctionData for ClientData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `age(timestamp)` and `age(timestamp, timestamp)`.
pub fn age_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("age");
    let mut current = ScalarFunction::new(
        "age",
        vec![LogicalType::TIMESTAMP],
        LogicalType::INTERVAL,
        |args, state, result| {
            let client = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ClientData>())
                .ok_or_else(|| Error::internal("age without bind data"))?;
            // Midnight (UTC) of the day the transaction started.
            let start = Timestamp(client.0.transaction_start_micros());
            let today = timestamp_from_datetime(start.to_date(), Time(0))?;
            binary_age_rows(args, result, today)
        },
    )
    .with_bind(|_, input| {
        Ok(Some(
            std::sync::Arc::new(ClientData(std::sync::Arc::clone(input.client)))
                as std::sync::Arc<dyn rustdb_function::FunctionData>,
        ))
    });
    current.stability = rustdb_function::Stability::ConsistentWithinQuery;
    set.add(current);
    set.add(ScalarFunction::new(
        "age",
        vec![LogicalType::TIMESTAMP, LogicalType::TIMESTAMP],
        LogicalType::INTERVAL,
        |args, _, result| {
            binary::<Timestamp, Timestamp, Interval>(args, result, |first, second, out| {
                if !first.is_finite() || !second.is_finite() {
                    return Ok(false);
                }
                *out = age(first, second);
                Ok(true)
            })
        },
    ));
    set
}

/// `age(current_date, value)` per row.
fn binary_age_rows(args: &DataChunk, result: &mut Vector, today: Timestamp) -> Result<()> {
    crate::scalar::executor::unary::<Timestamp, Interval>(args, result, |value, out| {
        if !value.is_finite() {
            return Ok(false);
        }
        *out = age(today, value);
        Ok(true)
    })
}
