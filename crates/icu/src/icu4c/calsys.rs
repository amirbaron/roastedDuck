//! The calendar systems: ports of the `handle*` hooks of ICU's `Calendar` subclasses
//! (`GregorianCalendar` in `gregocal.cpp`, `JapaneseCalendar`, `BuddhistCalendar`,
//! `TaiwanCalendar`, `HebrewCalendar`, `IndianCalendar`, `IslamicCalendar`,
//! `CopticCalendar` / `EthiopicCalendar` (`CECalendar`) and `PersianCalendar`).
//! [`Calendar`] dispatches to them by [`CalendarSystem`].

use super::calendar::{Calendar, Limit, Status, field as F};
use super::grego::{self, EPOCH_START_AS_JULIAN_DAY, JAN1_1_JULIAN_DAY, ONE_DAY};
use super::japanese_eras::JAPANESE_ERA_STARTS;

/// A calendar's limits table (`[field][ELimitType]`).
pub(super) type Limits = [[i32; 4]; F::COUNT];

/// `kGregorianCalendarLimits`.
pub(super) const GREGORIAN_LIMITS: Limits = [
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

/// The limits shared by the lunar and solar calendars, with their own YEAR, MONTH,
/// WEEK_OF_YEAR, DAY_OF_MONTH, DAY_OF_YEAR and DAY_OF_WEEK_IN_MONTH rows.
const fn limits(
    era: [i32; 4],
    year: [i32; 4],
    month: [i32; 4],
    week_of_year: [i32; 4],
    day_of_month: [i32; 4],
    day_of_year: [i32; 4],
    day_of_week_in_month: [i32; 4],
    extended_year: [i32; 4],
) -> Limits {
    const NA: [i32; 4] = [-1, -1, -1, -1];
    [
        era,
        year,
        month,
        week_of_year,
        NA,
        day_of_month,
        day_of_year,
        NA,
        day_of_week_in_month,
        NA,
        NA,
        NA,
        NA,
        NA,
        NA,
        NA,
        NA,
        extended_year,
        NA,
        extended_year,
        NA,
        NA,
        NA,
    ]
}

/// `HEBRWCAL_LIMITS`.
const HEBREW_LIMITS: Limits = limits(
    [0, 0, 0, 0],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
    [0, 0, 12, 12],
    [1, 1, 51, 56],
    [1, 1, 29, 30],
    [1, 1, 353, 385],
    [-1, -1, 5, 5],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
);

/// `INDIANCAL_LIMITS`.
const INDIAN_LIMITS: Limits = limits(
    [0, 0, 0, 0],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
    [0, 0, 11, 11],
    [1, 1, 52, 53],
    [1, 1, 30, 31],
    [1, 1, 365, 366],
    [-1, -1, 5, 5],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
);

/// `ISLAMCAL_LIMITS`.
const ISLAMIC_LIMITS: Limits = {
    let mut table = limits(
        [0, 0, 0, 0],
        [1, 1, 5_000_000, 5_000_000],
        [0, 0, 11, 11],
        [1, 1, 50, 51],
        [1, 1, 29, 31],
        [1, 1, 354, 355],
        [-1, -1, 5, 5],
        [1, 1, 5_000_000, 5_000_000],
    );
    table[F::YEAR_WOY] = [1, 1, 5_000_000, 5_000_000];
    table
};

/// `CECAL_LIMITS`.
const CE_LIMITS: Limits = {
    let mut table = limits(
        [0, 0, 1, 1],
        [1, 1, 5_000_000, 5_000_000],
        [0, 0, 12, 12],
        [1, 1, 52, 53],
        [1, 1, 5, 30],
        [1, 1, 365, 366],
        [-1, -1, 1, 5],
        [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
    );
    table[F::YEAR_WOY] = [-5_000_000, -5_000_000, 5_000_000, 5_000_000];
    table
};

/// `kPersianCalendarLimits`.
const PERSIAN_LIMITS: Limits = limits(
    [0, 0, 0, 0],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
    [0, 0, 11, 11],
    [1, 1, 52, 53],
    [1, 1, 29, 31],
    [1, 1, 365, 366],
    [1, 1, 5, 5],
    [-5_000_000, -5_000_000, 5_000_000, 5_000_000],
);

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

/// `kBuddhistEraStart`: 544 BC (Gregorian).
const BUDDHIST_ERA_START: i32 = -543;
/// `kTaiwanEraStart`: 1911 (Gregorian).
const TAIWAN_ERA_START: i32 = 1911;
/// `TaiwanCalendar::BEFORE_MINGUO` / `MINGUO`.
const BEFORE_MINGUO: i32 = 0;
const MINGUO: i32 = 1;
/// The default of EXTENDED_YEAR in the Gregorian derivatives (`k*GregorianEpoch`).
const GREGORIAN_EPOCH: i32 = 1970;

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

/// The calculation of an `IslamicCalendar` (`ECalculationType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslamicType {
    Civil,
    Tbla,
    Umalqura,
}

/// A calendar system (the `Calendar` subclass and its state).
#[derive(Clone, Debug, PartialEq)]
pub enum CalendarSystem {
    /// `GregorianCalendar`.
    Gregorian(GregorianState),
    /// `JapaneseCalendar`: Gregorian with Japanese eras.
    Japanese(GregorianState),
    /// `BuddhistCalendar`: Gregorian with the Buddhist era.
    Buddhist(GregorianState),
    /// `TaiwanCalendar` (`roc`): Gregorian with the Minguo era.
    Taiwan(GregorianState),
    /// `HebrewCalendar`.
    Hebrew,
    /// `IndianCalendar`.
    Indian,
    /// `IslamicCalendar` with an arithmetic calculation.
    Islamic(IslamicType),
    /// `CopticCalendar`.
    Coptic,
    /// `EthiopicCalendar`, in the Amete Alem era only if set.
    Ethiopic { amete_alem: bool },
    /// `PersianCalendar`.
    Persian,
}

impl CalendarSystem {
    /// The calendar `createStandardCalendar` makes for a calendar keyword; `proleptic`
    /// applies `ucal_setGregorianChange(U_DATE_MIN)`, which only takes on plain
    /// `GregorianCalendar`s. Keywords without a port give `None`.
    pub fn for_keyword(keyword: &str, proleptic: bool) -> Option<Self> {
        let gregorian = || {
            if proleptic {
                GregorianState::proleptic()
            } else {
                GregorianState::papal()
            }
        };
        Some(match keyword.to_ascii_lowercase().as_str() {
            "gregorian" | "iso8601" => Self::Gregorian(gregorian()),
            "japanese" => Self::Japanese(GregorianState::papal()),
            "buddhist" => Self::Buddhist(GregorianState::papal()),
            "roc" => Self::Taiwan(GregorianState::papal()),
            "hebrew" => Self::Hebrew,
            "indian" => Self::Indian,
            "islamic-civil" => Self::Islamic(IslamicType::Civil),
            "islamic-tbla" => Self::Islamic(IslamicType::Tbla),
            "islamic-umalqura" => Self::Islamic(IslamicType::Umalqura),
            "coptic" => Self::Coptic,
            "ethiopic" => Self::Ethiopic { amete_alem: false },
            "ethiopic-amete-alem" => Self::Ethiopic { amete_alem: true },
            "persian" => Self::Persian,
            _ => return None,
        })
    }

    /// `Calendar::getType`.
    pub fn calendar_type(&self) -> &'static str {
        match self {
            Self::Gregorian(_) => "gregorian",
            Self::Japanese(_) => "japanese",
            Self::Buddhist(_) => "buddhist",
            Self::Taiwan(_) => "roc",
            Self::Hebrew => "hebrew",
            Self::Indian => "indian",
            Self::Islamic(IslamicType::Civil) => "islamic-civil",
            Self::Islamic(IslamicType::Tbla) => "islamic-tbla",
            Self::Islamic(IslamicType::Umalqura) => "islamic-umalqura",
            Self::Coptic => "coptic",
            Self::Ethiopic { amete_alem: false } => "ethiopic",
            Self::Ethiopic { amete_alem: true } => "ethiopic-amete-alem",
            Self::Persian => "persian",
        }
    }

    /// The Gregorian state of a `GregorianCalendar` derivative.
    fn gregorian(&self) -> Option<&GregorianState> {
        match self {
            Self::Gregorian(state) | Self::Japanese(state) | Self::Buddhist(state) | Self::Taiwan(state) => {
                Some(state)
            }
            _ => None,
        }
    }

    /// The subclass part of `isEquivalentTo` (`fGregorianCutover`).
    pub fn is_equivalent_to(&self, other: &Self) -> bool {
        match (self.gregorian(), other.gregorian()) {
            (Some(a), Some(b)) => a.cutover == b.cutover,
            _ => true,
        }
    }

    /// `handleGetLimit`.
    pub fn handle_get_limit(&self, field: usize, limit: Limit) -> i32 {
        match self {
            Self::Gregorian(_) => GREGORIAN_LIMITS[field][limit as usize],
            Self::Japanese(_) => japanese_get_limit(field, limit),
            Self::Buddhist(_) => {
                if field == F::ERA {
                    0 // BE
                } else {
                    GREGORIAN_LIMITS[field][limit as usize]
                }
            }
            Self::Taiwan(_) => {
                if field == F::ERA {
                    if matches!(limit, Limit::Minimum | Limit::GreatestMinimum) {
                        BEFORE_MINGUO
                    } else {
                        MINGUO
                    }
                } else {
                    GREGORIAN_LIMITS[field][limit as usize]
                }
            }
            Self::Hebrew => HEBREW_LIMITS[field][limit as usize],
            Self::Indian => INDIAN_LIMITS[field][limit as usize],
            Self::Islamic(_) => ISLAMIC_LIMITS[field][limit as usize],
            Self::Coptic => CE_LIMITS[field][limit as usize],
            Self::Ethiopic { amete_alem } => {
                if *amete_alem && field == F::ERA {
                    0 // Only one era in this mode, era is always 0
                } else {
                    CE_LIMITS[field][limit as usize]
                }
            }
            Self::Persian => PERSIAN_LIMITS[field][limit as usize],
        }
    }

    /// Whether the calendar is a `GregorianCalendar` (its `getActualMaximum(YEAR)` is a
    /// binary search over the year, and `getActualMinimum` is `getMinimum`).
    pub fn is_gregorian_family(&self) -> bool {
        self.gregorian().is_some()
    }

    /// Whether `getActualMinimum` is `getMinimum` (`GregorianCalendar`'s override).
    pub fn actual_minimum_is_minimum(&self) -> bool {
        self.gregorian().is_some()
    }

    /// `getFieldResolutionTable`.
    pub fn field_resolution_table(&self) -> &'static [&'static [&'static [i32]]] {
        Calendar::date_precedence()
    }
}

/// `JapaneseCalendar::handleGetLimit`.
fn japanese_get_limit(field: usize, limit: Limit) -> i32 {
    match field {
        F::ERA => {
            if matches!(limit, Limit::Minimum | Limit::GreatestMinimum) {
                0
            } else {
                JAPANESE_ERA_STARTS.len() as i32 - 1 // max known era, not gCurrentEra
            }
        }
        F::YEAR => match limit {
            Limit::Minimum | Limit::GreatestMinimum | Limit::LeastMaximum => 1,
            Limit::Maximum => {
                GREGORIAN_LIMITS[F::YEAR][Limit::Maximum as usize] - era_start_year(current_japanese_era())
            }
        },
        _ => GREGORIAN_LIMITS[field][limit as usize],
    }
}

// ------------------------------------------------------------------ EraRules (Japanese)

/// `EraRules::getStartYear`.
fn era_start_year(era: i32) -> i32 {
    usize::try_from(era)
        .ok()
        .and_then(|era| JAPANESE_ERA_STARTS.get(era))
        .map_or(i32::MAX, |start| start.0)
}

/// `EraRules::getStartDate`.
fn era_start_date(era: i32) -> Option<(i32, i32, i32)> {
    usize::try_from(era)
        .ok()
        .and_then(|era| JAPANESE_ERA_STARTS.get(era))
        .copied()
}

/// `EraRules::getEraIndex`: the era a (Julian before 1582) date falls in.
fn era_index(year: i32, month: i32, day: i32) -> i32 {
    let num_eras = JAPANESE_ERA_STARTS.len() as i32;
    let current = current_japanese_era();
    let starts_by = |era: i32| JAPANESE_ERA_STARTS[era as usize] <= (year, month, day);
    let mut high = num_eras; // last index + 1
    let mut low = if starts_by(current) { current } else { 0 };
    while low < high - 1 {
        let i = (low + high) / 2;
        if starts_by(i) {
            low = i;
        } else {
            high = i;
        }
    }
    low
}

/// `EraRules::getCurrentEraIndex`: the era of today (the latest era: Reiwa).
fn current_japanese_era() -> i32 {
    JAPANESE_ERA_STARTS.len() as i32 - 1
}

// ------------------------------------------------------------------ Hebrew helpers

/// Hebrew months (`HebrewCalendar::EMonths`).
const HESHVAN: i32 = 1;
const KISLEV: i32 = 2;
const ADAR_1: i32 = 5;
const ELUL: i32 = 12;

/// `MONTH_LENGTH`: deficient, normal and complete years.
const HEBREW_MONTH_LENGTH: [[i32; 3]; 13] = [
    [30, 30, 30], // Tishri
    [29, 29, 30], // Heshvan
    [29, 30, 30], // Kislev
    [29, 29, 29], // Tevet
    [30, 30, 30], // Shevat
    [30, 30, 30], // Adar I (leap years only)
    [29, 29, 29], // Adar
    [30, 30, 30], // Nisan
    [29, 29, 29], // Iyar
    [30, 30, 30], // Sivan
    [29, 29, 29], // Tammuz
    [30, 30, 30], // Av
    [29, 29, 29], // Elul
];

/// `MONTH_START`: the cumulative days to the end of each month in a non-leap year.
const HEBREW_MONTH_START: [[i32; 3]; 14] = [
    [0, 0, 0],       // (placeholder)
    [30, 30, 30],    // Tishri
    [59, 59, 60],    // Heshvan
    [88, 89, 90],    // Kislev
    [117, 118, 119], // Tevet
    [147, 148, 149], // Shevat
    [147, 148, 149], // (Adar I placeholder)
    [176, 177, 178], // Adar
    [206, 207, 208], // Nisan
    [235, 236, 237], // Iyar
    [265, 266, 267], // Sivan
    [294, 295, 296], // Tammuz
    [324, 325, 326], // Av
    [353, 354, 355], // Elul
];

/// `LEAP_MONTH_START`: the cumulative days to the end of each month in a leap year.
const HEBREW_LEAP_MONTH_START: [[i32; 3]; 14] = [
    [0, 0, 0],       // (placeholder)
    [30, 30, 30],    // Tishri
    [59, 59, 60],    // Heshvan
    [88, 89, 90],    // Kislev
    [117, 118, 119], // Tevet
    [147, 148, 149], // Shevat
    [177, 178, 179], // Adar I
    [206, 207, 208], // Adar II
    [236, 237, 238], // Nisan
    [265, 266, 267], // Iyar
    [295, 296, 297], // Sivan
    [324, 325, 326], // Tammuz
    [354, 355, 356], // Av
    [383, 384, 385], // Elul
];

/// Parts (halakim) of an hour: 1/1080 of an hour.
const HOUR_PARTS: i32 = 1080;
const DAY_PARTS: i32 = 24 * HOUR_PARTS;
/// An approximate value for the length of a lunar month.
const MONTH_FRACT: i32 = 12 * HOUR_PARTS + 793;
const MONTH_PARTS: i32 = 29 * DAY_PARTS + MONTH_FRACT;
/// The time of the new moon (in parts) on 1 Tishri, year 1 (the epoch) counting from noon
/// on the day before (BAHARAD).
const BAHARAD: i32 = 11 * HOUR_PARTS + 204;

/// `HebrewCalendar::isLeapYear`.
fn hebrew_is_leap_year(year: i32) -> bool {
    let x = (year.wrapping_mul(12).wrapping_add(17)) % 19;
    x >= if x < 0 { -7 } else { 12 }
}

/// `HebrewCalendar::monthsInYear`.
fn hebrew_months_in_year(year: i32) -> i32 {
    if hebrew_is_leap_year(year) { 13 } else { 12 }
}

/// `HebrewCalendar::startOfYear`: the day number of 1 Tishri of `year`.
fn hebrew_start_of_year(year: i32) -> i32 {
    let months = (235_i32.wrapping_mul(year).wrapping_sub(234)) / 19; // # of months before year

    let mut frac = i64::from(months) * i64::from(MONTH_FRACT) + i64::from(BAHARAD); // Fractional part of day #
    let mut day = months.wrapping_mul(29).wrapping_add((frac / i64::from(DAY_PARTS)) as i32); // Whole # part of calculation
    frac %= i64::from(DAY_PARTS); // Time of day

    let mut wd = day % 7; // Day of week (0 == Monday)

    if wd == 2 || wd == 4 || wd == 6 {
        // If the 1st is on Sun, Wed, or Fri, postpone to the next day
        day += 1;
        wd = day % 7;
    }
    if wd == 1 && frac > i64::from(15 * HOUR_PARTS + 204) && !hebrew_is_leap_year(year) {
        // If the new moon falls after 3:11:20am (15h204p from the previous noon) on a
        // Tuesday and it is not a leap year, postpone by 2 days.
        day += 2;
    } else if wd == 0 && frac > i64::from(21 * HOUR_PARTS + 589) && hebrew_is_leap_year(year - 1) {
        // If the new moon falls after 9:32:43 1/3am (21h589p from yesterday noon) on a
        // Monday and *last* year was a leap year, postpone by 1 day.
        day += 1;
    }
    day
}

/// `HebrewCalendar::handleGetYearLength`.
fn hebrew_year_length(eyear: i32) -> i32 {
    hebrew_start_of_year(eyear + 1) - hebrew_start_of_year(eyear)
}

/// `HebrewCalendar::yearType`: 0 deficient, 1 normal, 2 complete.
fn hebrew_year_type(year: i32) -> usize {
    let mut year_length = hebrew_year_length(year);
    if year_length > 380 {
        year_length -= 30; // Subtract length of leap month.
    }
    match year_length {
        353 => 0,
        355 => 2,
        _ => 1,
    }
}

/// `HebrewCalendar::handleGetMonthLength`.
fn hebrew_month_length(extended_year: i32, month: i32) -> i32 {
    let mut extended_year = extended_year;
    let mut month = month;
    // Resolve out-of-range months; allow 0..12 in all years.
    while month < 0 {
        extended_year -= 1;
        month += hebrew_months_in_year(extended_year);
    }
    while month > 12 {
        month -= hebrew_months_in_year(extended_year);
        extended_year += 1;
    }
    match month {
        // These two month lengths can vary
        HESHVAN | KISLEV => HEBREW_MONTH_LENGTH[month as usize][hebrew_year_type(extended_year)],
        // The rest are a fixed length
        _ => HEBREW_MONTH_LENGTH[month as usize][0],
    }
}

/// `HebrewCalendar::handleComputeMonthStart`.
fn hebrew_month_start(eyear: i32, month: i32) -> i32 {
    let mut eyear = eyear;
    let mut month = month;
    while month < 0 {
        eyear -= 1;
        month += hebrew_months_in_year(eyear);
    }
    while month > 12 {
        month -= hebrew_months_in_year(eyear);
        eyear += 1;
    }
    let mut day = hebrew_start_of_year(eyear);
    if month != 0 {
        let table = if hebrew_is_leap_year(eyear) {
            &HEBREW_LEAP_MONTH_START
        } else {
            &HEBREW_MONTH_START
        };
        day += table[month as usize][hebrew_year_type(eyear)];
    }
    day.wrapping_add(347_997)
}

// ------------------------------------------------------------------ Indian helpers

const INDIAN_ERA_START: i32 = 78;
const INDIAN_YEAR_START: i32 = 80;

/// `gregorianToJD`.
fn gregorian_to_jd(year: i32, month: i32, date: i32) -> f64 {
    grego::fields_to_day(year, month, date) + f64::from(EPOCH_START_AS_JULIAN_DAY) - 0.5
}

/// `IndianToJD`.
fn indian_to_jd(year: i32, month: i32, date: i32) -> f64 {
    let gyear = year + INDIAN_ERA_START;
    let (leap_month, start) = if grego::is_leap_year(gyear) {
        (31, gregorian_to_jd(gyear, 2, 21))
    } else {
        (30, gregorian_to_jd(gyear, 2, 22))
    };
    if month == 1 {
        start + f64::from(date - 1)
    } else {
        let mut jd = start + f64::from(leap_month);
        let m = (month - 2).min(5);
        jd += f64::from(m * 31);
        if month >= 8 {
            let m = month - 7;
            jd += f64::from(m * 30);
        }
        jd + f64::from(date - 1)
    }
}

// ------------------------------------------------------------------ Islamic helpers

/// `CIVIL_EPOC`: CE 622 July 16 Friday (Julian calendar).
const CIVIL_EPOC: i32 = 1_948_440;
/// `ASTRONOMICAL_EPOC`: CE 622 July 15 Thursday (Julian calendar).
const ASTRONOMICAL_EPOC: i32 = 1_948_439;
const UMALQURA_YEAR_START: i32 = 1300;
const UMALQURA_YEAR_END: i32 = 1600;

/// `UMALQURA_MONTHLENGTH`: one bit per month (30 days if set) for 1300..=1600 AH.
#[rustfmt::skip]
const UMALQURA_MONTHLENGTH: [i32; 301] = [
    0x0AAA, 0x0D54, 0x0EC9, 0x06D4, 0x06EA, 0x036C, 0x0AAD, 0x0555, 0x06A9, 0x0792,
    0x0BA9, 0x05D4, 0x0ADA, 0x055C, 0x0D2D, 0x0695, 0x074A, 0x0B54, 0x0B6A, 0x05AD,
    0x04AE, 0x0A4F, 0x0517, 0x068B, 0x06A5, 0x0AD5, 0x02D6, 0x095B, 0x049D, 0x0A4D,
    0x0D26, 0x0D95, 0x05AC, 0x09B6, 0x02BA, 0x0A5B, 0x052B, 0x0A95, 0x06CA, 0x0AE9,
    0x02F4, 0x0976, 0x02B6, 0x0956, 0x0ACA, 0x0BA4, 0x0BD2, 0x05D9, 0x02DC, 0x096D,
    0x054D, 0x0AA5, 0x0B52, 0x0BA5, 0x05B4, 0x09B6, 0x0557, 0x0297, 0x054B, 0x06A3,
    0x0752, 0x0B65, 0x056A, 0x0AAB, 0x052B, 0x0C95, 0x0D4A, 0x0DA5, 0x05CA, 0x0AD6,
    0x0957, 0x04AB, 0x094B, 0x0AA5, 0x0B52, 0x0B6A, 0x0575, 0x0276, 0x08B7, 0x045B,
    0x0555, 0x05A9, 0x05B4, 0x09DA, 0x04DD, 0x026E, 0x0936, 0x0AAA, 0x0D54, 0x0DB2,
    0x05D5, 0x02DA, 0x095B, 0x04AB, 0x0A55, 0x0B49, 0x0B64, 0x0B71, 0x05B4, 0x0AB5,
    0x0A55, 0x0D25, 0x0E92, 0x0EC9, 0x06D4, 0x0AE9, 0x096B, 0x04AB, 0x0A93, 0x0D49,
    0x0DA4, 0x0DB2, 0x0AB9, 0x04BA, 0x0A5B, 0x052B, 0x0A95, 0x0B2A, 0x0B55, 0x055C,
    0x04BD, 0x023D, 0x091D, 0x0A95, 0x0B4A, 0x0B5A, 0x056D, 0x02B6, 0x093B, 0x049B,
    0x0655, 0x06A9, 0x0754, 0x0B6A, 0x056C, 0x0AAD, 0x0555, 0x0B29, 0x0B92, 0x0BA9,
    0x05D4, 0x0ADA, 0x055A, 0x0AAB, 0x0595, 0x0749, 0x0764, 0x0BAA, 0x05B5, 0x02B6,
    0x0A56, 0x0E4D, 0x0B25, 0x0B52, 0x0B6A, 0x05AD, 0x02AE, 0x092F, 0x0497, 0x064B,
    0x06A5, 0x06AC, 0x0AD6, 0x055D, 0x049D, 0x0A4D, 0x0D16, 0x0D95, 0x05AA, 0x05B5,
    0x02DA, 0x095B, 0x04AD, 0x0595, 0x06CA, 0x06E4, 0x0AEA, 0x04F5, 0x02B6, 0x0956,
    0x0AAA, 0x0B54, 0x0BD2, 0x05D9, 0x02EA, 0x096D, 0x04AD, 0x0A95, 0x0B4A, 0x0BA5,
    0x05B2, 0x09B5, 0x04D6, 0x0A97, 0x0547, 0x0693, 0x0749, 0x0B55, 0x056A, 0x0A6B,
    0x052B, 0x0A8B, 0x0D46, 0x0DA3, 0x05CA, 0x0AD6, 0x04DB, 0x026B, 0x094B, 0x0AA5,
    0x0B52, 0x0B69, 0x0575, 0x0176, 0x08B7, 0x025B, 0x052B, 0x0565, 0x05B4, 0x09DA,
    0x04ED, 0x016D, 0x08B6, 0x0AA6, 0x0D52, 0x0DA9, 0x05D4, 0x0ADA, 0x095B, 0x04AB,
    0x0653, 0x0729, 0x0762, 0x0BA9, 0x05B2, 0x0AB5, 0x0555, 0x0B25, 0x0D92, 0x0EC9,
    0x06D2, 0x0AE9, 0x056B, 0x04AB, 0x0A55, 0x0D29, 0x0D54, 0x0DAA, 0x09B5, 0x04BA,
    0x0A3B, 0x049B, 0x0A4D, 0x0AAA, 0x0AD5, 0x02DA, 0x095D, 0x045E, 0x0A2E, 0x0C9A,
    0x0D55, 0x06B2, 0x06B9, 0x04BA, 0x0A5D, 0x052D, 0x0A95, 0x0B52, 0x0BA8, 0x0BB4,
    0x05B9, 0x02DA, 0x095A, 0x0B4A, 0x0DA4, 0x0ED1, 0x06E8, 0x0B6A, 0x056D, 0x0535,
    0x0695, 0x0D4A, 0x0DA8, 0x0DD4, 0x06DA, 0x055B, 0x029D, 0x062B, 0x0B15, 0x0B4A,
    0x0B95, 0x05AA, 0x0AAE, 0x092E, 0x0C8F, 0x0527, 0x0695, 0x06AA, 0x0AD6, 0x055D,
    0x029D,
];

/// `umAlQuraYrStartEstimateFix`.
#[rustfmt::skip]
const UM_AL_QURA_YR_START_ESTIMATE_FIX: [i32; 301] = [
     0,  0, -1,  0, -1,  0,  0,  0,  0,  0, // 1300..
    -1,  0,  0,  0,  0,  0,  0,  0, -1,  0, // 1310..
     1,  0,  1,  1,  0,  0,  0,  0,  1,  0, // 1320..
     0,  0,  0,  0,  0,  0,  1,  0,  0,  0, // 1330..
     0,  0,  1,  0,  0, -1, -1,  0,  0,  0, // 1340..
     1,  0,  0, -1,  0,  0,  0,  1,  1,  0, // 1350..
     0,  0,  0,  0,  0,  0,  0, -1,  0,  0, // 1360..
     0,  1,  1,  0,  0, -1,  0,  1,  0,  1, // 1370..
     1,  0,  0, -1,  0,  1,  0,  0,  0, -1, // 1380..
     0,  1,  0,  1,  0,  0,  0, -1,  0,  0, // 1390..
     0,  0, -1, -1,  0, -1,  0,  1,  0,  0, // 1400..
     0, -1,  0,  0,  0,  1,  0,  0,  0,  0, // 1410..
     0,  1,  0,  0, -1, -1,  0,  0,  0,  1, // 1420..
     0,  0, -1, -1,  0, -1,  0,  0, -1, -1, // 1430..
     0, -1,  0, -1,  0,  0, -1, -1,  0,  0, // 1440..
     0,  0,  0,  0, -1,  0,  1,  0,  1,  1, // 1450..
     0,  0, -1,  0,  1,  0,  0,  0,  0,  0, // 1460..
     1,  0,  1,  0,  0,  0, -1,  0,  1,  0, // 1470..
     0, -1, -1,  0,  0,  0,  1,  0,  0,  0, // 1480..
     0,  0,  0,  0,  1,  0,  0,  0,  0,  0, // 1490..
     1,  0,  0, -1,  0,  0,  0,  1,  1,  0, // 1500..
     0, -1,  0,  1,  0,  1,  1,  0,  0,  0, // 1510..
     0,  1,  0,  0,  0, -1,  0,  0,  0,  1, // 1520..
     0,  0,  0, -1,  0,  0,  0,  0,  0, -1, // 1530..
     0, -1,  0,  1,  0,  0,  0, -1,  0,  1, // 1540..
     0,  1,  0,  0,  0,  0,  0,  1,  0,  0, // 1550..
    -1,  0,  0,  0,  0,  1,  0,  0,  0, -1, // 1560..
     0,  0,  0,  0, -1, -1,  0, -1,  0,  1, // 1570..
     0,  0, -1, -1,  0,  0,  1,  1,  0,  0, // 1580..
    -1,  0,  0,  0,  0,  1,  0,  0,  0,  0, // 1590..
     1, // 1600
];

/// `getUmalqura_MonthLength`.
fn umalqura_month_length(y: i32, m: i32) -> i32 {
    let mask = 0x01 << (11 - m); // set mask for bit corresponding to month
    if UMALQURA_MONTHLENGTH[y as usize] & mask == 0 { 29 } else { 30 }
}

/// `IslamicCalendar::civilLeapYear`.
fn civil_leap_year(year: i32) -> bool {
    (14 + 11 * year) % 30 < 11
}

/// Whether `year` uses the civil arithmetic.
fn islamic_is_arithmetic(kind: IslamicType, year: i32) -> bool {
    kind != IslamicType::Umalqura || !(UMALQURA_YEAR_START..=UMALQURA_YEAR_END).contains(&year)
}

/// `IslamicCalendar::yearStart`.
fn islamic_year_start(kind: IslamicType, year: i32) -> i32 {
    if islamic_is_arithmetic(kind, year) {
        (year - 1)
            .wrapping_mul(354)
            .wrapping_add(grego::floor_divide_i64(3 + 11 * i64::from(year), 30) as i32)
    } else {
        let year = year - UMALQURA_YEAR_START;
        let yr_start_linear_estimate = (354.367_20 * f64::from(year) + 460_322.05 + 0.5) as i32;
        yr_start_linear_estimate + UM_AL_QURA_YR_START_ESTIMATE_FIX[year as usize]
    }
}

/// `IslamicCalendar::handleGetMonthLength`.
fn islamic_month_length(kind: IslamicType, extended_year: i32, month: i32) -> i32 {
    if islamic_is_arithmetic(kind, extended_year) {
        let mut length = 29 + (month + 1) % 2;
        if month == 11 && civil_leap_year(extended_year) {
            length += 1;
        }
        length
    } else {
        umalqura_month_length(extended_year - UMALQURA_YEAR_START, month)
    }
}

/// `IslamicCalendar::handleGetYearLength`.
fn islamic_year_length(kind: IslamicType, extended_year: i32) -> i32 {
    if islamic_is_arithmetic(kind, extended_year) {
        354 + i32::from(civil_leap_year(extended_year))
    } else {
        (0..12)
            .map(|month| islamic_month_length(kind, extended_year, month))
            .sum()
    }
}

/// `IslamicCalendar::monthStart`.
fn islamic_month_start(kind: IslamicType, year: i32, month: i32) -> i32 {
    if kind == IslamicType::Civil || kind == IslamicType::Tbla {
        ((29.5 * f64::from(month)).ceil() as i32)
            .wrapping_add((year - 1).wrapping_mul(354))
            .wrapping_add(grego::floor_divide_i64(3 + 11 * i64::from(year), 30) as i32)
    } else {
        let mut ms = islamic_year_start(kind, year);
        for i in 0..month {
            ms += islamic_month_length(kind, year, i);
        }
        ms
    }
}

// ------------------------------------------------------------------ CE helpers

const COPTIC_JD_EPOCH_OFFSET: i32 = 1_824_665;
const JD_EPOCH_OFFSET_AMETE_MIHRET: i32 = 1_723_856;
/// 5501 - 1 (Amete Alem 5501 = Amete Mihret 1).
const AMETE_MIHRET_DELTA: i32 = 5500;
/// `CopticCalendar::BCE` / `CE` and `EthiopicCalendar::AMETE_ALEM` / `AMETE_MIHRET`.
const BCE: i32 = 0;
const CE: i32 = 1;
const AMETE_ALEM: i32 = 0;
const AMETE_MIHRET: i32 = 1;

/// `CECalendar::ceToJD`.
fn ce_to_jd(year: i32, month: i32, date: i32, jd_epoch_offset: i32) -> i32 {
    let mut year = year;
    let mut month = month;
    if month >= 0 {
        year += month / 13;
        month %= 13;
    } else {
        month += 1;
        year += month / 13 - 1;
        month = month % 13 + 12;
    }
    jd_epoch_offset
        .wrapping_add(365_i32.wrapping_mul(year))
        .wrapping_add(grego::floor_divide_i32(year, 4))
        .wrapping_add(30 * month)
        .wrapping_add(date - 1)
}

/// `CECalendar::jdToCE`: `(year, month, day)`.
fn jd_to_ce(julian_day: i32, jd_epoch_offset: i32) -> (i32, i32, i32) {
    let (c4, r4) = grego::floor_divide_rem_i32(f64::from(julian_day) - f64::from(jd_epoch_offset), 1461);
    let year = 4 * c4 + (r4 / 365 - r4 / 1460);
    let doy = if r4 == 1460 { 365 } else { r4 % 365 };
    (year, doy / 30, (doy % 30) + 1)
}

/// `PERSIAN_EPOCH`.
const PERSIAN_EPOCH: i32 = 1_948_320;
/// `kPersianNumDays`.
const PERSIAN_NUM_DAYS: [i32; 12] = [0, 31, 62, 93, 124, 155, 186, 216, 246, 276, 306, 336];
/// `kPersianMonthLength`.
const PERSIAN_MONTH_LENGTH: [i32; 12] = [31, 31, 31, 31, 31, 31, 30, 30, 30, 30, 30, 29];
/// `kPersianLeapMonthLength`.
const PERSIAN_LEAP_MONTH_LENGTH: [i32; 12] = [31, 31, 31, 31, 31, 31, 30, 30, 30, 30, 30, 30];

/// `PersianCalendar::isLeapYear`.
fn persian_is_leap_year(year: i32) -> bool {
    let (_, remainder) = grego::floor_divide_rem_i32(f64::from(25 * year + 11), 33);
    remainder < 8
}

/// `PersianCalendar::handleComputeMonthStart`.
fn persian_month_start(eyear: i32, month: i32) -> i32 {
    let mut eyear = eyear;
    let mut month = month;
    if !(0..=11).contains(&month) {
        let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
        eyear += quotient;
        month = remainder;
    }
    let mut julian_day = PERSIAN_EPOCH - 1 + 365 * (eyear - 1) + grego::floor_divide_i32(8 * eyear + 21, 33);
    if month != 0 {
        julian_day += PERSIAN_NUM_DAYS[month as usize];
    }
    julian_day
}

impl Calendar {
    fn gregorian_state(&self) -> &GregorianState {
        match &self.system {
            CalendarSystem::Gregorian(state)
            | CalendarSystem::Japanese(state)
            | CalendarSystem::Buddhist(state)
            | CalendarSystem::Taiwan(state) => state,
            _ => unreachable!("not a GregorianCalendar"),
        }
    }

    fn gregorian_state_mut(&mut self) -> &mut GregorianState {
        match &mut self.system {
            CalendarSystem::Gregorian(state)
            | CalendarSystem::Japanese(state)
            | CalendarSystem::Buddhist(state)
            | CalendarSystem::Taiwan(state) => state,
            _ => unreachable!("not a GregorianCalendar"),
        }
    }

    /// `handleComputeFields`.
    pub(super) fn handle_compute_fields(&mut self, julian_day: i32, status: &mut Status) {
        match self.system.clone() {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_compute_fields(julian_day, status),
            CalendarSystem::Japanese(_) => {
                self.gregorian_handle_compute_fields(julian_day, status);
                let year = self.internal_get(F::EXTENDED_YEAR); // Gregorian year
                let era = era_index(
                    year,
                    self.internal_get(F::MONTH) + 1,
                    self.internal_get(F::DAY_OF_MONTH),
                );
                self.internal_set(F::ERA, era);
                self.internal_set(F::YEAR, year.wrapping_sub(era_start_year(era)).wrapping_add(1));
            }
            CalendarSystem::Buddhist(_) => {
                self.gregorian_handle_compute_fields(julian_day, status);
                let y = self.internal_get(F::EXTENDED_YEAR) - BUDDHIST_ERA_START;
                self.internal_set(F::ERA, 0);
                self.internal_set(F::YEAR, y);
            }
            CalendarSystem::Taiwan(_) => {
                self.gregorian_handle_compute_fields(julian_day, status);
                let y = self.internal_get(F::EXTENDED_YEAR) - TAIWAN_ERA_START;
                if y > 0 {
                    self.internal_set(F::ERA, MINGUO);
                    self.internal_set(F::YEAR, y);
                } else {
                    self.internal_set(F::ERA, BEFORE_MINGUO);
                    self.internal_set(F::YEAR, 1 - y);
                }
            }
            CalendarSystem::Hebrew => self.hebrew_handle_compute_fields(julian_day, status),
            CalendarSystem::Indian => self.indian_handle_compute_fields(julian_day),
            CalendarSystem::Islamic(kind) => self.islamic_handle_compute_fields(kind, julian_day),
            CalendarSystem::Coptic => {
                let (eyear, month, day) = jd_to_ce(julian_day, COPTIC_JD_EPOCH_OFFSET);
                let (era, year) = if eyear <= 0 { (BCE, 1 - eyear) } else { (CE, eyear) };
                self.set_ce_fields(eyear, era, year, month, day);
            }
            CalendarSystem::Ethiopic { amete_alem } => {
                let (eyear, month, day) = jd_to_ce(julian_day, JD_EPOCH_OFFSET_AMETE_MIHRET);
                let (era, year) = if amete_alem || eyear <= 0 {
                    (AMETE_ALEM, eyear + AMETE_MIHRET_DELTA)
                } else {
                    (AMETE_MIHRET, eyear)
                };
                self.set_ce_fields(eyear, era, year, month, day);
            }
            CalendarSystem::Persian => self.persian_handle_compute_fields(julian_day),
        }
    }

    /// The fields `CopticCalendar` / `EthiopicCalendar::handleComputeFields` set.
    fn set_ce_fields(&mut self, eyear: i32, era: i32, year: i32, month: i32, day: i32) {
        self.internal_set(F::EXTENDED_YEAR, eyear);
        self.internal_set(F::ERA, era);
        self.internal_set(F::YEAR, year);
        self.internal_set(F::MONTH, month);
        self.internal_set(F::DATE, day);
        self.internal_set(F::DAY_OF_YEAR, 30 * month + day);
    }

    /// `handleComputeMonthStart`.
    pub(super) fn handle_compute_month_start(&mut self, eyear: i32, month: i32, use_month: bool) -> i32 {
        match self.system {
            CalendarSystem::Gregorian(_)
            | CalendarSystem::Japanese(_)
            | CalendarSystem::Buddhist(_)
            | CalendarSystem::Taiwan(_) => self.gregorian_handle_compute_month_start(eyear, month, use_month),
            CalendarSystem::Hebrew => hebrew_month_start(eyear, month),
            CalendarSystem::Indian => {
                let mut eyear = eyear;
                let mut month = month;
                if !(0..=11).contains(&month) {
                    let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
                    eyear = eyear.wrapping_add(quotient);
                    month = remainder;
                }
                let imonth = if month == 12 { 1 } else { month + 1 };
                indian_to_jd(eyear, imonth, 1) as i32
            }
            CalendarSystem::Islamic(kind) => {
                let mut eyear = eyear;
                let mut month = month;
                if month > 11 {
                    eyear += month / 12;
                    month %= 12;
                } else if month < 0 {
                    month += 1;
                    eyear += month / 12 - 1;
                    month = (month % 12) + 11;
                }
                let epoch = if kind == IslamicType::Tbla {
                    ASTRONOMICAL_EPOC
                } else {
                    CIVIL_EPOC
                };
                islamic_month_start(kind, eyear, month)
                    .wrapping_add(epoch)
                    .wrapping_sub(1)
            }
            CalendarSystem::Coptic => ce_to_jd(eyear, month, 0, COPTIC_JD_EPOCH_OFFSET),
            CalendarSystem::Ethiopic { .. } => ce_to_jd(eyear, month, 0, JD_EPOCH_OFFSET_AMETE_MIHRET),
            CalendarSystem::Persian => persian_month_start(eyear, month),
        }
    }

    /// `handleGetMonthLength`.
    pub(super) fn handle_get_month_length(&mut self, extended_year: i32, month: i32) -> i32 {
        match self.system {
            CalendarSystem::Gregorian(_)
            | CalendarSystem::Japanese(_)
            | CalendarSystem::Buddhist(_)
            | CalendarSystem::Taiwan(_) => self.gregorian_handle_get_month_length(extended_year, month),
            CalendarSystem::Hebrew => hebrew_month_length(extended_year, month),
            CalendarSystem::Indian => {
                let mut eyear = extended_year;
                let mut month = month;
                if !(0..=11).contains(&month) {
                    let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
                    eyear = eyear.wrapping_add(quotient);
                    month = remainder;
                }
                if grego::is_leap_year(eyear + INDIAN_ERA_START) && month == 0 {
                    31
                } else if (1..=5).contains(&month) {
                    31
                } else {
                    30
                }
            }
            CalendarSystem::Islamic(kind) => islamic_month_length(kind, extended_year, month),
            CalendarSystem::Persian => {
                let mut eyear = extended_year;
                let mut month = month;
                if !(0..=11).contains(&month) {
                    let (quotient, remainder) = grego::floor_divide_rem_i32(f64::from(month), 12);
                    eyear = eyear.wrapping_add(quotient);
                    month = remainder;
                }
                if persian_is_leap_year(eyear) {
                    PERSIAN_LEAP_MONTH_LENGTH[month as usize]
                } else {
                    PERSIAN_MONTH_LENGTH[month as usize]
                }
            }
            CalendarSystem::Coptic | CalendarSystem::Ethiopic { .. } => {
                self.calendar_handle_get_month_length(extended_year, month)
            }
        }
    }

    /// `handleGetYearLength`.
    pub(super) fn handle_get_year_length(&mut self, eyear: i32) -> i32 {
        match self.system {
            CalendarSystem::Gregorian(_)
            | CalendarSystem::Japanese(_)
            | CalendarSystem::Buddhist(_)
            | CalendarSystem::Taiwan(_) => {
                if self.gregorian_is_leap_year(eyear) {
                    366
                } else {
                    365
                }
            }
            CalendarSystem::Hebrew => hebrew_year_length(eyear),
            CalendarSystem::Indian => {
                if grego::is_leap_year(eyear + INDIAN_ERA_START) {
                    366
                } else {
                    365
                }
            }
            CalendarSystem::Islamic(kind) => islamic_year_length(kind, eyear),
            CalendarSystem::Persian => {
                if persian_is_leap_year(eyear) {
                    366
                } else {
                    365
                }
            }
            CalendarSystem::Coptic | CalendarSystem::Ethiopic { .. } => self.calendar_handle_get_year_length(eyear),
        }
    }

    /// `handleGetExtendedYear`.
    pub(super) fn handle_get_extended_year(&mut self) -> i32 {
        match self.system {
            CalendarSystem::Gregorian(_) => self.gregorian_handle_get_extended_year(),
            CalendarSystem::Japanese(_) => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR
                    && self.newer_field(F::EXTENDED_YEAR, F::ERA) == F::EXTENDED_YEAR
                {
                    self.internal_get_or(F::EXTENDED_YEAR, GREGORIAN_EPOCH)
                } else {
                    let era_start = era_start_year(self.internal_get_or(F::ERA, current_japanese_era()));
                    // pin to minimum of year 1 (first year), add gregorian starting year,
                    // subtract one because year starts at 1
                    self.internal_get_or(F::YEAR, 1)
                        .wrapping_add(era_start)
                        .wrapping_sub(1)
                }
            }
            CalendarSystem::Buddhist(_) => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR {
                    self.internal_get_or(F::EXTENDED_YEAR, GREGORIAN_EPOCH)
                } else {
                    self.internal_get_or(F::YEAR, GREGORIAN_EPOCH - BUDDHIST_ERA_START) + BUDDHIST_ERA_START
                }
            }
            CalendarSystem::Taiwan(_) => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR
                    && self.newer_field(F::EXTENDED_YEAR, F::ERA) == F::EXTENDED_YEAR
                {
                    self.internal_get_or(F::EXTENDED_YEAR, GREGORIAN_EPOCH)
                } else {
                    match self.internal_get_or(F::ERA, MINGUO) {
                        MINGUO => self.internal_get_or(F::YEAR, 1) + TAIWAN_ERA_START,
                        BEFORE_MINGUO => 1 - self.internal_get_or(F::YEAR, 1) + TAIWAN_ERA_START,
                        _ => GREGORIAN_EPOCH,
                    }
                }
            }
            CalendarSystem::Hebrew
            | CalendarSystem::Indian
            | CalendarSystem::Islamic(_)
            | CalendarSystem::Persian => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR {
                    self.internal_get_or(F::EXTENDED_YEAR, 1) // Default to year 1
                } else {
                    self.internal_get_or(F::YEAR, 1) // Default to year 1
                }
            }
            CalendarSystem::Coptic => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR {
                    self.internal_get_or(F::EXTENDED_YEAR, 1) // Default to year 1
                } else if self.internal_get_or(F::ERA, CE) == BCE {
                    1 - self.internal_get_or(F::YEAR, 1) // Convert to extended year
                } else {
                    self.internal_get_or(F::YEAR, 1) // Default to year 1
                }
            }
            CalendarSystem::Ethiopic { amete_alem } => {
                if self.newer_field(F::EXTENDED_YEAR, F::YEAR) == F::EXTENDED_YEAR {
                    self.internal_get_or(F::EXTENDED_YEAR, 1) // Default to year 1
                } else if amete_alem {
                    // Default to year 1 of Amelete Mihret
                    self.internal_get_or(F::YEAR, 1 + AMETE_MIHRET_DELTA) - AMETE_MIHRET_DELTA
                } else if self.internal_get_or(F::ERA, AMETE_MIHRET) == AMETE_MIHRET {
                    self.internal_get_or(F::YEAR, 1) // Default to year 1
                } else {
                    self.internal_get_or(F::YEAR, 1) - AMETE_MIHRET_DELTA
                }
            }
        }
    }

    /// `handleComputeJulianDay`.
    pub(super) fn handle_compute_julian_day(&mut self, best_field: usize) -> i32 {
        if self.system.is_gregorian_family() {
            self.gregorian_handle_compute_julian_day(best_field)
        } else {
            self.calendar_handle_compute_julian_day(best_field)
        }
    }

    /// `getDefaultMonthInYear` (`JapaneseCalendar` starts years at the era start).
    pub(super) fn get_default_month_in_year(&self, eyear: i32) -> i32 {
        if matches!(self.system, CalendarSystem::Japanese(_)) {
            let era = self.internal_get_or(F::ERA, current_japanese_era());
            if let Some((year, month, _)) = era_start_date(era)
                && eyear == year
            {
                return month - 1; // return 0-based month
            }
        }
        0
    }

    /// `getDefaultDayInMonth` (`JapaneseCalendar` starts years at the era start).
    pub(super) fn get_default_day_in_month(&self, eyear: i32, month: i32) -> i32 {
        if matches!(self.system, CalendarSystem::Japanese(_)) {
            let era = self.internal_get_or(F::ERA, current_japanese_era());
            if let Some((year, start_month, day)) = era_start_date(era)
                && eyear == year
                && month == start_month - 1
            {
                return day;
            }
        }
        1
    }

    /// The `add` overrides: `HebrewCalendar::add(MONTH)` skips ADAR_1 in common years.
    /// Returns `false` if the calendar does not override `add` for `field`.
    pub(super) fn subclass_add(&mut self, field: usize, amount: i32, status: &mut Status) -> bool {
        if !matches!(self.system, CalendarSystem::Hebrew) || field != F::MONTH {
            return false;
        }
        if status.is_failure() {
            return true;
        }
        // We can't just do a set(MONTH, get(MONTH) + amount).  The reason is ADAR_1.
        let mut month = self.get(F::MONTH, status);
        let mut year = self.get(F::YEAR, status);
        if amount > 0 {
            let mut across_adar1 = month < ADAR_1; // started before ADAR_1?
            month = month.wrapping_add(amount);
            loop {
                if across_adar1 && month >= ADAR_1 && !hebrew_is_leap_year(year) {
                    month += 1;
                }
                if month <= ELUL {
                    break;
                }
                month -= ELUL + 1;
                year += 1;
                across_adar1 = true;
            }
        } else {
            let mut across_adar1 = month > ADAR_1; // started after ADAR_1?
            month = month.wrapping_add(amount);
            loop {
                if across_adar1 && month <= ADAR_1 && !hebrew_is_leap_year(year) {
                    month -= 1;
                }
                if month >= 0 {
                    break;
                }
                month += ELUL + 1;
                year -= 1;
                across_adar1 = true;
            }
        }
        self.set(F::MONTH, month);
        self.set(F::YEAR, year);
        self.pin_field(F::DAY_OF_MONTH, status);
        true
    }

    /// `JapaneseCalendar::getActualMaximum(UCAL_YEAR)`.
    pub(super) fn japanese_actual_maximum_year(&mut self, status: &mut Status) -> i32 {
        let era = self.get(F::ERA, status);
        if status.is_failure() {
            return 0; // error case... any value
        }
        if era == JAPANESE_ERA_STARTS.len() as i32 - 1 {
            // max known era, not gCurrentEra
            return japanese_get_limit(F::YEAR, Limit::Maximum);
        }
        let Some((next_era_year, next_era_month, next_era_date)) = era_start_date(era + 1) else {
            status.fail();
            return 0;
        };
        let era_start = era_start_year(era);
        let mut max_year = next_era_year - era_start + 1; // 1-base
        if next_era_month == 1 && next_era_date == 1 {
            max_year -= 1;
        }
        max_year
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
            let january1 = 365_i32
                .wrapping_mul(eyear - 1)
                .wrapping_add(grego::floor_divide_i32(eyear - 1, 4));
            day_of_year = julian_epoch_day.wrapping_sub(january1); // 0-based

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

    // -------------------------------------------------------------- HebrewCalendar

    /// `HebrewCalendar::handleComputeFields`.
    fn hebrew_handle_compute_fields(&mut self, julian_day: i32, status: &mut Status) {
        let d = julian_day.wrapping_sub(347_997);
        let m = f64::from(d) * f64::from(DAY_PARTS) / f64::from(MONTH_PARTS); // Months (approx)
        let mut year = ((19.0 * m + 234.0) / 235.0 + 1.0) as i32; // Years (approx)
        let mut ys = hebrew_start_of_year(year); // 1st day of year
        let mut day_of_year = d.wrapping_sub(ys);

        // Because of the postponement rules, it's possible to guess wrong.  Fix it.
        while day_of_year < 1 {
            year -= 1;
            ys = hebrew_start_of_year(year);
            day_of_year = d.wrapping_sub(ys);
        }

        // Now figure out which month we're in, and the date within that month
        let year_type = hebrew_year_type(year);
        let is_leap = hebrew_is_leap_year(year);
        let table = if is_leap {
            &HEBREW_LEAP_MONTH_START
        } else {
            &HEBREW_MONTH_START
        };

        let mut month = 0;
        let momax = table.len();
        while month < momax && day_of_year > table[month][year_type] {
            month += 1;
        }
        if month >= momax || month == 0 {
            status.fail();
            return;
        }
        month -= 1;
        let day_of_month = day_of_year - table[month][year_type];

        self.internal_set(F::ERA, 0);
        self.internal_set(F::YEAR, year);
        self.internal_set(F::EXTENDED_YEAR, year);
        self.internal_set(F::MONTH, month as i32);
        self.internal_set(F::DAY_OF_MONTH, day_of_month);
        self.internal_set(F::DAY_OF_YEAR, day_of_year);
    }

    // -------------------------------------------------------------- IndianCalendar

    /// `IndianCalendar::handleComputeFields`.
    fn indian_handle_compute_fields(&mut self, julian_day: i32) {
        // Gregorian date for Julian day
        let gregorian_year =
            grego::day_to_fields(f64::from(julian_day) - f64::from(EPOCH_START_AS_JULIAN_DAY)).year;
        let mut indian_year = gregorian_year - INDIAN_ERA_START; // Year in Saka era
        let jd_at_start_of_greg_year = gregorian_to_jd(gregorian_year, 0, 1); // JD at start of Gregorian year
        let mut yday = (f64::from(julian_day) - jd_at_start_of_greg_year) as i32; // Day number in Gregorian year (starting from 0)

        let leap_month;
        if yday < INDIAN_YEAR_START {
            // Day is at the end of the preceding Saka year
            indian_year -= 1;
            leap_month = if grego::is_leap_year(gregorian_year - 1) { 31 } else { 30 }; // Days in leapMonth this year, previous Gregorian year
            yday += leap_month + (31 * 5) + (30 * 3) + 10;
        } else {
            leap_month = if grego::is_leap_year(gregorian_year) { 31 } else { 30 }; // Days in leapMonth this year
            yday -= INDIAN_YEAR_START;
        }

        let (indian_month, indian_day_of_month);
        if yday < leap_month {
            indian_month = 0;
            indian_day_of_month = yday + 1;
        } else {
            let mut mday = yday - leap_month;
            if mday < 31 * 5 {
                indian_month = mday / 31 + 1;
                indian_day_of_month = (mday % 31) + 1;
            } else {
                mday -= 31 * 5;
                indian_month = mday / 30 + 6;
                indian_day_of_month = (mday % 30) + 1;
            }
        }

        self.internal_set(F::ERA, 0);
        self.internal_set(F::EXTENDED_YEAR, indian_year);
        self.internal_set(F::YEAR, indian_year);
        self.internal_set(F::MONTH, indian_month);
        self.internal_set(F::DAY_OF_MONTH, indian_day_of_month);
        self.internal_set(F::DAY_OF_YEAR, yday + 1); // yday is 0-based
    }

    // -------------------------------------------------------------- IslamicCalendar

    /// `IslamicCalendar::handleComputeFields` for the arithmetic calculations.
    fn islamic_handle_compute_fields(&mut self, kind: IslamicType, julian_day: i32) {
        let mut days = julian_day.wrapping_sub(CIVIL_EPOC);
        let (year, month);
        let civil_fields = |days: i32| {
            let year = grego::floor_divide_i64(30 * i64::from(days) + 10646, 10631) as i32;
            let month = ((f64::from(days - 29 - islamic_year_start(kind, year))) / 29.5).ceil() as i32;
            (year, month.min(11))
        };
        if kind == IslamicType::Civil || kind == IslamicType::Tbla {
            if kind == IslamicType::Tbla {
                days = julian_day.wrapping_sub(ASTRONOMICAL_EPOC);
            }
            (year, month) = civil_fields(days);
        } else {
            let umalqura_start_days = islamic_year_start(kind, UMALQURA_YEAR_START);
            if days < umalqura_start_days {
                (year, month) = civil_fields(days);
            } else {
                let mut y = UMALQURA_YEAR_START - 1;
                let mut m = 0;
                let mut d: i64 = 1;
                while d > 0 {
                    y += 1;
                    d = i64::from(days) - i64::from(islamic_year_start(kind, y)) + 1;
                    let year_length = i64::from(islamic_year_length(kind, y));
                    if d == year_length {
                        m = 11;
                        break;
                    } else if d < year_length {
                        let mut month_len = i64::from(islamic_month_length(kind, y, m));
                        m = 0;
                        while d > month_len {
                            d -= month_len;
                            m += 1;
                            month_len = i64::from(islamic_month_length(kind, y, m));
                        }
                        break;
                    }
                }
                year = y;
                month = m;
            }
        }
        let day_of_month = days.wrapping_sub(islamic_month_start(kind, year, month)) + 1;
        let day_of_year = days.wrapping_sub(islamic_month_start(kind, year, 0)) + 1;

        self.internal_set(F::ERA, 0);
        self.internal_set(F::YEAR, year);
        self.internal_set(F::EXTENDED_YEAR, year);
        self.internal_set(F::MONTH, month);
        self.internal_set(F::DAY_OF_MONTH, day_of_month);
        self.internal_set(F::DAY_OF_YEAR, day_of_year);
    }

    // -------------------------------------------------------------- PersianCalendar

    /// `PersianCalendar::handleComputeFields`.
    fn persian_handle_compute_fields(&mut self, julian_day: i32) {
        let days_since_epoch = julian_day - PERSIAN_EPOCH;
        let year = 1 + grego::floor_divide_i64(33 * i64::from(days_since_epoch) + 3, 12053) as i32;

        let farvardin1 = 365 * (year - 1) + grego::floor_divide_i32(8 * year + 21, 33);
        let mut day_of_year = days_since_epoch - farvardin1; // 0-based
        let month = if day_of_year < 216 {
            // Compute 0-based month
            day_of_year / 31
        } else {
            (day_of_year - 6) / 30
        };
        let day_of_month = day_of_year - PERSIAN_NUM_DAYS[month as usize] + 1;
        day_of_year += 1; // Make it 1-based now

        self.internal_set(F::ERA, 0);
        self.internal_set(F::YEAR, year);
        self.internal_set(F::EXTENDED_YEAR, year);
        self.internal_set(F::MONTH, month);
        self.internal_set(F::DAY_OF_MONTH, day_of_month);
        self.internal_set(F::DAY_OF_YEAR, day_of_year);
    }
}
