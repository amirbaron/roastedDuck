//! Distributions, value selections and date handling (DuckDB's `dbgen/bm_utils.cpp`,
//! with `permute` and `permute_dist` of `dbgen/permute.cpp`).

use std::sync::LazyLock;

use roastedduck_types::Date;

use super::dss::{MAX_LONG, STARTDATE, Seed, TOTDATE};
use super::rnd::dss_random;

/// The characters of random alphanumeric strings (`alpha_num`).
const ALPHA_NUM: &[u8; 64] = b"0123456789abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ,";

/// The distributions file (`dists.dss`, embedded as `dists_dss` in DuckDB's
/// `dbgen/dists_dss.h`).
const DISTS_DSS: &str = include_str!("dists.dss");

/// A weighted set of strings (DuckDB's `distribution`); the weights are cumulative.
#[derive(Debug, Default)]
pub(crate) struct Distribution {
    /// `(cumulative weight, text)` of every member (`set_member`).
    pub(crate) list: Vec<(i64, &'static [u8])>,
}

impl Distribution {
    /// `DIST_SIZE`: the number of members.
    pub(crate) fn count(&self) -> usize {
        self.list.len()
    }

    /// The total weight (the cumulative weight of the last member).
    pub(crate) fn max_weight(&self) -> i64 {
        self.list.last().map_or(0, |&(weight, _)| weight)
    }

    /// `DIST_MEMBER`: the text of member `i`.
    pub(crate) fn member(&self, i: usize) -> &'static [u8] {
        self.list[i].1
    }

    /// `pick_str`: the index of a member chosen by weight with one draw of `seed`.
    pub(crate) fn pick_index(&self, seed: &mut Seed) -> usize {
        let j = dss_random(1, self.max_weight(), seed);
        self.list.iter().position(|&(weight, _)| weight >= j).unwrap_or(0)
    }

    /// `pick_str`: the text of a member chosen by weight.
    pub(crate) fn pick(&self, seed: &mut Seed) -> &'static [u8] {
        self.member(self.pick_index(seed))
    }
}

/// `read_line_into_buffer`: the next line of `src` (with its newline, at most 255
/// bytes) and whether more text follows it. Like DuckDB's `while` loop, the caller does
/// not process the line when nothing follows.
fn read_line(src: &mut &'static str) -> (&'static str, bool) {
    let limit = src.len().min(255);
    let end = src.as_bytes()[..limit]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(limit, |newline| newline + 1);
    let (line, rest) = src.split_at(end);
    *src = rest;
    (line, !rest.is_empty())
}

/// `sscanf(line, "%[^|]|%ld", token, &weight)`: the text before the first `|` (not
/// empty) and the integer after it.
fn scan_member(line: &'static str) -> Option<(&'static str, i64)> {
    let (token, rest) = line.split_once('|')?;
    if token.is_empty() {
        return None;
    }
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits_start = usize::from(rest.starts_with(['+', '-']));
    let digits = rest[digits_start..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    rest[..digits_start + digits]
        .parse()
        .ok()
        .map(|weight| (token, weight))
}

/// `read_dist`: the distribution called `name` in the distributions file.
pub(crate) fn read_dist(name: &str) -> Distribution {
    let mut target = Distribution::default();
    let mut src = DISTS_DSS;
    let mut name_set = false;
    loop {
        let (line, more) = read_line(&mut src);
        if !more {
            break;
        }
        let line = line.split('\n').next().unwrap_or_default();
        let line = line.split('#').next().unwrap_or_default();
        if line.is_empty() {
            continue;
        }
        if !name_set {
            let mut tokens = line.split(['\n', '\t', ' ']).filter(|token| !token.is_empty());
            if tokens
                .next()
                .is_some_and(|token| token.eq_ignore_ascii_case("BEGIN"))
                && tokens
                    .next()
                    .is_some_and(|token| token.eq_ignore_ascii_case(name))
            {
                name_set = true;
            }
            continue;
        }
        if line.len() >= 3 && line[..3].eq_ignore_ascii_case("END") {
            break;
        }
        let Some((token, weight)) = scan_member(line) else {
            continue;
        };
        if token.eq_ignore_ascii_case("count") {
            target.list = Vec::with_capacity(usize::try_from(weight).unwrap_or(0));
            continue;
        }
        let max = target.max_weight() + weight;
        target.list.push((max, token.as_bytes()));
    }
    target
}

/// `tpch_a_rnd`: a random string of the characters in `alpha_num` whose length is
/// drawn from `[min, max]`; one draw yields five characters.
pub(crate) fn tpch_a_rnd(min: i32, max: i32, seed: &mut Seed, dest: &mut Vec<u8>) {
    let len = dss_random(i64::from(min), i64::from(max), seed);
    dest.clear();
    let mut char_int = 0;
    for i in 0..len {
        if i % 5 == 0 {
            char_int = dss_random(0, MAX_LONG, seed);
        }
        dest.push(ALPHA_NUM[(char_int & 0o77) as usize]);
        char_int >>= 6;
    }
}

/// `permute`: shuffles `a` with one draw of `seed` per element.
fn permute(a: &mut [usize], seed: &mut Seed) {
    let count = a.len() as i64;
    for i in 0..a.len() {
        let source = dss_random(i as i64, count - 1, seed) as usize;
        a.swap(source, i);
    }
}

/// `permute_dist`: a random permutation of the member indexes of `d`, in `permute`
/// (`DBGenContext::permute`).
fn permute_dist(d: &Distribution, seed: &mut Seed, permutation: &mut Vec<usize>) {
    permutation.clear();
    permutation.extend(0..d.count());
    permute(permutation, seed);
}

/// `agg_str`: `count` distinct members of `set`, separated by spaces.
pub(crate) fn agg_str(
    set: &Distribution,
    count: usize,
    seed: &mut Seed,
    dest: &mut Vec<u8>,
    permutation: &mut Vec<usize>,
) {
    permute_dist(set, seed, permutation);
    dest.clear();
    for (i, &member) in permutation[..count].iter().enumerate() {
        if i > 0 {
            dest.push(b' ');
        }
        dest.extend_from_slice(set.member(member));
    }
}

/// `LEAP`: whether the two-digit year `y` is a leap year.
pub(crate) const fn leap(y: i64) -> i64 {
    if y % 4 == 0 && y % 100 != 0 { 1 } else { 0 }
}

/// `julian`: the `yyddd` date `date` days (counted from `STARTDATE`) after the start,
/// with the day-of-year overflow carried into the following years.
pub(crate) const fn julian(date: i64) -> i64 {
    let mut offset = date - STARTDATE;
    let mut result = STARTDATE;
    loop {
        let yr = result / 1000;
        let yend = yr * 1000 + 365 + leap(yr);
        if result + offset > yend {
            // Overflow into the next year.
            offset -= yend - result + 1;
            result += 1000;
        } else {
            break;
        }
    }
    result + offset
}

/// The days before each month (`months[].dcnt` of `build.cpp`), January at 1.
const MONTH_DAYS_BEFORE: [i64; 13] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365];

/// `LEAP_ADJ`.
fn leap_adj(yr: i64, month: usize) -> i64 {
    i64::from(leap(yr) == 1 && month >= 2)
}

/// `mk_time`: the date of day `index` (1 for `STARTDATE`) as `(year, month, day)`.
fn mk_time(index: i64) -> (i64, usize, i64) {
    let date = julian(index + STARTDATE - 1);
    let y = date / 1000;
    let d = date % 1000;
    let mut m = 0;
    while d > MONTH_DAYS_BEFORE[m] + leap_adj(y, m) {
        m += 1;
    }
    let day = d - MONTH_DAYS_BEFORE[m - 1] - i64::from(leap(y) == 1 && m > 2);
    (1900 + y, m, day)
}

/// `mk_ascdate`: the dates of the `TOTDATE` days from `STARTDATE`, which DuckDB prints
/// as text and parses back (`Date::FromString`).
pub(crate) fn asc_dates() -> &'static [Date] {
    static DATES: LazyLock<Vec<Date>> = LazyLock::new(|| {
        (0..TOTDATE)
            .map(|i| {
                let (year, month, day) = mk_time(i + 1);
                Date::from_ymd(year as i32, month as i32, day as i32).unwrap_or(Date::EPOCH)
            })
            .collect()
    });
    &DATES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_consecutive_days_from_1992() {
        let dates = asc_dates();
        assert_eq!(dates.len(), TOTDATE as usize);
        assert_eq!(dates[0], Date::from_ymd(1992, 1, 1).unwrap());
        for pair in dates.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1);
        }
        assert_eq!(dates[TOTDATE as usize - 1], Date::from_ymd(1998, 12, 31).unwrap());
    }

    #[test]
    fn julian_carries_into_the_next_year() {
        assert_eq!(julian(92001), 92001);
        assert_eq!(julian(92366), 92366);
        assert_eq!(julian(92367), 93001);
        assert_eq!(julian(92001 + 366 + 365), 94001);
    }

    #[test]
    fn scan_member_follows_sscanf() {
        assert_eq!(scan_member("over|1 "), Some(("over", 1)));
        assert_eq!(scan_member("ETHIOPIA|-4"), Some(("ETHIOPIA", -4)));
        assert_eq!(scan_member("|4"), None);
        assert_eq!(scan_member("x| y"), None);
    }
}
