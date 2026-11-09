//! The `customer_address` table (`w_customer_address.c`).

use roastedduck_common::Result;

use super::Output;
use super::address::Address;
use super::build_support::{bkey_str, mk_bkey};
use super::columns::{CA_ADDRESS, CA_LOCATION_TYPE, CA_NULLS};
use super::context::Context;
use super::dist::{Dist, dist};
use super::tables::CUSTOMER_ADDRESS;

/// The generator of `customer_address` (`mk_w_customer_address`).
#[derive(Debug)]
pub(crate) struct CustomerAddress {
    address: Address,
    location_type: &'static Dist,
    street_name: String,
    zip: String,
}

impl CustomerAddress {
    pub(crate) fn new() -> Self {
        Self {
            address: Address::default(),
            location_type: dist("location_type"),
            street_name: String::new(),
            zip: String::new(),
        }
    }

    /// `mk_w_customer_address`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(CUSTOMER_ADDRESS, CA_NULLS);
        let addr_id = mk_bkey(index);
        let (location_type, _) = self.location_type.pick_str(&mut ctx.rng, 1, 1, CA_LOCATION_TYPE);
        ctx.mk_address(&mut self.address, CA_ADDRESS);

        let address = &self.address;
        address.street_name(&mut self.street_name);
        address.zip_code(&mut self.zip);
        let row = out.row(CUSTOMER_ADDRESS).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&addr_id))?;
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
        row.varchar(location_type)?;
        out.end(CUSTOMER_ADDRESS)
    }
}
