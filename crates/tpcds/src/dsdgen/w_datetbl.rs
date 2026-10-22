//! The `date_dim` table (`w_datetbl.c`).

use std::fmt::Write as _;

use rustdb_common::Result;

use super::Output;
use super::build_support::{bkey_str, mk_bkey};
use super::columns::D_NULLS;
use super::constants::{CURRENT_DAY, CURRENT_MONTH, CURRENT_QUARTER, CURRENT_WEEK, CURRENT_YEAR};
use super::context::Context;
use super::date::{Date, WEEKDAY_NAMES, is_leap};
use super::tables::DATET;

/// The generator of `date_dim` (`mk_w_date` and its statics).
#[derive(Debug)]
pub(crate) struct DateDim {
    base_date: Date,
    /// The `d_current_*` flags of the last row of the current year: they keep their
    /// values in the other years.
    current_month: bool,
    current_quarter: bool,
    current_week: bool,
    quarter_name: String,
}

/// `"Y"` or `"N"`.
const fn flag(value: bool) -> &'static str {
    if value { "Y" } else { "N" }
}

impl DateDim {
    pub(crate) fn new() -> Self {
        Self {
            base_date: Date::parse("1900-01-01"),
            current_month: false,
            current_quarter: false,
            current_week: false,
            quarter_name: String::new(),
        }
    }

    /// `mk_w_date`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(DATET, D_NULLS);
        let julian = (index as i32).wrapping_add(self.base_date.julian);
        let date_sk = i64::from(julian);
        let date_id = mk_bkey(date_sk);
        let date = Date::from_julian(julian);
        let year = date.year;
        let dow = date.day_of_week();
        let moy = date.month;
        let dom = date.day;
        // The sequence counts assume that the table starts on a year boundary.
        let week_seq = (index as i32 + 6) / 7;
        let month_seq = (year - 1900) * 12 + moy - 1;
        let quarter_seq = (year - 1900) * 4 + moy / 3 + 1;
        let day_index = date.day_number();
        let calendar = ctx.dists.calendar;
        let qoy = calendar.member_int(day_index, 6);
        // The fiscal year is the calendar year.
        let fy_year = year;
        let fy_quarter_seq = quarter_seq;
        let fy_week_seq = week_seq;
        let day_name = WEEKDAY_NAMES[(dow + 1) as usize];
        let holiday = calendar.member_int(day_index, 8) != 0;
        let weekend = dow == 5 || dow == 6;
        let following_holiday = if day_index == 1 {
            calendar.member_int(365 + i32::from(is_leap(year - 1)), 8)
        } else {
            calendar.member_int(day_index - 1, 8)
        } != 0;
        let first_dom = date.first_dom().julian;
        let last_dom = date.last_dom().julian;
        let same_day_ly = date.same_day_last_year().julian;
        let same_day_lq = date.same_day_last_quarter().julian;
        let current_day = date_sk == i64::from(CURRENT_DAY);
        let current_year = year == CURRENT_YEAR;
        if current_year {
            self.current_month = moy == CURRENT_MONTH;
            self.current_quarter = qoy == CURRENT_QUARTER;
            self.current_week = week_seq == CURRENT_WEEK;
        }

        self.quarter_name.clear();
        let _ = write!(self.quarter_name, "{year:4}Q{qoy}");
        let row = out.row(DATET).start(&ctx.null_bit_maps);
        row.key(date_sk)?;
        row.varchar(bkey_str(&date_id))?;
        row.date(date_sk)?;
        row.integer(month_seq)?;
        row.integer(week_seq)?;
        row.integer(quarter_seq)?;
        row.integer(year)?;
        row.integer(dow)?;
        row.integer(moy)?;
        row.integer(dom)?;
        row.integer(qoy)?;
        row.integer(fy_year)?;
        row.integer(fy_quarter_seq)?;
        row.integer(fy_week_seq)?;
        row.varchar(day_name)?;
        row.varchar(&self.quarter_name)?;
        row.varchar(flag(holiday))?;
        row.varchar(flag(weekend))?;
        row.varchar(flag(following_holiday))?;
        row.integer(first_dom)?;
        row.integer(last_dom)?;
        row.integer(same_day_ly)?;
        row.integer(same_day_lq)?;
        row.varchar(flag(current_day))?;
        row.varchar(flag(self.current_week))?;
        row.varchar(flag(self.current_month))?;
        row.varchar(flag(self.current_quarter))?;
        row.varchar(flag(current_year))?;
        out.end(DATET)
    }
}
