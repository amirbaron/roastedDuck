//! The state of a generation (`InitializeDSDgen`): the scale, the random number streams,
//! the row counts, the NULL bitmaps of the tables and the constants the original routines
//! compute on their first call (`InitConstants`).
//!
//! The original keeps all of this in globals and statics; DuckDB resets the ones guarded
//! by `InitConstants` before each generation. A [`Context`] per generation is the same.

use roastedduck_common::{Error, Result};

use super::constants::{DATA_END_DATE, DATA_START_DATE, DATE_MINIMUM, TODAYS_DATE};
use super::date::Date;
use super::dist::{Dist, dist};
use super::genrand::{DIST_UNIFORM, MAXINT, Streams};
use super::scaling::base_rowcounts;
use super::tables::{ACTIVE_CITIES, ACTIVE_COUNTIES, CATALOG_PAGE, CONCURRENT_WEB_SITES};
use super::tdefs::{TDEF_COUNT, get_simple_tdefs_by_number, get_table_from_column};

/// The dates `setSCDKeys`, `scd_join` and `matchSCDSK` derive from the data set's range.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScdDates {
    pub(crate) minimum: i64,
    pub(crate) maximum: i64,
    /// The middle of the range.
    pub(crate) h1: i64,
    /// The first and second third of the range.
    pub(crate) t1: i64,
    pub(crate) t2: i64,
}

impl ScdDates {
    fn new() -> Self {
        let minimum = i64::from(Date::parse(DATA_START_DATE).julian);
        let maximum = i64::from(Date::parse(DATA_END_DATE).julian);
        let h1 = minimum + (maximum - minimum) / 2;
        let mut t2 = (maximum - minimum) / 3;
        let t1 = minimum + t2;
        t2 += t1;
        Self {
            minimum,
            maximum,
            h1,
            t1,
            t2,
        }
    }
}

/// The distributions the generators use most, looked up once.
#[derive(Debug)]
pub(crate) struct Dists {
    pub(crate) calendar: &'static Dist,
    pub(crate) hours: &'static Dist,
    pub(crate) catalog_page_type: &'static Dist,
    pub(crate) street_names: &'static Dist,
    pub(crate) street_type: &'static Dist,
    pub(crate) cities: &'static Dist,
    pub(crate) fips_county: &'static Dist,
    pub(crate) first_names: &'static Dist,
    pub(crate) last_names: &'static Dist,
    pub(crate) top_domains: &'static Dist,
    pub(crate) syllables: &'static Dist,
    pub(crate) sentences: &'static Dist,
    pub(crate) nouns: &'static Dist,
    pub(crate) verbs: &'static Dist,
    pub(crate) adjectives: &'static Dist,
    pub(crate) adverbs: &'static Dist,
    pub(crate) auxiliaries: &'static Dist,
    pub(crate) prepositions: &'static Dist,
    pub(crate) articles: &'static Dist,
    pub(crate) terminators: &'static Dist,
}

impl Dists {
    fn new() -> Self {
        Self {
            calendar: dist("calendar"),
            hours: dist("hours"),
            catalog_page_type: dist("catalog_page_type"),
            street_names: dist("street_names"),
            street_type: dist("street_type"),
            cities: dist("cities"),
            fips_county: dist("fips_county"),
            first_names: dist("first_names"),
            last_names: dist("last_names"),
            top_domains: dist("top_domains"),
            syllables: dist("syllables"),
            sentences: dist("sentences"),
            nouns: dist("nouns"),
            verbs: dist("verbs"),
            adjectives: dist("adjectives"),
            adverbs: dist("adverbs"),
            auxiliaries: dist("auxiliaries"),
            prepositions: dist("prepositions"),
            articles: dist("articles"),
            terminators: dist("terminators"),
        }
    }
}

/// The state of one generation.
#[derive(Debug)]
pub(crate) struct Context {
    pub(crate) rng: Streams,
    /// `get_dbl("SCALE")`.
    pub(crate) scale: f64,
    /// `arRowcount[].kBaseRowcount`.
    pub(crate) rowcounts: Vec<i64>,
    /// `kNullBitMap` of each table: the columns that are NULL in its current row.
    pub(crate) null_bit_maps: [i64; TDEF_COUNT],
    pub(crate) dists: Dists,
    pub(crate) scd_dates: ScdDates,
    /// `date_join`'s `jToday`.
    pub(crate) today: i64,
    /// The julian dates of `DATA_START_DATE` and `DATE_MINIMUM`.
    pub(crate) data_start_date: i32,
    pub(crate) date_minimum: i32,
    /// `mk_address`'s `nMaxCities` and `nMaxCounties`.
    pub(crate) max_cities: i32,
    pub(crate) max_counties: i32,
    /// `cp_join`'s and `mk_w_catalog_page`'s pages per catalog.
    pub(crate) pages_per_catalog: i32,
    /// `web_join`'s `nConcurrentSites` multiple of the sites' duration, and its offset.
    pub(crate) site_duration: i64,
    pub(crate) site_offset: i64,
}

impl Context {
    /// `InitializeDSDgen`: `SCALE` is set from `std::to_string(scale)` and read back with
    /// `atof`, so the scale has six decimals.
    pub(crate) fn new(scale: f64) -> Result<Self> {
        let scale: f64 = format!("{scale:.6}").parse().unwrap_or_default();
        if scale > 100_000.0 {
            // `ReportErrorNoLine(QERR_BAD_SCALE, NULL, 1)` exits the process.
            return Err(Error::invalid_input(
                "Selected scale factor is NOT valid for result publication",
            ));
        }
        let rowcounts = base_rowcounts(scale);
        let mut null_bit_maps = [0; TDEF_COUNT];
        for (table, map) in null_bit_maps.iter_mut().enumerate() {
            *map = get_simple_tdefs_by_number(table).null_bit_map;
        }
        let mut context = Self {
            rng: Streams::new(),
            scale,
            rowcounts,
            null_bit_maps,
            dists: Dists::new(),
            scd_dates: ScdDates::new(),
            today: i64::from(Date::parse(TODAYS_DATE).julian),
            data_start_date: Date::parse(DATA_START_DATE).julian,
            date_minimum: Date::parse(DATE_MINIMUM).julian,
            max_cities: 0,
            max_counties: 0,
            pages_per_catalog: 0,
            site_duration: 0,
            site_offset: 0,
        };
        context.max_cities = context.get_rowcount(ACTIVE_CITIES) as i32;
        context.max_counties = context.get_rowcount(ACTIVE_COUNTIES) as i32;
        context.pages_per_catalog = (context.get_rowcount(CATALOG_PAGE) as i32
            / super::constants::CP_CATALOGS_PER_YEAR)
            / (super::constants::YEAR_MAXIMUM - super::constants::YEAR_MINIMUM + 2);
        let site_close = i64::from(Date::parse(super::constants::WEB_END_DATE).julian);
        let site_open = i64::from(Date::parse(super::constants::WEB_START_DATE).julian);
        let concurrent_sites = i64::from(context.get_rowcount(CONCURRENT_WEB_SITES) as i32);
        context.site_duration = (site_close - site_open) * concurrent_sites;
        context.site_offset = (site_close - site_open) / (2 * context.site_duration);
        Ok(context)
    }

    /// `nullSet(&kNullBitMap of table, stream)`: picks the columns of the next row of
    /// `table` that are NULL, with the NULL settings of the table `stream` belongs to.
    pub(crate) fn null_set(&mut self, table: usize, stream: usize) {
        let tdef = get_simple_tdefs_by_number(get_table_from_column(stream));
        // Burn the RNG calls.
        let threshold = self.rng.genrand_integer(DIST_UNIFORM, 0, 9999, 0, stream);
        let bit_map = self.rng.genrand_key(DIST_UNIFORM, 1, MAXINT, 0, stream);
        // Set the bitmap based on the threshold and the NOT NULL definitions.
        self.null_bit_maps[table] = if threshold < tdef.null_pct {
            bit_map & !tdef.not_null_bit_map
        } else {
            0
        };
    }

    /// `genrand_date` with `DIST_UNIFORM`, the only distribution its callers use: a date in
    /// `[min, max]`.
    pub(crate) fn genrand_date(&mut self, min: &Date, max: &Date, stream: usize) -> Date {
        let start = min.to_julian();
        let range = max.to_julian() - start;
        let offset = self.rng.genrand_integer(DIST_UNIFORM, 0, range, 0, stream);
        Date::from_julian(start + offset)
    }
}
