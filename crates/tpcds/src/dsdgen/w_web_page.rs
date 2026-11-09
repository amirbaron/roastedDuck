//! The `web_page` table (`w_web_page.c`).

use roastedduck_common::Result;

use super::Output;
use super::build_support::bkey_str;
use super::columns::{
    WP_ACCESS_DATE_SK, WP_AUTOGEN_FLAG, WP_CHAR_COUNT, WP_CREATION_DATE_SK, WP_CUSTOMER_SK, WP_IMAGE_COUNT,
    WP_LINK_COUNT, WP_MAX_AD_COUNT, WP_NULLS, WP_PAGE_ID, WP_SCD, WP_TYPE,
};
use super::constants::{
    CURRENT_DAY, CURRENT_MONTH, CURRENT_YEAR, WP_AD_MAX, WP_AD_MIN, WP_AUTOGEN_PCT, WP_IDLE_TIME_MAX,
    WP_IMAGE_MAX, WP_IMAGE_MIN, WP_LINK_MAX, WP_LINK_MIN,
};
use super::context::Context;
use super::date::Date;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::scd::{ScdKeys, change_scd, skip_scd};
use super::tables::{CUSTOMER, DATET, WEB_PAGE};

/// A row of `web_page` (`struct W_WEB_PAGE_TBL`).
#[derive(Clone, Debug, Default)]
struct WebPageRow {
    page_sk: i64,
    keys: ScdKeys,
    creation_date_sk: i64,
    access_date_sk: i64,
    autogen_flag: i32,
    customer_sk: i64,
    url: String,
    kind: &'static str,
    char_count: i32,
    link_count: i32,
    image_count: i32,
    max_ad_count: i32,
}

/// The generator of `web_page` (`mk_w_web_page` and its statics).
#[derive(Debug)]
pub(crate) struct WebPage {
    r: WebPageRow,
    /// `g_OldValues`, the values the next revision may keep.
    old: WebPageRow,
    today: Date,
    uses: &'static Dist,
}

impl WebPage {
    pub(crate) fn new() -> Self {
        Self {
            r: WebPageRow::default(),
            old: WebPageRow::default(),
            // `sprintf(szTemp, "%d-%d-%d", CURRENT_YEAR, CURRENT_MONTH, CURRENT_DAY)`.
            today: Date::new(CURRENT_YEAR, CURRENT_MONTH, CURRENT_DAY),
            uses: dist("web_page_use"),
        }
    }

    /// `mk_w_web_page`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        let r = &mut self.r;
        let old = &mut self.old;
        ctx.null_set(WEB_PAGE, WP_NULLS);
        r.page_sk = index;

        // A new business key resets the fields that do not change between revisions.
        let first_record = ctx.set_scd_keys(WP_PAGE_ID, index, &mut r.keys);

        // The bits of this number decide which fields a revision changes.
        let mut flags = ctx.rng.next_random(WP_SCD) as i32;

        r.creation_date_sk = ctx.mk_join(WP_CREATION_DATE_SK, DATET, index);
        change_scd(
            &mut r.creation_date_sk,
            &mut old.creation_date_sk,
            &mut flags,
            first_record,
        );

        let access = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 0, WP_IDLE_TIME_MAX, 0, WP_ACCESS_DATE_SK);
        r.access_date_sk = i64::from(self.today.julian - access);
        change_scd(
            &mut r.access_date_sk,
            &mut old.access_date_sk,
            &mut flags,
            first_record,
        );
        if r.access_date_sk == 0 {
            // A special case for dates.
            r.access_date_sk = -1;
        }

        let temp = ctx.rng.genrand_integer(DIST_UNIFORM, 0, 99, 0, WP_AUTOGEN_FLAG);
        r.autogen_flag = i32::from(temp < WP_AUTOGEN_PCT);
        change_scd(
            &mut r.autogen_flag,
            &mut old.autogen_flag,
            &mut flags,
            first_record,
        );

        r.customer_sk = ctx.mk_join(WP_CUSTOMER_SK, CUSTOMER, 1);
        change_scd(&mut r.customer_sk, &mut old.customer_sk, &mut flags, first_record);
        if r.autogen_flag == 0 {
            r.customer_sk = -1;
        }

        // `genrand_url`.
        r.url.clear();
        r.url.push_str("http://www.foo.com");
        change_scd(&mut r.url, &mut old.url, &mut flags, first_record);

        r.kind = self.uses.pick_str(&mut ctx.rng, 1, 1, WP_TYPE).0;
        skip_scd(&mut flags);

        r.link_count = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, WP_LINK_MIN, WP_LINK_MAX, 0, WP_LINK_COUNT);
        change_scd(&mut r.link_count, &mut old.link_count, &mut flags, first_record);

        r.image_count = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, WP_IMAGE_MIN, WP_IMAGE_MAX, 0, WP_IMAGE_COUNT);
        change_scd(&mut r.image_count, &mut old.image_count, &mut flags, first_record);

        r.max_ad_count = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, WP_AD_MIN, WP_AD_MAX, 0, WP_MAX_AD_COUNT);
        change_scd(
            &mut r.max_ad_count,
            &mut old.max_ad_count,
            &mut flags,
            first_record,
        );

        r.char_count = ctx.rng.genrand_integer(
            DIST_UNIFORM,
            r.link_count * 125 + r.image_count * 50,
            r.link_count * 300 + r.image_count * 150,
            0,
            WP_CHAR_COUNT,
        );
        change_scd(&mut r.char_count, &mut old.char_count, &mut flags, first_record);

        let row = out.row(WEB_PAGE).start(&ctx.null_bit_maps);
        row.key(r.page_sk)?;
        row.varchar(bkey_str(&r.keys.business_key))?;
        row.date(r.keys.begin_date)?;
        row.date(r.keys.end_date)?;
        row.key(r.creation_date_sk)?;
        row.key(r.access_date_sk)?;
        row.varchar(if r.autogen_flag != 0 { "Y" } else { "N" })?;
        row.key(r.customer_sk)?;
        row.varchar(&r.url)?;
        row.varchar(r.kind)?;
        row.integer(r.char_count)?;
        row.integer(r.link_count)?;
        row.integer(r.image_count)?;
        row.integer(r.max_ad_count)?;
        out.end(WEB_PAGE)
    }
}
