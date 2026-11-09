//! Calendar arithmetic shared by the date functions (DuckDB's `Date::Extract*` helpers in
//! `date.cpp`).

use roastedduck_types::Date;

/// Day number (days since 1970-01-01) of a proleptic Gregorian date, without range checks
/// (DuckDB's `Date::FromDate`).
pub fn days_from_ymd(year: i32, month: i32, day: i32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The ISO day of the week, Monday = 1 .. Sunday = 7 (DuckDB's
/// `Date::ExtractISODayOfTheWeek`).
pub fn iso_day_of_week(days: i64) -> i64 {
    if days < 0 {
        7 - ((-days + 3) % 7)
    } else {
        ((days + 3) % 7) + 1
    }
}

/// The day of the week, Sunday = 0 .. Saturday = 6.
pub fn day_of_week(days: i64) -> i64 {
    iso_day_of_week(days) % 7
}

/// The day of the year, 1-based.
pub fn day_of_year(date: Date) -> i64 {
    let (year, _, _) = date.to_ymd();
    i64::from(date.0) - days_from_ymd(year, 1, 1) + 1
}

/// The Monday starting ISO week 1 of `year` (DuckDB's `GetISOWeekOne`).
fn iso_week_one(year: i32) -> i64 {
    let first_day = days_from_ymd(year, 1, 1);
    let first_weekday = iso_day_of_week(first_day) - 1;
    let monday = first_day - first_weekday;
    if first_weekday > 3 { monday + 7 } else { monday }
}

/// The ISO year and week number (DuckDB's `GetISOYearWeek`).
pub fn iso_year_week(date: Date) -> (i32, i32) {
    let (mut year, _, _) = date.to_ymd();
    let days = i64::from(date.0);
    let mut week = (days - iso_week_one(year)).div_euclid(7);
    if week < 0 {
        year -= 1;
        week = (days - iso_week_one(year)).div_euclid(7);
    } else if week >= 52 && days >= iso_week_one(year + 1) {
        year += 1;
        week = 0;
    }
    (year, (week + 1) as i32)
}

/// The Julian day number (DuckDB's `Date::ExtractJulianDay`).
pub fn julian_day(date: Date) -> i64 {
    const JULIAN_EPOCH: i64 = -2_440_588;
    i64::from(date.0) - JULIAN_EPOCH
}

/// `Date::MONTH_NAMES`.
pub const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `Date::MONTH_NAMES_ABBREVIATED`.
pub const MONTH_NAMES_ABBREVIATED: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `Date::DAY_NAMES` (Sunday first).
pub const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `Date::DAY_NAMES_ABBREVIATED`.
pub const DAY_NAMES_ABBREVIATED: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_helpers() {
        assert_eq!(days_from_ymd(1970, 1, 1), 0);
        assert_eq!(days_from_ymd(2000, 3, 1), 11_017);
        assert_eq!(days_from_ymd(-1, 12, 31), -719_529);
        assert_eq!(iso_day_of_week(0), 4);
        assert_eq!(iso_day_of_week(-4), 7);
        let date = Date(days_from_ymd(2021, 1, 3) as i32);
        assert_eq!(iso_year_week(date), (2020, 53));
    }
}
