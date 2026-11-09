//! The `web_sales` table and its child `web_returns` (`w_web_sales.c`, `w_web_returns.c`).

use roastedduck_common::Result;

use super::Output;
use super::build_support::{get_permutation_entry, make_permutation};
use super::columns::{
    WR_IS_RETURNED, WR_NULLS, WR_PRICING, WR_REASON_SK, WR_REFUNDED_ADDR_SK, WR_REFUNDED_CDEMO_SK,
    WR_REFUNDED_CUSTOMER_SK, WR_REFUNDED_HDEMO_SK, WR_RETURNED_DATE_SK, WR_RETURNED_TIME_SK,
    WR_RETURNING_CUSTOMER_SK, WS_BILL_ADDR_SK, WS_BILL_CDEMO_SK, WS_BILL_CUSTOMER_SK, WS_BILL_HDEMO_SK,
    WS_ITEM_SK, WS_NULLS, WS_ORDER_NUMBER, WS_PERMUTATION, WS_PRICING, WS_PROMO_SK, WS_SHIP_ADDR_SK,
    WS_SHIP_CDEMO_SK, WS_SHIP_CUSTOMER_SK, WS_SHIP_DATE_SK, WS_SHIP_HDEMO_SK, WS_SHIP_MODE_SK,
    WS_SOLD_DATE_SK, WS_SOLD_TIME_SK, WS_WAREHOUSE_SK, WS_WEB_PAGE_SK, WS_WEB_SITE_SK,
};
use super::constants::WR_RETURN_PCT;
use super::context::Context;
use super::genrand::DIST_UNIFORM;
use super::pricing::{Pricing, PricingConstants};
use super::tables::{
    CUSTOMER, CUSTOMER_ADDRESS, CUSTOMER_DEMOGRAPHICS, DATET, HOUSEHOLD_DEMOGRAPHICS, ITEM, PROMOTION,
    REASON, SHIP_MODE, TIME, WAREHOUSE, WEB_PAGE, WEB_RETURNS, WEB_SALES, WEB_SITE,
};

/// `WS_QUANTITY_MAX`.
pub(crate) const WS_QUANTITY_MAX: &str = "100";
/// `WS_MARKUP_MAX`.
pub(crate) const WS_MARKUP_MAX: &str = "2.00";
/// `WS_DISCOUNT_MAX`.
pub(crate) const WS_DISCOUNT_MAX: &str = "1.00";
/// `WS_WHOLESALE_MAX`.
pub(crate) const WS_WHOLESALE_MAX: &str = "100.00";
/// `WS_GIFT_PCT`: the likelihood that a purchase is shipped to someone else.
const WS_GIFT_PCT: i32 = 7;
/// `WS_MIN_SHIP_DELAY`: the time between the order and its shipping.
pub(crate) const WS_MIN_SHIP_DELAY: i32 = 1;
/// `WS_MAX_SHIP_DELAY`.
pub(crate) const WS_MAX_SHIP_DELAY: i32 = 120;

/// A row of `web_sales` (`struct W_WEB_SALES_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct WebSalesRow {
    sold_date_sk: i64,
    sold_time_sk: i64,
    ship_date_sk: i64,
    item_sk: i64,
    bill_customer_sk: i64,
    bill_cdemo_sk: i64,
    bill_hdemo_sk: i64,
    bill_addr_sk: i64,
    ship_customer_sk: i64,
    ship_cdemo_sk: i64,
    ship_hdemo_sk: i64,
    ship_addr_sk: i64,
    web_page_sk: i64,
    web_site_sk: i64,
    ship_mode_sk: i64,
    warehouse_sk: i64,
    promo_sk: i64,
    order_number: i64,
    pricing: Pricing,
}

/// A row of `web_returns` (`struct W_WEB_RETURNS_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct WebReturnsRow {
    returned_date_sk: i64,
    returned_time_sk: i64,
    item_sk: i64,
    refunded_customer_sk: i64,
    refunded_cdemo_sk: i64,
    refunded_hdemo_sk: i64,
    refunded_addr_sk: i64,
    returning_customer_sk: i64,
    returning_cdemo_sk: i64,
    returning_hdemo_sk: i64,
    returning_addr_sk: i64,
    web_page_sk: i64,
    reason_sk: i64,
    order_number: i64,
    pricing: Pricing,
}

/// The generator of `web_sales` and `web_returns` (`mk_w_web_sales`, `mk_master`,
/// `mk_detail` and their statics).
#[derive(Debug)]
pub(crate) struct WebSales {
    /// `g_w_web_sales`, the current line item.
    r: WebSalesRow,
    pricing: PricingConstants,
    /// The position of the current line item in the item permutation.
    item_index: i32,
    item_count: i32,
    /// `mk_detail`'s permutation, drawn on its first call.
    item_permutation: Option<Vec<i32>>,
}

impl WebSales {
    /// `mk_master`'s initialization.
    pub(crate) fn new(ctx: &Context) -> Self {
        Self {
            r: WebSalesRow::default(),
            pricing: PricingConstants::new(),
            item_index: 0,
            item_count: ctx.get_id_count(ITEM) as i32,
            item_permutation: None,
        }
    }

    /// `mk_w_web_sales`: an order and its line items.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        self.mk_master(ctx, index);
        // The number of line items of the order.
        let line_items = ctx.rng.genrand_integer(DIST_UNIFORM, 8, 16, 9, WS_ORDER_NUMBER);
        for _ in 0..line_items {
            self.mk_detail(ctx, out)?;
        }
        Ok(())
    }

    /// `mk_master`: the fields that are the same for every line item of an order.
    ///
    /// The original also moves a date forward by the orders of each day (`dateScaling`),
    /// which nothing reads: the sales draw their dates. It is not kept, as at tiny scale
    /// factors no day has an order and DuckDB never stops moving it.
    fn mk_master(&mut self, ctx: &mut Context, index: i64) {
        let r = &mut self.r;
        r.sold_date_sk = ctx.mk_join(WS_SOLD_DATE_SK, DATET, 1);
        r.sold_time_sk = ctx.mk_join(WS_SOLD_TIME_SK, TIME, 1);
        r.bill_customer_sk = ctx.mk_join(WS_BILL_CUSTOMER_SK, CUSTOMER, 1);
        r.bill_cdemo_sk = ctx.mk_join(WS_BILL_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        r.bill_hdemo_sk = ctx.mk_join(WS_BILL_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        r.bill_addr_sk = ctx.mk_join(WS_BILL_ADDR_SK, CUSTOMER_ADDRESS, 1);

        // Most orders are for the ordering customers, some are not.
        let gift_pct = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 0, 99, 0, WS_SHIP_CUSTOMER_SK);
        if gift_pct > WS_GIFT_PCT {
            r.ship_customer_sk = ctx.mk_join(WS_SHIP_CUSTOMER_SK, CUSTOMER, 2);
            r.ship_cdemo_sk = ctx.mk_join(WS_SHIP_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 2);
            r.ship_hdemo_sk = ctx.mk_join(WS_SHIP_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 2);
            r.ship_addr_sk = ctx.mk_join(WS_SHIP_ADDR_SK, CUSTOMER_ADDRESS, 2);
        } else {
            r.ship_customer_sk = r.bill_customer_sk;
            r.ship_cdemo_sk = r.bill_cdemo_sk;
            r.ship_hdemo_sk = r.bill_hdemo_sk;
            r.ship_addr_sk = r.bill_addr_sk;
        }

        r.order_number = index;
        self.item_index = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, self.item_count, 0, WS_ITEM_SK);
    }

    /// `mk_detail`: a line item, and its return if it is returned.
    fn mk_detail(&mut self, ctx: &mut Context, out: &mut Output<'_>) -> Result<()> {
        if self.item_permutation.is_none() {
            self.item_permutation = Some(make_permutation(&mut ctx.rng, self.item_count, WS_PERMUTATION));
        }
        ctx.null_set(WEB_SALES, WS_NULLS);
        let r = &mut self.r;

        // Orders are shipped some number of days after they are ordered, and not all line
        // items ship at the same time.
        let ship_lag = ctx.rng.genrand_integer(
            DIST_UNIFORM,
            WS_MIN_SHIP_DELAY,
            WS_MAX_SHIP_DELAY,
            0,
            WS_SHIP_DATE_SK,
        );
        r.ship_date_sk = r.sold_date_sk + i64::from(ship_lag);

        self.item_index += 1;
        if self.item_index > self.item_count {
            self.item_index = 1;
        }
        let permutation = self.item_permutation.as_deref().unwrap_or_default();
        let item = get_permutation_entry(permutation, self.item_index);
        r.item_sk = ctx.match_scd_sk(i64::from(item), r.sold_date_sk, ITEM);

        // The web page needs to be valid for the sale date.
        r.web_page_sk = ctx.mk_join(WS_WEB_PAGE_SK, WEB_PAGE, r.sold_date_sk);
        r.web_site_sk = ctx.mk_join(WS_WEB_SITE_SK, WEB_SITE, r.sold_date_sk);

        r.ship_mode_sk = ctx.mk_join(WS_SHIP_MODE_SK, SHIP_MODE, 1);
        r.warehouse_sk = ctx.mk_join(WS_WAREHOUSE_SK, WAREHOUSE, 1);
        r.promo_sk = ctx.mk_join(WS_PROMO_SK, PROMOTION, 1);
        self.pricing.set_pricing(&mut ctx.rng, WS_PRICING, &mut r.pricing);

        // Having made the sale, see if it gets returned.
        let returned = ctx.rng.genrand_integer(DIST_UNIFORM, 0, 99, 0, WR_IS_RETURNED);
        if returned < WR_RETURN_PCT {
            let rr = self.mk_return(ctx);
            let p = &rr.pricing;
            let row = out.row(WEB_RETURNS).start(&ctx.null_bit_maps);
            row.key(rr.returned_date_sk)?;
            row.key(rr.returned_time_sk)?;
            row.key(rr.item_sk)?;
            row.key(rr.refunded_customer_sk)?;
            row.key(rr.refunded_cdemo_sk)?;
            row.key(rr.refunded_hdemo_sk)?;
            row.key(rr.refunded_addr_sk)?;
            row.key(rr.returning_customer_sk)?;
            row.key(rr.returning_cdemo_sk)?;
            row.key(rr.returning_hdemo_sk)?;
            row.key(rr.returning_addr_sk)?;
            row.key(rr.web_page_sk)?;
            row.key(rr.reason_sk)?;
            row.key(rr.order_number)?;
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
            out.end(WEB_RETURNS)?;
        }

        let r = &self.r;
        let p = &r.pricing;
        let row = out.row(WEB_SALES).start(&ctx.null_bit_maps);
        row.key(r.sold_date_sk)?;
        row.key(r.sold_time_sk)?;
        row.key(r.ship_date_sk)?;
        row.key(r.item_sk)?;
        row.key(r.bill_customer_sk)?;
        row.key(r.bill_cdemo_sk)?;
        row.key(r.bill_hdemo_sk)?;
        row.key(r.bill_addr_sk)?;
        row.key(r.ship_customer_sk)?;
        row.key(r.ship_cdemo_sk)?;
        row.key(r.ship_hdemo_sk)?;
        row.key(r.ship_addr_sk)?;
        row.key(r.web_page_sk)?;
        row.key(r.web_site_sk)?;
        row.key(r.ship_mode_sk)?;
        row.key(r.warehouse_sk)?;
        row.key(r.promo_sk)?;
        row.key(r.order_number)?;
        row.integer(p.quantity)?;
        row.decimal(&p.wholesale_cost)?;
        row.decimal(&p.list_price)?;
        row.decimal(&p.sales_price)?;
        row.decimal(&p.ext_discount_amt)?;
        row.decimal(&p.ext_sales_price)?;
        row.decimal(&p.ext_wholesale_cost)?;
        row.decimal(&p.ext_list_price)?;
        row.decimal(&p.ext_tax)?;
        row.decimal(&p.coupon_amt)?;
        row.decimal(&p.ext_ship_cost)?;
        row.decimal(&p.net_paid)?;
        row.decimal(&p.net_paid_inc_tax)?;
        row.decimal(&p.net_paid_inc_ship)?;
        row.decimal(&p.net_paid_inc_ship_tax)?;
        row.decimal(&p.net_profit)?;
        out.end(WEB_SALES)
    }

    /// `mk_w_web_returns`: the return of the current line item.
    fn mk_return(&self, ctx: &mut Context) -> WebReturnsRow {
        let sale = &self.r;
        ctx.null_set(WEB_RETURNS, WR_NULLS);
        // Some of the information in the return is taken from the sale.
        let mut r = WebReturnsRow {
            item_sk: sale.item_sk,
            order_number: sale.order_number,
            pricing: sale.pricing,
            web_page_sk: sale.web_page_sk,
            ..WebReturnsRow::default()
        };

        // The rest of the columns are generated for this return. Items cannot be returned
        // before they are shipped; `mk_join` adds the lag to the ship date.
        r.returned_date_sk = ctx.mk_join(WR_RETURNED_DATE_SK, DATET, sale.ship_date_sk);
        r.returned_time_sk = ctx.mk_join(WR_RETURNED_TIME_SK, TIME, 1);

        // Most items are returned by the people they were shipped to, some by others.
        r.refunded_customer_sk = ctx.mk_join(WR_REFUNDED_CUSTOMER_SK, CUSTOMER, 1);
        r.refunded_cdemo_sk = ctx.mk_join(WR_REFUNDED_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        r.refunded_hdemo_sk = ctx.mk_join(WR_REFUNDED_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        r.refunded_addr_sk = ctx.mk_join(WR_REFUNDED_ADDR_SK, CUSTOMER_ADDRESS, 1);
        if ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 0, 99, 0, WR_RETURNING_CUSTOMER_SK)
            < WS_GIFT_PCT
        {
            r.refunded_customer_sk = sale.ship_customer_sk;
            r.refunded_cdemo_sk = sale.ship_cdemo_sk;
            r.refunded_hdemo_sk = sale.ship_hdemo_sk;
            r.refunded_addr_sk = sale.ship_addr_sk;
        }
        r.returning_customer_sk = r.refunded_customer_sk;
        r.returning_cdemo_sk = r.refunded_cdemo_sk;
        r.returning_hdemo_sk = r.refunded_hdemo_sk;
        r.returning_addr_sk = r.refunded_addr_sk;

        r.reason_sk = ctx.mk_join(WR_REASON_SK, REASON, 1);
        r.pricing.quantity = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, sale.pricing.quantity, 0, WR_PRICING);
        self.pricing.set_pricing(&mut ctx.rng, WR_PRICING, &mut r.pricing);
        r
    }
}
