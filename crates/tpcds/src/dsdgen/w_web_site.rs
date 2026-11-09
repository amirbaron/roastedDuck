//! The `web_site` table (`w_web_site.c`).

use std::fmt::Write as _;

use roastedduck_common::Result;

use super::Output;
use super::address::Address;
use super::build_support::{bkey_str, mk_word};
use super::columns::{
    WEB_ADDRESS, WEB_CLOSE_DATE, WEB_COMPANY_ID, WEB_MANAGER, WEB_MARKET_CLASS, WEB_MARKET_DESC,
    WEB_MARKET_ID, WEB_MARKET_MANAGER, WEB_NULLS, WEB_OPEN_DATE, WEB_SCD, WEB_SITE_ID, WEB_TAX_PERCENTAGE,
};
use super::constants::{RS_WEB_COMPANY_NAME, RS_WEB_MARKET_CLASS, RS_WEB_MARKET_DESC};
use super::context::Context;
use super::decimal::Decimal;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::scd::{ScdKeys, change_scd, skip_scd};
use super::tables::{DATET, WEB_SITE};

/// `WEB_MIN_TAX_PERCENTAGE`.
const WEB_MIN_TAX_PERCENTAGE: &str = "0.00";
/// `WEB_MAX_TAX_PERCENTAGE`.
const WEB_MAX_TAX_PERCENTAGE: &str = "0.12";

/// A row of `web_site` (`struct W_WEB_SITE_TBL`).
#[derive(Clone, Debug, Default)]
struct WebSiteRow {
    site_sk: i64,
    keys: ScdKeys,
    name: String,
    open_date: i64,
    close_date: i64,
    class: &'static str,
    manager: String,
    market_id: i32,
    market_class: String,
    market_desc: String,
    market_manager: String,
    company_id: i32,
    company_name: String,
    address: Address,
    tax_percentage: Decimal,
}

/// The generator of `web_site` (`mk_w_web_site` and its statics).
#[derive(Debug)]
pub(crate) struct WebSite {
    r: WebSiteRow,
    /// `g_OldValues`, the values the next revision may keep.
    old: WebSiteRow,
    min_tax_percentage: Decimal,
    max_tax_percentage: Decimal,
    /// `mk_word`'s "Syllables", found ignoring case.
    syllables: &'static Dist,
    street_name: String,
    zip: String,
}

impl WebSite {
    pub(crate) fn new() -> Self {
        Self {
            r: WebSiteRow {
                class: "Unknown",
                ..WebSiteRow::default()
            },
            old: WebSiteRow::default(),
            min_tax_percentage: Decimal::from_str(WEB_MIN_TAX_PERCENTAGE),
            max_tax_percentage: Decimal::from_str(WEB_MAX_TAX_PERCENTAGE),
            syllables: dist("Syllables"),
            street_name: String::new(),
            zip: String::new(),
        }
    }

    /// `mk_w_web_site`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        let r = &mut self.r;
        let old = &mut self.old;
        ctx.null_set(WEB_SITE, WEB_NULLS);
        r.site_sk = index;

        // A new business key resets the fields that do not change between revisions.
        let mut first_record = false;
        if ctx.set_scd_keys(WEB_SITE_ID, index, &mut r.keys) {
            r.open_date = ctx.mk_join(WEB_OPEN_DATE, DATET, index);
            r.close_date = ctx.mk_join(WEB_CLOSE_DATE, DATET, index);
            if r.close_date > r.keys.end_date {
                r.close_date = -1;
            }
            r.name.clear();
            let _ = write!(r.name, "site_{}", (index / 6) as i32);
            first_record = true;
        }

        // The bits of this number decide which fields a revision changes.
        let mut flags = ctx.rng.next_random(WEB_SCD) as i32;

        let (first, _) = ctx.dists.first_names.pick_str(&mut ctx.rng, 1, 1, WEB_MANAGER);
        let (last, _) = ctx.dists.last_names.pick_str(&mut ctx.rng, 1, 1, WEB_MANAGER);
        r.manager.clear();
        let _ = write!(r.manager, "{first} {last}");
        change_scd(&mut r.manager, &mut old.manager, &mut flags, first_record);

        r.market_id = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 6, 0, WEB_MARKET_ID);
        change_scd(&mut r.market_id, &mut old.market_id, &mut flags, first_record);

        ctx.gen_text(&mut r.market_class, 20, RS_WEB_MARKET_CLASS, WEB_MARKET_CLASS);
        change_scd(
            &mut r.market_class,
            &mut old.market_class,
            &mut flags,
            first_record,
        );

        ctx.gen_text(&mut r.market_desc, 20, RS_WEB_MARKET_DESC, WEB_MARKET_DESC);
        change_scd(&mut r.market_desc, &mut old.market_desc, &mut flags, first_record);

        let (first, _) = ctx
            .dists
            .first_names
            .pick_str(&mut ctx.rng, 1, 1, WEB_MARKET_MANAGER);
        let (last, _) = ctx
            .dists
            .last_names
            .pick_str(&mut ctx.rng, 1, 1, WEB_MARKET_MANAGER);
        r.market_manager.clear();
        let _ = write!(r.market_manager, "{first} {last}");
        change_scd(
            &mut r.market_manager,
            &mut old.market_manager,
            &mut flags,
            first_record,
        );

        r.company_id = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 6, 0, WEB_COMPANY_ID);
        change_scd(&mut r.company_id, &mut old.company_id, &mut flags, first_record);

        mk_word(
            &mut r.company_name,
            self.syllables,
            i64::from(r.company_id),
            RS_WEB_COMPANY_NAME,
        );
        change_scd(
            &mut r.company_name,
            &mut old.company_name,
            &mut flags,
            first_record,
        );

        ctx.mk_address(&mut r.address, WEB_ADDRESS);
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

        r.tax_percentage = ctx.rng.genrand_decimal(
            DIST_UNIFORM,
            &self.min_tax_percentage,
            &self.max_tax_percentage,
            None,
            WEB_TAX_PERCENTAGE,
        );
        change_scd(
            &mut r.tax_percentage,
            &mut old.tax_percentage,
            &mut flags,
            first_record,
        );

        r.address.street_name(&mut self.street_name);
        r.address.zip_code(&mut self.zip);
        let row = out.row(WEB_SITE).start(&ctx.null_bit_maps);
        row.key(r.site_sk)?;
        row.varchar(bkey_str(&r.keys.business_key))?;
        row.date(i64::from(r.keys.begin_date as i32))?;
        row.date(i64::from(r.keys.end_date as i32))?;
        row.varchar(&r.name)?;
        row.key(r.open_date)?;
        row.key(r.close_date)?;
        row.varchar(r.class)?;
        row.varchar(&r.manager)?;
        row.integer(r.market_id)?;
        row.varchar(&r.market_class)?;
        row.varchar(&r.market_desc)?;
        row.varchar(&r.market_manager)?;
        row.integer(r.company_id)?;
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
        out.end(WEB_SITE)
    }
}
