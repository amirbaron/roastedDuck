//! The `store_sales` table and its child `store_returns` (`w_store_sales.c`,
//! `w_store_returns.c`).

use roastedduck_common::Result;

use super::Output;
use super::build_support::{get_permutation_entry, make_permutation};
use super::columns::{
    SR_ADDR_SK, SR_CDEMO_SK, SR_CUSTOMER_SK, SR_HDEMO_SK, SR_IS_RETURNED, SR_NULLS, SR_PRICING, SR_REASON_SK,
    SR_RETURNED_DATE_SK, SR_RETURNED_TIME_SK, SR_STORE_SK, SR_TICKET_NUMBER, SS_NULLS, SS_PERMUTATION,
    SS_PRICING, SS_SOLD_ADDR_SK, SS_SOLD_CDEMO_SK, SS_SOLD_CUSTOMER_SK, SS_SOLD_DATE_SK, SS_SOLD_HDEMO_SK,
    SS_SOLD_ITEM_SK, SS_SOLD_PROMO_SK, SS_SOLD_STORE_SK, SS_SOLD_TIME_SK, SS_TICKET_NUMBER,
};
use super::constants::SR_RETURN_PCT;
use super::context::Context;
use super::genrand::DIST_UNIFORM;
use super::pricing::{Pricing, PricingConstants};
use super::tables::{
    CUSTOMER, CUSTOMER_ADDRESS, CUSTOMER_DEMOGRAPHICS, DATET, HOUSEHOLD_DEMOGRAPHICS, ITEM, PROMOTION,
    REASON, STORE, STORE_RETURNS, STORE_SALES, TIME,
};

/// `SR_SAME_CUSTOMER`: the percentage of returns by the customer of the sale.
const SR_SAME_CUSTOMER: i32 = 80;

/// A row of `store_sales` (`struct W_STORE_SALES_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct StoreSalesRow {
    sold_date_sk: i64,
    sold_time_sk: i64,
    sold_item_sk: i64,
    sold_customer_sk: i64,
    sold_cdemo_sk: i64,
    sold_hdemo_sk: i64,
    sold_addr_sk: i64,
    sold_store_sk: i64,
    sold_promo_sk: i64,
    ticket_number: i64,
    pricing: Pricing,
}

/// A row of `store_returns` (`struct W_STORE_RETURNS_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct StoreReturnsRow {
    returned_date_sk: i64,
    returned_time_sk: i64,
    item_sk: i64,
    customer_sk: i64,
    cdemo_sk: i64,
    hdemo_sk: i64,
    addr_sk: i64,
    store_sk: i64,
    reason_sk: i64,
    ticket_number: i64,
    pricing: Pricing,
}

/// The generator of `store_sales` and `store_returns` (`mk_w_store_sales`, `mk_master`,
/// `mk_detail` and their statics).
#[derive(Debug)]
pub(crate) struct StoreSales {
    /// `g_w_store_sales`, the current line item.
    r: StoreSalesRow,
    pricing: PricingConstants,
    /// The position of the current line item in the item permutation.
    item_index: i32,
    item_permutation: Vec<i32>,
    item_count: i32,
}

impl StoreSales {
    /// `mk_master`'s initialization, which draws the item permutation.
    pub(crate) fn new(ctx: &mut Context) -> Self {
        let item_count = ctx.get_id_count(ITEM) as i32;
        let item_permutation = make_permutation(&mut ctx.rng, item_count, SS_PERMUTATION);
        Self {
            r: StoreSalesRow::default(),
            pricing: PricingConstants::new(),
            item_index: 0,
            item_permutation,
            item_count,
        }
    }

    /// `mk_w_store_sales`: a ticket and its line items.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        self.mk_master(ctx, index);
        // The number of line items of the ticket.
        let line_items = ctx.rng.genrand_integer(DIST_UNIFORM, 8, 16, 0, SS_TICKET_NUMBER);
        for _ in 0..line_items {
            self.mk_detail(ctx, out)?;
        }
        Ok(())
    }

    /// `mk_master`: the fields that are the same for every line item of a ticket.
    ///
    /// The original also moves a date forward by the tickets of each day (`dateScaling`),
    /// which nothing reads: the sales draw their dates. It is not kept, as at tiny scale
    /// factors no day has a ticket and DuckDB never stops moving it.
    fn mk_master(&mut self, ctx: &mut Context, index: i64) {
        let r = &mut self.r;
        r.sold_store_sk = ctx.mk_join(SS_SOLD_STORE_SK, STORE, 1);
        r.sold_time_sk = ctx.mk_join(SS_SOLD_TIME_SK, TIME, 1);
        r.sold_date_sk = ctx.mk_join(SS_SOLD_DATE_SK, DATET, 1);
        r.sold_customer_sk = ctx.mk_join(SS_SOLD_CUSTOMER_SK, CUSTOMER, 1);
        r.sold_cdemo_sk = ctx.mk_join(SS_SOLD_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        r.sold_hdemo_sk = ctx.mk_join(SS_SOLD_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        r.sold_addr_sk = ctx.mk_join(SS_SOLD_ADDR_SK, CUSTOMER_ADDRESS, 1);
        r.ticket_number = index;
        self.item_index = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, self.item_count, 0, SS_SOLD_ITEM_SK);
    }

    /// `mk_detail`: a line item, and its return if it is returned.
    fn mk_detail(&mut self, ctx: &mut Context, out: &mut Output<'_>) -> Result<()> {
        ctx.null_set(STORE_SALES, SS_NULLS);
        let r = &mut self.r;
        // The items of a ticket are unique: a sequence within the permutation.
        self.item_index += 1;
        if self.item_index > self.item_count {
            self.item_index = 1;
        }
        let item = get_permutation_entry(&self.item_permutation, self.item_index);
        r.sold_item_sk = ctx.match_scd_sk(i64::from(item), r.sold_date_sk, ITEM);
        r.sold_promo_sk = ctx.mk_join(SS_SOLD_PROMO_SK, PROMOTION, 1);
        self.pricing.set_pricing(&mut ctx.rng, SS_PRICING, &mut r.pricing);

        // Having made the sale, see if it gets returned.
        let returned = ctx.rng.genrand_integer(DIST_UNIFORM, 0, 99, 0, SR_IS_RETURNED);
        if returned < SR_RETURN_PCT {
            let rr = self.mk_return(ctx);
            let p = &rr.pricing;
            let row = out.row(STORE_RETURNS).start(&ctx.null_bit_maps);
            row.key(rr.returned_date_sk)?;
            row.key(rr.returned_time_sk)?;
            row.key(rr.item_sk)?;
            row.key(rr.customer_sk)?;
            row.key(rr.cdemo_sk)?;
            row.key(rr.hdemo_sk)?;
            row.key(rr.addr_sk)?;
            row.key(rr.store_sk)?;
            row.key(rr.reason_sk)?;
            row.key(rr.ticket_number)?;
            row.integer(p.quantity)?;
            row.decimal(&p.net_paid)?;
            row.decimal(&p.ext_tax)?;
            row.decimal(&p.net_paid_inc_tax)?;
            row.decimal(&p.fee)?;
            row.decimal(&p.ext_ship_cost)?;
            row.decimal(&p.refunded_cash)?;
            row.decimal(&p.reversed_charge)?;
            row.decimal(&p.store_credit)?;
            row.decimal(&p.net_loss)?;
            out.end(STORE_RETURNS)?;
        }

        let r = &self.r;
        let p = &r.pricing;
        let row = out.row(STORE_SALES).start(&ctx.null_bit_maps);
        row.key(r.sold_date_sk)?;
        row.key(r.sold_time_sk)?;
        row.key(r.sold_item_sk)?;
        row.key(r.sold_customer_sk)?;
        row.key(r.sold_cdemo_sk)?;
        row.key(r.sold_hdemo_sk)?;
        row.key(r.sold_addr_sk)?;
        row.key(r.sold_store_sk)?;
        row.key(r.sold_promo_sk)?;
        row.key(r.ticket_number)?;
        row.integer(p.quantity)?;
        row.decimal(&p.wholesale_cost)?;
        row.decimal(&p.list_price)?;
        row.decimal(&p.sales_price)?;
        // `ss_ext_discount_amt` is the coupon amount, as DuckDB appends it.
        row.decimal(&p.coupon_amt)?;
        row.decimal(&p.ext_sales_price)?;
        row.decimal(&p.ext_wholesale_cost)?;
        row.decimal(&p.ext_list_price)?;
        row.decimal(&p.ext_tax)?;
        row.decimal(&p.coupon_amt)?;
        row.decimal(&p.net_paid)?;
        row.decimal(&p.net_paid_inc_tax)?;
        row.decimal(&p.net_profit)?;
        out.end(STORE_SALES)
    }

    /// `mk_w_store_returns`: the return of the current line item.
    fn mk_return(&self, ctx: &mut Context) -> StoreReturnsRow {
        let sale = &self.r;
        ctx.null_set(STORE_RETURNS, SR_NULLS);
        // Some of the information in the return is taken from the sale.
        let mut r = StoreReturnsRow {
            ticket_number: sale.ticket_number,
            item_sk: sale.sold_item_sk,
            pricing: sale.pricing,
            ..StoreReturnsRow::default()
        };

        // Some of the fields are conditionally taken from the sale.
        r.customer_sk = ctx.mk_join(SR_CUSTOMER_SK, CUSTOMER, 1);
        if ctx.rng.genrand_integer(DIST_UNIFORM, 1, 100, 0, SR_TICKET_NUMBER) < SR_SAME_CUSTOMER {
            r.customer_sk = sale.sold_customer_sk;
        }

        // The rest of the columns are generated for this return. Items cannot be returned
        // before they are sold; `mk_join` adds the lag to the sale date.
        r.returned_date_sk = ctx.mk_join(SR_RETURNED_DATE_SK, DATET, sale.sold_date_sk);
        r.returned_time_sk = i64::from(ctx.rng.genrand_integer(
            DIST_UNIFORM,
            (8 * 3600) - 1,
            (17 * 3600) - 1,
            0,
            SR_RETURNED_TIME_SK,
        ));
        r.cdemo_sk = ctx.mk_join(SR_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        r.hdemo_sk = ctx.mk_join(SR_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        r.addr_sk = ctx.mk_join(SR_ADDR_SK, CUSTOMER_ADDRESS, 1);
        r.store_sk = ctx.mk_join(SR_STORE_SK, STORE, 1);
        r.reason_sk = ctx.mk_join(SR_REASON_SK, REASON, 1);
        r.pricing.quantity = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, sale.pricing.quantity, 0, SR_PRICING);
        self.pricing.set_pricing(&mut ctx.rng, SR_PRICING, &mut r.pricing);
        r
    }
}
