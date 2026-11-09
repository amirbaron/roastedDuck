//! `to_years`, `to_days`, `to_seconds`, ...: intervals from numbers (DuckDB's
//! `to_interval.cpp`).

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::numeric::TryCastTo;
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{Interval, LogicalType, NativeType};

use crate::scalar::executor::unary;
use crate::values::cast_error;

/// `Cast::Operation<T, int32_t>` with DuckDB's error.
fn to_i32<T: TryCastTo<i32> + roastedduck_function::cast::numeric::CastNumber>(value: T) -> Result<i32> {
    value.try_cast_to().ok_or_else(|| cast_error(value, "INT32"))
}

/// A unit whose count fills the months or days field.
#[derive(Clone, Copy)]
enum WholeUnit {
    Millennia,
    Centuries,
    Decades,
    Years,
    Quarters,
    Months,
    Weeks,
    Days,
}

impl WholeUnit {
    const fn name(self) -> &'static str {
        match self {
            Self::Millennia => "millennia",
            Self::Centuries => "centuries",
            Self::Decades => "decades",
            Self::Years => "years",
            Self::Quarters => "quarters",
            Self::Months => "months",
            Self::Weeks => "weeks",
            Self::Days => "days",
        }
    }

    /// Months (or days for weeks/days) per unit.
    const fn factor(self) -> i32 {
        match self {
            Self::Millennia => 12_000,
            Self::Centuries => 1_200,
            Self::Decades => 120,
            Self::Years => 12,
            Self::Quarters => 3,
            Self::Months | Self::Days => 1,
            Self::Weeks => 7,
        }
    }

    fn interval<T>(self, value: T) -> Result<Interval>
    where
        T: TryCastTo<i32> + roastedduck_function::cast::numeric::CastNumber + std::fmt::Display,
    {
        let count = to_i32(value)?;
        let scaled = count.checked_mul(self.factor()).ok_or_else(|| {
            Error::out_of_range(format!("Interval value {value} {} out of range", self.name()))
        })?;
        Ok(match self {
            Self::Weeks | Self::Days => Interval::new(0, scaled, 0),
            _ => Interval::new(scaled, 0, 0),
        })
    }
}

fn whole_unit_set(name: &str, unit: WholeUnit) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::INTEGER],
        LogicalType::INTERVAL,
        move |args, _, result| {
            unary::<i32, Interval>(args, result, |value, out| {
                *out = unit.interval(value)?;
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::BIGINT],
        LogicalType::INTERVAL,
        move |args, _, result| {
            unary::<i64, Interval>(args, result, |value, out| {
                *out = unit.interval(value)?;
                Ok(true)
            })
        },
    ));
    set
}

/// A unit converted to microseconds.
fn micros_set<T: NativeType + Copy>(
    name: &str,
    input: LogicalType,
    convert: fn(T) -> Result<i64>,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![input],
        LogicalType::INTERVAL,
        move |args, _, result| {
            unary::<T, Interval>(args, result, |value, out| {
                *out = Interval::new(0, 0, convert(value)?);
                Ok(true)
            })
        },
    ));
    set
}

/// `value * micros` for an integer count (DuckDB's `TryMultiplyOperator<int64_t>`).
fn integer_micros(value: i64, micros: i64, unit: &str) -> Result<i64> {
    value
        .checked_mul(micros)
        .ok_or_else(|| Error::out_of_range(format!("Interval value {value} {unit} out of range")))
}

/// `value * micros` for a DOUBLE count (DuckDB's `TryMultiplyOperator<double, int64_t>`,
/// rounding to the nearest microsecond).
fn double_micros(value: f64, micros: i64, unit: &str) -> Result<i64> {
    let product = value * micros as f64;
    // DuckDB renders the value with `std::to_string` (`%f`).
    product
        .try_cast_to()
        .ok_or_else(|| Error::out_of_range(format!("Interval value {value:.6} {unit} out of range")))
}

/// Every `to_*` interval function.
pub fn to_interval_functions() -> Vec<ScalarFunctionSet> {
    vec![
        whole_unit_set("to_millennia", WholeUnit::Millennia),
        whole_unit_set("to_centuries", WholeUnit::Centuries),
        whole_unit_set("to_decades", WholeUnit::Decades),
        whole_unit_set("to_years", WholeUnit::Years),
        whole_unit_set("to_quarters", WholeUnit::Quarters),
        whole_unit_set("to_months", WholeUnit::Months),
        whole_unit_set("to_weeks", WholeUnit::Weeks),
        whole_unit_set("to_days", WholeUnit::Days),
        micros_set::<i64>("to_hours", LogicalType::BIGINT, |v| {
            integer_micros(v, Interval::MICROS_PER_HOUR, "hours")
        }),
        micros_set::<i64>("to_minutes", LogicalType::BIGINT, |v| {
            integer_micros(v, Interval::MICROS_PER_MINUTE, "minutes")
        }),
        micros_set::<f64>("to_seconds", LogicalType::DOUBLE, |v| {
            double_micros(v, Interval::MICROS_PER_SEC, "seconds")
        }),
        micros_set::<f64>("to_milliseconds", LogicalType::DOUBLE, |v| {
            double_micros(v, Interval::MICROS_PER_MSEC, "milliseconds")
        }),
        micros_set::<i64>("to_microseconds", LogicalType::BIGINT, Ok),
    ]
}
