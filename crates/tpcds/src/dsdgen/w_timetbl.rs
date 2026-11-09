//! The `time_dim` and `warehouse` tables (`w_timetbl.c`, `w_warehouse.c`).

use roastedduck_common::Result;

use super::Output;
use super::address::Address;
use super::build_support::{bkey_str, mk_bkey};
use super::columns::{T_NULLS, W_NULLS, W_WAREHOUSE_ADDRESS, W_WAREHOUSE_NAME, W_WAREHOUSE_SQ_FT};
use super::constants::{RS_W_WAREHOUSE_NAME, W_NAME_MIN, W_SQFT_MAX, W_SQFT_MIN};
use super::context::Context;
use super::dist::{Dist, dist};
use super::genrand::DIST_UNIFORM;
use super::tables::{TIME, WAREHOUSE};

/// The generator of `time_dim` (`mk_w_time`).
#[derive(Debug)]
pub(crate) struct TimeDim {
    hours: &'static Dist,
}

impl TimeDim {
    pub(crate) fn new() -> Self {
        Self { hours: dist("hours") }
    }

    /// `mk_w_time`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(TIME, T_NULLS);
        let time_sk = index - 1;
        let time_id = mk_bkey(index);
        let time = (index - 1) as i32;
        let mut temp = (index - 1) as i32;
        let second = temp % 60;
        temp /= 60;
        let minute = temp % 60;
        temp /= 60;
        let hour = temp % 24;
        let hours = self.hours;
        let am_pm = hours.member_str(hour + 1, 2);
        let shift = hours.member_str(hour + 1, 3);
        let sub_shift = hours.member_str(hour + 1, 4);
        let meal_time = hours.member_str(hour + 1, 5);

        let row = out.row(TIME).start(&ctx.null_bit_maps);
        row.key(time_sk)?;
        row.varchar(bkey_str(&time_id))?;
        row.integer(time)?;
        row.integer(hour)?;
        row.integer(minute)?;
        row.integer(second)?;
        row.varchar(am_pm)?;
        row.varchar(shift)?;
        row.varchar(sub_shift)?;
        row.varchar(meal_time)?;
        out.end(TIME)
    }
}

/// The generator of `warehouse` (`mk_w_warehouse`).
#[derive(Debug, Default)]
pub(crate) struct Warehouse {
    name: String,
    address: Address,
    street_name: String,
    zip: String,
}

impl Warehouse {
    /// `mk_w_warehouse`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(WAREHOUSE, W_NULLS);
        let warehouse_id = mk_bkey(index);
        ctx.gen_text(&mut self.name, W_NAME_MIN, RS_W_WAREHOUSE_NAME, W_WAREHOUSE_NAME);
        let sq_ft = ctx
            .rng
            .genrand_integer(DIST_UNIFORM, W_SQFT_MIN, W_SQFT_MAX, 0, W_WAREHOUSE_SQ_FT);
        ctx.mk_address(&mut self.address, W_WAREHOUSE_ADDRESS);

        let address = &self.address;
        address.street_name(&mut self.street_name);
        address.zip_code(&mut self.zip);
        let row = out.row(WAREHOUSE).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&warehouse_id))?;
        row.varchar(&self.name)?;
        row.integer(sq_ft)?;
        row.integer(address.street_num)?;
        row.varchar(&self.street_name)?;
        row.varchar(address.street_type)?;
        row.varchar(&address.suite_num)?;
        row.varchar(address.city)?;
        row.varchar(address.county)?;
        row.varchar(address.state)?;
        row.varchar(&self.zip)?;
        row.varchar(address.country)?;
        row.integer_decimal(address.gmt_offset)?;
        out.end(WAREHOUSE)
    }
}
