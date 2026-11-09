//! `equi_width_bins(min, max, bin_count, nice_rounding)` (DuckDB's
//! `core_functions/scalar/generic/binning.cpp`): the upper boundaries of `bin_count`
//! equal-width bins, optionally rounded to "nice" numbers.

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use roastedduck_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{
    DataChunk, Date, Interval, LogicalType, LogicalTypeId, Time, Timestamp, Value, Vector,
};

use crate::temporal::{interval_invert, timestamp_add_interval};

/// DuckDB's `NiceRounding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rounding {
    Ceiling,
    Round,
}

fn previous_power_of_ten_int(input: i128) -> i128 {
    let mut power = 1i128;
    while power < input {
        power = power.saturating_mul(10);
    }
    power / 10
}

/// `hugeint_t::operator/`: unchecked, so a zero divisor yields 0 (`Hugeint::DivMod`).
fn hugeint_div(lhs: i128, rhs: i128) -> i128 {
    if rhs == 0 { 0 } else { lhs.wrapping_div(rhs) }
}

fn round_to_number_int(input: i128, number: i128, rounding: Rounding) -> i128 {
    match rounding {
        Rounding::Round => hugeint_div(input + number / 2, number) * number,
        Rounding::Ceiling => hugeint_div(input + (number - 1), number) * number,
    }
}

/// DuckDB's `MakeNumberNice(hugeint_t, ...)`.
fn make_nice_int(input: i128, step: i128, rounding: Rounding) -> i128 {
    let power = previous_power_of_ten_int(step);
    let mut two = power * 2;
    let mut five = power;
    if power * 3 <= step {
        two *= 5;
    }
    if power * 2 <= step {
        five *= 5;
    }
    let round_to_two = round_to_number_int(input, two, rounding);
    let round_to_five = round_to_number_int(input, five, rounding);
    if (input - round_to_two).abs() < (input - round_to_five).abs() {
        round_to_two
    } else {
        round_to_five
    }
}

/// DuckDB's `EquiWidthBinsInteger::Operation`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "shares the signature of the other kernels"
)]
fn integer_bins(input_min: i64, input_max: i64, mut bin_count: u64, nice: bool) -> Result<Vec<i64>> {
    const FACTOR: i128 = 1000;
    let min = i128::from(input_min) * FACTOR;
    let mut max = i128::from(input_max) * FACTOR;
    let span = max - min;
    let mut step = span / i128::from(bin_count);
    if nice {
        let new_step = make_nice_int(step, step, Rounding::Round);
        let new_max = round_to_number_int(max, new_step, Rounding::Ceiling);
        if new_max != min && new_step != 0 {
            max = new_max;
            step = new_step;
        }
        bin_count *= 2;
    }
    let mut result: Vec<i64> = Vec::new();
    let mut boundary = max;
    while boundary > min {
        // The rounded-up maximum can leave the BIGINT range; DuckDB keeps the low bits, and
        // the caller then replaces that first boundary with `max`.
        #[allow(clippy::cast_possible_truncation, reason = "Hugeint::Cast keeps the low bits")]
        let real = (boundary / FACTOR) as i64;
        let skip = match result.last() {
            Some(&last) => {
                if real < input_min || result.len() as u64 >= bin_count {
                    break;
                }
                real == last
            }
            None => false,
        };
        if !skip {
            result.push(real);
        }
        if step == 0 {
            // DuckDB loops forever here; stop instead.
            break;
        }
        boundary -= step;
    }
    Ok(result)
}

fn previous_power_of_ten(input: f64) -> f64 {
    let mut power = 1.0;
    if input < 1.0 {
        while power > input {
            power /= 10.0;
        }
        return power;
    }
    while power < input {
        power *= 10.0;
    }
    power / 10.0
}

fn round_to_number(input: f64, number: f64, rounding: Rounding) -> f64 {
    let result = match rounding {
        Rounding::Round => (input / number).round() * number,
        Rounding::Ceiling => (input / number).ceil() * number,
    };
    if result.is_finite() { result } else { input }
}

/// DuckDB's `MakeNumberNice(double, ...)`.
fn make_nice(input: f64, step: f64, rounding: Rounding) -> f64 {
    if input == 0.0 {
        return 0.0;
    }
    let power = previous_power_of_ten(step);
    let mut two = power * 2.0;
    let mut five = power;
    if power * 3.0 <= step {
        two *= 5.0;
    }
    if power * 2.0 <= step {
        five *= 5.0;
    }
    let round_to_two = round_to_number(input, two, rounding);
    let round_to_five = round_to_number(input, five, rounding);
    if (input - round_to_two).abs() < (input - round_to_five).abs() {
        round_to_two
    } else {
        round_to_five
    }
}

/// DuckDB's `EquiWidthBinsDouble::Operation`.
#[allow(clippy::float_cmp, reason = "DuckDB compares the boundaries exactly")]
fn double_bins(min: f64, input_max: f64, mut bin_count: u64, nice: bool) -> Result<Vec<f64>> {
    let mut max = input_max;
    if !min.is_finite() || !max.is_finite() {
        return Err(Error::invalid_input(
            "equi_width_bucket does not support infinite or nan as min/max value",
        ));
    }
    let span = max - min;
    let mut step = if span.is_finite() {
        span / bin_count as f64
    } else {
        max / bin_count as f64 - min / bin_count as f64
    };
    let step_power_of_ten = previous_power_of_ten(step);
    if nice {
        step = make_nice(step, step, Rounding::Round);
        max = round_to_number(input_max, step, Rounding::Ceiling);
        bin_count *= 2;
    }
    if step == 0.0 {
        return Err(Error::internal("step is 0!?"));
    }
    let round_multiplication = 10.0 / step_power_of_ten;
    let mut result: Vec<f64> = Vec::new();
    let mut boundary = max;
    while boundary > min {
        let real = if nice {
            (boundary * round_multiplication).round() / round_multiplication
        } else {
            boundary
        };
        if result.last() != Some(&real) {
            if real <= min || result.len() as u64 >= bin_count {
                break;
            }
            result.push(real);
        }
        boundary -= step;
    }
    Ok(result)
}

/// The calendar fields of a timestamp.
#[derive(Clone, Copy, Debug)]
struct Fields {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    micros: i32,
}

impl Fields {
    fn of(timestamp: Timestamp) -> Self {
        let (year, month, day) = timestamp.to_date().to_ymd();
        let micros = timestamp.to_time().0;
        Self {
            year,
            month,
            day,
            hour: (micros / Interval::MICROS_PER_HOUR) as i32,
            minute: (micros % Interval::MICROS_PER_HOUR / Interval::MICROS_PER_MINUTE) as i32,
            second: (micros % Interval::MICROS_PER_MINUTE / Interval::MICROS_PER_SEC) as i32,
            micros: (micros % Interval::MICROS_PER_SEC) as i32,
        }
    }

    fn next_month(&mut self) {
        self.month += 1;
        if self.month == 13 {
            self.year += 1;
            self.month = 1;
        }
    }

    fn next_day(&mut self) {
        self.day += 1;
        if !Date::is_valid(self.year, self.month, self.day) {
            self.next_month();
            self.day = 1;
        }
    }

    fn next_hour(&mut self) {
        self.hour += 1;
        if self.hour >= 24 {
            self.next_day();
            self.hour = 0;
        }
    }

    fn next_minute(&mut self) {
        self.minute += 1;
        if self.minute >= 60 {
            self.next_hour();
            self.minute = 0;
        }
    }

    fn next_second(&mut self) {
        self.second += 1;
        if self.second >= 60 {
            self.next_minute();
            self.second = 0;
        }
    }

    /// DuckDB's `MakeTimestampNice`.
    fn nice(mut self, step: Interval) -> Result<Timestamp> {
        if step.months >= 12 {
            if self.day > 1 || self.hour > 0 || self.minute > 0 || self.second > 0 || self.micros > 0 {
                self.next_month();
                self.hour = 0;
                self.minute = 0;
                self.second = 0;
                self.micros = 0;
                self.day = 1;
            }
        } else if step.months > 0 || step.days >= 1 {
            if self.hour > 0 || self.minute > 0 || self.second > 0 || self.micros > 0 {
                self.next_day();
                self.hour = 0;
                self.minute = 0;
                self.second = 0;
                self.micros = 0;
            }
        } else if step.days > 0 || step.micros >= Interval::MICROS_PER_HOUR {
            if self.minute > 0 || self.second > 0 || self.micros > 0 {
                self.next_hour();
                self.minute = 0;
                self.second = 0;
                self.micros = 0;
            }
        } else if step.micros >= Interval::MICROS_PER_MINUTE {
            if self.second > 0 || self.micros > 0 {
                self.next_minute();
                self.second = 0;
                self.micros = 0;
            }
        } else if step.micros >= Interval::MICROS_PER_SEC && self.micros > 0 {
            self.next_second();
            self.micros = 0;
        }
        let date = Date::from_ymd(self.year, self.month, self.day)
            .ok_or_else(|| Error::conversion("Date out of range"))?;
        let time = Time::from_hms_micros(self.hour, self.minute, self.second, self.micros);
        Timestamp::from_datetime(date, time)
            .ok_or_else(|| Error::conversion("Date and time not in timestamp range"))
    }
}

fn round_to_divisor(number: i64, divisor: i64) -> i64 {
    (number + divisor / 2) / divisor * divisor
}

/// DuckDB's `MakeIntervalNice`.
fn make_interval_nice(mut interval: Interval) -> Interval {
    const HOUR: i64 = Interval::MICROS_PER_HOUR;
    const MINUTE: i64 = Interval::MICROS_PER_MINUTE;
    const SECOND: i64 = Interval::MICROS_PER_SEC;
    if interval.months >= 6 {
        interval.days = 0;
        interval.micros = 0;
    } else if interval.months > 0 || interval.days >= 5 {
        interval.micros = 0;
    } else if interval.days > 0 || interval.micros >= 6 * HOUR {
        interval.micros = round_to_divisor(interval.micros, HOUR);
    } else if interval.micros >= HOUR {
        interval.micros = round_to_divisor(interval.micros, MINUTE * 15);
    } else if interval.micros >= MINUTE * 10 {
        interval.micros = round_to_divisor(interval.micros, MINUTE);
    } else if interval.micros >= MINUTE {
        interval.micros = round_to_divisor(interval.micros, SECOND * 15);
    } else if interval.micros >= SECOND * 10 {
        interval.micros = round_to_divisor(interval.micros, SECOND);
    }
    interval
}

/// DuckDB's `EquiWidthBinsTimestamp::Operation`.
#[allow(clippy::cast_possible_truncation, reason = "static_cast of the step parts")]
fn timestamp_bins(
    input_min: Timestamp,
    input_max: Timestamp,
    bin_count: u64,
    nice: bool,
) -> Result<Vec<Timestamp>> {
    if !input_min.is_finite() || !input_max.is_finite() {
        return Err(Error::invalid_input(
            "equi_width_bucket does not support infinite or nan as min/max value",
        ));
    }
    if !nice {
        return Ok(integer_bins(input_min.0, input_max.0, bin_count, false)?
            .into_iter()
            .map(Timestamp)
            .collect());
    }
    let min = Fields::of(input_min);
    let max = Fields::of(input_max);
    let months = (max.year - min.year) * 12 + (max.month - min.month);
    let days = max.day - min.day;
    let micros = i64::from(max.hour - min.hour) * Interval::MICROS_PER_HOUR
        + i64::from(max.minute - min.minute) * Interval::MICROS_PER_MINUTE
        + i64::from(max.second - min.second) * Interval::MICROS_PER_SEC
        + i64::from(max.micros - min.micros);
    let step_months = f64::from(months) / bin_count as f64;
    let mut step_days = f64::from(days) / f64::from(bin_count as i32);
    let mut step_micros = micros as f64 / bin_count as f64;
    if step_months > 0.0 {
        step_days += (step_months - step_months.floor()) * 30.0;
    }
    if step_days > 0.0 {
        step_micros += (step_days - step_days.floor()) * Interval::MICROS_PER_DAY as f64;
    }
    let mut step = make_interval_nice(Interval {
        months: step_months as i32,
        days: step_days as i32,
        micros: step_micros as i64,
    });
    let mut value = max.nice(step)?;
    if step.months <= 0 && step.days <= 0 && step.micros <= 0 {
        step = Interval {
            months: 0,
            days: 0,
            micros: 1,
        };
    }
    let mut result = Vec::new();
    while value.0 >= input_min.0 && (result.len() as u64) < bin_count {
        result.push(value);
        value = timestamp_add_interval(value, interval_invert(step))?;
    }
    Ok(result)
}

/// Which `equi_width_bins` overload runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Integer,
    Double,
    Timestamp,
}

/// The bins of one row, validated like DuckDB's `EquiWidthBinFunction`.
fn row_bins<T: Copy + PartialOrd>(
    min: T,
    max: T,
    bins: i64,
    nice: bool,
    compute: impl Fn(T, T, u64, bool) -> Result<Vec<T>>,
) -> Result<Vec<T>> {
    const MAX_BIN_COUNT: i64 = 1_000_000;
    if max < min {
        return Err(Error::invalid_input(
            "Invalid input for bin function - max value is smaller than min value",
        ));
    }
    if bins <= 0 {
        return Err(Error::invalid_input(
            "Invalid input for bin function - there must be > 0 bins",
        ));
    }
    if bins > MAX_BIN_COUNT {
        return Err(Error::invalid_input(format!(
            "Invalid input for bin function - max bin count of {MAX_BIN_COUNT} exceeded"
        )));
    }
    let mut values = if max == min {
        vec![max]
    } else {
        let mut values = compute(min, max, bins as u64, nice)?;
        if let Some(first) = values.first_mut()
            && *first < max
        {
            *first = max;
        }
        values.reverse();
        values
    };
    values.shrink_to_fit();
    Ok(values)
}

fn bins_kernel(args: &DataChunk, result: &mut Vector, kind: Kind) -> Result<()> {
    let count = args.len();
    let element = match kind {
        Kind::Integer => LogicalType::BIGINT,
        Kind::Double => LogicalType::DOUBLE,
        Kind::Timestamp => LogicalType::TIMESTAMP,
    };
    let list_type = LogicalType::list(element.clone());
    let mut lists = Vector::flat(list_type.clone(), count.max(1));
    for row in 0..count {
        let min = args.column(0).get_value(row);
        let max = args.column(1).get_value(row);
        let (Some(bins), Some(nice)) = (
            args.column(2).get_value(row).as_i64(),
            args.column(3).get_value(row).as_bool(),
        ) else {
            lists.set_value(row, &Value::null(list_type.clone()))?;
            continue;
        };
        if min.is_null() || max.is_null() {
            lists.set_value(row, &Value::null(list_type.clone()))?;
            continue;
        }
        let values: Vec<Value> = match kind {
            Kind::Integer => row_bins(bigint(&min)?, bigint(&max)?, bins, nice, integer_bins)?
                .into_iter()
                .map(Value::bigint)
                .collect(),
            Kind::Double => {
                let (min, max) = (min.as_f64().unwrap_or(0.0), max.as_f64().unwrap_or(0.0));
                row_bins(min, max, bins, nice, double_bins)?
                    .into_iter()
                    .map(Value::double)
                    .collect()
            }
            Kind::Timestamp => {
                let (Some(min), Some(max)) = (min.as_timestamp(), max.as_timestamp()) else {
                    return Err(Error::internal("equi_width_bins bound is not a TIMESTAMP"));
                };
                row_bins(min, max, bins, nice, timestamp_bins)?
                    .into_iter()
                    .map(Value::timestamp)
                    .collect()
            }
        };
        lists.set_value(row, &Value::list(element.clone(), values))?;
    }
    let constant = args.columns().iter().all(Vector::is_constant);
    let target = result.logical_type().clone();
    *result = cast_vector(
        &CastFunctionSet::new(),
        &lists,
        &target,
        count,
        &mut CastParameters::new(),
    )?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn bigint(value: &Value) -> Result<i64> {
    value
        .as_i128()
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| Error::internal("equi_width_bins bound is not a BIGINT"))
}

/// DuckDB's `BindEquiWidthFunction`: the result holds the type of `max`.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<std::sync::Arc<dyn FunctionData>>> {
    let child = match input.argument_types[1].id() {
        LogicalTypeId::Unknown | LogicalTypeId::SqlNull => return Ok(None),
        LogicalTypeId::Decimal => LogicalType::DOUBLE,
        _ => input.argument_types[1].clone(),
    };
    function.return_type = LogicalType::list(child);
    Ok(None)
}

/// `equi_width_bins`.
pub fn equi_width_bins_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("equi_width_bins");
    let any_list = LogicalType::list(LogicalType::ANY);
    for (ty, kind) in [
        (LogicalType::BIGINT, Kind::Integer),
        (LogicalType::DOUBLE, Kind::Double),
        (LogicalType::TIMESTAMP, Kind::Timestamp),
    ] {
        set.add(
            ScalarFunction::new(
                "equi_width_bins",
                vec![ty.clone(), ty, LogicalType::BIGINT, LogicalType::BOOLEAN],
                any_list.clone(),
                move |args, _, result| bins_kernel(args, result, kind),
            )
            .with_bind(bind),
        );
    }
    let unsupported = LogicalType::any_params(LogicalType::ANY, 150);
    set.add(
        ScalarFunction::new(
            "equi_width_bins",
            vec![
                unsupported.clone(),
                unsupported,
                LogicalType::BIGINT,
                LogicalType::BOOLEAN,
            ],
            any_list,
            |args, _, _| {
                Err(Error::binder(format!(
                    "Unsupported type \"{}\" for equi_width_bins",
                    args.column(0).logical_type()
                )))
            },
        )
        .with_bind(bind),
    );
    // `EquiWidthBinSerialize` writes nothing: the bind runs again.
    for function in &mut set.functions {
        function.serialize = Some(std::sync::Arc::new(|_, _, _| Ok(())));
    }
    set
}
