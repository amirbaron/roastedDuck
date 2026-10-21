//! Port of ICU's `Calendar` (`i18n/calendar.cpp`): the lenient field/time machinery
//! shared by every calendar system. A calendar holds an instant (`fTime`) and the fields
//! computed from it in its time zone; setting fields stamps them, and the newest
//! consistent combination of fields is resolved back to an instant (`computeTime`).
//!
//! The calendar systems' hooks (`handleComputeFields`, `handleComputeMonthStart`, ...)
//! live in [`super::calsys`].

use super::calsys::{CalendarSystem, GREGORIAN_LIMITS};
use super::grego::{
    self, EPOCH_START_AS_JULIAN_DAY, MAX_MILLIS, MIN_MILLIS, ONE_DAY, ONE_HOUR, ONE_MINUTE, ONE_SECOND,
    ONE_WEEK,
};
use super::timezone::TimeZone;

/// `UCalendarDateFields`.
pub mod field {
    pub const ERA: usize = 0;
    pub const YEAR: usize = 1;
    pub const MONTH: usize = 2;
    pub const WEEK_OF_YEAR: usize = 3;
    pub const WEEK_OF_MONTH: usize = 4;
    pub const DATE: usize = 5;
    pub const DAY_OF_MONTH: usize = 5;
    pub const DAY_OF_YEAR: usize = 6;
    pub const DAY_OF_WEEK: usize = 7;
    pub const DAY_OF_WEEK_IN_MONTH: usize = 8;
    pub const AM_PM: usize = 9;
    pub const HOUR: usize = 10;
    pub const HOUR_OF_DAY: usize = 11;
    pub const MINUTE: usize = 12;
    pub const SECOND: usize = 13;
    pub const MILLISECOND: usize = 14;
    pub const ZONE_OFFSET: usize = 15;
    pub const DST_OFFSET: usize = 16;
    pub const YEAR_WOY: usize = 17;
    pub const DOW_LOCAL: usize = 18;
    pub const EXTENDED_YEAR: usize = 19;
    pub const JULIAN_DAY: usize = 20;
    pub const MILLISECONDS_IN_DAY: usize = 21;
    pub const IS_LEAP_MONTH: usize = 22;
    /// `UCAL_FIELD_COUNT`.
    pub const COUNT: usize = 23;
}

use field as F;

/// `ELimitType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Minimum = 0,
    GreatestMinimum = 1,
    LeastMaximum = 2,
    Maximum = 3,
}

/// ICU's `UErrorCode`, reduced to success or failure (the calendar only reports
/// `U_ILLEGAL_ARGUMENT_ERROR`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    failed: bool,
}

impl Status {
    /// `U_ZERO_ERROR`.
    pub const fn ok() -> Self {
        Self { failed: false }
    }

    /// `U_FAILURE`.
    pub const fn is_failure(self) -> bool {
        self.failed
    }

    /// `U_SUCCESS`.
    pub const fn is_success(self) -> bool {
        !self.failed
    }

    /// Records `U_ILLEGAL_ARGUMENT_ERROR`.
    pub fn fail(&mut self) {
        self.failed = true;
    }
}

/// `kUnset`.
const UNSET: i32 = 0;
/// `kInternallySet`.
const INTERNALLY_SET: i32 = 1;
/// `kMinimumUserStamp`.
const MINIMUM_USER_STAMP: i32 = 2;
/// `STAMP_MAX`.
const STAMP_MAX: i32 = 10000;

/// `kResolveSTOP`.
const RESOLVE_STOP: i32 = -1;
/// `kResolveRemap`.
const RESOLVE_REMAP: i32 = 32;

/// A field resolution table (`UFieldResolutionTable`): groups of lines of fields.
type ResolutionTable = &'static [&'static [&'static [i32]]];

/// `Calendar::kDatePrecedence`.
const DATE_PRECEDENCE: ResolutionTable = &[
    &[
        &[F::DAY_OF_MONTH as i32, RESOLVE_STOP],
        &[F::WEEK_OF_YEAR as i32, F::DAY_OF_WEEK as i32, RESOLVE_STOP],
        &[F::WEEK_OF_MONTH as i32, F::DAY_OF_WEEK as i32, RESOLVE_STOP],
        &[
            F::DAY_OF_WEEK_IN_MONTH as i32,
            F::DAY_OF_WEEK as i32,
            RESOLVE_STOP,
        ],
        &[F::WEEK_OF_YEAR as i32, F::DOW_LOCAL as i32, RESOLVE_STOP],
        &[F::WEEK_OF_MONTH as i32, F::DOW_LOCAL as i32, RESOLVE_STOP],
        &[F::DAY_OF_WEEK_IN_MONTH as i32, F::DOW_LOCAL as i32, RESOLVE_STOP],
        &[F::DAY_OF_YEAR as i32, RESOLVE_STOP],
        // if YEAR is set over YEAR_WOY use DAY_OF_MONTH
        &[
            RESOLVE_REMAP | F::DAY_OF_MONTH as i32,
            F::YEAR as i32,
            RESOLVE_STOP,
        ],
        // if YEAR_WOY is set,  calc based on WEEK_OF_YEAR
        &[
            RESOLVE_REMAP | F::WEEK_OF_YEAR as i32,
            F::YEAR_WOY as i32,
            RESOLVE_STOP,
        ],
    ],
    &[
        &[F::WEEK_OF_YEAR as i32, RESOLVE_STOP],
        &[F::WEEK_OF_MONTH as i32, RESOLVE_STOP],
        &[F::DAY_OF_WEEK_IN_MONTH as i32, RESOLVE_STOP],
        &[
            RESOLVE_REMAP | F::DAY_OF_WEEK_IN_MONTH as i32,
            F::DAY_OF_WEEK as i32,
            RESOLVE_STOP,
        ],
        &[
            RESOLVE_REMAP | F::DAY_OF_WEEK_IN_MONTH as i32,
            F::DOW_LOCAL as i32,
            RESOLVE_STOP,
        ],
    ],
];

/// `Calendar::kDOWPrecedence`.
const DOW_PRECEDENCE: ResolutionTable = &[&[
    &[F::DAY_OF_WEEK as i32, RESOLVE_STOP, RESOLVE_STOP],
    &[F::DOW_LOCAL as i32, RESOLVE_STOP, RESOLVE_STOP],
]];

/// `Calendar::kYearPrecedence`.
const YEAR_PRECEDENCE: ResolutionTable = &[&[
    &[F::YEAR as i32, RESOLVE_STOP],
    &[F::EXTENDED_YEAR as i32, RESOLVE_STOP],
    // YEAR_WOY is useless without WEEK_OF_YEAR
    &[F::YEAR_WOY as i32, F::WEEK_OF_YEAR as i32, RESOLVE_STOP],
]];

/// `kCalendarLimits`: the limits shared by every calendar.
const CALENDAR_LIMITS: [[i32; 4]; F::COUNT] = [
    [-1, -1, -1, -1],                                               // ERA
    [-1, -1, -1, -1],                                               // YEAR
    [-1, -1, -1, -1],                                               // MONTH
    [-1, -1, -1, -1],                                               // WEEK_OF_YEAR
    [-1, -1, -1, -1],                                               // WEEK_OF_MONTH
    [-1, -1, -1, -1],                                               // DAY_OF_MONTH
    [-1, -1, -1, -1],                                               // DAY_OF_YEAR
    [1, 1, 7, 7],                                                   // DAY_OF_WEEK
    [-1, -1, -1, -1],                                               // DAY_OF_WEEK_IN_MONTH
    [0, 0, 1, 1],                                                   // AM_PM
    [0, 0, 11, 11],                                                 // HOUR
    [0, 0, 23, 23],                                                 // HOUR_OF_DAY
    [0, 0, 59, 59],                                                 // MINUTE
    [0, 0, 59, 59],                                                 // SECOND
    [0, 0, 999, 999],                                               // MILLISECOND
    [-12 * ONE_HOUR, -12 * ONE_HOUR, 12 * ONE_HOUR, 15 * ONE_HOUR], // ZONE_OFFSET
    [0, 0, ONE_HOUR, ONE_HOUR],                                     // DST_OFFSET
    [-1, -1, -1, -1],                                               // YEAR_WOY
    [1, 1, 7, 7],                                                   // DOW_LOCAL
    [-1, -1, -1, -1],                                               // EXTENDED_YEAR
    [-0x7F00_0000, -0x7F00_0000, 0x7F00_0000, 0x7F00_0000],         // JULIAN_DAY
    [0, 0, 24 * ONE_HOUR - 1, 24 * ONE_HOUR - 1],                   // MILLISECONDS_IN_DAY
    [0, 0, 1, 1],                                                   // IS_LEAP_MONTH
];

/// `UCalendarWallTimeOption`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WallTimeOption {
    Last,
    #[allow(dead_code)] // DuckDB never calls `setSkippedWallTimeOption`
    First,
}

/// A calendar (ICU's `Calendar` with its subclass state in [`CalendarSystem`]).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // ICU's `Calendar` flags
pub struct Calendar {
    pub(super) fields: [i32; F::COUNT],
    pub(super) stamp: [i32; F::COUNT],
    time: f64,
    is_time_set: bool,
    are_fields_set: bool,
    are_all_fields_set: bool,
    are_fields_virtually_set: bool,
    next_stamp: i32,
    lenient: bool,
    first_day_of_week: i32,
    minimal_days_in_first_week: i32,
    repeated_wall_time: WallTimeOption,
    skipped_wall_time: WallTimeOption,
    zone: TimeZone,
    /// `fGregorianYear` and friends: the Gregorian fields of the current Julian day.
    pub(super) gregorian_year: i32,
    pub(super) gregorian_month: i32,
    pub(super) gregorian_day_of_month: i32,
    pub(super) gregorian_day_of_year: i32,
    /// The calendar system and its state.
    pub(super) system: CalendarSystem,
}

impl Calendar {
    /// `Calendar::Calendar(TimeZone*, const Locale&, UErrorCode&)` followed by the
    /// subclass constructor's `setTimeInMillis(getNow())`. The week data are those of the
    /// root locale's likely region (US: weeks start on Sunday, one minimal day).
    pub fn new(zone: TimeZone, system: CalendarSystem, now: f64) -> Self {
        let mut calendar = Self {
            fields: [0; F::COUNT],
            stamp: [UNSET; F::COUNT],
            time: 0.0,
            is_time_set: false,
            are_fields_set: false,
            are_all_fields_set: false,
            are_fields_virtually_set: false,
            next_stamp: MINIMUM_USER_STAMP,
            lenient: true,
            first_day_of_week: grego::SUNDAY,
            minimal_days_in_first_week: 1,
            repeated_wall_time: WallTimeOption::Last,
            skipped_wall_time: WallTimeOption::Last,
            zone,
            gregorian_year: 0,
            gregorian_month: 0,
            gregorian_day_of_month: 0,
            gregorian_day_of_year: 0,
            system,
        };
        calendar.clear();
        let mut status = Status::ok();
        calendar.set_time_in_millis(now, &mut status);
        calendar
    }

    // ------------------------------------------------------------------ time

    /// `Calendar::getTimeInMillis` (`getTime`).
    pub fn get_time_in_millis(&mut self, status: &mut Status) -> f64 {
        if status.is_failure() {
            return 0.0;
        }
        if !self.is_time_set {
            self.update_time(status);
        }
        if status.is_failure() {
            return 0.0;
        }
        self.time
    }

    /// `Calendar::setTimeInMillis` (`setTime`): lenient calendars pin out-of-range
    /// instants to the supported range.
    pub fn set_time_in_millis(&mut self, millis: f64, status: &mut Status) {
        if status.is_failure() {
            return;
        }
        let mut millis = millis;
        if millis > MAX_MILLIS {
            if self.lenient {
                millis = MAX_MILLIS;
            } else {
                status.fail();
                return;
            }
        } else if millis < MIN_MILLIS {
            if self.lenient {
                millis = MIN_MILLIS;
            } else {
                status.fail();
                return;
            }
        }
        self.time = millis;
        self.are_fields_set = false;
        self.are_all_fields_set = false;
        self.is_time_set = true;
        self.are_fields_virtually_set = true;
        self.fields = [0; F::COUNT];
        self.stamp = [UNSET; F::COUNT];
    }

    /// `Calendar::internalGetTime`.
    pub(super) fn internal_get_time(&self) -> f64 {
        self.time
    }

    // ------------------------------------------------------------------ fields

    /// `Calendar::get`.
    pub fn get(&mut self, field: usize, status: &mut Status) -> i32 {
        if status.is_success() {
            self.complete(status);
        }
        if status.is_success() {
            self.fields[field]
        } else {
            0
        }
    }

    /// `Calendar::set`.
    pub fn set(&mut self, field: usize, value: i32) {
        if self.are_fields_virtually_set {
            let mut status = Status::ok();
            self.compute_fields(&mut status);
        }
        self.fields[field] = value;
        // Ensure that the fNextStamp value doesn't go pass max value for int32_t
        if self.next_stamp == STAMP_MAX {
            self.recalculate_stamp();
        }
        self.stamp[field] = self.next_stamp;
        self.next_stamp += 1;
        self.is_time_set = false;
        self.are_fields_set = false;
        self.are_fields_virtually_set = false;
    }

    /// `Calendar::clear()`.
    pub fn clear(&mut self) {
        self.fields = [0; F::COUNT];
        self.stamp = [UNSET; F::COUNT];
        self.is_time_set = false;
        self.are_fields_set = false;
        self.are_all_fields_set = false;
        self.are_fields_virtually_set = false;
    }

    /// `Calendar::clear(UCalendarDateFields)`.
    pub fn clear_field(&mut self, field: usize) {
        if self.are_fields_virtually_set {
            let mut status = Status::ok();
            self.compute_fields(&mut status);
        }
        self.fields[field] = 0;
        self.stamp[field] = UNSET;
        self.is_time_set = false;
        self.are_fields_set = false;
        self.are_all_fields_set = false;
        self.are_fields_virtually_set = false;
    }

    /// `Calendar::isSet`.
    pub(super) fn is_set(&self, field: usize) -> bool {
        self.are_fields_virtually_set || self.stamp[field] != UNSET
    }

    /// `Calendar::internalGet(field)`.
    pub(super) fn internal_get(&self, field: usize) -> i32 {
        self.fields[field]
    }

    /// `Calendar::internalGet(field, defaultValue)`.
    pub(super) fn internal_get_or(&self, field: usize, default: i32) -> i32 {
        if self.stamp[field] > UNSET {
            self.fields[field]
        } else {
            default
        }
    }

    /// `Calendar::internalSet`.
    pub(super) fn internal_set(&mut self, field: usize, value: i32) {
        self.fields[field] = value;
        self.stamp[field] = INTERNALLY_SET;
    }

    /// `Calendar::newestStamp`.
    fn newest_stamp(&self, first: usize, last: usize, best_stamp_so_far: i32) -> i32 {
        let mut best = best_stamp_so_far;
        for &stamp in &self.stamp[first..=last] {
            if stamp > best {
                best = stamp;
            }
        }
        best
    }

    /// `Calendar::recalculateStamp`.
    fn recalculate_stamp(&mut self) {
        self.next_stamp = 1;
        for _ in 0..F::COUNT {
            let mut current = STAMP_MAX;
            let mut index = None;
            for (i, &stamp) in self.stamp.iter().enumerate() {
                if stamp > self.next_stamp && stamp < current {
                    current = stamp;
                    index = Some(i);
                }
            }
            match index {
                Some(index) => {
                    self.next_stamp += 1;
                    self.stamp[index] = self.next_stamp;
                }
                None => break,
            }
        }
        self.next_stamp += 1;
    }

    /// `Calendar::complete`.
    fn complete(&mut self, status: &mut Status) {
        if !self.is_time_set {
            self.update_time(status);
            if status.is_failure() {
                return;
            }
        }
        if !self.are_fields_set {
            self.compute_fields(status);
            if status.is_failure() {
                return;
            }
            self.are_fields_set = true;
            self.are_all_fields_set = true;
        }
    }

    /// `Calendar::updateTime`.
    fn update_time(&mut self, status: &mut Status) {
        self.compute_time(status);
        if status.is_failure() {
            return;
        }
        // If we are lenient, we need to recompute the fields to normalize the values.
        if self.lenient || !self.are_all_fields_set {
            self.are_fields_set = false;
        }
        self.is_time_set = true;
        self.are_fields_virtually_set = false;
    }

    // ------------------------------------------------------------------ settings

    /// `Calendar::setLenient`.
    pub fn set_lenient(&mut self, lenient: bool) {
        self.lenient = lenient;
    }

    /// `Calendar::setFirstDayOfWeek`.
    pub fn set_first_day_of_week(&mut self, value: i32) {
        if self.first_day_of_week != value && (grego::SUNDAY..=grego::SATURDAY).contains(&value) {
            self.first_day_of_week = value;
            self.are_fields_set = false;
        }
    }

    /// `Calendar::setMinimalDaysInFirstWeek`.
    pub fn set_minimal_days_in_first_week(&mut self, value: i32) {
        let value = value.clamp(1, 7);
        if self.minimal_days_in_first_week != value {
            self.minimal_days_in_first_week = value;
            self.are_fields_set = false;
        }
    }

    /// `Calendar::adoptTimeZone` / `setTimeZone`.
    pub fn set_time_zone(&mut self, zone: TimeZone) {
        self.zone = zone;
        // if the zone changes, we need to recompute the time fields
        self.are_fields_set = false;
    }

    /// `Calendar::getTimeZone`.
    pub fn time_zone(&self) -> &TimeZone {
        &self.zone
    }

    /// `Calendar::getType`.
    pub fn calendar_type(&self) -> &'static str {
        self.system.calendar_type()
    }

    /// `Calendar::isEquivalentTo` (and `GregorianCalendar`'s cutover check).
    pub fn is_equivalent_to(&self, other: &Self) -> bool {
        self.calendar_type() == other.calendar_type()
            && self.lenient == other.lenient
            && self.repeated_wall_time == other.repeated_wall_time
            && self.skipped_wall_time == other.skipped_wall_time
            && self.first_day_of_week == other.first_day_of_week
            && self.minimal_days_in_first_week == other.minimal_days_in_first_week
            && self.zone == other.zone
            && self.system.is_equivalent_to(&other.system)
    }

    // ------------------------------------------------------------------ fields <- time

    /// `Calendar::computeFields`.
    pub(super) fn compute_fields(&mut self, status: &mut Status) {
        if status.is_failure() {
            return;
        }
        // Compute local wall millis
        let mut local_millis = self.internal_get_time();
        let (raw_offset, dst_offset) = self.zone.get_offset(local_millis);
        local_millis += f64::from(raw_offset + dst_offset);

        // Mark fields as set.  Do this before calling handleComputeFields().
        let mut mask: u32 = (1 << F::ERA)
            | (1 << F::YEAR)
            | (1 << F::MONTH)
            | (1 << F::DAY_OF_MONTH)
            | (1 << F::DAY_OF_YEAR)
            | (1 << F::EXTENDED_YEAR);
        for stamp in &mut self.stamp {
            *stamp = if mask & 1 == 0 { INTERNALLY_SET } else { UNSET };
            mask >>= 1;
        }

        let days = grego::floor_divide_f64(local_millis, ONE_DAY) as i32;
        self.internal_set(F::JULIAN_DAY, days.wrapping_add(EPOCH_START_AS_JULIAN_DAY));

        let julian_day = self.fields[F::JULIAN_DAY];
        self.compute_gregorian_and_dow_fields(julian_day);

        // Call framework method to have subclass compute its fields.
        self.handle_compute_fields(julian_day, status);

        // Compute week-related fields, based on the subclass-computed fields.
        self.compute_week_fields(status);

        // Compute time-related fields.
        let mut millis_in_day = (local_millis - f64::from(days) * ONE_DAY) as i32;
        self.fields[F::MILLISECONDS_IN_DAY] = millis_in_day;
        self.fields[F::MILLISECOND] = millis_in_day % 1000;
        millis_in_day /= 1000;
        self.fields[F::SECOND] = millis_in_day % 60;
        millis_in_day /= 60;
        self.fields[F::MINUTE] = millis_in_day % 60;
        millis_in_day /= 60;
        self.fields[F::HOUR_OF_DAY] = millis_in_day;
        self.fields[F::AM_PM] = millis_in_day / 12; // Assume AM == 0
        self.fields[F::HOUR] = millis_in_day % 12;
        self.fields[F::ZONE_OFFSET] = raw_offset;
        self.fields[F::DST_OFFSET] = dst_offset;
    }

    /// `Calendar::julianDayToDayOfWeek`.
    pub(super) fn julian_day_to_day_of_week(julian: f64) -> i32 {
        // If julian is negative, then julian%7 will be negative, so we adjust
        // accordingly.  We add 1 because Julian day 0 is Monday.
        let day_of_week = ((julian + 1.0) % 7.0) as i32;
        day_of_week
            + if day_of_week < 0 {
                7 + grego::SUNDAY
            } else {
                grego::SUNDAY
            }
    }

    /// `Calendar::computeGregorianAndDOWFields`.
    fn compute_gregorian_and_dow_fields(&mut self, julian_day: i32) {
        self.compute_gregorian_fields(julian_day);

        // Compute day of week: JD 0 = Monday
        let dow = Self::julian_day_to_day_of_week(f64::from(julian_day));
        self.internal_set(F::DAY_OF_WEEK, dow);

        // Calculate 1-based localized day of week
        let mut dow_local = dow - self.first_day_of_week + 1;
        if dow_local < 1 {
            dow_local += 7;
        }
        self.internal_set(F::DOW_LOCAL, dow_local);
    }

    /// `Calendar::computeGregorianFields`.
    pub(super) fn compute_gregorian_fields(&mut self, julian_day: i32) {
        let fields = grego::day_to_fields(f64::from(julian_day) - f64::from(EPOCH_START_AS_JULIAN_DAY));
        self.gregorian_year = fields.year;
        self.gregorian_month = fields.month;
        self.gregorian_day_of_month = fields.dom;
        self.gregorian_day_of_year = fields.doy;
    }

    /// `Calendar::computeWeekFields`.
    fn compute_week_fields(&mut self, status: &mut Status) {
        if status.is_failure() {
            return;
        }
        let eyear = self.fields[F::EXTENDED_YEAR];
        let day_of_week = self.fields[F::DAY_OF_WEEK];
        let day_of_year = self.fields[F::DAY_OF_YEAR];

        let mut year_of_week_of_year = eyear;
        let rel_dow = (day_of_week + 7 - self.first_day_of_week) % 7; // 0..6
        let rel_dow_jan1 = (day_of_week - day_of_year + 7001 - self.first_day_of_week) % 7; // 0..6
        let mut woy = (day_of_year - 1 + rel_dow_jan1) / 7; // 0..53
        if (7 - rel_dow_jan1) >= self.minimal_days_in_first_week {
            woy += 1;
        }

        // Adjust for weeks at the year end that overlap into the previous or next
        // calendar year.
        if woy == 0 {
            // We are the last week of the previous year.
            let prev_doy = day_of_year + self.handle_get_year_length(eyear - 1);
            woy = self.week_number(prev_doy, day_of_week);
            year_of_week_of_year -= 1;
        } else {
            let last_doy = self.handle_get_year_length(eyear);
            // Fast check: For it to be week 1 of the next year, the DOY must be on or
            // after L-5, where L is yearLength().
            if day_of_year >= (last_doy - 5) {
                let mut last_rel_dow = (rel_dow + last_doy - day_of_year) % 7;
                if last_rel_dow < 0 {
                    last_rel_dow += 7;
                }
                if (6 - last_rel_dow) >= self.minimal_days_in_first_week
                    && (day_of_year + 7 - rel_dow) > last_doy
                {
                    woy = 1;
                    year_of_week_of_year += 1;
                }
            }
        }
        self.fields[F::WEEK_OF_YEAR] = woy;
        self.fields[F::YEAR_WOY] = year_of_week_of_year;

        let day_of_month = self.fields[F::DAY_OF_MONTH];
        self.fields[F::WEEK_OF_MONTH] = self.week_number(day_of_month, day_of_week);
        self.fields[F::DAY_OF_WEEK_IN_MONTH] = (day_of_month - 1) / 7 + 1;
    }

    /// `Calendar::weekNumber(desiredDay, dayOfPeriod, dayOfWeek)`.
    fn week_number_of(&self, desired_day: i32, day_of_period: i32, day_of_week: i32) -> i32 {
        // Determine the day of the week of the first day of the period in question (either
        // a year or a month).  Zero represents the first day of the week on this calendar.
        let mut period_start_day_of_week = (day_of_week - self.first_day_of_week - day_of_period + 1) % 7;
        if period_start_day_of_week < 0 {
            period_start_day_of_week += 7;
        }

        // Compute the week number.  Initially, ignore the first week, which may be
        // fractional (or may not be).  We add periodStartDayOfWeek in order to fill out
        // the first week, if it is fractional.
        let mut week_no = (desired_day + period_start_day_of_week - 1) / 7;

        // If the first week is long enough, then count it.
        if (7 - period_start_day_of_week) >= self.minimal_days_in_first_week {
            week_no += 1;
        }
        week_no
    }

    /// `Calendar::weekNumber(dayOfPeriod, dayOfWeek)`.
    fn week_number(&self, day_of_period: i32, day_of_week: i32) -> i32 {
        self.week_number_of(day_of_period, day_of_period, day_of_week)
    }

    // ------------------------------------------------------------------ add

    /// `Calendar::add`.
    pub fn add(&mut self, field: usize, amount: i32, status: &mut Status) {
        if self.subclass_add(field, amount, status) {
            return;
        }
        if amount == 0 {
            return; // Do nothing!
        }
        let mut amount = amount;
        let mut delta = f64::from(amount); // delta in ms
        let mut keep_wall_time_invariant = true;

        match field {
            F::ERA => {
                let value = self.get(field, status) + amount;
                self.set(field, value);
                self.pin_field(F::ERA, status);
                return;
            }
            F::YEAR | F::YEAR_WOY | F::EXTENDED_YEAR | F::MONTH => {
                if field == F::YEAR || field == F::YEAR_WOY {
                    // If era=0 and years go backwards in time, change sign of amount.
                    let era = self.get(F::ERA, status);
                    if era == 0 && matches!(self.calendar_type(), "gregorian" | "roc" | "coptic") {
                        amount = -amount;
                    }
                }
                let old_lenient = self.lenient;
                self.set_lenient(true);
                let value = self.get(field, status).wrapping_add(amount);
                self.set(field, value);
                self.pin_field(F::DAY_OF_MONTH, status);
                if !old_lenient {
                    self.complete(status); // force recalculate
                    self.set_lenient(old_lenient);
                }
                return;
            }
            F::WEEK_OF_YEAR | F::WEEK_OF_MONTH | F::DAY_OF_WEEK_IN_MONTH => {
                delta *= ONE_WEEK;
            }
            F::AM_PM => {
                delta *= f64::from(12 * ONE_HOUR);
            }
            F::DAY_OF_MONTH | F::DAY_OF_YEAR | F::DAY_OF_WEEK | F::DOW_LOCAL | F::JULIAN_DAY => {
                delta *= ONE_DAY;
            }
            F::HOUR_OF_DAY | F::HOUR => {
                delta *= f64::from(ONE_HOUR);
                keep_wall_time_invariant = false;
            }
            F::MINUTE => {
                delta *= f64::from(ONE_MINUTE);
                keep_wall_time_invariant = false;
            }
            F::SECOND => {
                delta *= f64::from(ONE_SECOND);
                keep_wall_time_invariant = false;
            }
            F::MILLISECOND | F::MILLISECONDS_IN_DAY => {
                keep_wall_time_invariant = false;
            }
            _ => {
                status.fail();
                return;
            }
        }

        // In order to keep the wall time invariant (for fields where this is
        // appropriate), check the combined DST & ZONE offset before and after the add()
        // operation. If it changes, then adjust the millis to compensate.
        let mut prev_offset = 0;
        let mut prev_wall_time = 0;
        if keep_wall_time_invariant {
            prev_offset = self.get(F::DST_OFFSET, status) + self.get(F::ZONE_OFFSET, status);
            prev_wall_time = self.get(F::MILLISECONDS_IN_DAY, status);
        }

        let millis = self.get_time_in_millis(status) + delta;
        self.set_time_in_millis(millis, status);

        if keep_wall_time_invariant {
            let mut new_wall_time = self.get(F::MILLISECONDS_IN_DAY, status);
            if new_wall_time != prev_wall_time {
                // There is at least one zone transition between the base time and the
                // result time. As the result, wall time has changed.
                let t = self.internal_get_time();
                let new_offset = self.get(F::DST_OFFSET, status) + self.get(F::ZONE_OFFSET, status);
                if new_offset != prev_offset {
                    // When the difference of the previous UTC offset and the new UTC
                    // offset exceeds 1 full day, we do not want to roll over/back the date.
                    let mut adj_amount = prev_offset - new_offset;
                    adj_amount = if adj_amount >= 0 {
                        adj_amount % grego::MILLIS_PER_DAY
                    } else {
                        -(-adj_amount % grego::MILLIS_PER_DAY)
                    };
                    if adj_amount != 0 {
                        self.set_time_in_millis(t + f64::from(adj_amount), status);
                        new_wall_time = self.get(F::MILLISECONDS_IN_DAY, status);
                    }
                    if new_wall_time != prev_wall_time {
                        // The result wall time or adjusted wall time was shifted because
                        // the target wall time does not exist on the result date.
                        match self.skipped_wall_time {
                            WallTimeOption::First => {
                                if adj_amount > 0 {
                                    self.set_time_in_millis(t, status);
                                }
                            }
                            WallTimeOption::Last => {
                                if adj_amount < 0 {
                                    self.set_time_in_millis(t, status);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// `Calendar::pinField`.
    pub(super) fn pin_field(&mut self, field: usize, status: &mut Status) {
        let max = self.get_actual_maximum(field, status);
        let min = self.get_actual_minimum(field, status);
        if self.fields[field] > max {
            self.set(field, max);
        } else if self.fields[field] < min {
            self.set(field, min);
        }
    }

    /// `Calendar::fieldDifference`.
    #[allow(clippy::float_cmp)] // ICU compares the millis exactly
    pub fn field_difference(&mut self, target_ms: f64, field: usize, status: &mut Status) -> i32 {
        if status.is_failure() {
            return 0;
        }
        let mut min: i32 = 0;
        let start_ms = self.get_time_in_millis(status);
        // Always add from the start millis.  This accomodates operations like adding years
        // from February 29, 2000 up to February 29, 2004.
        if start_ms < target_ms {
            let mut max: i32 = 1;
            // Find a value that is too large
            while status.is_success() {
                self.set_time_in_millis(start_ms, status);
                self.add(field, max, status);
                let ms = self.get_time_in_millis(status);
                if ms == target_ms {
                    return max;
                } else if ms > target_ms {
                    break;
                } else if max < i32::MAX {
                    min = max;
                    max = max.wrapping_shl(1);
                    if max < 0 {
                        max = i32::MAX;
                    }
                } else {
                    // Field difference too large to fit into int32_t
                    status.fail();
                }
            }
            // Do a binary search
            while (max - min) > 1 && status.is_success() {
                let t = min + (max - min) / 2; // make sure intermediate values don't exceed INT32_MAX
                self.set_time_in_millis(start_ms, status);
                self.add(field, t, status);
                let ms = self.get_time_in_millis(status);
                if ms == target_ms {
                    return t;
                } else if ms > target_ms {
                    max = t;
                } else {
                    min = t;
                }
            }
        } else if start_ms > target_ms {
            let mut max: i32 = -1;
            // Find a value that is too small
            while status.is_success() {
                self.set_time_in_millis(start_ms, status);
                self.add(field, max, status);
                let ms = self.get_time_in_millis(status);
                if ms == target_ms {
                    return max;
                } else if ms < target_ms {
                    break;
                }
                min = max;
                max = ((max as u32) << 1) as i32;
                if max == 0 {
                    // Field difference too large to fit into int32_t
                    status.fail();
                }
            }
            // Do a binary search
            while (min - max) > 1 && status.is_success() {
                let t = min + (max - min) / 2; // make sure intermediate values don't exceed INT32_MAX
                self.set_time_in_millis(start_ms, status);
                self.add(field, t, status);
                let ms = self.get_time_in_millis(status);
                if ms == target_ms {
                    return t;
                } else if ms < target_ms {
                    max = t;
                } else {
                    min = t;
                }
            }
        }
        // Set calendar to end point
        self.set_time_in_millis(start_ms, status);
        self.add(field, min, status);

        if status.is_failure() {
            return 0;
        }
        min
    }

    // ------------------------------------------------------------------ limits

    /// `Calendar::getLimit`.
    pub(super) fn get_limit(&self, field: usize, limit: Limit) -> i32 {
        match field {
            F::DAY_OF_WEEK
            | F::AM_PM
            | F::HOUR
            | F::HOUR_OF_DAY
            | F::MINUTE
            | F::SECOND
            | F::MILLISECOND
            | F::ZONE_OFFSET
            | F::DST_OFFSET
            | F::DOW_LOCAL
            | F::JULIAN_DAY
            | F::MILLISECONDS_IN_DAY
            | F::IS_LEAP_MONTH => CALENDAR_LIMITS[field][limit as usize],
            F::WEEK_OF_MONTH => match limit {
                Limit::Minimum => i32::from(self.minimal_days_in_first_week == 1),
                Limit::GreatestMinimum => 1,
                _ => {
                    let min_days_in_first = self.minimal_days_in_first_week;
                    let days_in_month = self.system.handle_get_limit(F::DAY_OF_MONTH, limit);
                    if limit == Limit::LeastMaximum {
                        (days_in_month + (7 - min_days_in_first)) / 7
                    } else {
                        (days_in_month + 6 + (7 - min_days_in_first)) / 7
                    }
                }
            },
            _ => self.system.handle_get_limit(field, limit),
        }
    }

    /// `Calendar::getMinimum`.
    pub fn get_minimum(&self, field: usize) -> i32 {
        self.get_limit(field, Limit::Minimum)
    }

    /// `Calendar::getMaximum`.
    pub fn get_maximum(&self, field: usize) -> i32 {
        self.get_limit(field, Limit::Maximum)
    }

    /// `Calendar::getGreatestMinimum`.
    fn get_greatest_minimum(&self, field: usize) -> i32 {
        self.get_limit(field, Limit::GreatestMinimum)
    }

    /// `Calendar::getLeastMaximum`.
    pub(super) fn get_least_maximum(&self, field: usize) -> i32 {
        self.get_limit(field, Limit::LeastMaximum)
    }

    /// `Calendar::getActualMinimum` (`GregorianCalendar::getActualMinimum` returns
    /// `getMinimum`).
    fn get_actual_minimum(&mut self, field: usize, status: &mut Status) -> i32 {
        if self.system.actual_minimum_is_minimum() {
            return self.get_minimum(field);
        }
        let mut field_value = self.get_greatest_minimum(field);
        let end_value = self.get_minimum(field);

        // if we know that the minimum value is always the same, just return it
        if field_value == end_value {
            return field_value;
        }

        // clone the calendar so we don't mess with the real one, and set it to accept
        // anything for the field values
        let mut work = self.clone();
        work.set_lenient(true);

        // now try each value from getLeastMaximum() to getMaximum() one by one until we
        // get a value that normalizes to another value.
        let mut result = field_value;
        loop {
            work.set(field, field_value);
            if work.get(field, status) != field_value {
                break;
            }
            result = field_value;
            field_value -= 1;
            if field_value < end_value {
                break;
            }
        }
        if status.is_failure() {
            return 0;
        }
        result
    }

    /// `Calendar::getActualMaximum` (with `GregorianCalendar`'s special case for YEAR).
    pub fn get_actual_maximum(&mut self, field: usize, status: &mut Status) -> i32 {
        if field == F::YEAR && matches!(self.system, CalendarSystem::Japanese(_)) {
            return self.japanese_actual_maximum_year(status);
        }
        if field == F::YEAR && self.system.is_gregorian_family() {
            return self.gregorian_actual_maximum_year(status);
        }
        match field {
            F::DATE => {
                if status.is_failure() {
                    return 0;
                }
                let mut cal = self.clone();
                cal.set_lenient(true);
                cal.prepare_get_actual(field, false, status);
                let eyear = cal.get(F::EXTENDED_YEAR, status);
                let month = cal.get(F::MONTH, status);
                self.handle_get_month_length(eyear, month)
            }
            F::DAY_OF_YEAR => {
                if status.is_failure() {
                    return 0;
                }
                let mut cal = self.clone();
                cal.set_lenient(true);
                cal.prepare_get_actual(field, false, status);
                let eyear = cal.get(F::EXTENDED_YEAR, status);
                self.handle_get_year_length(eyear)
            }
            F::DAY_OF_WEEK
            | F::AM_PM
            | F::HOUR
            | F::HOUR_OF_DAY
            | F::MINUTE
            | F::SECOND
            | F::MILLISECOND
            | F::ZONE_OFFSET
            | F::DST_OFFSET
            | F::DOW_LOCAL
            | F::JULIAN_DAY
            | F::MILLISECONDS_IN_DAY => self.get_maximum(field),
            _ => {
                let least = self.get_least_maximum(field);
                let max = self.get_maximum(field);
                self.get_actual_helper(field, least, max, status)
            }
        }
    }

    /// `GregorianCalendar::getActualMaximum(UCAL_YEAR)`.
    fn gregorian_actual_maximum_year(&mut self, status: &mut Status) -> i32 {
        if status.is_failure() {
            return 0;
        }
        let mut cal = self.clone();
        cal.set_lenient(true);
        let era = cal.get(F::ERA, status);
        let d = cal.get_time_in_millis(status);

        // Perform a binary search, with the invariant that lowGood is a valid year, and
        // highBad is an out of range year.
        let mut low_good = GREGORIAN_LIMITS[F::YEAR][Limit::GreatestMinimum as usize];
        let mut high_bad = GREGORIAN_LIMITS[F::YEAR][Limit::LeastMaximum as usize] + 1;
        while (low_good + 1) < high_bad {
            let y = i32::midpoint(low_good, high_bad);
            cal.set(F::YEAR, y);
            if cal.get(F::YEAR, status) == y && cal.get(F::ERA, status) == era {
                low_good = y;
            } else {
                high_bad = y;
                cal.set_time_in_millis(d, status); // Restore original fields
            }
        }
        low_good
    }

    /// `Calendar::prepareGetActual`.
    fn prepare_get_actual(&mut self, field: usize, is_minimum: bool, status: &mut Status) {
        self.set(F::MILLISECONDS_IN_DAY, 0);

        match field {
            F::YEAR | F::EXTENDED_YEAR => {
                let value = self.get_greatest_minimum(F::DAY_OF_YEAR);
                self.set(F::DAY_OF_YEAR, value);
            }
            F::YEAR_WOY | F::MONTH => {
                if field == F::YEAR_WOY {
                    let value = self.get_greatest_minimum(F::WEEK_OF_YEAR);
                    self.set(F::WEEK_OF_YEAR, value);
                }
                let value = self.get_greatest_minimum(F::DATE);
                self.set(F::DATE, value);
            }
            F::DAY_OF_WEEK_IN_MONTH => {
                // For dowim, the maximum occurs for the DOW of the first of the month.
                self.set(F::DATE, 1);
                let dow = self.get(F::DAY_OF_WEEK, status);
                self.set(F::DAY_OF_WEEK, dow); // Make this user set
            }
            F::WEEK_OF_MONTH | F::WEEK_OF_YEAR => {
                // If we're counting weeks, set the day of the week to either the first or
                // last localized DOW.
                let mut dow = self.first_day_of_week;
                if is_minimum {
                    dow = (dow + 6) % 7; // set to last DOW
                    if dow < grego::SUNDAY {
                        dow += 7;
                    }
                }
                self.set(F::DAY_OF_WEEK, dow);
            }
            _ => {}
        }

        // Do this last to give it the newest time stamp
        let value = self.get_greatest_minimum(field);
        self.set(field, value);
    }

    /// `Calendar::getActualHelper`.
    fn get_actual_helper(
        &mut self,
        field: usize,
        start_value: i32,
        end_value: i32,
        status: &mut Status,
    ) -> i32 {
        if start_value == end_value {
            // if we know that the maximum value is always the same, just return it
            return start_value;
        }
        let delta = if end_value > start_value { 1 } else { -1 };

        if status.is_failure() {
            return start_value;
        }
        let mut work = self.clone();

        // need to resolve time here, otherwise, fields set for actual limit may cause
        // conflict with fields previously set (but not yet resolved).
        work.complete(status);

        work.set_lenient(true);
        work.prepare_get_actual(field, delta < 0, status);

        // now try each value from the start to the end one by one until we get a value
        // that normalizes to another value.
        let mut start_value = start_value;
        work.set(field, start_value);

        let mut result = start_value;
        if (work.get(field, status) != start_value && field != F::WEEK_OF_MONTH && delta > 0)
            || status.is_failure()
        {
            // keep the start value
        } else {
            loop {
                start_value += delta;
                work.add(field, delta, status);
                if work.get(field, status) != start_value || status.is_failure() {
                    break;
                }
                result = start_value;
                if start_value == end_value {
                    break;
                }
            }
        }
        result
    }

    // ------------------------------------------------------------------ time <- fields

    /// `Calendar::resolveFields`.
    fn resolve_fields(&self, precedence_table: ResolutionTable) -> usize {
        let mut best_field = F::COUNT as i32;
        for group in precedence_table {
            if best_field != F::COUNT as i32 {
                break;
            }
            let mut best_stamp = UNSET;
            'lines: for line in *group {
                let mut line_stamp = UNSET;
                // Skip over first entry if it is negative
                let start = usize::from(line[0] >= RESOLVE_REMAP);
                for &field in &line[start..] {
                    if field == RESOLVE_STOP {
                        break;
                    }
                    let s = self.stamp[field as usize];
                    // If any field is unset then don't use this line
                    if s == UNSET {
                        continue 'lines;
                    } else if s > line_stamp {
                        line_stamp = s;
                    }
                }
                // Record new maximum stamp & field no.
                if line_stamp > best_stamp {
                    let mut temp_best_field = line[0]; // First field refers to entire line
                    if temp_best_field >= RESOLVE_REMAP {
                        temp_best_field &= RESOLVE_REMAP - 1;
                        // This check is needed to resolve some issues with UCAL_YEAR
                        // precedence mapping
                        if temp_best_field != F::DATE as i32
                            || self.stamp[F::WEEK_OF_MONTH] < self.stamp[temp_best_field as usize]
                        {
                            best_field = temp_best_field;
                        }
                    } else {
                        best_field = temp_best_field;
                    }

                    if best_field == temp_best_field {
                        best_stamp = line_stamp;
                    }
                }
            }
        }
        best_field as usize
    }

    /// `Calendar::newerField`.
    pub(super) fn newer_field(&self, default_field: usize, alternate_field: usize) -> usize {
        if self.stamp[alternate_field] > self.stamp[default_field] {
            alternate_field
        } else {
            default_field
        }
    }

    /// `Calendar::computeTime`.
    fn compute_time(&mut self, status: &mut Status) {
        // Compute the Julian day
        let julian_day = self.compute_julian_day();
        if julian_day < self.get_minimum(F::JULIAN_DAY) || julian_day > self.get_maximum(F::JULIAN_DAY) {
            status.fail();
            return;
        }

        let millis = grego::julian_day_to_millis(julian_day);

        // We only use MILLISECONDS_IN_DAY if it has been set by the user.
        let millis_in_day = if self.stamp[F::MILLISECONDS_IN_DAY] >= MINIMUM_USER_STAMP
            && self.newest_stamp(F::AM_PM, F::MILLISECOND, UNSET) <= self.stamp[F::MILLISECONDS_IN_DAY]
        {
            f64::from(self.internal_get(F::MILLISECONDS_IN_DAY))
        } else {
            self.compute_millis_in_day()
        };

        let t = if self.stamp[F::ZONE_OFFSET] >= MINIMUM_USER_STAMP
            || self.stamp[F::DST_OFFSET] >= MINIMUM_USER_STAMP
        {
            millis + millis_in_day
                - f64::from(self.internal_get(F::ZONE_OFFSET) + self.internal_get(F::DST_OFFSET))
        } else {
            millis + millis_in_day - f64::from(self.compute_zone_offset(millis, millis_in_day))
        };
        self.time = t;
    }

    /// `Calendar::computeMillisInDay`.
    fn compute_millis_in_day(&self) -> f64 {
        let mut millis_in_day = 0.0;

        // Find the best set of fields specifying the time of day.  There are only two
        // possibilities here; the HOUR_OF_DAY or the AM_PM and the HOUR.
        let hour_of_day_stamp = self.stamp[F::HOUR_OF_DAY];
        let hour_stamp = self.stamp[F::HOUR].max(self.stamp[F::AM_PM]);
        let best_stamp = hour_stamp.max(hour_of_day_stamp);

        // Hours
        if best_stamp != UNSET {
            if best_stamp == hour_of_day_stamp {
                // Don't normalize here; let overflow bump into the next period.
                millis_in_day += f64::from(self.internal_get(F::HOUR_OF_DAY));
            } else {
                millis_in_day += f64::from(self.internal_get(F::HOUR));
                millis_in_day += f64::from(12 * self.internal_get(F::AM_PM)); // Default works for unset AM_PM
            }
        }

        // We use the fact that unset == 0; we start with millisInDay == HOUR_OF_DAY.
        millis_in_day *= 60.0;
        millis_in_day += f64::from(self.internal_get(F::MINUTE)); // now have minutes
        millis_in_day *= 60.0;
        millis_in_day += f64::from(self.internal_get(F::SECOND)); // now have seconds
        millis_in_day *= 1000.0;
        millis_in_day += f64::from(self.internal_get(F::MILLISECOND)); // now have millis
        millis_in_day
    }

    /// `Calendar::computeZoneOffset`: the offset of the wall time, with skipped wall
    /// times read with the offsets before the transition and repeated ones with those
    /// after it (`UCAL_WALLTIME_LAST`).
    fn compute_zone_offset(&self, millis: f64, millis_in_day: f64) -> i32 {
        let wall = millis + millis_in_day;
        let (raw_offset, dst_offset) = self.zone.get_offset_from_local(wall);
        raw_offset + dst_offset
    }

    /// `Calendar::computeJulianDay`.
    fn compute_julian_day(&mut self) -> i32 {
        // We want to see if any of the date fields is newer than the JULIAN_DAY.  If not,
        // then we use JULIAN_DAY.  We only use JULIAN_DAY if it has been set by the user.
        if self.stamp[F::JULIAN_DAY] >= MINIMUM_USER_STAMP {
            let mut best_stamp = self.newest_stamp(F::ERA, F::DAY_OF_WEEK_IN_MONTH, UNSET);
            best_stamp = self.newest_stamp(F::YEAR_WOY, F::EXTENDED_YEAR, best_stamp);
            if best_stamp <= self.stamp[F::JULIAN_DAY] {
                return self.internal_get(F::JULIAN_DAY);
            }
        }

        let mut best_field = self.resolve_fields(self.system.field_resolution_table());
        if best_field == F::COUNT {
            best_field = F::DAY_OF_MONTH;
        }
        self.handle_compute_julian_day(best_field)
    }

    /// `Calendar::getFieldResolutionTable`.
    pub(super) const fn date_precedence() -> ResolutionTable {
        DATE_PRECEDENCE
    }

    /// `Calendar::handleComputeJulianDay`.
    pub(super) fn calendar_handle_compute_julian_day(&mut self, best_field: usize) -> i32 {
        let use_month = best_field == F::DAY_OF_MONTH
            || best_field == F::WEEK_OF_MONTH
            || best_field == F::DAY_OF_WEEK_IN_MONTH;

        let year = if best_field == F::WEEK_OF_YEAR && self.newer_field(F::YEAR_WOY, F::YEAR) == F::YEAR_WOY {
            self.internal_get(F::YEAR_WOY)
        } else {
            self.handle_get_extended_year()
        };

        self.internal_set(F::EXTENDED_YEAR, year);

        // Get the Julian day of the day BEFORE the start of this year.  If useMonth is
        // true, get the day before the start of the month.
        let month = if self.is_set(F::MONTH) {
            self.internal_get(F::MONTH)
        } else {
            self.get_default_month_in_year(year)
        };

        let mut julian_day =
            self.handle_compute_month_start(year, if use_month { month } else { 0 }, use_month);

        if best_field == F::DAY_OF_MONTH {
            // give calendar subclass a chance to have a default 'first' dom
            let day_of_month = if self.is_set(F::DAY_OF_MONTH) {
                self.internal_get_or(F::DAY_OF_MONTH, 1)
            } else {
                self.get_default_day_in_month(year, month)
            };
            return julian_day.wrapping_add(day_of_month);
        }

        if best_field == F::DAY_OF_YEAR {
            return julian_day.wrapping_add(self.internal_get(F::DAY_OF_YEAR));
        }

        let first_day_of_week = self.first_day_of_week; // Localized fdw

        // Get the 0-based localized DOW of day one of the month or year.
        let mut first = Self::julian_day_to_day_of_week(f64::from(julian_day) + 1.0) - first_day_of_week;
        if first < 0 {
            first += 7;
        }

        let dow_local = self.get_local_dow();

        // Find the first target DOW (dowLocal) in the month or year.
        let mut date = 1 - first + dow_local;

        if best_field == F::DAY_OF_WEEK_IN_MONTH {
            // Adjust the target DOW to be in the month or year.
            if date < 1 {
                date += 7;
            }
            let dim = self.internal_get_or(F::DAY_OF_WEEK_IN_MONTH, 1);
            if dim >= 0 {
                date += 7 * (dim - 1);
            } else {
                // Move date to the last of this day-of-week in this month, then back up as
                // needed.
                let m = self.internal_get_or(F::MONTH, 0);
                let month_length = self.handle_get_month_length(year, m);
                date += ((month_length - date) / 7 + dim + 1) * 7;
            }
        } else {
            if best_field == F::WEEK_OF_YEAR
                && (!self.is_set(F::YEAR_WOY)
                    || (self.resolve_fields(YEAR_PRECEDENCE) != F::YEAR_WOY
                        && self.stamp[F::YEAR_WOY] != INTERNALLY_SET))
            {
                // need to be sure to stay in 'real' year.
                let woy = self.internal_get(best_field);

                let next_julian_day = self.handle_compute_month_start(year + 1, 0, false); // jd of day before jan 1
                let mut next_first =
                    Self::julian_day_to_day_of_week(f64::from(next_julian_day) + 1.0) - first_day_of_week;
                if next_first < 0 {
                    // 0..6 ldow of Jan 1
                    next_first += 7;
                }

                if woy == 1 {
                    // nextFirst is now the localized DOW of Jan 1  of y-woy+1
                    if next_first > 0 && (7 - next_first) >= self.minimal_days_in_first_week {
                        // Jan 1 of (yearWoy+1) is in yearWoy+1 - recalculate JD to next year
                        julian_day = next_julian_day;

                        // recalculate 'first' [0-based local dow of jan 1]
                        first =
                            Self::julian_day_to_day_of_week(f64::from(julian_day) + 1.0) - first_day_of_week;
                        if first < 0 {
                            first += 7;
                        }
                        // recalculate date.
                        date = 1 - first + dow_local;
                    }
                } else if woy >= self.get_least_maximum(best_field) {
                    // could be in the last week- find out if this JD would overstep
                    let mut test_date = date;
                    if (7 - first) < self.minimal_days_in_first_week {
                        test_date += 7;
                    }

                    // Now adjust for the week number.
                    test_date += 7 * (woy - 1);

                    if julian_day + test_date > next_julian_day {
                        // is it past Dec 31?  (nextJulianDay is day BEFORE year+1's  Jan 1)
                        // Fire up the calculating engines.. retry YWOY = (year-1)
                        julian_day = self.handle_compute_month_start(year - 1, 0, false); // jd before Jan 1 of previous year
                        first =
                            Self::julian_day_to_day_of_week(f64::from(julian_day) + 1.0) - first_day_of_week; // 0 based local dow   of first week

                        if first < 0 {
                            // 0..6
                            first += 7;
                        }
                        date = 1 - first + dow_local;
                    }
                }
            }

            // Adjust for minimal days in first week
            if (7 - first) < self.minimal_days_in_first_week {
                date += 7;
            }

            // Now adjust for the week number.
            date = date.wrapping_add(7_i32.wrapping_mul(self.internal_get(best_field).wrapping_sub(1)));
        }

        julian_day.wrapping_add(date)
    }

    /// `Calendar::getLocalDOW`.
    fn get_local_dow(&self) -> i32 {
        // Get zero-based localized DOW, valid range 0..6.  This is the DOW we are looking
        // for.
        let mut dow_local = match self.resolve_fields(DOW_PRECEDENCE) {
            F::DAY_OF_WEEK => self.internal_get(F::DAY_OF_WEEK) - self.first_day_of_week,
            F::DOW_LOCAL => self.internal_get(F::DOW_LOCAL) - 1,
            _ => 0,
        };
        dow_local %= 7;
        if dow_local < 0 {
            dow_local += 7;
        }
        dow_local
    }

    /// `Calendar::handleGetExtendedYearFromWeekFields`.
    pub(super) fn calendar_extended_year_from_week_fields(&mut self, year_woy: i32, woy: i32) -> i32 {
        // First, we need a reliable DOW.
        let best_field = self.resolve_fields(DATE_PRECEDENCE);

        // Now, a local DOW
        let dow_local = self.get_local_dow(); // 0..6
        let first_day_of_week = self.first_day_of_week; // Localized fdw
        let jan1_start = self.handle_compute_month_start(year_woy, 0, false);
        let next_jan1_start = self.handle_compute_month_start(year_woy + 1, 0, false); // next year's Jan1 start

        // Get the 0-based localized DOW of day one of the month or year.
        let mut first = Self::julian_day_to_day_of_week(f64::from(jan1_start) + 1.0) - first_day_of_week;
        if first < 0 {
            first += 7;
        }

        let min_days = self.minimal_days_in_first_week;
        // January 1st in the year of WOY is the 1st week?  (i.e. first week is < minimal )
        let jan1_in_prev_year = (7 - first) < min_days;

        match best_field {
            F::WEEK_OF_YEAR => {
                if woy == 1 {
                    if jan1_in_prev_year {
                        // the first week of January is in the previous year therefore WOY1
                        // is always solidly within yearWoy
                        year_woy
                    } else if dow_local < first {
                        // First WOY is split between two years; we are prior to Jan 1
                        year_woy - 1 // previous year
                    } else {
                        year_woy // in this year
                    }
                } else if woy >= self.get_least_maximum(best_field) {
                    // we _might_ be in the last week..
                    let mut jd = jan1_start + (7 - first) + (woy - 1) * 7 + dow_local;
                    if !jan1_in_prev_year {
                        jd -= 7; // woy already includes Jan 1's week.
                    }
                    if (jd + 1) >= next_jan1_start {
                        // we are in week 52 or 53 etc. - actual year is yearWoy+1
                        year_woy + 1
                    } else {
                        // still in yearWoy;
                        year_woy
                    }
                } else {
                    // we're not possibly in the last week -must be ywoy
                    year_woy
                }
            }
            F::DATE => {
                if self.internal_get(F::MONTH) == 0 && woy >= self.get_least_maximum(F::WEEK_OF_YEAR) {
                    year_woy + 1 // month 0, late woy = in the next year
                } else if woy == 1 {
                    if self.internal_get(F::MONTH) == 0 {
                        year_woy
                    } else {
                        year_woy - 1
                    }
                } else {
                    year_woy
                }
            }
            _ => year_woy, // assume the year is appropriate
        }
    }

    /// `Calendar::handleGetMonthLength` (the generic version).
    pub(super) fn calendar_handle_get_month_length(&mut self, extended_year: i32, month: i32) -> i32 {
        self.handle_compute_month_start(extended_year, month + 1, true)
            - self.handle_compute_month_start(extended_year, month, true)
    }

    /// `Calendar::handleGetYearLength` (the generic version).
    pub(super) fn calendar_handle_get_year_length(&mut self, eyear: i32) -> i32 {
        self.handle_compute_month_start(eyear + 1, 0, false)
            - self.handle_compute_month_start(eyear, 0, false)
    }
}
