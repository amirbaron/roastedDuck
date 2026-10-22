//! Addresses (`address.c`).

use std::fmt::Write as _;

use super::context::Context;
use super::genrand::DIST_UNIFORM;
use super::tdefs::{FL_SMALL, get_simple_tdefs_by_number, get_table_from_column};

/// `ds_addr_t`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Address {
    pub(crate) suite_num: String,
    pub(crate) street_num: i32,
    pub(crate) street_name1: &'static str,
    pub(crate) street_name2: &'static str,
    pub(crate) street_type: &'static str,
    pub(crate) city: &'static str,
    pub(crate) county: &'static str,
    pub(crate) state: &'static str,
    pub(crate) country: &'static str,
    pub(crate) zip: i32,
    pub(crate) plus4: i32,
    pub(crate) gmt_offset: i32,
}

impl Address {
    /// The street name the tables store: both names, with a space between them even when
    /// the second is empty (the second name is never a NULL pointer).
    pub(crate) fn street_name(&self, dest: &mut String) {
        dest.clear();
        dest.push_str(self.street_name1);
        dest.push(' ');
        dest.push_str(self.street_name2);
    }

    /// The zip code the tables store: `%05d`.
    pub(crate) fn zip_code(&self, dest: &mut String) {
        dest.clear();
        let _ = write!(dest, "{:05}", self.zip);
    }
}

/// `city_hash`: a 4 digit hash of a name.
pub(crate) fn city_hash(name: &str) -> i32 {
    let mut hash_value: i64 = 0;
    let mut res: i64 = 0;
    for &byte in name.as_bytes() {
        hash_value *= 26;
        // Simulate the overflow as if it were an int.
        if hash_value > super::genrand::MAXINT {
            hash_value %= super::genrand::MAXINT;
            hash_value -= super::genrand::MAXINT;
            hash_value -= 2;
        } else if hash_value < -super::genrand::MAXINT {
            hash_value %= super::genrand::MAXINT;
            hash_value += super::genrand::MAXINT;
            hash_value += 2;
        }
        hash_value -= i64::from(b'A');
        // `char` is signed.
        hash_value += i64::from(byte as i8);
        if hash_value > 1_000_000 {
            hash_value %= 10000;
            res += hash_value;
            hash_value = 0;
        }
    }
    hash_value %= 1000;
    res += hash_value;
    // A 4 digit result.
    res %= 10000;
    res as i32
}

impl Context {
    /// `mk_address`: an address for column `column`. The tables with few rows use only
    /// the first cities and counties.
    pub(crate) fn mk_address(&mut self, address: &mut Address, column: usize) {
        let dists = &self.dists;
        let rng = &mut self.rng;
        // The street number is in [1..1000].
        address.street_num = rng.genrand_integer(DIST_UNIFORM, 1, 1000, 0, column);
        // Street names and types are picked from distributions.
        address.street_name1 = dists.street_names.pick_str(rng, 1, 1, column).0;
        address.street_name2 = dists.street_names.pick_str(rng, 1, 2, column).0;
        address.street_type = dists.street_type.pick_str(rng, 1, 1, column).0;
        // The suite number is alphabetic 50% of the time.
        let i = rng.genrand_integer(DIST_UNIFORM, 1, 100, 0, column);
        address.suite_num.clear();
        if i & 0x01 != 0 {
            let _ = write!(address.suite_num, "Suite {}", (i >> 1) * 10);
        } else {
            let _ = write!(
                address.suite_num,
                "Suite {}",
                char::from((((i >> 1) % 25) + i32::from(b'A')) as u8)
            );
        }

        let table = get_table_from_column(column);
        let small = get_simple_tdefs_by_number(table).is(FL_SMALL);
        // The city is picked from a distribution which maps to large, medium and small.
        if small {
            let rows = self.get_rowcount(table) as i32;
            let i = self
                .rng
                .genrand_integer(DIST_UNIFORM, 1, self.max_cities.min(rows), 0, column);
            address.city = self.dists.cities.member_str(i, 1);
        } else {
            address.city = self.dists.cities.pick_str(&mut self.rng, 1, 6, column).0;
        }
        // The county is picked from a distribution based on population, and keys the
        // rest.
        let region = if small {
            let rows = self.get_rowcount(table) as i32;
            let region = self
                .rng
                .genrand_integer(DIST_UNIFORM, 1, self.max_counties.min(rows), 0, column);
            address.county = self.dists.fips_county.member_str(region, 2);
            region
        } else {
            let (county, region) = self.dists.fips_county.pick_str(&mut self.rng, 2, 1, column);
            address.county = county;
            region
        };
        let fips_county = self.dists.fips_county;
        // The state and the zip prefix of the selected county.
        address.state = fips_county.member_str(region, 3);
        address.zip = city_hash(address.city);
        // 00000 - 00600 are unused; avoid them.
        let zip_prefix = i32::from(fips_county.member_str(region, 5).as_bytes()[0]) - i32::from(b'0');
        if zip_prefix == 0 && address.zip < 9400 {
            address.zip += 600;
        }
        address.zip += zip_prefix * 10000;
        let street = format!(
            "{} {} {} {}",
            address.street_num, address.street_name1, address.street_name2, address.street_type
        );
        address.plus4 = city_hash(&street);
        address.gmt_offset = fips_county.member_int(region, 6);
        address.country = "United States";
    }
}
