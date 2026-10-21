//! Port of ICU's `gregoimp.h` / `gregoimp.cpp` (`ClockMath` and `Grego`): proleptic
//! Gregorian day arithmetic shared by the calendars and time zones.

/// `U_MILLIS_PER_DAY`.
pub const MILLIS_PER_DAY: i32 = 86_400_000;
/// `kOneDay`.
pub const ONE_DAY: f64 = 86_400_000.0;
/// `kOneHour`.
pub const ONE_HOUR: i32 = 60 * 60 * 1000;
/// `kOneMinute`.
pub const ONE_MINUTE: i32 = 60_000;
/// `kOneSecond`.
pub const ONE_SECOND: i32 = 1000;
/// `kOneWeek`.
pub const ONE_WEEK: f64 = 7.0 * ONE_DAY;
/// `kJan1_1JulianDay`: January 1, year 1 (Gregorian).
pub const JAN1_1_JULIAN_DAY: i32 = 1_721_426;
/// `kEpochStartAsJulianDay`: January 1, 1970 (Gregorian).
pub const EPOCH_START_AS_JULIAN_DAY: i32 = 2_440_588;
/// `kEpochYear`.
pub const EPOCH_YEAR: i32 = 1970;
/// `MIN_JULIAN`.
pub const MIN_JULIAN: i32 = -0x7F00_0000;
/// `MAX_JULIAN`.
pub const MAX_JULIAN: i32 = 0x7F00_0000;
/// `MIN_MILLIS`.
pub const MIN_MILLIS: f64 = (MIN_JULIAN - EPOCH_START_AS_JULIAN_DAY) as f64 * ONE_DAY;
/// `MAX_MILLIS`.
pub const MAX_MILLIS: f64 = (MAX_JULIAN - EPOCH_START_AS_JULIAN_DAY) as f64 * ONE_DAY;

/// `JULIAN_1_CE`.
const JULIAN_1_CE: i32 = 1_721_426;
/// `JULIAN_1970_CE`.
const JULIAN_1970_CE: i32 = 2_440_588;

/// `Grego::DAYS_BEFORE`.
const DAYS_BEFORE: [i16; 24] = [
    0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305,
    335,
];

/// `Grego::MONTH_LENGTH`.
const MONTH_LENGTH: [i8; 24] = [
    31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
];

/// `ClockMath::floorDivide(int32_t, int32_t)`.
pub const fn floor_divide_i32(numerator: i32, denominator: i32) -> i32 {
    if numerator >= 0 {
        numerator / denominator
    } else {
        ((numerator + 1) / denominator) - 1
    }
}

/// `ClockMath::floorDivide(int64_t, int64_t)`.
pub const fn floor_divide_i64(numerator: i64, denominator: i64) -> i64 {
    if numerator >= 0 {
        numerator / denominator
    } else {
        ((numerator + 1) / denominator) - 1
    }
}

/// `ClockMath::floorDivide(double, double)`.
pub fn floor_divide_f64(numerator: f64, denominator: f64) -> f64 {
    (numerator / denominator).floor()
}

/// `ClockMath::floorDivide(double numerator, int32_t denominator, int32_t &remainder)`.
pub fn floor_divide_rem_i32(numerator: f64, denominator: i32) -> (i32, i32) {
    let quotient = (numerator / f64::from(denominator)).floor();
    let remainder = (numerator - quotient * f64::from(denominator)) as i32;
    (quotient as i32, remainder)
}

/// `ClockMath::floorDivide(double dividend, double divisor, double &remainder)`.
#[allow(clippy::float_cmp)] // ICU compares the quotients exactly
pub fn floor_divide_rem_f64(dividend: f64, divisor: f64) -> (f64, f64) {
    let mut quotient = floor_divide_f64(dividend, divisor);
    let mut remainder = dividend - quotient * divisor;
    if remainder < 0.0 || remainder >= divisor {
        let q = quotient;
        quotient += if remainder < 0.0 { -1.0 } else { 1.0 };
        if q == quotient {
            remainder = 0.0;
        } else {
            remainder = dividend - quotient * divisor;
        }
    }
    (quotient, remainder)
}

/// `Grego::isLeapYear`.
pub const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// `Grego::monthLength`.
pub const fn month_length(year: i32, month: i32) -> i32 {
    MONTH_LENGTH[(month + if is_leap_year(year) { 12 } else { 0 }) as usize] as i32
}

/// `Grego::fieldsToDay`: the epoch day of a (0-based month) Gregorian date.
pub fn fields_to_day(year: i32, month: i32, dom: i32) -> f64 {
    let y = year - 1;
    let julian = f64::from(365 * y + floor_divide_i32(y, 4) + (JULIAN_1_CE - 3))
        + f64::from(floor_divide_i32(y, 400) - floor_divide_i32(y, 100) + 2)
        + f64::from(DAYS_BEFORE[(month + if is_leap_year(year) { 12 } else { 0 }) as usize])
        + f64::from(dom);
    julian - f64::from(JULIAN_1970_CE)
}

/// The fields of a Gregorian day (`Grego::dayToFields` out-parameters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayFields {
    pub year: i32,
    /// 0-based month.
    pub month: i32,
    pub dom: i32,
    pub dow: i32,
    /// 1-based day of year.
    pub doy: i32,
}

/// `Grego::dayToFields`: the Gregorian fields of epoch day `day`.
pub fn day_to_fields(day: f64) -> DayFields {
    // Convert from 1970 CE epoch to 1 CE epoch (Gregorian calendar)
    let day = day + f64::from(JULIAN_1970_CE - JULIAN_1_CE);

    let (n400, doy) = floor_divide_rem_i32(day, 146_097);
    let (n100, doy) = floor_divide_rem_i32(f64::from(doy), 36_524);
    let (n4, doy) = floor_divide_rem_i32(f64::from(doy), 1461);
    let (n1, mut doy) = floor_divide_rem_i32(f64::from(doy), 365);
    let mut year = 400 * n400 + 100 * n100 + 4 * n4 + n1;
    if n100 == 4 || n1 == 4 {
        doy = 365; // Dec 31 at end of 4- or 400-year cycle
    } else {
        year += 1;
    }

    let is_leap = is_leap_year(year);

    // Gregorian day zero is a Monday.
    let mut dow = ((day + 1.0) % 7.0) as i32;
    dow += if dow < 0 { SUNDAY + 7 } else { SUNDAY };

    // Common Julian/Gregorian calculation
    let march1 = if is_leap { 60 } else { 59 }; // zero-based DOY for March 1
    let correction = if doy >= march1 {
        if is_leap { 1 } else { 2 }
    } else {
        0
    };
    let month = (12 * (doy + correction) + 6) / 367; // zero-based month
    let dom = doy - i32::from(DAYS_BEFORE[(month + if is_leap { 12 } else { 0 }) as usize]) + 1; // one-based DOM
    DayFields {
        year,
        month,
        dom,
        dow,
        doy: doy + 1,
    }
}

/// `Grego::julianDayToMillis`.
pub fn julian_day_to_millis(julian: i32) -> f64 {
    f64::from(julian - EPOCH_START_AS_JULIAN_DAY) * ONE_DAY
}

/// `Grego::gregorianShift`.
pub fn gregorian_shift(eyear: i32) -> i32 {
    let y = i64::from(eyear) - 1;
    (floor_divide_i64(y, 400) - floor_divide_i64(y, 100) + 2) as i32
}

/// `UCAL_SUNDAY`.
pub const SUNDAY: i32 = 1;
/// `UCAL_MONDAY`.
pub const MONDAY: i32 = 2;
/// `UCAL_SATURDAY`.
pub const SATURDAY: i32 = 7;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip() {
        for day in [-800_000.0, -1.0, 0.0, 1.0, 10_957.0, 19_000.0, 2_000_000.0] {
            let fields = day_to_fields(day);
            assert_eq!(fields_to_day(fields.year, fields.month, fields.dom), day);
        }
        let epoch = day_to_fields(0.0);
        assert_eq!(
            epoch,
            DayFields {
                year: 1970,
                month: 0,
                dom: 1,
                dow: 5,
                doy: 1
            }
        );
    }
}
