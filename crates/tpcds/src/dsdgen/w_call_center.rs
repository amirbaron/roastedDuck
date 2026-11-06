//! The `call_center` table (`w_call_center.c`).

use std::fmt::Write as _;

use rustdb_common::Result;

use super::Output;
use super::address::Address;
use super::build_support::{bkey_str, mk_word};
use super::columns::{
    CC_ADDRESS, CC_CALL_CENTER_ID, CC_CLASS, CC_COMPANY, CC_EMPLOYEES, CC_HOURS, CC_MANAGER, CC_MARKET_CLASS,
    CC_MARKET_DESC, CC_MARKET_ID, CC_MARKET_MANAGER, CC_NULLS, CC_OPEN_DATE_ID, CC_SCD, CC_SQ_FT,
    CC_TAX_PERCENTAGE,
};
use super::constants::{
    CC_EMPLOYEE_MAX, DATA_START_DATE, RS_CC_DIVISION_NAME, RS_CC_MARKET_CLASS, RS_CC_MARKET_DESC,
};
use super::context::Context;
use super::date::Date;
use super::decimal::Decimal;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::scd::{ScdKeys, change_scd, skip_scd};
use super::tables::{CALL_CENTER, WEB_SITE};

/// `MIN_CC_TAX_PERCENTAGE`.
const MIN_CC_TAX_PERCENTAGE: &str = "0.00";
/// `MAX_CC_TAX_PERCENTAGE`.
const MAX_CC_TAX_PERCENTAGE: &str = "0.12";

/// A row of `call_center` (`struct CALL_CENTER_TBL`).
#[derive(Clone, Debug, Default)]
struct CallCenterRow {
    call_center_sk: i64,
    keys: ScdKeys,
    closed_date_id: i64,
    open_date_id: i64,
    name: String,
    class: &'static str,
    employees: i32,
    sq_ft: i32,
    hours: &'static str,
    manager: String,
    market_id: i32,
    market_class: String,
    market_desc: String,
    market_manager: String,
    division_id: i32,
    division_name: String,
    company: i32,
    company_name: String,
    address: Address,
    tax_percentage: Decimal,
}

/// The generator of `call_center` (`mk_w_call_center` and its statics).
#[derive(Debug)]
pub(crate) struct CallCenter {
    /// `g_w_call_center`, the last row.
    r: CallCenterRow,
    /// `g_OldValues`, the values the next revision may keep.
    old: CallCenterRow,
    date_start: i64,
    min_tax_percentage: Decimal,
    max_tax_percentage: Decimal,
    names: &'static Dist,
    classes: &'static Dist,
    hours: &'static Dist,
    street_name: String,
    zip: String,
}

impl CallCenter {
    pub(crate) fn new() -> Self {
        // Columns that are part of the SCD code or need further definition.
        let r = CallCenterRow {
            division_id: -1,
            closed_date_id: -1,
            division_name: "No Name".to_owned(),
            ..CallCenterRow::default()
        };
        Self {
            r,
            old: CallCenterRow::default(),
            date_start: i64::from(Date::parse(DATA_START_DATE).julian) - WEB_SITE as i64,
            min_tax_percentage: Decimal::from_str(MIN_CC_TAX_PERCENTAGE),
            max_tax_percentage: Decimal::from_str(MAX_CC_TAX_PERCENTAGE),
            names: dist("call_centers"),
            classes: dist("call_center_class"),
            hours: dist("call_center_hours"),
            street_name: String::new(),
            zip: String::new(),
        }
    }

    /// `mk_w_call_center`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        let r = &mut self.r;
        let old = &mut self.old;
        ctx.null_set(CALL_CENTER, CC_NULLS);
        r.call_center_sk = index;

        // A new business key resets the fields that do not change between revisions.
        let mut first_record = false;
        if ctx.set_scd_keys(CC_CALL_CENTER_ID, index, &mut r.keys) {
            r.open_date_id = self.date_start
                - i64::from(ctx.rng.genrand_integer(DIST_UNIFORM, -365, 0, 0, CC_OPEN_DATE_ID));
            let size = self.names.size() as i64;
            let suffix = index as i32 / size as i32;
            let name = self.names.member_str((index % size) as i32 + 1, 1);
            r.name.clear();
            r.name.push_str(name);
            if suffix > 0 {
                let _ = write!(r.name, "_{suffix}");
            }
            ctx.mk_address(&mut r.address, CC_ADDRESS);
            first_record = true;
        }

        // The bits of this number decide which fields a revision changes.
        let mut flags = ctx.rng.next_random(CC_SCD) as i32;

        r.class = self.classes.pick_str(&mut ctx.rng, 1, 1, CC_CLASS).0;
        skip_scd(&mut flags);

        let max_employees = if ctx.scale >= 1.0 {
            (f64::from(CC_EMPLOYEE_MAX) * ctx.scale * ctx.scale) as i32
        } else {
            CC_EMPLOYEE_MAX
        };
        r.employees = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, max_employees, 0, CC_EMPLOYEES);
        change_scd(&mut r.employees, &mut old.employees, &mut flags, first_record);

        r.sq_ft = ctx.rng.genrand_integer(DIST_UNIFORM, 100, 700, 0, CC_SQ_FT);
        r.sq_ft = r.sq_ft.wrapping_mul(r.employees);
        change_scd(&mut r.sq_ft, &mut old.sq_ft, &mut flags, first_record);

        r.hours = self.hours.pick_str(&mut ctx.rng, 1, 1, CC_HOURS).0;
        skip_scd(&mut flags);

        let (first, _) = ctx.dists.first_names.pick_str(&mut ctx.rng, 1, 1, CC_MANAGER);
        let (last, _) = ctx.dists.last_names.pick_str(&mut ctx.rng, 1, 1, CC_MANAGER);
        r.manager.clear();
        let _ = write!(r.manager, "{first} {last}");
        change_scd(&mut r.manager, &mut old.manager, &mut flags, first_record);

        r.market_id = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 6, 0, CC_MARKET_ID);
        change_scd(&mut r.market_id, &mut old.market_id, &mut flags, first_record);

        ctx.gen_text(&mut r.market_class, 20, RS_CC_MARKET_CLASS, CC_MARKET_CLASS);
        change_scd(
            &mut r.market_class,
            &mut old.market_class,
            &mut flags,
            first_record,
        );

        ctx.gen_text(&mut r.market_desc, 20, RS_CC_MARKET_DESC, CC_MARKET_DESC);
        change_scd(&mut r.market_desc, &mut old.market_desc, &mut flags, first_record);

        let (first, _) = ctx
            .dists
            .first_names
            .pick_str(&mut ctx.rng, 1, 1, CC_MARKET_MANAGER);
        let (last, _) = ctx
            .dists
            .last_names
            .pick_str(&mut ctx.rng, 1, 1, CC_MARKET_MANAGER);
        r.market_manager.clear();
        let _ = write!(r.market_manager, "{first} {last}");
        change_scd(
            &mut r.market_manager,
            &mut old.market_manager,
            &mut flags,
            first_record,
        );

        r.company = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 6, 0, CC_COMPANY);
        change_scd(&mut r.company, &mut old.company, &mut flags, first_record);

        r.division_id = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 6, 0, CC_COMPANY);
        change_scd(&mut r.division_id, &mut old.division_id, &mut flags, first_record);

        mk_word(
            &mut r.division_name,
            ctx.dists.syllables,
            i64::from(r.division_id),
            RS_CC_DIVISION_NAME,
        );
        change_scd(
            &mut r.division_name,
            &mut old.division_name,
            &mut flags,
            first_record,
        );

        // `mk_companyname`.
        mk_word(&mut r.company_name, ctx.dists.syllables, i64::from(r.company), 10);
        change_scd(
            &mut r.company_name,
            &mut old.company_name,
            &mut flags,
            first_record,
        );

        r.tax_percentage = ctx.rng.genrand_decimal(
            DIST_UNIFORM,
            &self.min_tax_percentage,
            &self.max_tax_percentage,
            None,
            CC_TAX_PERCENTAGE,
        );
        change_scd(
            &mut r.tax_percentage,
            &mut old.tax_percentage,
            &mut flags,
            first_record,
        );

        r.address.street_name(&mut self.street_name);
        r.address.zip_code(&mut self.zip);
        let row = out.row(CALL_CENTER).start(&ctx.null_bit_maps);
        row.key(r.call_center_sk)?;
        row.varchar(bkey_str(&r.keys.business_key))?;
        row.date(r.keys.begin_date)?;
        row.date(r.keys.end_date)?;
        row.key(r.closed_date_id)?;
        row.key(r.open_date_id)?;
        row.varchar(&r.name)?;
        row.varchar(r.class)?;
        row.integer(r.employees)?;
        row.integer(r.sq_ft)?;
        row.varchar(r.hours)?;
        row.varchar(&r.manager)?;
        row.integer(r.market_id)?;
        row.varchar(&r.market_class)?;
        row.varchar(&r.market_desc)?;
        row.varchar(&r.market_manager)?;
        row.integer(r.division_id)?;
        row.varchar(&r.division_name)?;
        row.integer(r.company)?;
        row.varchar(&r.company_name)?;
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
        out.end(CALL_CENTER)
    }
}
