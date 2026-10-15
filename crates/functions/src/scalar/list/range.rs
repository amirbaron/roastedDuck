//! The scalar `range` and `generate_series` (DuckDB's `core_functions/scalar/list/range.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, Interval, ListEntry, LogicalType, NativeType, Timestamp, Unified, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::all_constant;
use crate::temporal::timestamp_add_interval;

fn too_large() -> Error {
    Error::invalid_input("Lists larger than 2^32 elements are not supported")
}

/// The element type of a range and how it steps.
trait RangeInfo {
    type Value: NativeType;
    type Increment: NativeType;
    fn default_start() -> Self::Value;
    fn default_increment() -> Self::Increment;
    fn length(start: Self::Value, end: Self::Value, increment: Self::Increment, inclusive: bool) -> Result<u64>;
    fn step(value: Self::Value, increment: Self::Increment) -> Result<Self::Value>;
}

/// BIGINT ranges (DuckDB's `NumericRangeInfo`).
struct Numeric;

impl RangeInfo for Numeric {
    type Value = i64;
    type Increment = i64;
    fn default_start() -> i64 {
        0
    }
    fn default_increment() -> i64 {
        1
    }
    fn length(start: i64, end: i64, increment: i64, inclusive: bool) -> Result<u64> {
        if increment == 0 || (start > end && increment > 0) || (start < end && increment < 0) {
            return Ok(0);
        }
        let total_diff = (i128::from(end) - i128::from(start)).abs();
        let step = i128::from(increment).abs();
        let mut total = total_diff / step;
        if total_diff % step == 0 {
            if inclusive {
                total += 1;
            }
        } else {
            total += 1;
        }
        if total > i128::from(u32::MAX) {
            return Err(too_large());
        }
        Ok(total as u64)
    }
    fn step(value: i64, increment: i64) -> Result<i64> {
        Ok(value.wrapping_add(increment))
    }
}

/// TIMESTAMP ranges stepping by an INTERVAL (DuckDB's `TimestampRangeInfo`).
struct Timestamps;

impl RangeInfo for Timestamps {
    type Value = Timestamp;
    type Increment = Interval;
    fn default_start() -> Timestamp {
        Timestamp(0)
    }
    fn default_increment() -> Interval {
        Interval::new(0, 0, 0)
    }
    fn length(start: Timestamp, end: Timestamp, increment: Interval, inclusive: bool) -> Result<u64> {
        let positive = increment.months > 0 || increment.days > 0 || increment.micros > 0;
        let negative = increment.months < 0 || increment.days < 0 || increment.micros < 0;
        if !negative && !positive {
            return Ok(0);
        }
        if !start.is_finite() || !end.is_finite() {
            return Err(Error::invalid_input("Interval infinite bounds not supported"));
        }
        if negative && positive {
            return Err(Error::invalid_input(
                "Interval with mix of negative/positive entries not supported",
            ));
        }
        if (start > end && positive) || (start < end && negative) {
            return Ok(0);
        }
        let mut total: u64 = 0;
        let mut value = start;
        loop {
            let more = match (negative, inclusive) {
                (true, true) => value >= end,
                (true, false) => value > end,
                (false, true) => value <= end,
                (false, false) => value < end,
            };
            if !more {
                break;
            }
            value = timestamp_add_interval(value, increment)?;
            total += 1;
            if total > u64::from(u32::MAX) {
                return Err(too_large());
            }
        }
        Ok(total)
    }
    fn step(value: Timestamp, increment: Interval) -> Result<Timestamp> {
        timestamp_add_interval(value, increment)
    }
}

/// DuckDB's `ListRangeFunction`.
fn range_rows<I: RangeInfo>(args: &DataChunk, result: &mut Vector, inclusive: bool) -> Result<()> {
    let count = args.len();
    let columns: Vec<_> = args.columns().iter().map(|column| addressable(column, count)).collect();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let arity = columns.len();
    let starts: Option<Unified<'_, I::Value>> = (arity > 1).then(|| columns[0].unified::<I::Value>());
    let ends = columns[usize::from(arity > 1)].unified::<I::Value>();
    let increments: Option<Unified<'_, I::Increment>> = (arity > 2).then(|| columns[2].unified::<I::Increment>());
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut values: Vec<I::Value> = Vec::new();
    for row in 0..rows {
        let start = match &starts {
            Some(starts) => starts.get(row),
            None => Some(I::default_start()),
        };
        let increment = match &increments {
            Some(increments) => increments.get(row),
            None => Some(I::default_increment()),
        };
        let (Some(start), Some(end), Some(increment)) = (start, ends.get(row), increment) else {
            nulls.push(row);
            entries.push(ListEntry::new(values.len(), 0));
            continue;
        };
        let length = I::length(start, end, increment, inclusive)? as usize;
        entries.push(ListEntry::new(values.len(), length));
        let mut value = start;
        for index in 0..length {
            if index > 0 {
                value = I::step(value, increment)?;
            }
            values.push(value);
        }
    }
    let size = values.len();
    let child_type = crate::scalar::nested::list_child_type(result.logical_type())?;
    let mut child = Vector::flat(child_type, size.max(1));
    child.reset_flat(size);
    child.values_mut::<I::Value>()[..size].copy_from_slice(&values);
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
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

fn range_set(name: &str, inclusive: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    let list = LogicalType::list(LogicalType::BIGINT);
    for arity in 1..=3 {
        set.add(ScalarFunction::new(
            name,
            vec![LogicalType::BIGINT; arity],
            list.clone(),
            move |args, _, result| range_rows::<Numeric>(args, result, inclusive),
        ));
    }
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::TIMESTAMP, LogicalType::TIMESTAMP, LogicalType::INTERVAL],
        LogicalType::list(LogicalType::TIMESTAMP),
        move |args, _, result| range_rows::<Timestamps>(args, result, inclusive),
    ));
    set
}

/// The scalar `range` (exclusive end) and `generate_series` (inclusive end).
pub fn range_functions() -> Vec<ScalarFunctionSet> {
    vec![range_set("range", false), range_set("generate_series", true)]
}
