//! The calendar systems: ports of the `handle*` hooks of ICU's `Calendar` subclasses
//! (`GregorianCalendar` in `gregocal.cpp`, ...). [`Calendar`] dispatches to them by
//! [`CalendarSystem`].

use super::calendar::{Calendar, Limit, Status, field as F};
use super::grego::{self, JAN1_1_JULIAN_DAY, ONE_DAY};

/// `kGregorianCalendarLimits`.
const GREGORIAN_LIMITS: [[i32; 4]; F::COUNT] = [
    [0, 0, 1, 1],                           // ERA
    [1, 1, 140_742, 144_683],               // YEAR
    [0, 0, 11, 11],                         // MONTH
    [1, 1, 52, 53],                         // WEEK_OF_YEAR
    [-1, -1, -1, -1],                       // WEEK_OF_MONTH
    [1, 1, 28, 31],                         // DAY_OF_MONTH
    [1, 1, 365, 366],                       // DAY_OF_YEAR
    [-1, -1, -1, -1],                       // DAY_OF_WEEK
    [-1, -1, 4, 5],                         // DAY_OF_WEEK_IN_MONTH
    [-1, -1, -1, -1],                       // AM_PM
    [-1, -1, -1, -1],                       // HOUR
    [-1, -1, -1, -1],                       // HOUR_OF_DAY
    [-1, -1, -1, -1],                       // MINUTE
    [-1, -1, -1, -1],                       // SECOND
    [-1, -1, -1, -1],                       // MILLISECOND
    [-1, -1, -1, -1],                       // ZONE_OFFSET
    [-1, -1, -1, -1],                       // DST_OFFSET
    [-140_742, -140_742, 140_742, 144_683], // YEAR_WOY
    [-1, -1, -1, -1],                       // DOW_LOCAL
    [-140_742, -140_742, 140_742, 144_683], // EXTENDED_YEAR
    [-1, -1, -1, -1],                       // JULIAN_DAY
    [-1, -1, -1, -1],                       // MILLISECONDS_IN_DAY
    [-1, -1, -1, -1],                       // IS_LEAP_MONTH
];

/// `kNumDays`.
const NUM_DAYS: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
/// `kLeapNumDays`.
const LEAP_NUM_DAYS: [i32; 12] = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];
/// `kMonthLength`.
const MONTH_LENGTH: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
/// `kLeapMonthLength`.
const LEAP_MONTH_LENGTH: [i32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// `kCutoverJulianDay`: October 15, 1582.
const CUTOVER_JULIAN_DAY: i32 = 2_299_161;

/// `GregorianCalendar::AD` / `BC`.
const BC: i32 = 0;
const AD: i32 = 1;

/// The state of a `GregorianCalendar`: its Julian/Gregorian cutover.
#[derive(Clone, Debug, PartialEq)]
pub struct GregorianState {
    /// `fGregorianCutover`.
    cutover: f64,
    /// `fCutoverJulianDay`.
    cutover_julian_day: i32,
    /// `fGregorianCutoverYear`.
    cutover_year: i32,
    /// `fIsGregorian`.
    is_gregorian: bool,
    /// `fInvertGregorian`.
    invert_gregorian: bool,
}

impl GregorianState {
    /// The default cutover of October 15, 1582 (`kPapalCutover`).
    pub fn papal() -> Self {
        Self {
            cutover: (f64::from(CUTOVER_JULIAN_DAY) - f64::from(grego::EPOCH_START_AS_JULIAN_DAY)) * ONE_DAY,
            cutover_julian_day: CUTOVER_JULIAN_DAY,
            cutover_year: 1582,
            is_gregorian: true,
            invert_gregorian: false,
        }
    }

    /// `ucal_setGregorianChange(cal, U_DATE_MIN)`: a proleptic Gregorian calendar. The
    /// cutover day pins to `INT32_MIN`, and the cutover year is the year of
    /// `MIN_MILLIS` in a calendar with the default cutover (Julian, 5838270 BC).
    pub fn proleptic() -> Self {
        // `cal->setTime(U_DATE_MIN)` pins to MIN_MILLIS, whose Julian year is 5838270 BC.
        let julian_epoch_day = grego::MIN_JULIAN - (JAN1_1_JULIAN_DAY - 2);
        let eyear = grego::floor_divide_rem_i32(4.0 * f64::from(julian_epoch_day) + 1464.0, 1461).0;
        Self {
            cutover: f64::from(i32::MIN) * ONE_DAY,
            cutover_julian_day: i32::MIN,
            cutover_year: eyear,
            is_gregorian: true,
            invert_gregorian: false,
        }
    }
}

/// A calendar system (the `Calendar` subclass and its state).
#[derive(Clone, Debug, PartialEq)]
pub enum CalendarSystem {
    /// `GregorianCalendar`, reporting `type` as its `getType`.
    Gregorian(GregorianState),
}

impl CalendarSystem {
    /// `Calendar::getType`.
    pub fn calendar_type(&self) -> &'static str {
        match self {
            Self::Gregorian(_) => "gregorian",
        }
    }

    /// The subclass part of `isEquivalentTo` (`fGregorianCutover`).
    pub fn is_equivalent_to(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Gregorian(a), Self::Gregorian(b)) => a.cutover == b.cutover,
        }
    }

    /// `handleGetLimit`.
    pub fn handle_get_limit(&self, field: usize, limit: Limit) -> i32 {
        match self {
            Self::Gregorian(_) => GREGORIAN_LIMITS[field][limit as usize],
        }
    }

    /// Whether the calendar is a `GregorianCalendar` (its `getActualMaximum(YEAR)` is a
    /// binary search over the year).
    pub fn is_gregorian_family(&self) -> bool {
        matches!(self, Self::Gregorian(_))
    }

    /// Whether `getActualMinimum` is `getMinimum` (`GregorianCalendar`'s override).
    pub fn actual_minimum_is_minimum(&self) -> bool {
        matches!(self, Self::Gregorian(_))
    }

    /// `getDefaultMonthInYear`.
    pub fn get_default_month_in_year(&self, _eyear: i32) -> i32 {
        0
    }

    /// `getDefaultDayInMonth`.
    pub fn get_default_day_in_month(&self, _eyear: i32, _month: i32) -> i32 {
        1
    }

    /// `getFieldResolutionTable`.
    pub fn field_resolution_table(
        &self,
    ) -> &'static [&'static [&'static [i32]]] {
        Calendar::date_precedence()
    }
}

impl Calendar {
    fn gregorian_state(&self) -> &GregorianState {
        match &self.system {
            CalendarSystem::Gregorian(state) => state,
        }
    }

    fn gregorian_state_mut(&mut self) -> &mut GregorianState {
        match &mut self.system {
            CalendarSystem::Gregorian(state) => state,
        }
    }

    /// `handleComputeFields`.
    pub(super) fn handle_compute_fields(&mut self, julian_day: i32, status: &mut Status) {
        match &self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_compute_fields(julian_day, status),
        }
    }

    /// `handleComputeMonthStart`.
    pub(super) fn handle_compute_month_start(&mut self, eyear: i32, month: i32, use_month: bool) -> i32 {
        match &self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_compute_month_start(eyear, month, use_month),
        }
    }

    /// `handleGetMonthLength`.
    pub(super) fn handle_get_month_length(&mut self, extended_year: i32, month: i32) -> i32 {
        match &self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_get_month_length(extended_year, month),
        }
    }

    /// `handleGetYearLength`.
    pub(super) fn handle_get_year_length(&mut self, eyear: i32) -> i32 {
        match &self.system {
            CalendarSystem::Gregorian(_) => {
                if self.gregorian_is_leap_year(eyear) {
                    366
                } else {
                    365
                }
            }
        }
    }

    /// `handleGetExtendedYear`.
    pub(super) fn handle_get_extended_year(&mut self) -> i32 {
        match &self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_get_extended_year(),
        }
    }

    /// `handleComputeJulianDay`.
    pub(super) fn handle_compute_julian_day(&mut self, best_field: usize) -> i32 {
        match &self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_compute_julian_day(best_field),
        }
    }

    // -------------------------------------------------------------- GregorianCalendar

    /// `GregorianCalendar::isLeapYear`.
    fn gregorian_is_leap_year(&self, year: i32) -> bool {
        if year >= self.gregorian_state().cutover_year {
            // Gregorian
            (year & 0x3) == 0 && (year % 100 != 0 || year % 400 == 0)
        } else {
            // Julian
            (year & 0x3) == 0
        }
    }

    /// `GregorianCalendar::handleComputeFields`.
    fn gregorian_handle_compute_fields(&mut self, julian_day: i32, status: &mut Status) {
        if status.is_failure() {
            return;
        }
        let state = self.gregorian_state().clone();
        let (eyear, month, day_of_month, mut day_of_year);
        if julian_day >= state.cutover_julian_day {
            month = self.gregorian_month;
            day_of_month = self.gregorian_day_of_month;
            day_of_year = self.gregorian_day_of_year;
            eyear = self.gregorian_year;
        } else {
            // The Julian epoch day (not the same as Julian Day) is zero on Saturday
            // December 30, 0 (Gregorian).
            let julian_epoch_day = julian_day - (JAN1_1_JULIAN_DAY - 2);
            eyear = grego::floor_divide_rem_i32(4.0 * f64::from(julian_epoch_day) + 1464.0, 1461).0;

            // Compute the Julian calendar day number for January 1, eyear
            let january1 = 365 * (eyear - 1) + grego::floor_divide_i32(eyear - 1, 4);
            day_of_year = julian_epoch_day - january1; // 0-based

            // Julian leap years occurred historically every 4 years starting with 8 AD.
            // We implement the proleptic calendar.
            let is_leap = (eyear & 0x3) == 0; // equiv. to (eyear%4 == 0)

            // Common Julian/Gregorian calculation
            let march1 = if is_leap { 60 } else { 59 }; // zero-based DOY for March 1
            let correction = if day_of_year >= march1 {
                if is_leap { 1 } else { 2 }
            } else {
                0
            };
            month = (12 * (day_of_year + correction) + 6) / 367; // zero-based month
            day_of_month = day_of_year
                - if is_leap {
                    LEAP_NUM_DAYS[month as usize]
                } else {
                    NUM_DAYS[month as usize]
                }
                + 1; // one-based DOM
            day_of_year += 1;
        }

        // [j81] if we are after the cutover in its year, shift the day of the year
        if eyear == state.cutover_year && julian_day >= state.cutover_julian_day {
            // from handleComputeMonthStart
            let greg_shift = grego::gregorian_shift(eyear);
            day_of_year += greg_shift;
        }

        self.internal_set(F::MONTH, month);
        self.internal_set(F::DAY_OF_MONTH, day_of_month);
        self.internal_set(F::DAY_OF_YEAR, day_of_year);
        self.internal_set(F::EXTENDED_YEAR, eyear);
        let mut era = AD;
        let mut year = eyear;
        if eyear < 1 {
            era = BC;
            year = 1 - eyear;
        }
        self.internal_set(F::ERA, era);
        self.internal_set(F::YEAR, year);
    }

    /// `GregorianCalendar::handleComputeJulianDay`.
    fn gregorian_handle_compute_julian_day(&mut self, best_field: usize) -> i32 {
        self.gregorian_state_mut().invert_gregorian = false;

        let mut jd = self.calendar_handle_compute_julian_day(best_field);

        let state = self.gregorian_state().clone();
        // if we are doing WOY calculations, we are counting relative to Jan 1 *julian*
        if best_field == F::WEEK_OF_YEAR
            && self.internal_get(F::EXTENDED_YEAR) == state.cutover_year
            && jd >= state.cutover_julian_day
        {
            // So that the Julian Jan 1 will be used in handleComputeMonthStart
            self.gregorian_state_mut().invert_gregorian = true;
            return self.calendar_handle_compute_julian_day(best_field);
        }

        // The following check handles portions of the cutover year BEFORE the cutover
        // itself happens.
        if self.gregorian_state().is_gregorian != (jd >= state.cutover_julian_day) {
            self.gregorian_state_mut().invert_gregorian = true;
            jd = self.calendar_handle_compute_julian_day(best_field);
        }

        if self.gregorian_state().is_gregorian && self.internal_get(F::EXTENDED_YEAR) == state.cutover_year {
            let greg_shift = grego::gregorian_shift(self.internal_get(F::EXTENDED_YEAR));
            if best_field == F::DAY_OF_YEAR {
                jd -= greg_shift;
            } else if best_field == F::WEEK_OF_MONTH {
                let week_shift = 14;
                jd += week_shift; // shift by weeks for week based fields.
            }
        }
        jd
    }

    /// `GregorianCalendar::handleComputeMonthStart`.
    fn gregorian_handle_compute_month_start(&mut self, eyear: i32, month: i32, _use_month: bool) -> i32 {
        let mut eyear = eyear;
        let mut month = month;
        // If the month is out of range, adjust it into range, and modify the extended year
        // value accordingly.
        if !(0..=11).contains(&month) {
            let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
            eyear = eyear.wrapping_add(quotient);
            month = remainder;
        }

        let mut is_leap = eyear % 4 == 0;
        let y = i64::from(eyear) - 1;
        let mut julian_day = 365 * y + grego::floor_divide_i64(y, 4) + i64::from(JAN1_1_JULIAN_DAY - 3);

        let state = self.gregorian_state_mut();
        state.is_gregorian = eyear >= state.cutover_year;
        if state.invert_gregorian {
            state.is_gregorian = !state.is_gregorian;
        }
        if state.is_gregorian {
            is_leap = is_leap && (eyear % 100 != 0 || eyear % 400 == 0);
            // Add 2 because Gregorian calendar starts 2 days after Julian calendar
            let greg_shift = grego::gregorian_shift(eyear);
            julian_day += i64::from(greg_shift);
        }

        // At this point julianDay indicates the day BEFORE the first day of January 1,
        // <eyear> of either the Julian or Gregorian calendar.
        if month != 0 {
            julian_day += i64::from(if is_leap {
                LEAP_NUM_DAYS[month as usize]
            } else {
                NUM_DAYS[month as usize]
            });
        }
        julian_day as i32
    }

    /// `GregorianCalendar::handleGetMonthLength`.
    fn gregorian_handle_get_month_length(&self, extended_year: i32, month: i32) -> i32 {
        let mut extended_year = extended_year;
        let mut month = month;
        // If the month is out of range, adjust it into range, and modify the extended year
        // value accordingly.
        if !(0..=11).contains(&month) {
            let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
            extended_year = extended_year.wrapping_add(quotient);
            month = remainder;
        }
        if self.gregorian_is_leap_year(extended_year) {
            LEAP_MONTH_LENGTH[month as usize]
        } else {
            MONTH_LENGTH[month as usize]
        }
    }

    /// `GregorianCalendar::handleGetExtendedYear`.
    fn gregorian_handle_get_extended_year(&mut self) -> i32 {
        // There are three separate fields which could be used to derive the proper year.
        // Use the one most recently set.
        let mut year_field = F::EXTENDED_YEAR;
        if self.stamp[year_field] < self.stamp[F::YEAR] {
            year_field = F::YEAR;
        }
        if self.stamp[year_field] < self.stamp[F::YEAR_WOY] {
            year_field = F::YEAR_WOY;
        }

        // based on the "best" year field, get the year
        match year_field {
            F::EXTENDED_YEAR => self.internal_get_or(F::EXTENDED_YEAR, grego::EPOCH_YEAR),
            F::YEAR => {
                // The year defaults to the epoch start, the era to AD
                let era = self.internal_get_or(F::ERA, AD);
                if era == BC {
                    1 - self.internal_get_or(F::YEAR, 1) // Convert to extended year
                } else {
                    self.internal_get_or(F::YEAR, grego::EPOCH_YEAR)
                }
            }
            _ => {
                let year_woy = self.internal_get(F::YEAR_WOY);
                let woy = self.internal_get(F::WEEK_OF_YEAR);
                self.gregorian_extended_year_from_week_fields(year_woy, woy)
            }
        }
    }

    /// `GregorianCalendar::handleGetExtendedYearFromWeekFields`.
    fn gregorian_extended_year_from_week_fields(&mut self, year_woy: i32, woy: i32) -> i32 {
        // convert year to extended form
        let era = self.internal_get_or(F::ERA, AD);
        let year_woy = if era == BC { 1 - year_woy } else { year_woy };
        self.calendar_extended_year_from_week_fields(year_woy, woy)
    }
}
