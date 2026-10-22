//! The `promotion`, `reason` and `ship_mode` tables (`w_promotion.c`, `w_reason.c`,
//! `w_ship_mode.c`).

use rustdb_common::Result;

use super::Output;
use super::build_support::{bitmap_to_dist_str, bkey_str, mk_bkey, mk_word};
use super::columns::{
    P_CHANNEL_DETAILS, P_CHANNEL_DMAIL, P_END_DATE_ID, P_ITEM_SK, P_NULLS, P_PURPOSE, P_START_DATE_ID,
    R_NULLS, SM_CONTRACT, SM_NULLS,
};
use super::constants::{
    DATE_MINIMUM, PROMO_DETAIL_LEN_MAX, PROMO_DETAIL_LEN_MIN, PROMO_LEN_MAX, PROMO_LEN_MEAN, PROMO_LEN_MIN,
    PROMO_NAME_LEN, PROMO_START_MAX, PROMO_START_MEAN, PROMO_START_MIN, RS_SM_CONTRACT,
};
use super::context::Context;
use super::date::Date;
use super::decimal::Decimal;
use super::dist::{Dist, dist};
use super::genrand::{ALPHANUM, DIST_UNIFORM};
use super::tables::{ITEM, PROMOTION, REASON, SHIP_MODE};

/// The generator of `promotion` (`mk_w_promotion` and its statics).
#[derive(Debug)]
pub(crate) struct Promotion {
    start_date: i64,
    cost: Decimal,
    promo_name: String,
    channel_details: String,
    promo_purpose: &'static Dist,
}

/// `"Y"` or `"N"`.
const fn flag(value: i32) -> &'static str {
    if value != 0 { "Y" } else { "N" }
}

impl Promotion {
    pub(crate) fn new() -> Self {
        Self {
            start_date: i64::from(Date::parse(DATE_MINIMUM).julian),
            cost: Decimal::from_str("1000.00"),
            promo_name: String::new(),
            channel_details: String::new(),
            promo_purpose: dist("promo_purpose"),
        }
    }

    /// `mk_w_promotion`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(PROMOTION, P_NULLS);
        let promo_id = mk_bkey(index);
        let start_date_id = self.start_date
            + i64::from(ctx.rng.genrand_integer(
                DIST_UNIFORM,
                PROMO_START_MIN,
                PROMO_START_MAX,
                PROMO_START_MEAN,
                P_START_DATE_ID,
            ));
        let end_date_id = start_date_id
            + i64::from(ctx.rng.genrand_integer(
                DIST_UNIFORM,
                PROMO_LEN_MIN,
                PROMO_LEN_MAX,
                PROMO_LEN_MEAN,
                P_END_DATE_ID,
            ));
        let item_sk = ctx.mk_join(P_ITEM_SK, ITEM, 1);
        let response_target = 1;
        mk_word(
            &mut self.promo_name,
            ctx.dists.syllables,
            i64::from(index as i32),
            PROMO_NAME_LEN,
        );
        // The channels shift the flags the wrong way: all but the first are 0.
        let mut flags = ctx.rng.genrand_integer(DIST_UNIFORM, 0, 511, 0, P_CHANNEL_DMAIL);
        let mut channels = [0; 9];
        for (i, channel) in channels.iter_mut().enumerate() {
            if i > 0 {
                flags <<= 1;
            }
            *channel = flags & 0x01;
        }
        let [
            dmail,
            email,
            catalog,
            tv,
            radio,
            press,
            event,
            demo,
            discount_active,
        ] = channels;
        ctx.gen_text(
            &mut self.channel_details,
            PROMO_DETAIL_LEN_MIN,
            PROMO_DETAIL_LEN_MAX,
            P_CHANNEL_DETAILS,
        );
        let (purpose, _) = self.promo_purpose.pick_str(&mut ctx.rng, 1, 1, P_PURPOSE);

        let row = out.row(PROMOTION).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&promo_id))?;
        row.key(start_date_id)?;
        row.key(end_date_id)?;
        row.key(item_sk)?;
        row.decimal(&self.cost)?;
        row.integer(response_target)?;
        row.varchar(&self.promo_name)?;
        row.varchar(flag(dmail))?;
        row.varchar(flag(email))?;
        row.varchar(flag(catalog))?;
        row.varchar(flag(tv))?;
        row.varchar(flag(radio))?;
        row.varchar(flag(press))?;
        row.varchar(flag(event))?;
        row.varchar(flag(demo))?;
        row.varchar(&self.channel_details)?;
        row.varchar(purpose)?;
        row.varchar(flag(discount_active))?;
        out.end(PROMOTION)
    }
}

/// The generator of `reason` (`mk_w_reason`).
#[derive(Debug)]
pub(crate) struct Reason {
    return_reasons: &'static Dist,
}

impl Reason {
    pub(crate) fn new() -> Self {
        Self {
            return_reasons: dist("return_reasons"),
        }
    }

    /// `mk_w_reason`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(REASON, R_NULLS);
        let reason_id = mk_bkey(index);
        let description = self.return_reasons.member_str(index as i32, 1);

        let row = out.row(REASON).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&reason_id))?;
        row.varchar(description)?;
        out.end(REASON)
    }
}

/// The generator of `ship_mode` (`mk_w_ship_mode`).
#[derive(Debug)]
pub(crate) struct ShipMode {
    types: &'static Dist,
    codes: &'static Dist,
    carriers: &'static Dist,
    contract: String,
}

impl ShipMode {
    pub(crate) fn new() -> Self {
        Self {
            types: dist("ship_mode_type"),
            codes: dist("ship_mode_code"),
            carriers: dist("ship_mode_carrier"),
            contract: String::new(),
        }
    }

    /// `mk_w_ship_mode`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(SHIP_MODE, SM_NULLS);
        let ship_mode_id = mk_bkey(index);
        let mut temp = index;
        let kind = bitmap_to_dist_str(self.types, &mut temp, 1);
        let code = bitmap_to_dist_str(self.codes, &mut temp, 1);
        let carrier = self.carriers.member_str(index as i32, 1);
        ctx.rng
            .gen_charset(&mut self.contract, ALPHANUM, 1, RS_SM_CONTRACT, SM_CONTRACT);

        let row = out.row(SHIP_MODE).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&ship_mode_id))?;
        row.varchar(kind)?;
        row.varchar(code)?;
        row.varchar(carrier)?;
        row.varchar(&self.contract)?;
        out.end(SHIP_MODE)
    }
}
