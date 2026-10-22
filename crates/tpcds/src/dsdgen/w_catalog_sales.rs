//! The `catalog_sales` table and its child `catalog_returns` (`w_catalog_sales.c`,
//! `w_catalog_returns.c`).

use rustdb_common::Result;

use super::Output;
use super::build_support::{get_permutation_entry, make_permutation};
use super::columns::{
    CR_IS_RETURNED, CR_NULLS, CR_PRICING, CR_REASON_SK, CR_RETURNED_DATE_SK, CR_RETURNED_TIME_SK,
    CR_RETURNING_ADDR_SK, CR_RETURNING_CDEMO_SK, CR_RETURNING_CUSTOMER_SK, CR_RETURNING_HDEMO_SK,
    CR_SHIP_MODE_SK, CR_WAREHOUSE_SK, CS_BILL_ADDR_SK, CS_BILL_CDEMO_SK, CS_BILL_CUSTOMER_SK,
    CS_BILL_HDEMO_SK, CS_CALL_CENTER_SK, CS_CATALOG_PAGE_SK, CS_NULLS, CS_ORDER_NUMBER, CS_PERMUTE,
    CS_PRICING, CS_PROMO_SK, CS_SHIP_ADDR_SK, CS_SHIP_CDEMO_SK, CS_SHIP_CUSTOMER_SK, CS_SHIP_DATE_SK,
    CS_SHIP_HDEMO_SK, CS_SHIP_MODE_SK, CS_SOLD_ITEM_SK, CS_SOLD_TIME_SK, CS_WAREHOUSE_SK,
};
use super::constants::{CR_RETURN_PCT, CS_GIFT_PCT, CS_MAX_SHIP_DELAY, CS_MIN_SHIP_DELAY};
use super::context::Context;
use super::genrand::DIST_UNIFORM;
use super::pricing::{Pricing, PricingConstants};
use super::skip_days::skip_days;
use super::tables::{
    CALL_CENTER, CATALOG_PAGE, CATALOG_RETURNS, CATALOG_SALES, CUSTOMER, CUSTOMER_ADDRESS,
    CUSTOMER_DEMOGRAPHICS, DATET, HOUSEHOLD_DEMOGRAPHICS, ITEM, PROMOTION, REASON, SHIP_MODE, TIME,
    WAREHOUSE,
};

/// A row of `catalog_sales` (`struct W_CATALOG_SALES_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct CatalogSalesRow {
    sold_date_sk: i64,
    sold_time_sk: i64,
    ship_date_sk: i64,
    bill_customer_sk: i64,
    bill_cdemo_sk: i64,
    bill_hdemo_sk: i64,
    bill_addr_sk: i64,
    ship_customer_sk: i64,
    ship_cdemo_sk: i64,
    ship_hdemo_sk: i64,
    ship_addr_sk: i64,
    call_center_sk: i64,
    catalog_page_sk: i64,
    ship_mode_sk: i64,
    warehouse_sk: i64,
    sold_item_sk: i64,
    promo_sk: i64,
    order_number: i64,
    pricing: Pricing,
}

/// A row of `catalog_returns` (`struct W_CATALOG_RETURNS_TBL`).
#[derive(Clone, Copy, Debug, Default)]
struct CatalogReturnsRow {
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
    call_center_sk: i64,
    catalog_page_sk: i64,
    ship_mode_sk: i64,
    warehouse_sk: i64,
    reason_sk: i64,
    order_number: i64,
    pricing: Pricing,
}

/// The generator of `catalog_sales` and `catalog_returns` (`mk_w_catalog_sales`,
/// `mk_master`, `mk_detail` and their statics).
#[derive(Debug)]
pub(crate) struct CatalogSales {
    /// `g_w_catalog_sales`, the current line item.
    r: CatalogSalesRow,
    pricing: PricingConstants,
    /// The date of the current order and the last order number of that date.
    date: i64,
    new_date_index: i64,
    /// The position of the current line item in the item permutation.
    ticket_item_base: i32,
    item_permutation: Vec<i32>,
    item_count: i32,
}

impl CatalogSales {
    /// `mk_master`'s initialization, which draws the item permutation.
    pub(crate) fn new(ctx: &mut Context) -> Self {
        let mut new_date_index = 0;
        let date = skip_days(ctx, CATALOG_SALES, &mut new_date_index);
        let item_count = ctx.get_id_count(ITEM) as i32;
        let item_permutation = make_permutation(&mut ctx.rng, item_count, CS_PERMUTE);
        Self {
            r: CatalogSalesRow::default(),
            pricing: PricingConstants::new(),
            date,
            new_date_index,
            ticket_item_base: 1,
            item_permutation,
            item_count,
        }
    }

    /// `mk_w_catalog_sales`: an order and its line items.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        self.mk_master(ctx, index);
        // The number of line items in the order.
        let line_items = ctx.rng.genrand_integer(DIST_UNIFORM, 4, 14, 0, CS_ORDER_NUMBER);
        for _ in 0..line_items {
            self.mk_detail(ctx, out)?;
        }
        Ok(())
    }

    /// `mk_master`: the fields that are the same for every line item of an order.
    fn mk_master(&mut self, ctx: &mut Context, index: i64) {
        // Move to a new date.
        while index > self.new_date_index {
            self.date += 1;
            self.new_date_index += ctx.date_scaling(CATALOG_SALES, self.date);
        }
        let r = &mut self.r;
        r.sold_date_sk = self.date;
        r.sold_time_sk = ctx.mk_join(CS_SOLD_TIME_SK, TIME, r.call_center_sk);
        r.call_center_sk = if r.sold_date_sk == -1 {
            -1
        } else {
            ctx.mk_join(CS_CALL_CENTER_SK, CALL_CENTER, r.sold_date_sk)
        };

        r.bill_customer_sk = ctx.mk_join(CS_BILL_CUSTOMER_SK, CUSTOMER, 1);
        r.bill_cdemo_sk = ctx.mk_join(CS_BILL_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        r.bill_hdemo_sk = ctx.mk_join(CS_BILL_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        r.bill_addr_sk = ctx.mk_join(CS_BILL_ADDR_SK, CUSTOMER_ADDRESS, 1);

        // Most orders are for the ordering customers, some are not.
        let gift_pct = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 0, 99, 0, CS_SHIP_CUSTOMER_SK);
        if gift_pct <= CS_GIFT_PCT {
            r.ship_customer_sk = ctx.mk_join(CS_SHIP_CUSTOMER_SK, CUSTOMER, 2);
            r.ship_cdemo_sk = ctx.mk_join(CS_SHIP_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 2);
            r.ship_hdemo_sk = ctx.mk_join(CS_SHIP_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 2);
            r.ship_addr_sk = ctx.mk_join(CS_SHIP_ADDR_SK, CUSTOMER_ADDRESS, 2);
        } else {
            r.ship_customer_sk = r.bill_customer_sk;
            r.ship_cdemo_sk = r.bill_cdemo_sk;
            r.ship_hdemo_sk = r.bill_hdemo_sk;
            r.ship_addr_sk = r.bill_addr_sk;
        }

        r.order_number = index;
        self.ticket_item_base = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, self.item_count, 0, CS_SOLD_ITEM_SK);
    }

    /// `mk_detail`: a line item, and its return if it is returned.
    fn mk_detail(&mut self, ctx: &mut Context, out: &mut Output<'_>) -> Result<()> {
        ctx.null_set(CATALOG_SALES, CS_NULLS);
        let r = &mut self.r;

        // Orders are shipped some number of days after they are ordered.
        let ship_lag = ctx.rng.genrand_integer(
            DIST_UNIFORM,
            CS_MIN_SHIP_DELAY,
            CS_MAX_SHIP_DELAY,
            0,
            CS_SHIP_DATE_SK,
        );
        r.ship_date_sk = if r.sold_date_sk == -1 {
            -1
        } else {
            r.sold_date_sk + i64::from(ship_lag)
        };

        // The items of an order are unique: a sequence within the permutation.
        self.ticket_item_base += 1;
        if self.ticket_item_base > self.item_count {
            self.ticket_item_base = 1;
        }
        let item = get_permutation_entry(&self.item_permutation, self.ticket_item_base);
        r.sold_item_sk = ctx.match_scd_sk(i64::from(item), r.sold_date_sk, ITEM);

        // The catalog page is from a catalog active at the time of the sale.
        r.catalog_page_sk = if r.sold_date_sk == -1 {
            -1
        } else {
            ctx.mk_join(CS_CATALOG_PAGE_SK, CATALOG_PAGE, r.sold_date_sk)
        };

        r.ship_mode_sk = ctx.mk_join(CS_SHIP_MODE_SK, SHIP_MODE, 1);
        r.warehouse_sk = ctx.mk_join(CS_WAREHOUSE_SK, WAREHOUSE, 1);
        r.promo_sk = ctx.mk_join(CS_PROMO_SK, PROMOTION, 1);
        self.pricing.set_pricing(&mut ctx.rng, CS_PRICING, &mut r.pricing);

        // Having made the sale, see if it gets returned.
        let returned = ctx.rng.genrand_integer(DIST_UNIFORM, 0, 99, 0, CR_IS_RETURNED);
        if returned < CR_RETURN_PCT {
            let rr = self.mk_return(ctx);
            let p = &rr.pricing;
            let row = out.row(CATALOG_RETURNS).start(&ctx.null_bit_maps);
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
            row.key(rr.call_center_sk)?;
            row.key(rr.catalog_page_sk)?;
            row.key(rr.ship_mode_sk)?;
            row.key(rr.warehouse_sk)?;
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
            out.end(CATALOG_RETURNS)?;
        }

        let r = &self.r;
        let p = &r.pricing;
        let row = out.row(CATALOG_SALES).start(&ctx.null_bit_maps);
        row.key(r.sold_date_sk)?;
        row.key(r.sold_time_sk)?;
        row.key(r.ship_date_sk)?;
        row.key(r.bill_customer_sk)?;
        row.key(r.bill_cdemo_sk)?;
        row.key(r.bill_hdemo_sk)?;
        row.key(r.bill_addr_sk)?;
        row.key(r.ship_customer_sk)?;
        row.key(r.ship_cdemo_sk)?;
        row.key(r.ship_hdemo_sk)?;
        row.key(r.ship_addr_sk)?;
        row.key(r.call_center_sk)?;
        row.key(r.catalog_page_sk)?;
        row.key(r.ship_mode_sk)?;
        row.key(r.warehouse_sk)?;
        row.key(r.sold_item_sk)?;
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
        out.end(CATALOG_SALES)
    }

    /// `mk_w_catalog_returns`: the return of the current line item.
    fn mk_return(&self, ctx: &mut Context) -> CatalogReturnsRow {
        let sale = &self.r;
        ctx.null_set(CATALOG_RETURNS, CR_NULLS);
        // Some of the information in the return is taken from the sale.
        let mut r = CatalogReturnsRow {
            item_sk: sale.sold_item_sk,
            catalog_page_sk: sale.catalog_page_sk,
            order_number: sale.order_number,
            pricing: sale.pricing,
            refunded_customer_sk: sale.bill_customer_sk,
            refunded_cdemo_sk: sale.bill_cdemo_sk,
            refunded_hdemo_sk: sale.bill_hdemo_sk,
            refunded_addr_sk: sale.bill_addr_sk,
            call_center_sk: sale.call_center_sk,
            ..CatalogReturnsRow::default()
        };

        // Some of the fields are conditionally taken from the sale.
        r.returning_customer_sk = ctx.mk_join(CR_RETURNING_CUSTOMER_SK, CUSTOMER, 2);
        r.returning_cdemo_sk = ctx.mk_join(CR_RETURNING_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 2);
        r.returning_hdemo_sk = ctx.mk_join(CR_RETURNING_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 2);
        r.returning_addr_sk = ctx.mk_join(CR_RETURNING_ADDR_SK, CUSTOMER_ADDRESS, 2);
        if ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 0, 99, 0, CR_RETURNING_CUSTOMER_SK)
            < CS_GIFT_PCT
        {
            r.returning_customer_sk = sale.ship_customer_sk;
            r.returning_cdemo_sk = sale.ship_cdemo_sk;
            // The sale has no ship hdemo; `cr_returning_hdemo_sk` stays.
            r.returning_addr_sk = sale.ship_addr_sk;
        }

        // The rest of the columns are generated for this return. Items cannot be returned
        // before they are shipped; `mk_join` adds the lag to the ship date.
        r.returned_date_sk = ctx.mk_join(CR_RETURNED_DATE_SK, DATET, sale.ship_date_sk);
        // The call center determines the time of the return.
        r.returned_time_sk = ctx.mk_join(CR_RETURNED_TIME_SK, TIME, 1);
        r.ship_mode_sk = ctx.mk_join(CR_SHIP_MODE_SK, SHIP_MODE, 1);
        r.warehouse_sk = ctx.mk_join(CR_WAREHOUSE_SK, WAREHOUSE, 1);
        r.reason_sk = ctx.mk_join(CR_REASON_SK, REASON, 1);
        r.pricing.quantity = if sale.pricing.quantity == -1 {
            -1
        } else {
            ctx.rng
                .genrand_integer(DIST_UNIFORM, 1, sale.pricing.quantity, 0, CR_PRICING)
        };
        self.pricing.set_pricing(&mut ctx.rng, CR_PRICING, &mut r.pricing);
        r
    }
}
