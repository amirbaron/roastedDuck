//! The `customer` table (`w_customer.c`).

use roastedduck_common::Result;

use super::Output;
use super::build_support::{bkey_str, mk_bkey};
use super::columns::{
    C_BIRTH_COUNTRY, C_BIRTH_DAY, C_CURRENT_ADDR_SK, C_CURRENT_CDEMO_SK, C_CURRENT_HDEMO_SK, C_EMAIL_ADDRESS,
    C_FIRST_NAME, C_FIRST_SALES_DATE_ID, C_LAST_NAME, C_LAST_REVIEW_DATE, C_NULLS, C_PREFERRED_CUST_FLAG,
    C_SALUTATION,
};
use super::constants::TODAYS_DATE;
use super::context::Context;
use super::date::Date;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::tables::{CUSTOMER, CUSTOMER_ADDRESS, CUSTOMER_DEMOGRAPHICS, HOUSEHOLD_DEMOGRAPHICS};

/// `C_PREFERRED_PCT`.
const C_PREFERRED_PCT: i32 = 50;

/// The generator of `customer` (`mk_w_customer` and its statics).
#[derive(Debug)]
pub(crate) struct Customer {
    birth_min: Date,
    birth_max: Date,
    today: Date,
    one_year_ago: Date,
    ten_years_ago: Date,
    salutations: &'static Dist,
    countries: &'static Dist,
    email: String,
}

impl Customer {
    pub(crate) fn new() -> Self {
        let today = Date::parse(TODAYS_DATE);
        Self {
            birth_min: Date::parse("1924-01-01"),
            birth_max: Date::parse("1992-12-31"),
            today,
            one_year_ago: Date::from_julian(today.julian - 365),
            ten_years_ago: Date::from_julian(today.julian - 3650),
            salutations: dist("salutations"),
            countries: dist("countries"),
            email: String::new(),
        }
    }

    /// `mk_w_customer`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(CUSTOMER, C_NULLS);
        let customer_id = mk_bkey(index);
        let temp = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, 1, 100, 0, C_PREFERRED_CUST_FLAG);
        let preferred_cust_flag = temp < C_PREFERRED_PCT;

        // The demographic keys are a composite of values; rebuild them a la bitmap_to_dist.
        let current_hdemo_sk = ctx.mk_join(C_CURRENT_HDEMO_SK, HOUSEHOLD_DEMOGRAPHICS, 1);
        let current_cdemo_sk = ctx.mk_join(C_CURRENT_CDEMO_SK, CUSTOMER_DEMOGRAPHICS, 1);
        let current_addr_sk = ctx.mk_join(C_CURRENT_ADDR_SK, CUSTOMER_ADDRESS, index);
        let (first_name, name_index) = ctx.dists.first_names.pick_str(&mut ctx.rng, 1, 3, C_FIRST_NAME);
        let (last_name, _) = ctx.dists.last_names.pick_str(&mut ctx.rng, 1, 1, C_LAST_NAME);
        let gender = ctx.dists.first_names.weight(name_index, 2);
        let (salutation, _) =
            self.salutations
                .pick_str(&mut ctx.rng, 1, if gender == 0 { 2 } else { 3 }, C_SALUTATION);

        let birth = ctx.genrand_date(&self.birth_min, &self.birth_max, C_BIRTH_DAY);
        ctx.genrand_email(&mut self.email, first_name, last_name, C_EMAIL_ADDRESS);
        let last_review_date = ctx
            .genrand_date(&self.one_year_ago, &self.today, C_LAST_REVIEW_DATE)
            .julian;
        let first_sales_date_id = ctx
            .genrand_date(&self.ten_years_ago, &self.today, C_FIRST_SALES_DATE_ID)
            .julian;
        let first_shipto_date_id = first_sales_date_id + 30;

        let (birth_country, _) = self.countries.pick_str(&mut ctx.rng, 1, 1, C_BIRTH_COUNTRY);

        let row = out.row(CUSTOMER).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&customer_id))?;
        row.key(current_cdemo_sk)?;
        row.key(current_hdemo_sk)?;
        row.key(current_addr_sk)?;
        row.integer(first_shipto_date_id)?;
        row.integer(first_sales_date_id)?;
        row.varchar(salutation)?;
        row.varchar(first_name)?;
        row.varchar(last_name)?;
        row.varchar(if preferred_cust_flag { "Y" } else { "N" })?;
        row.integer(birth.day)?;
        row.integer(birth.month)?;
        row.integer(birth.year)?;
        row.varchar(birth_country)?;
        // `c_login` is never set.
        row.varchar("")?;
        row.varchar(&self.email)?;
        row.integer(last_review_date)?;
        out.end(CUSTOMER)
    }
}
