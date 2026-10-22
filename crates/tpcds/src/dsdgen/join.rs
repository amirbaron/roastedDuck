//! Foreign keys between the tables (`join.c`, and the joins to slowly changing dimensions
//! of `scd.c`).

use super::columns::{
    WEB_CLOSE_DATE, WEB_OPEN_DATE, WEB_REC_END_DATE_ID, WEB_REC_START_DATE_ID, WP_CREATION_DATE_SK,
    WP_REC_END_DATE_ID, WP_REC_START_DATE_ID, WR_WEB_PAGE_SK, WS_WEB_PAGE_SK,
};
use super::constants::{
    CALENDAR_SALES, CP_CATALOGS_PER_YEAR, CS_MAX_SHIP_DELAY, CS_MIN_SHIP_DELAY, SS_MAX_SHIP_DELAY,
    SS_MIN_SHIP_DELAY, WEB_DATE_STAGGER, WEB_PAGES_PER_SITE, YEAR_MAXIMUM, YEAR_MINIMUM, web_is_replaced,
    web_is_replacement,
};
use super::context::Context;
use super::date::{Date, is_leap};
use super::genrand::DIST_UNIFORM;
use super::tables::{
    CATALOG_PAGE, CATALOG_RETURNS, CATALOG_SALES, DATET, STORE_RETURNS, STORE_SALES, TIME, WEB_PAGE,
    WEB_RETURNS, WEB_SALES, WEB_SITE,
};
use super::tdefs::{FL_SPARSE, FL_TYPE_2, get_simple_tdefs_by_number, get_table_from_column};
use super::w_web_sales::{WS_MAX_SHIP_DELAY, WS_MIN_SHIP_DELAY};

impl Context {
    /// `date_join`: a date of year `year` for a row of `from_table`, or a return date
    /// `join_count` (the sale's date) plus a lag.
    fn date_join(&mut self, from_table: usize, from_col: usize, join_count: i64, year: i32) -> i64 {
        let (min, max) = match from_table {
            STORE_SALES | CATALOG_SALES | WEB_SALES => {
                let (day, _) = self.dists.calendar.pick_int(
                    &mut self.rng,
                    1,
                    CALENDAR_SALES + usize::from(is_leap(year)),
                    from_col,
                );
                return self.date_of_year_day(year, day);
            }
            // Returns are keyed to the sale date, with the lag between sale and return
            // selected within a known range, based on the sales channel.
            STORE_RETURNS => (SS_MIN_SHIP_DELAY, SS_MAX_SHIP_DELAY),
            CATALOG_RETURNS => (CS_MIN_SHIP_DELAY, CS_MAX_SHIP_DELAY),
            WEB_RETURNS => (WS_MIN_SHIP_DELAY, WS_MAX_SHIP_DELAY),
            WEB_SITE | WEB_PAGE => return self.web_join(from_col, join_count),
            _ => {
                let (day, _) =
                    self.dists
                        .calendar
                        .pick_int(&mut self.rng, 1, 1 + usize::from(is_leap(year)), from_col);
                return self.date_of_year_day(year, day);
            }
        };
        let lag = self
            .rng
            .genrand_integer(DIST_UNIFORM, min * 2, max * 2, 0, from_col);
        join_count + i64::from(lag)
    }

    /// The julian date `day` days after the first of January of `year`, or -1 after today.
    fn date_of_year_day(&self, year: i32, day: i32) -> i64 {
        let result = Date::new(year, 1, 1).to_julian() + day;
        if i64::from(result) > self.today {
            -1
        } else {
            i64::from(result)
        }
    }

    /// `time_join`: a time of day, skewed by the channel of the table.
    fn time_join(&mut self, table: usize, column: usize) -> i64 {
        let wset = match table {
            STORE_SALES | STORE_RETURNS => 2,
            CATALOG_SALES | WEB_SALES | CATALOG_RETURNS | WEB_RETURNS => 3,
            _ => 1,
        };
        let (hour, _) = self.dists.hours.pick_int(&mut self.rng, 1, wset, column);
        let secs = self.rng.genrand_integer(DIST_UNIFORM, 0, 3599, 0, column);
        i64::from(hour * 3600 + secs)
    }

    /// `cp_join`: a page of a catalog that is current on `date`.
    fn cp_join(&mut self, column: usize, date: i64) -> i64 {
        let (_, kind) = self.dists.catalog_page_type.pick_str(&mut self.rng, 1, 2, column);
        let page = self
            .rng
            .genrand_integer(DIST_UNIFORM, 1, self.pages_per_catalog, 0, column);
        let mut offset = date as i32 - self.data_start_date - 1;
        let mut count = (offset / 365) * CP_CATALOGS_PER_YEAR;
        offset %= 365;
        match kind {
            // Bi-annual.
            1 if offset > 183 => count += 1,
            // Quarterly.
            2 => count += offset / 91,
            // Monthly.
            3 => count += offset / 31,
            _ => {}
        }
        // `CP_SK(c, s, p)`.
        i64::from(count * self.pages_per_catalog + page)
    }

    /// `web_join`: the dates of a web site or page, which must fit within the lifetime of
    /// its site, or a page for a date.
    fn web_join(&mut self, column: usize, join_key: i64) -> i64 {
        let site_param = i64::from(get_simple_tdefs_by_number(WEB_SITE).param);
        let page_param = i64::from(get_simple_tdefs_by_number(WEB_PAGE).param);
        let duration = self.site_duration;
        let site_open = i64::from(self.date_minimum);
        match column {
            // `join_key` is the `_sk` of a row of the dimension.
            WEB_OPEN_DATE => {
                let mut res = site_open - ((join_key * WEB_DATE_STAGGER) % duration / 2);
                // A replaced site's replacement opens on a revision boundary.
                if web_is_replaced(join_key) && web_is_replacement(join_key) {
                    res += self.site_offset * duration;
                }
                res
            }
            WEB_CLOSE_DATE => {
                let mut res = site_open - ((join_key * WEB_DATE_STAGGER) % duration / 2);
                res += site_param * duration;
                // A replaced site closes on a revision boundary.
                if web_is_replaced(join_key) && !web_is_replacement(join_key) {
                    res -= site_param * duration / 2;
                }
                res
            }
            WEB_REC_START_DATE_ID => {
                let mut res = site_open - (((join_key - 1) * WEB_DATE_STAGGER) % duration / 2);
                res += (join_key % site_param) * duration;
                res
            }
            WEB_REC_END_DATE_ID => {
                let mut res = site_open - ((join_key * WEB_DATE_STAGGER) % duration / 2);
                res += ((join_key + 1) % site_param) * duration * 5 - 1;
                res
            }
            WP_REC_START_DATE_ID => {
                let mut res = site_open - (((join_key - 1) * WEB_DATE_STAGGER) % duration / 2);
                res += (join_key % page_param) * duration * 5;
                res
            }
            WP_REC_END_DATE_ID => {
                let mut res = site_open - ((join_key * WEB_DATE_STAGGER) % duration / 2);
                res += ((join_key + 1) % page_param) * duration - 1;
                res
            }
            WP_CREATION_DATE_SK => {
                // Pages are created in the gap between the creation of their site and its
                // activity, so that the number of pages stays the same.
                let site = (join_key / i64::from(WEB_PAGES_PER_SITE) + 1) as i32;
                let stagger = i64::from(site.wrapping_mul(WEB_DATE_STAGGER as i32));
                let mut res = site_open - stagger % duration / 2;
                if i64::from(site) % page_param == 0 {
                    // A site that gets replaced.
                    res = i64::from(self.rng.genrand_integer(
                        DIST_UNIFORM,
                        res as i32,
                        site_open as i32,
                        0,
                        column,
                    ));
                }
                res
            }
            // `join_key` is a date for which a valid site or page is needed.
            WR_WEB_PAGE_SK | WS_WEB_PAGE_SK => {
                i64::from(
                    self.rng
                        .genrand_integer(DIST_UNIFORM, 1, WEB_PAGES_PER_SITE, 0, column),
                )
            }
            _ => -1,
        }
    }

    /// `mk_join`: a key of `to_table` for a row whose column `from_col` refers to it.
    pub(crate) fn mk_join(&mut self, from_col: usize, to_table: usize, join_count: i64) -> i64 {
        let from_table = get_table_from_column(from_col);
        let tdef = get_simple_tdefs_by_number(to_table);
        // No table of the warehouse uses sparse keys.
        debug_assert!(!tdef.is(FL_SPARSE));
        match to_table {
            // Some tables require special handling.
            CATALOG_PAGE => self.cp_join(from_col, join_count),
            DATET => {
                let year = self
                    .rng
                    .genrand_integer(DIST_UNIFORM, YEAR_MINIMUM, YEAR_MAXIMUM, 0, from_col);
                self.date_join(from_table, from_col, join_count, year)
            }
            TIME => self.time_join(from_table, from_col),
            // History-keeping dimensions need a special join.
            _ if tdef.is(FL_TYPE_2) => self.scd_join(to_table, from_col, join_count),
            // The rest of the tables use standard, uniform joins.
            _ => self
                .rng
                .genrand_key(DIST_UNIFORM, 1, self.get_rowcount(to_table), 0, from_col),
        }
    }

    /// `scd_join`: a key of a slowly changing dimension, mapped to the revision current on
    /// `date`.
    pub(crate) fn scd_join(&mut self, table: usize, column: usize, date: i64) -> i64 {
        let rowcount = self.get_id_count(table);
        // Pick the id, then map it to the date-sensitive surrogate key.
        let id = self.rng.genrand_key(DIST_UNIFORM, 1, rowcount, 0, column);
        let mut res = self.match_scd_sk(id, date, table);
        // No revision can be in the future, per bug 114.
        if date > self.scd_dates.maximum {
            res = -1;
        }
        if res > self.get_rowcount(table) { -1 } else { res }
    }

    /// `matchSCDSK`: the surrogate key of id `unique`'s revision current on `date`.
    pub(crate) fn match_scd_sk(&self, unique: i64, date: i64, table: usize) -> i64 {
        let dates = &self.scd_dates;
        let mut res = match unique % 3 {
            // Only one occurrence of this id.
            1 => (unique / 3) * 6 + 1,
            // Two revisions of this id.
            2 => (unique / 3) * 6 + 2 + i64::from(date > dates.h1),
            // Three revisions of this id.
            0 => (unique / 3) * 6 - 2 + i64::from(date > dates.t1) + i64::from(date > dates.t2),
            _ => -1,
        };
        let rowcount = self.get_rowcount(table);
        if res > rowcount {
            res = rowcount;
        }
        res
    }
}
