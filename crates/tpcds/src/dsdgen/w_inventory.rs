//! The `inventory` table (`w_inventory.c`): the quantity of every item in every
//! warehouse, weekly.

use rustdb_common::Result;

use super::Output;
use super::columns::{INV_NULLS, INV_QUANTITY_ON_HAND};
use super::constants::{DATE_MINIMUM, INV_QUANTITY_MAX, INV_QUANTITY_MIN};
use super::context::Context;
use super::date::Date;
use super::genrand::DIST_UNIFORM;
use super::tables::{INVENTORY, ITEM, WAREHOUSE};

/// The generator of `inventory` (`mk_w_inventory` and its statics).
#[derive(Debug)]
pub(crate) struct Inventory {
    item_count: i64,
    warehouse_count: i64,
    base_date: i32,
}

impl Inventory {
    pub(crate) fn new(ctx: &Context) -> Self {
        Self {
            item_count: ctx.get_id_count(ITEM),
            warehouse_count: ctx.get_rowcount(WAREHOUSE),
            base_date: Date::parse(DATE_MINIMUM).julian,
        }
    }

    /// `mk_w_inventory`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(INVENTORY, INV_NULLS);
        let mut temp = (index as i32).wrapping_sub(1);
        let item_sk = (i64::from(temp) % self.item_count) + 1;
        temp /= self.item_count as i32;
        let warehouse_sk = (i64::from(temp) % self.warehouse_count) + 1;
        temp /= self.warehouse_count as i32;
        // Inventory is updated weekly.
        let date_sk = i64::from(self.base_date.wrapping_add(temp.wrapping_mul(7)));

        // The item id selected above identifies a unique part, but item is a slowly
        // changing dimension: join to the revision current on the date.
        let item_sk = ctx.match_scd_sk(item_sk, date_sk, ITEM);
        let quantity_on_hand = ctx.rng.genrand_integer(
            DIST_UNIFORM,
            INV_QUANTITY_MIN,
            INV_QUANTITY_MAX,
            0,
            INV_QUANTITY_ON_HAND,
        );

        let row = out.row(INVENTORY).start(&ctx.null_bit_maps);
        row.key(date_sk)?;
        row.key(item_sk)?;
        row.key(warehouse_sk)?;
        row.integer(quantity_on_hand)?;
        out.end(INVENTORY)
    }
}
