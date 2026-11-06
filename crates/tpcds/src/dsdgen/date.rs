//! Dates as julian day numbers (`date.c`). (`date_t`'s `flags` are never read and are not
//! kept.)

/// `date_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Date {
    pub(crate) year: i32,
    pub(crate) month: i32,
    pub(crate) day: i32,
    pub(crate) julian: i32,
}

/// `m_days`: the days before each month, in a normal and in a leap year.
const M_DAYS: [[i32; 13]; 2] = [
    [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334],
    [0, 0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335],
];

/// `qtr_start`: the first day of each quarter.
const QTR_START: [(i32, i32); 5] = [(0, 0), (1, 1), (4, 1), (7, 1), (10, 1)];

/// `weekday_names`.
pub(crate) const WEEKDAY_NAMES: [&str; 8] = [
    "",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `set_dow`'s doomsdays per century.
const DOOMSDAY: [i32; 4] = [3, 2, 0, 5];
/// `set_dow`'s known days per month (January and February of normal years).
const KNOWN: [i32; 13] = [0, 3, 0, 0, 4, 9, 6, 11, 8, 5, 10, 7, 12];

impl Date {
    /// A date from its parts, with its julian day number (`strtodt` of `year-month-day`).
    pub(crate) fn new(year: i32, month: i32, day: i32) -> Self {
        let mut date = Self {
            year,
            month,
            day,
            julian: 0,
        };
        date.julian = date.to_julian();
        date
    }

    /// `strtodt` of a `%4d-%d-%d` string.
    pub(crate) fn parse(s: &str) -> Self {
        let mut parts = s.splitn(3, '-').map(super::decimal::atoi);
        let year = parts.next().unwrap_or_default();
        let month = parts.next().unwrap_or_default();
        let day = parts.next().unwrap_or_default();
        Self::new(year, month, day)
    }

    /// `jtodt`: the date of julian day `src` (Fliegel and Van Flandern, CACM vol. 11 #10,
    /// 1968).
    pub(crate) fn from_julian(src: i32) -> Self {
        let mut l = i64::from(src) + 68569;
        let n = (4 * l) / 146_097;
        l -= (146_097 * n + 3) / 4;
        let i = (4000 * (l + 1)) / 1_461_001;
        l = l - (1461 * i) / 4 + 31;
        let j = (80 * l) / 2447;
        let day = l - (2447 * j) / 80;
        l = j / 11;
        let month = j + 2 - 12 * l;
        let year = 100 * (n - 49) + i + l;
        Self {
            year: year as i32,
            month: month as i32,
            day: day as i32,
            julian: src,
        }
    }

    /// `dttoj`: the julian day number of the date.
    pub(crate) fn to_julian(self) -> i32 {
        let mut y = self.year;
        let mut m = self.month;
        if m <= 2 {
            m += 12;
            y -= 1;
        }
        // Plus one to make `dttoj` and `jtodt` match, as in the original.
        self.day + (153 * m - 457) / 5 + 365 * y + y / 4 - y / 100 + y / 400 + 1_721_118 + 1
    }

    /// `set_dow`: the day of the week, 0 for Sunday.
    pub(crate) fn day_of_week(self) -> i32 {
        let mut known = KNOWN;
        if is_leap(self.year) {
            // Adjust the known dates for January and February.
            known[1] = 4;
            known[2] = 1;
        }
        // The doomsday of the century, then of the year.
        let mut dday = DOOMSDAY[((self.year / 100 - 15) % 4) as usize];
        let q = self.year % 100;
        let r = q % 12;
        let s = r / 4;
        dday += q / 12 + r + s;
        dday %= 7;

        let mut res = self.day - known[self.month as usize];
        while res < 0 {
            res += 7;
        }
        while res > 6 {
            res -= 7;
        }
        (res + dday) % 7
    }

    /// `day_number`: the row of the `calendar` distribution for the date, which skips the
    /// leap day.
    pub(crate) fn day_number(self) -> i32 {
        M_DAYS[usize::from(is_leap(self.year))][self.month as usize] + self.day
    }

    /// `date_t_op(dest, OP_FIRST_DOM, d1, NULL)`: the first day of the month.
    pub(crate) fn first_dom(self) -> Self {
        Self::from_julian(self.julian - self.day + 1)
    }

    /// `date_t_op(dest, OP_LAST_DOM, d1, NULL)`: the last day of the month (the 28th of a
    /// leap February).
    pub(crate) fn last_dom(self) -> Self {
        Self::from_julian(
            self.julian - self.day + M_DAYS[usize::from(is_leap(self.year))][self.month as usize],
        )
    }

    /// `date_t_op(dest, OP_SAME_LY, d1, NULL)`: the same day last year.
    pub(crate) fn same_day_last_year(self) -> Self {
        if is_leap(self.year) && self.month == 2 && self.day == 29 {
            Self::new(self.year - 1, 2, 28)
        } else {
            Self::new(self.year - 1, self.month, self.day)
        }
    }

    /// `date_t_op(dest, OP_SAME_LQ, d1, NULL)`: the same offset into the prior quarter.
    pub(crate) fn same_day_last_quarter(self) -> Self {
        let quarter = ((self.month - 1) / 3 + 1) as usize;
        let (start_month, start_day) = QTR_START[quarter];
        let mut julian = self.julian - Self::new(self.year, start_month, start_day).julian;
        let (year, prior) = if quarter == 1 {
            (self.year - 1, 4)
        } else {
            (self.year, quarter - 1)
        };
        let (prior_month, prior_day) = QTR_START[prior];
        julian += Self::new(year, prior_month, prior_day).julian;
        Self::from_julian(julian)
    }
}

/// `is_leap`. A century is a leap year when it is even divided by 400 (so 1900 is one),
/// as in the original.
pub(crate) fn is_leap(year: i32) -> bool {
    if year % 100 == 0 {
        (year % 400) % 2 == 0
    } else {
        year % 4 == 0
    }
}
