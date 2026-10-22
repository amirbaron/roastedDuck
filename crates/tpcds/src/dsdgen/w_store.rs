//! The `store` table (`w_store.c`).

use std::fmt::Write as _;

use rustdb_common::Result;

use super::Output;
use super::address::Address;
use super::build_support::{bkey_str, mk_word};
use super::columns::{
    S_STORE_ID, W_STORE_ADDRESS, W_STORE_CLOSED_DATE_ID, W_STORE_COMPANY_NAME, W_STORE_DIVISION_NAME,
    W_STORE_EMPLOYEES, W_STORE_FLOOR_SPACE, W_STORE_GEOGRAPHY_CLASS, W_STORE_HOURS, W_STORE_MANAGER,
    W_STORE_MARKET_DESC, W_STORE_MARKET_ID, W_STORE_MARKET_MANAGER, W_STORE_NULLS, W_STORE_SCD,
    W_STORE_TAX_PERCENTAGE, W_STORE_TYPE,
};
use super::constants::{DATE_MINIMUM, RS_S_MARKET_DESC};
use super::context::Context;
use super::date::Date;
use super::decimal::Decimal;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::scd::{ScdKeys, change_scd, skip_scd};
use super::tables::STORE;

/// `STORE_MIN_TAX_PERCENTAGE`.
const STORE_MIN_TAX_PERCENTAGE: &str = "0.00";
/// `STORE_MAX_TAX_PERCENTAGE`.
const STORE_MAX_TAX_PERCENTAGE: &str = "0.11";
/// `STORE_MIN_DAYS_OPEN`.
const STORE_MIN_DAYS_OPEN: i32 = 5;
/// `STORE_MAX_DAYS_OPEN`.
const STORE_MAX_DAYS_OPEN: i32 = 500;
/// `STORE_CLOSED_PCT`.
const STORE_CLOSED_PCT: i32 = 30;
/// `STORE_DESC_MIN`.
const STORE_DESC_MIN: i32 = 15;

/// A row of `store` (`struct W_STORE_TBL`).
#[derive(Clone, Debug, Default)]
struct StoreRow {
    store_sk: i64,
    keys: ScdKeys,
    closed_date_id: i64,
    store_name: String,
    employees: i32,
    floor_space: i32,
    hours: &'static str,
    store_manager: String,
    market_id: i32,
    tax_percentage: Decimal,
    geography_class: &'static str,
    market_desc: String,
    market_manager: String,
    division_id: i64,
    division_name: &'static str,
    company_id: i64,
    company_name: &'static str,
    address: Address,
}

/// The generator of `store` (`mk_w_store` and its statics).
#[derive(Debug)]
pub(crate) struct Store {
    r: StoreRow,
    /// `g_OldValues`, the values the next revision may keep.
    old: StoreRow,
    date: i64,
    min_tax_percentage: Decimal,
    max_tax_percentage: Decimal,
    types: &'static Dist,
    call_center_hours: &'static Dist,
    geography_class: &'static Dist,
    divisions: &'static Dist,
    stores: &'static Dist,
    street_name: String,
    zip: String,
}

impl Store {
    pub(crate) fn new() -> Self {
        Self {
            r: StoreRow {
                // A column that should be dynamic.
                keys: ScdKeys {
                    end_date: -1,
                    ..ScdKeys::default()
                },
                ..StoreRow::default()
            },
            old: StoreRow::default(),
            date: i64::from(Date::parse(DATE_MINIMUM).julian),
            min_tax_percentage: Decimal::from_str(STORE_MIN_TAX_PERCENTAGE),
            max_tax_percentage: Decimal::from_str(STORE_MAX_TAX_PERCENTAGE),
            types: dist("store_type"),
            call_center_hours: dist("call_center_hours"),
            geography_class: dist("geography_class"),
            divisions: dist("divisions"),
            stores: dist("stores"),
            street_name: String::new(),
            zip: String::new(),
        }
    }

    /// `mk_w_store`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        let r = &mut self.r;
        let old = &mut self.old;
        ctx.null_set(STORE, W_STORE_NULLS);
        r.store_sk = index;

        // A new business key resets the fields that do not change between revisions.
        let first_record = ctx.set_scd_keys(S_STORE_ID, index, &mut r.keys);

        // The bits of this number decide which fields a revision changes.
        let mut flags = ctx.rng.next_random(W_STORE_SCD) as i32;

        let percentage = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, 100, 0, W_STORE_CLOSED_DATE_ID);
        let days_open = ctx.rng.genrand_integer(
            DIST_UNIFORM,
            STORE_MIN_DAYS_OPEN,
            STORE_MAX_DAYS_OPEN,
            0,
            W_STORE_CLOSED_DATE_ID,
        );
        r.closed_date_id = if percentage < STORE_CLOSED_PCT {
            self.date + i64::from(days_open)
        } else {
            -1
        };
        change_scd(
            &mut r.closed_date_id,
            &mut old.closed_date_id,
            &mut flags,
            first_record,
        );
        if r.closed_date_id == 0 {
            // Dates use a special NULL indicator.
            r.closed_date_id = -1;
        }

        mk_word(&mut r.store_name, ctx.dists.syllables, i64::from(index as i32), 5);
        change_scd(&mut r.store_name, &mut old.store_name, &mut flags, first_record);

        // The store type sets the parameters of the rest of the attributes.
        let (_, store_type) = self.types.pick_str(&mut ctx.rng, 1, 1, W_STORE_TYPE);
        let min = self.types.member_int(store_type, 2);
        let max = self.types.member_int(store_type, 3);
        r.employees = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, min, max, 0, W_STORE_EMPLOYEES);
        change_scd(&mut r.employees, &mut old.employees, &mut flags, first_record);

        let min = self.types.member_int(store_type, 4);
        let max = self.types.member_int(store_type, 5);
        r.floor_space = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, min, max, 0, W_STORE_FLOOR_SPACE);
        change_scd(&mut r.floor_space, &mut old.floor_space, &mut flags, first_record);

        r.hours = self
            .call_center_hours
            .pick_str(&mut ctx.rng, 1, 1, W_STORE_HOURS)
            .0;
        skip_scd(&mut flags);

        let (first, _) = ctx
            .dists
            .first_names
            .pick_str(&mut ctx.rng, 1, 1, W_STORE_MANAGER);
        let (last, _) = ctx.dists.last_names.pick_str(&mut ctx.rng, 1, 1, W_STORE_MANAGER);
        r.store_manager.clear();
        let _ = write!(r.store_manager, "{first} {last}");
        change_scd(
            &mut r.store_manager,
            &mut old.store_manager,
            &mut flags,
            first_record,
        );

        r.market_id = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 10, 0, W_STORE_MARKET_ID);
        change_scd(&mut r.market_id, &mut old.market_id, &mut flags, first_record);

        r.tax_percentage = ctx.rng.genrand_decimal(
            DIST_UNIFORM,
            &self.min_tax_percentage,
            &self.max_tax_percentage,
            None,
            W_STORE_TAX_PERCENTAGE,
        );
        change_scd(
            &mut r.tax_percentage,
            &mut old.tax_percentage,
            &mut flags,
            first_record,
        );

        r.geography_class = self
            .geography_class
            .pick_str(&mut ctx.rng, 1, 1, W_STORE_GEOGRAPHY_CLASS)
            .0;
        skip_scd(&mut flags);

        ctx.gen_text(
            &mut r.market_desc,
            STORE_DESC_MIN,
            RS_S_MARKET_DESC,
            W_STORE_MARKET_DESC,
        );
        change_scd(&mut r.market_desc, &mut old.market_desc, &mut flags, first_record);

        let (first, _) = ctx
            .dists
            .first_names
            .pick_str(&mut ctx.rng, 1, 1, W_STORE_MARKET_MANAGER);
        let (last, _) = ctx
            .dists
            .last_names
            .pick_str(&mut ctx.rng, 1, 1, W_STORE_MARKET_MANAGER);
        r.market_manager.clear();
        let _ = write!(r.market_manager, "{first} {last}");
        change_scd(
            &mut r.market_manager,
            &mut old.market_manager,
            &mut flags,
            first_record,
        );

        let (division_name, division_id) = self.divisions.pick_str(&mut ctx.rng, 1, 1, W_STORE_DIVISION_NAME);
        r.division_name = division_name;
        r.division_id = i64::from(division_id);
        change_scd(&mut r.division_id, &mut old.division_id, &mut flags, first_record);
        skip_scd(&mut flags);

        let (company_name, company_id) = self.stores.pick_str(&mut ctx.rng, 1, 1, W_STORE_COMPANY_NAME);
        r.company_name = company_name;
        r.company_id = i64::from(company_id);
        change_scd(&mut r.company_id, &mut old.company_id, &mut flags, first_record);
        skip_scd(&mut flags);

        ctx.mk_address(&mut r.address, W_STORE_ADDRESS);
        // city, county
        skip_scd(&mut flags);
        skip_scd(&mut flags);
        change_scd(
            &mut r.address.gmt_offset,
            &mut old.address.gmt_offset,
            &mut flags,
            first_record,
        );
        // state, street_type, street_name1, street_name2
        skip_scd(&mut flags);
        skip_scd(&mut flags);
        skip_scd(&mut flags);
        skip_scd(&mut flags);
        change_scd(
            &mut r.address.street_num,
            &mut old.address.street_num,
            &mut flags,
            first_record,
        );
        change_scd(&mut r.address.zip, &mut old.address.zip, &mut flags, first_record);

        r.address.street_name(&mut self.street_name);
        r.address.zip_code(&mut self.zip);
        let row = out.row(STORE).start(&ctx.null_bit_maps);
        row.key(r.store_sk)?;
        row.varchar(bkey_str(&r.keys.business_key))?;
        row.date(r.keys.begin_date)?;
        row.date(r.keys.end_date)?;
        row.key(r.closed_date_id)?;
        row.varchar(&r.store_name)?;
        row.integer(r.employees)?;
        row.integer(r.floor_space)?;
        row.varchar(r.hours)?;
        row.varchar(&r.store_manager)?;
        row.integer(r.market_id)?;
        row.varchar(r.geography_class)?;
        row.varchar(&r.market_desc)?;
        row.varchar(&r.market_manager)?;
        row.key(r.division_id)?;
        row.varchar(r.division_name)?;
        row.key(r.company_id)?;
        row.varchar(r.company_name)?;
        row.integer(r.address.street_num)?;
        row.varchar(&self.street_name)?;
        row.varchar(r.address.street_type)?;
        row.varchar(&r.address.suite_num)?;
        row.varchar(r.address.city)?;
        row.varchar(r.address.county)?;
        row.varchar(r.address.state)?;
        row.varchar(&self.zip)?;
        row.varchar(r.address.country)?;
        row.integer_decimal(r.address.gmt_offset)?;
        row.decimal(&r.tax_percentage)?;
        out.end(STORE)
    }
}
