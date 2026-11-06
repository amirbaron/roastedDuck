//! The prices of the sales and returns (`pricing.c`).

use super::columns::{CR_PRICING, CS_PRICING, SR_PRICING, SS_PRICING, WR_PRICING, WS_PRICING};
use super::constants::{
    CS_DISCOUNT_MAX, CS_MARKUP_MAX, CS_QUANTITY_MAX, CS_WHOLESALE_MAX, SS_DISCOUNT_MAX, SS_MARKUP_MAX,
    SS_QUANTITY_MAX, SS_WHOLESALE_MAX,
};
use super::decimal::{Decimal, DecimalOp, atoi};
use super::genrand::{DIST_UNIFORM, Streams};
use super::w_web_sales::{WS_DISCOUNT_MAX, WS_MARKUP_MAX, WS_QUANTITY_MAX, WS_WHOLESALE_MAX};

/// `ds_pricing_t`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Pricing {
    pub(crate) wholesale_cost: Decimal,
    pub(crate) list_price: Decimal,
    pub(crate) sales_price: Decimal,
    pub(crate) quantity: i32,
    pub(crate) ext_discount_amt: Decimal,
    pub(crate) ext_sales_price: Decimal,
    pub(crate) ext_wholesale_cost: Decimal,
    pub(crate) ext_list_price: Decimal,
    pub(crate) tax_pct: Decimal,
    pub(crate) ext_tax: Decimal,
    pub(crate) coupon_amt: Decimal,
    pub(crate) ship_cost: Decimal,
    pub(crate) ext_ship_cost: Decimal,
    pub(crate) net_paid: Decimal,
    pub(crate) net_paid_inc_tax: Decimal,
    pub(crate) net_paid_inc_ship: Decimal,
    pub(crate) net_paid_inc_ship_tax: Decimal,
    pub(crate) net_profit: Decimal,
    pub(crate) refunded_cash: Decimal,
    pub(crate) reversed_charge: Decimal,
    pub(crate) store_credit: Decimal,
    pub(crate) fee: Decimal,
    pub(crate) net_loss: Decimal,
}

/// The maximums of the prices of a channel (`ds_limits_t`).
#[derive(Clone, Copy, Debug)]
struct Limits {
    quantity: i32,
    markup: Decimal,
    discount: Decimal,
    wholesale: Decimal,
}

impl Limits {
    /// The coupon limit of `ds_limits_t` is never read: coupons are in `[0, 1]`.
    fn new(quantity: &str, markup: &str, discount: &str, wholesale: &str) -> Self {
        Self {
            quantity: atoi(quantity),
            markup: Decimal::from_str(markup),
            discount: Decimal::from_str(discount),
            wholesale: Decimal::from_str(wholesale),
        }
    }
}

/// `set_pricing`'s constants, and the limits of each channel (`aPriceLimits`).
#[derive(Debug)]
pub(crate) struct PricingConstants {
    markup_min: Decimal,
    discount_min: Decimal,
    wholesale_min: Decimal,
    zero: Decimal,
    one_half: Decimal,
    nine_pct: Decimal,
    one: Decimal,
    hundred: Decimal,
    catalog: Limits,
    store: Limits,
    web: Limits,
}

/// `nQuantityMin`.
const QUANTITY_MIN: i32 = 1;

impl PricingConstants {
    pub(crate) fn new() -> Self {
        Self {
            markup_min: Decimal::from_str("0.00"),
            discount_min: Decimal::from_str("0.00"),
            wholesale_min: Decimal::from_str("1.00"),
            zero: Decimal::from_str("0.00"),
            one_half: Decimal::from_str("0.50"),
            nine_pct: Decimal::from_str("0.09"),
            one: Decimal::from_str("1.00"),
            hundred: Decimal::from_str("100.00"),
            catalog: Limits::new(CS_QUANTITY_MAX, CS_MARKUP_MAX, CS_DISCOUNT_MAX, CS_WHOLESALE_MAX),
            store: Limits::new(SS_QUANTITY_MAX, SS_MARKUP_MAX, SS_DISCOUNT_MAX, SS_WHOLESALE_MAX),
            web: Limits::new(WS_QUANTITY_MAX, WS_MARKUP_MAX, WS_DISCOUNT_MAX, WS_WHOLESALE_MAX),
        }
    }

    /// `set_pricing`: the prices of a sale (`SS_PRICING`, `CS_PRICING`, `WS_PRICING`), or
    /// of the return of a sale whose prices `pricing` holds, with its quantity set
    /// (`SR_PRICING`, `CR_PRICING`, `WR_PRICING`).
    pub(crate) fn set_pricing(&self, rng: &mut Streams, tab_id: usize, pricing: &mut Pricing) {
        let limits = match tab_id {
            CS_PRICING | CR_PRICING => &self.catalog,
            SS_PRICING | SR_PRICING => &self.store,
            WS_PRICING | WR_PRICING => &self.web,
            _ => unreachable!("No pricing limits defined"),
        };
        match tab_id {
            SS_PRICING | CS_PRICING | WS_PRICING => self.sale(rng, tab_id, limits, pricing),
            _ => self.return_(rng, tab_id, pricing),
        }
    }

    fn sale(&self, rng: &mut Streams, tab_id: usize, limits: &Limits, p: &mut Pricing) {
        use DecimalOp::{Minus, Mult, Plus};
        p.quantity = rng.genrand_integer(DIST_UNIFORM, QUANTITY_MIN, limits.quantity, 0, tab_id);
        let quantity = Decimal::from_int(p.quantity);
        p.wholesale_cost =
            rng.genrand_decimal(DIST_UNIFORM, &self.wholesale_min, &limits.wholesale, None, tab_id);

        // ext_wholesale_cost = wholesale_cost * quantity
        p.ext_wholesale_cost = Decimal::op(Mult, &quantity, &p.wholesale_cost);

        // list_price = wholesale_cost * (1 + markup)
        let mut markup = rng.genrand_decimal(DIST_UNIFORM, &self.markup_min, &limits.markup, None, tab_id);
        markup.op_assign(Plus, &self.one);
        p.list_price = Decimal::op(Mult, &p.wholesale_cost, &markup);

        // sales_price = list_price * (1 - discount)
        let mut discount =
            rng.genrand_decimal(DIST_UNIFORM, &self.discount_min, &limits.discount, None, tab_id);
        discount.negate();
        p.ext_discount_amt = Decimal::op(Plus, &discount, &self.one);
        p.sales_price = Decimal::op(Mult, &p.list_price, &p.ext_discount_amt);

        // ext_list_price = list_price * quantity
        p.ext_list_price = Decimal::op(Mult, &p.list_price, &quantity);

        // ext_sales_price = sales_price * quantity
        p.ext_sales_price = Decimal::op(Mult, &p.sales_price, &quantity);

        // ext_discount_amt = ext_list_price - ext_sales_price
        p.ext_discount_amt = Decimal::op(Minus, &p.ext_list_price, &p.ext_sales_price);

        // coupon_amt = ext_sales_price * coupon; 20% of sales employ a coupon
        let coupon = rng.genrand_decimal(DIST_UNIFORM, &self.zero, &self.one, None, tab_id);
        let coupon_usage = rng.genrand_integer(DIST_UNIFORM, 1, 100, 0, tab_id);
        p.coupon_amt = if coupon_usage <= 20 {
            Decimal::op(Mult, &p.ext_sales_price, &coupon)
        } else {
            self.zero
        };

        // net_paid = ext_sales_price - coupon_amt
        p.net_paid = Decimal::op(Minus, &p.ext_sales_price, &p.coupon_amt);

        // shipping_cost = list_price * shipping
        let shipping = rng.genrand_decimal(DIST_UNIFORM, &self.zero, &self.one_half, None, tab_id);
        p.ship_cost = Decimal::op(Mult, &p.list_price, &shipping);

        // ext_shipping_cost = shipping_cost * quantity
        p.ext_ship_cost = Decimal::op(Mult, &p.ship_cost, &quantity);

        // net_paid_inc_ship = net_paid + ext_shipping_cost
        p.net_paid_inc_ship = Decimal::op(Plus, &p.net_paid, &p.ext_ship_cost);

        // ext_tax = tax * net_paid
        p.tax_pct = rng.genrand_decimal(DIST_UNIFORM, &self.zero, &self.nine_pct, None, tab_id);
        p.ext_tax = Decimal::op(Mult, &p.net_paid, &p.tax_pct);

        // net_paid_inc_tax = net_paid + ext_tax
        p.net_paid_inc_tax = Decimal::op(Plus, &p.net_paid, &p.ext_tax);

        // net_paid_inc_ship_tax = net_paid_inc_tax + ext_shipping_cost
        p.net_paid_inc_ship_tax = Decimal::op(Plus, &p.net_paid_inc_ship, &p.ext_tax);

        // net_profit = net_paid - ext_wholesale_cost
        p.net_profit = Decimal::op(Minus, &p.net_paid, &p.ext_wholesale_cost);
    }

    fn return_(&self, rng: &mut Streams, tab_id: usize, p: &mut Pricing) {
        use DecimalOp::{Div, Minus, Mult, Plus};
        // The quantity is set before the call.
        let quantity = Decimal::from_int(p.quantity);
        // ext_wholesale_cost = wholesale_cost * quantity
        p.ext_wholesale_cost = Decimal::op(Mult, &quantity, &p.wholesale_cost);

        // ext_list_price = list_price * quantity
        p.ext_list_price = Decimal::op(Mult, &p.list_price, &quantity);

        // ext_sales_price = sales_price * quantity
        p.ext_sales_price = Decimal::op(Mult, &p.sales_price, &quantity);

        // net_paid = ext_list_price (coupons don't effect returns)
        p.net_paid = p.ext_sales_price;

        // shipping_cost = list_price * shipping
        let shipping = rng.genrand_decimal(DIST_UNIFORM, &self.zero, &self.one_half, None, tab_id);
        p.ship_cost = Decimal::op(Mult, &p.list_price, &shipping);

        // ext_shipping_cost = shipping_cost * quantity
        p.ext_ship_cost = Decimal::op(Mult, &p.ship_cost, &quantity);

        // net_paid_inc_ship = net_paid + ext_shipping_cost
        p.net_paid_inc_ship = Decimal::op(Plus, &p.net_paid, &p.ext_ship_cost);

        // ext_tax = tax * net_paid
        p.ext_tax = Decimal::op(Mult, &p.net_paid, &p.tax_pct);

        // net_paid_inc_tax = net_paid + ext_tax
        p.net_paid_inc_tax = Decimal::op(Plus, &p.net_paid, &p.ext_tax);

        // net_paid_inc_ship_tax = net_paid_inc_tax + ext_shipping_cost
        p.net_paid_inc_ship_tax = Decimal::op(Plus, &p.net_paid_inc_ship, &p.ext_tax);

        // net_profit = net_paid - ext_wholesale_cost
        p.net_profit = Decimal::op(Minus, &p.net_paid, &p.ext_wholesale_cost);

        // See to it that the returned amounts add up to the total returned: some of the
        // return is cash,
        let cash_pct = rng.genrand_integer(DIST_UNIFORM, 0, 100, 0, tab_id);
        let temp = Decimal::from_int(cash_pct);
        p.refunded_cash = Decimal::op(Div, &temp, &self.hundred);
        p.refunded_cash.op_assign(Mult, &p.net_paid);

        // some is reversed charges,
        let credit_pct = rng.genrand_integer(DIST_UNIFORM, 1, 100, 0, tab_id);
        let temp2 = Decimal::from_int(credit_pct);
        let temp = Decimal::op(Div, &temp2, &self.hundred);
        let temp2 = Decimal::op(Minus, &p.net_paid, &p.refunded_cash);
        p.reversed_charge = Decimal::op(Mult, &temp2, &temp);

        // and the rest is store credit.
        p.store_credit = Decimal::op(Minus, &p.net_paid, &p.reversed_charge);
        p.store_credit.op_assign(Minus, &p.refunded_cash);

        // Pick a fee for the return,
        p.fee = rng.genrand_decimal(
            DIST_UNIFORM,
            &self.one_half,
            &self.hundred,
            Some(&self.zero),
            tab_id,
        );

        // and calculate the net effect.
        p.net_loss = Decimal::op(Minus, &p.net_paid_inc_ship_tax, &p.store_credit);
        p.net_loss.op_assign(Minus, &p.refunded_cash);
        p.net_loss.op_assign(Minus, &p.reversed_charge);
        p.net_loss.op_assign(Plus, &p.fee);
    }
}
