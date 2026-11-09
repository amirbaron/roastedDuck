//! DuckDB's `icu-list-range.cpp`: the scalar `range` / `generate_series` over
//! TIMESTAMPTZ, stepping on the session's calendar (`ICUListRange`).

use std::borrow::Cow;

use roastedduck_common::{Error, Result};
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, Interval, ListEntry, LogicalType, Timestamp, Vector, VectorKind};

use crate::icu_dateadd::add;
use crate::icu_datefunc::{self, BindData, TzCalendar};

/// `RangeInfoStruct::ListLength`: the number of values from `start_value` to `end_value`.
fn list_length(
    start_value: Timestamp,
    end_value: Timestamp,
    increment_value: Interval,
    inclusive_bound: bool,
    calendar: &mut TzCalendar,
) -> Result<u64> {
    let is_positive = increment_value.months > 0 || increment_value.days > 0 || increment_value.micros > 0;
    let is_negative = increment_value.months < 0 || increment_value.days < 0 || increment_value.micros < 0;
    if !is_negative && !is_positive {
        // interval is 0: no result
        return Ok(0);
    }
    // We don't allow infinite bounds because they generate errors or infinite loops
    if !start_value.is_finite() || !end_value.is_finite() {
        return Err(Error::invalid_input("Interval infinite bounds not supported"));
    }
    if is_negative && is_positive {
        // we don't allow a mix of
        return Err(Error::invalid_input(
            "Interval with mix of negative/positive entries not supported",
        ));
    }
    if start_value > end_value && is_positive {
        return Ok(0);
    }
    if start_value < end_value && is_negative {
        return Ok(0);
    }
    let mut start_value = start_value;
    let mut total_values: u64 = 0;
    let in_range = |value: Timestamp| {
        if is_negative {
            // negative interval, start_value is going down
            if inclusive_bound {
                value >= end_value
            } else {
                value > end_value
            }
        } else if inclusive_bound {
            value <= end_value
        } else {
            value < end_value
        }
    };
    while in_range(start_value) {
        start_value = add(calendar, start_value, increment_value)?;
        total_values += 1;
        if total_values > u64::from(u32::MAX) {
            return Err(Error::invalid_input(
                "Lists larger than 2^32 elements are not supported",
            ));
        }
    }
    Ok(total_values)
}

/// `ICUListRange::ICUListRangeFunction`.
fn list_range_rows(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    inclusive_bound: bool,
) -> Result<()> {
    let bind_info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar = TzCalendar::new(&bind_info.calendar, &bind_info.cal_setting);
    let count = args.len();
    let columns: Vec<Cow<'_, Vector>> = args
        .columns()
        .iter()
        .map(|column| {
            if column.kind() == VectorKind::Sequence {
                Cow::Owned(column.flattened(count))
            } else {
                Cow::Borrowed(column)
            }
        })
        .collect();
    let constant = columns.iter().all(|column| column.is_constant());
    let rows = if constant { 1 } else { count };
    let starts = columns[0].unified::<Timestamp>();
    let ends = columns[1].unified::<Timestamp>();
    let increments = columns[2].unified::<Interval>();
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut values: Vec<Timestamp> = Vec::new();
    for row in 0..rows {
        let (Some(start), Some(end), Some(increment)) = (starts.get(row), ends.get(row), increments.get(row))
        else {
            nulls.push(row);
            entries.push(ListEntry::new(values.len(), 0));
            continue;
        };
        let length = list_length(start, end, increment, inclusive_bound, &mut calendar)? as usize;
        entries.push(ListEntry::new(values.len(), length));
        let mut range_value = start;
        for range_idx in 0..length {
            if range_idx > 0 {
                range_value = add(&mut calendar, range_value, increment)?;
            }
            values.push(range_value);
        }
    }
    let size = values.len();
    let mut child = Vector::flat(LogicalType::TIMESTAMP_TZ, size.max(1));
    child.reset_flat(size);
    child.values_mut::<Timestamp>()[..size].copy_from_slice(&values);
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

/// One `(TIMESTAMPTZ, TIMESTAMPTZ, INTERVAL) -> TIMESTAMPTZ[]` set.
fn list_range_set(name: &str, inclusive_bound: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(
        ScalarFunction::new(
            name,
            vec![
                LogicalType::TIMESTAMP_TZ,
                LogicalType::TIMESTAMP_TZ,
                LogicalType::INTERVAL,
            ],
            LogicalType::list(LogicalType::TIMESTAMP_TZ),
            move |args, state, result| list_range_rows(args, state, result, inclusive_bound),
        )
        .with_bind(icu_datefunc::bind),
    );
    set
}

/// `ICUListRange::AddICUListRangeFunction`: `range` and `generate_series` (inclusive
/// bounds on the RHS).
pub(crate) fn list_range_functions() -> Vec<ScalarFunctionSet> {
    vec![
        list_range_set("range", false),
        list_range_set("generate_series", true),
    ]
}
