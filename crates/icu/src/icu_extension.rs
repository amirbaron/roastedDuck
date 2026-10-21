//! DuckDB's `icu_extension.cpp`: the `TimeZone` and `Calendar` settings and
//! `icu_calendar_names()`.

use std::sync::Arc;

use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, Result};
use rustdb_function::{ExtensionOption, TableFunction, TableFunctionBindOutput, TableFunctionSet};
use rustdb_types::{LogicalType, Value, Vector};

use crate::icu4c::timezone::host_time_zone_id;
use crate::icu_helpers;

/// The calendar keyword values (`Calendar::getKeywordValuesForLocale("calendar", ...)`),
/// in ICU's order.
pub(crate) const CALENDAR_NAMES: [&str; 18] = [
    "gregorian",
    "japanese",
    "buddhist",
    "roc",
    "persian",
    "islamic-civil",
    "islamic",
    "hebrew",
    "chinese",
    "indian",
    "coptic",
    "ethiopic",
    "ethiopic-amete-alem",
    "iso8601",
    "dangi",
    "islamic-umalqura",
    "islamic-tbla",
    "islamic-rgsa",
];

/// The `getType()` of the calendar `@calendar=name` creates (`createStandardCalendar`):
/// unknown names give a Gregorian calendar, and `iso8601` is a Gregorian calendar.
pub(crate) fn calendar_type_for(name: &str) -> &'static str {
    match CALENDAR_NAMES
        .iter()
        .find(|calendar| calendar.eq_ignore_ascii_case(name))
    {
        Some(&"iso8601") | None => "gregorian",
        Some(calendar) => calendar,
    }
}

/// `SetICUTimeZone`: the value must name a time zone, and is replaced by its normalised
/// spelling.
fn set_icu_time_zone(parameter: &mut Value) -> Result<()> {
    let mut tz_str = parameter.to_varchar()?;
    icu_helpers::get_time_zone(&mut tz_str)?;
    *parameter = Value::varchar(tz_str);
    Ok(())
}

/// `SetICUCalendar`: the value must name a calendar (case-insensitively, then replaced by
/// ICU's spelling).
fn set_icu_calendar(parameter: &mut Value) -> Result<()> {
    let name = parameter.to_varchar()?;
    if name == calendar_type_for(&name) {
        return Ok(());
    }
    // Try to be friendlier: go through all the calendar names and look for a case
    // insensitive match. If we don't find one, make a suggestion.
    let mut candidates = Vec::new();
    for calendar in CALENDAR_NAMES {
        if calendar.eq_ignore_ascii_case(&name) {
            *parameter = Value::varchar(calendar);
            return Ok(());
        }
        candidates.push(calendar.to_owned());
    }
    let candidate_str = candidates_message(&top_n_jaro_winkler(&candidates, &name), "Candidate calendars");
    Err(Error::not_implemented(format!(
        "Unknown Calendar '{name}'!\n{candidate_str}"
    )))
}

/// The `TimeZone` and `Calendar` options (`config.AddExtensionOption` in `LoadInternal`).
/// The default time zone is the host's, if ICU knows it (else UTC).
pub(crate) fn extension_options() -> Vec<ExtensionOption> {
    let mut tz_string = host_time_zone_id();
    // If the environment TZ is invalid, look for some alternatives
    if icu_helpers::try_get_normalized_time_zone(&mut tz_string).is_none() {
        tz_string = "UTC".to_owned();
    }
    vec![
        ExtensionOption {
            name: "TimeZone".to_owned(),
            description: "The current time zone".to_owned(),
            input_type: LogicalType::VARCHAR,
            default_value: Value::varchar(tz_string),
            set_function: Some(Arc::new(set_icu_time_zone)),
        },
        ExtensionOption {
            name: "Calendar".to_owned(),
            description: "The current calendar".to_owned(),
            input_type: LogicalType::VARCHAR,
            default_value: Value::varchar("gregorian"),
            set_function: Some(Arc::new(set_icu_calendar)),
        },
    ]
}

/// `icu_calendar_names()` (`ICUCalendarFunction`).
pub(crate) fn icu_calendar_names() -> TableFunctionSet {
    let function = TableFunction::new("icu_calendar_names", vec![])
        .with_bind(|_| {
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::VARCHAR],
                names: vec!["name".to_owned()],
                bind_data: None,
            })
        })
        .with_scan(|_, _, state, output| {
            let names: &[&str] = if state.position == 0 { &CALENDAR_NAMES } else { &[] };
            state.position = 1;
            let mut vector = Vector::flat(LogicalType::VARCHAR, names.len());
            for (row, name) in names.iter().enumerate() {
                vector.set_value(row, &Value::varchar(*name))?;
            }
            output.set_columns(vec![vector], names.len());
            Ok(!names.is_empty())
        });
    let mut set = TableFunctionSet::new("icu_calendar_names");
    set.add(function);
    set
}
