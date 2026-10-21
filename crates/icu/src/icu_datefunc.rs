//! `ICUDateFunc` (DuckDB's `icu-datefunc.cpp` and `icu-datefunc.hpp`): the bind data
//! holding a calendar in the session's time zone and calendar, and the calendar helpers
//! every ICU date function uses. Also `TZCalendar` (`tz_calendar.hpp`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionClient, FunctionData, ScalarFunction};
use rustdb_types::{Interval, Timestamp};

use crate::icu4c::calendar::{Calendar, Status, field};
use crate::icu4c::calsys::{CalendarSystem, GregorianState};
use crate::icu4c::timezone::TimeZone;
use crate::icu_helpers;

/// `Interval::MICROS_PER_MSEC`.
pub(crate) const MICROS_PER_MSEC: i64 = 1000;

/// `CalendarPtr`: calendars are cloned per chunk like DuckDB's `calendar->clone()`.
pub(crate) type CalendarPtr = Box<Calendar>;

/// The current time in milliseconds (`Calendar::getNow`).
fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

/// The calendar systems by `@calendar=` keyword (`Calendar::createInstance` with
/// `createStandardCalendar`); unknown keywords give the Gregorian calendar. With
/// `proleptic`, Gregorian calendars get `ucal_setGregorianChange(U_DATE_MIN)` (which
/// only succeeds for plain `GregorianCalendar`s: `gregorian` and `iso8601`).
pub(crate) fn create_calendar_with_cutover(zone: TimeZone, cal_setting: &str, proleptic: bool) -> Calendar {
    let system = if proleptic {
        CalendarSystem::Gregorian(GregorianState::proleptic())
    } else {
        CalendarSystem::Gregorian(GregorianState::papal())
    };
    let mut calendar = Calendar::new(zone, system, now_millis());
    if cal_setting.eq_ignore_ascii_case("iso8601") {
        calendar.set_first_day_of_week(crate::icu4c::grego::MONDAY);
        calendar.set_minimal_days_in_first_week(4);
    }
    calendar
}

/// `BindData::InitCalendar`'s calendar: proleptic Gregorian where ICU allows it.
pub(crate) fn create_calendar(zone: TimeZone, cal_setting: &str) -> Calendar {
    create_calendar_with_cutover(zone, cal_setting, true)
}

/// `ICUDateFunc::BindData`: the session's time zone and calendar settings and a calendar
/// set up for them.
#[derive(Clone, Debug)]
pub(crate) struct BindData {
    pub tz_setting: String,
    pub cal_setting: String,
    pub calendar: CalendarPtr,
}

impl BindData {
    /// `BindData(const string &tz_setting, const string &cal_setting)`.
    pub fn new(tz_setting: &str, cal_setting: &str) -> Self {
        let calendar = Self::init_calendar(tz_setting, cal_setting);
        Self {
            tz_setting: tz_setting.to_owned(),
            cal_setting: cal_setting.to_owned(),
            calendar,
        }
    }

    /// `BindData(ClientContext &context)`: reads the `TimeZone` and `Calendar` settings.
    pub fn from_client(client: &FunctionClient) -> Self {
        let (tz_setting, cal_setting) = session_settings(client);
        Self::new(&tz_setting, &cal_setting)
    }

    /// `BindData::InitCalendar`.
    fn init_calendar(tz_setting: &str, cal_setting: &str) -> CalendarPtr {
        let zone = TimeZone::create(tz_setting);
        Box::new(create_calendar(zone, cal_setting))
    }

    /// `BindData::Equals`.
    pub fn equals(&self, other: &Self) -> bool {
        self.calendar.is_equivalent_to(&other.calendar)
    }
}

impl FunctionData for BindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| Self::equals(self, other))
    }
}

/// The session's `TimeZone` and `Calendar` settings (`TryGetCurrentSetting`): the time
/// zone is empty and the calendar `gregorian` if they are not set.
pub(crate) fn session_settings(client: &FunctionClient) -> (String, String) {
    let settings = client.settings();
    let get = |name: &str| {
        settings
            .as_ref()
            .and_then(|settings| settings.current_setting(name).ok())
            .filter(|value| !value.is_null())
            .and_then(|value| value.to_varchar().ok())
    };
    let tz_setting = get("TimeZone").unwrap_or_default();
    let cal_setting = get("Calendar").unwrap_or_else(|| "gregorian".to_owned());
    (tz_setting, cal_setting)
}

/// `ICUDateFunc::Bind`: binds a default calendar object for use by the function.
pub(crate) fn bind(_function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    Ok(Some(Arc::new(BindData::from_client(input.client))))
}

/// The bind data of a call bound by [`bind`] (or a bind data embedding it).
pub(crate) fn bind_data<T: 'static>(data: Option<&Arc<dyn FunctionData>>) -> Result<&T> {
    data.and_then(|data| data.as_any().downcast_ref::<T>())
        .ok_or_else(|| Error::internal("ICU function called without its bind data"))
}

/// `ICUDateFunc::TrySetTimeZone`: false if `tz_id` is not a time zone.
pub(crate) fn try_set_time_zone(calendar: &mut Calendar, tz_id: &str) -> bool {
    let mut tz_str = tz_id.to_owned();
    match icu_helpers::try_get_time_zone(&mut tz_str) {
        Some(tz) => {
            calendar.set_time_zone(tz);
            true
        }
        None => false,
    }
}

/// `ICUDateFunc::SetTimeZone`: throws if `tz_id` is not a time zone.
pub(crate) fn set_time_zone(calendar: &mut Calendar, tz_id: &str) -> Result<()> {
    let mut tz_str = tz_id.to_owned();
    let tz = icu_helpers::get_time_zone(&mut tz_str)?;
    calendar.set_time_zone(tz);
    Ok(())
}

/// `ICUDateFunc::SetTimeZone` with an error message: returns DuckDB's message instead of
/// throwing.
pub(crate) fn set_time_zone_message(calendar: &mut Calendar, tz_id: &str) -> Option<String> {
    let mut tz_str = tz_id.to_owned();
    match icu_helpers::get_time_zone_or_message(&mut tz_str) {
        Ok(tz) => {
            calendar.set_time_zone(tz);
            None
        }
        Err(message) => Some(message),
    }
}

/// `ICUDateFunc::GetTimeUnsafe`: the calendar's time plus `micros`, assumed in range.
pub(crate) fn get_time_unsafe(calendar: &mut Calendar, micros: u64) -> Result<Timestamp> {
    let mut status = Status::ok();
    let millis = calendar.get_time_in_millis(&mut status) as i64;
    if status.is_failure() {
        return Err(Error::internal("Unable to get ICU calendar time."));
    }
    Ok(Timestamp(
        millis.wrapping_mul(MICROS_PER_MSEC).wrapping_add(micros as i64),
    ))
}

/// `ICUDateFunc::TryGetTime`: the calendar's time plus `micros`, or `None` if it is out
/// of range.
pub(crate) fn try_get_time(calendar: &mut Calendar, micros: u64) -> Option<Timestamp> {
    let mut status = Status::ok();
    let millis = calendar.get_time_in_millis(&mut status) as i64;
    if status.is_failure() {
        return None;
    }
    // UDate is a double, so it can't overflow (it just loses accuracy), but converting
    // back to µs can.
    let micros = millis
        .checked_mul(MICROS_PER_MSEC)?
        .checked_add(micros as i64)?;
    // Now make sure the value is in range
    let result = Timestamp(micros);
    let date = result.to_date();
    i64::from(date.0).checked_mul(Interval::MICROS_PER_DAY)?;
    Some(result)
}

/// `ICUDateFunc::GetTime`: the calendar's time plus `micros`, or DuckDB's overflow error.
pub(crate) fn get_time(calendar: &mut Calendar, micros: u64) -> Result<Timestamp> {
    try_get_time(calendar, micros).ok_or_else(|| Error::conversion("ICU date overflows timestamp range"))
}

/// `ICUDateFunc::SetTime`: sets the calendar to `date` and returns the µs below the ms.
pub(crate) fn set_time(calendar: &mut Calendar, date: Timestamp) -> u64 {
    let mut millis = date.0 / MICROS_PER_MSEC;
    let mut micros = date.0 % MICROS_PER_MSEC;
    if micros < 0 {
        millis -= 1;
        micros += MICROS_PER_MSEC;
    }
    let mut status = Status::ok();
    calendar.set_time_in_millis(millis as f64, &mut status);
    micros as u64
}

/// `ICUDateFunc::ExtractField`.
pub(crate) fn extract_field(calendar: &mut Calendar, field: usize) -> Result<i32> {
    let mut status = Status::ok();
    let result = calendar.get(field, &mut status);
    if status.is_failure() {
        return Err(Error::internal("Unable to extract ICU calendar part."));
    }
    Ok(result)
}

/// `ICUDateFunc::SubtractField`.
pub(crate) fn subtract_field(calendar: &mut Calendar, field: usize, end_date: Timestamp) -> Result<i32> {
    let millis = end_date.0 / MICROS_PER_MSEC;
    let when = millis as f64;
    let mut status = Status::ok();
    let sub = calendar.field_difference(when, field, &mut status);
    if status.is_failure() {
        return Err(Error::internal("Unable to subtract ICU calendar part."));
    }
    Ok(sub)
}

/// The total UTC offset of the calendar's time in seconds (`ZONE_OFFSET + DST_OFFSET`).
pub(crate) fn extract_offset_seconds(calendar: &mut Calendar) -> Result<i32> {
    let offset = extract_field(calendar, field::ZONE_OFFSET)? + extract_field(calendar, field::DST_OFFSET)?;
    Ok(offset / 1000)
}

/// `TZCalendar`: a calendar with its Gregorian-ness and whether intervals apply to it.
#[derive(Debug)]
pub(crate) struct TzCalendar {
    pub calendar: CalendarPtr,
    pub is_gregorian: bool,
    pub supports_intervals: bool,
}

impl TzCalendar {
    /// `TZCalendar(icu::Calendar &calendar_p, const string &cal_setting)`.
    pub fn new(calendar: &Calendar, cal_setting: &str) -> Self {
        let calendar = Box::new(calendar.clone());
        Self {
            is_gregorian: cal_setting.is_empty() || cal_setting.eq_ignore_ascii_case("gregorian"),
            supports_intervals: calendar.get_maximum(field::MONTH) < 12, // 0-based
            calendar,
        }
    }
}
