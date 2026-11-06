//! The `item` table (`w_item.c`, with `hierarchy_item` of `build_support.c`).

use std::fmt::Write as _;

use rustdb_common::Result;

use super::Output;
use super::build_support::{bkey_str, embed_string, mk_word};
use super::columns::{
    I_CATEGORY, I_CLASS, I_COLOR, I_CURRENT_PRICE, I_FORMULATION, I_ITEM_DESC, I_ITEM_ID, I_MANAGER_ID,
    I_MANUFACT_ID, I_NULLS, I_PROMO_SK, I_SCD, I_SIZE, I_UNITS, I_WHOLESALE_COST,
};
use super::constants::{RS_I_FORMULATION, RS_I_ITEM_DESC, RS_I_MANUFACT, RS_I_PRODUCT_NAME};
use super::context::Context;
use super::decimal::{Decimal, DecimalOp};
use super::dist::{Dist, dist};
use super::genrand::{DIGITS, DIST_UNIFORM};
use super::scd::{ScdKeys, change_scd, skip_scd};
use super::tables::{ITEM, PROMOTION};

/// `I_PROMO_PERCENTAGE`: the percentage of items with a promotion.
const I_PROMO_PERCENTAGE: i32 = 20;
/// `MIN_ITEM_MARKDOWN_PCT`.
const MIN_ITEM_MARKDOWN_PCT: &str = "0.30";
/// `MAX_ITEM_MARKDOWN_PCT`.
const MAX_ITEM_MARKDOWN_PCT: &str = "0.90";

/// A row of `item` (`struct W_ITEM_TBL`).
#[derive(Clone, Debug, Default)]
struct ItemRow {
    item_sk: i64,
    keys: ScdKeys,
    item_desc: String,
    current_price: Decimal,
    wholesale_cost: Decimal,
    brand_id: i64,
    brand: String,
    class_id: i64,
    class: &'static str,
    category_id: i64,
    category: &'static str,
    manufact_id: i64,
    manufact: String,
    size: &'static str,
    formulation: String,
    color: &'static str,
    units: &'static str,
    container: &'static str,
    manager_id: i64,
    product_name: String,
    promo_sk: i64,
}

/// `hierarchy_item`'s statics: the category and class the brand belongs to.
#[derive(Debug, Default)]
struct Hierarchy {
    last_category: i32,
    last_class: i32,
    brand_base: i32,
    class_dist: Option<&'static Dist>,
}

/// The generator of `item` (`mk_w_item` and its statics).
#[derive(Debug)]
pub(crate) struct Item {
    r: ItemRow,
    /// `g_OldValues`, the values the next revision may keep.
    old: ItemRow,
    min_markdown: Decimal,
    max_markdown: Decimal,
    hierarchy: Hierarchy,
    manager_id: &'static Dist,
    current_price: &'static Dist,
    categories: &'static Dist,
    sizes: &'static Dist,
    manufact_id: &'static Dist,
    colors: &'static Dist,
    units: &'static Dist,
    container: &'static Dist,
    brand_syllables: &'static Dist,
}

impl Item {
    pub(crate) fn new() -> Self {
        Self {
            r: ItemRow::default(),
            old: ItemRow::default(),
            min_markdown: Decimal::from_str(MIN_ITEM_MARKDOWN_PCT),
            max_markdown: Decimal::from_str(MAX_ITEM_MARKDOWN_PCT),
            hierarchy: Hierarchy {
                last_category: -1,
                last_class: -1,
                ..Hierarchy::default()
            },
            manager_id: dist("i_manager_id"),
            current_price: dist("i_current_price"),
            categories: dist("categories"),
            sizes: dist("sizes"),
            manufact_id: dist("i_manufact_id"),
            colors: dist("colors"),
            units: dist("units"),
            container: dist("container"),
            brand_syllables: dist("brand_syllables"),
        }
    }

    /// `mk_w_item`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        // `memset(r, 0, ...)`: every field but the size is set below.
        let r = &mut self.r;
        r.size = "";

        // Build the new value.
        ctx.null_set(ITEM, I_NULLS);
        r.item_sk = index;

        let (min, row) = self.manager_id.pick_int(&mut ctx.rng, 2, 1, I_MANAGER_ID);
        let max = self.manager_id.member_int(row, 3);
        r.manager_id = ctx
            .rng
            .genrand_key(DIST_UNIFORM, i64::from(min), i64::from(max), 0, I_MANAGER_ID);

        // A new business key resets the fields that do not change between revisions.
        let first_record = ctx.set_scd_keys(I_ITEM_ID, index, &mut r.keys);

        // The bits of this number decide which fields a revision changes.
        let mut flags = ctx.rng.next_random(I_SCD) as i32;
        let old = &mut self.old;

        ctx.gen_text(&mut r.item_desc, 1, RS_I_ITEM_DESC, I_ITEM_DESC);
        change_scd(&mut r.item_desc, &mut old.item_desc, &mut flags, first_record);

        let (min_price, row) = self.current_price.pick_str(&mut ctx.rng, 2, 1, I_CURRENT_PRICE);
        let max_price = self.current_price.member_str(row, 3);
        let min_price = Decimal::from_str(min_price);
        let max_price = Decimal::from_str(max_price);
        r.current_price =
            ctx.rng
                .genrand_decimal(DIST_UNIFORM, &min_price, &max_price, None, I_CURRENT_PRICE);
        // `SCD_INT` of a decimal.
        skip_scd(&mut flags);

        let markdown = ctx.rng.genrand_decimal(
            DIST_UNIFORM,
            &self.min_markdown,
            &self.max_markdown,
            None,
            I_WHOLESALE_COST,
        );
        r.wholesale_cost = Decimal::op(DecimalOp::Mult, &r.current_price, &markdown);
        change_scd(
            &mut r.wholesale_cost,
            &mut old.wholesale_cost,
            &mut flags,
            first_record,
        );

        // `hierarchy_item(I_CATEGORY, ...)`.
        let hierarchy = &mut self.hierarchy;
        let (category, category_row) = self.categories.pick_str(&mut ctx.rng, 1, 1, I_CATEGORY);
        r.category = category;
        hierarchy.last_category = category_row;
        r.category_id = i64::from(category_row);
        hierarchy.brand_base = category_row;
        hierarchy.last_class = -1;

        // `hierarchy_item(I_CLASS, ...)`.
        let class_dist = dist(self.categories.member_str(hierarchy.last_category, 2));
        hierarchy.class_dist = Some(class_dist);
        let (class, class_row) = class_dist.pick_str(&mut ctx.rng, 1, 1, I_CLASS);
        r.class = class;
        hierarchy.last_class = class_row;
        hierarchy.last_category = -1;
        r.class_id = i64::from(class_row);
        change_scd(&mut r.class_id, &mut old.class_id, &mut flags, first_record);

        // `hierarchy_item(I_BRAND, ...)`.
        let brand_count = class_dist.member_int(hierarchy.last_class, 2);
        r.brand_id = index % i64::from(brand_count) + 1;
        mk_word(
            &mut r.brand,
            self.brand_syllables,
            i64::from(hierarchy.brand_base * 10 + hierarchy.last_class),
            45,
        );
        let _ = write!(r.brand, " #{}", r.brand_id as i32);
        r.brand_id += i64::from((hierarchy.brand_base * 1000 + hierarchy.last_class) * 1000);
        change_scd(&mut r.brand_id, &mut old.brand_id, &mut flags, first_record);

        // Some categories have meaningful sizes, some don't.
        if r.category_id != 0 {
            let use_size = self.categories.member_int(r.category_id as i32, 3);
            r.size = self
                .sizes
                .pick_str(&mut ctx.rng, 1, (use_size + 2) as usize, I_SIZE)
                .0;
            skip_scd(&mut flags);
        }

        let (min, row) = self.manufact_id.pick_int(&mut ctx.rng, 2, 1, I_MANUFACT_ID);
        let max = self.manufact_id.member_int(row, 3);
        r.manufact_id = i64::from(ctx.rng.genrand_integer(DIST_UNIFORM, min, max, 0, I_MANUFACT_ID));
        change_scd(&mut r.manufact_id, &mut old.manufact_id, &mut flags, first_record);

        mk_word(
            &mut r.manufact,
            ctx.dists.syllables,
            i64::from(r.manufact_id as i32),
            RS_I_MANUFACT,
        );
        change_scd(&mut r.manufact, &mut old.manufact, &mut flags, first_record);

        ctx.rng.gen_charset(
            &mut r.formulation,
            DIGITS,
            RS_I_FORMULATION,
            RS_I_FORMULATION,
            I_FORMULATION,
        );
        embed_string(&mut r.formulation, self.colors, 1, 2, &mut ctx.rng, I_FORMULATION);
        change_scd(&mut r.formulation, &mut old.formulation, &mut flags, first_record);

        r.color = self.colors.pick_str(&mut ctx.rng, 1, 2, I_COLOR).0;
        skip_scd(&mut flags);

        r.units = self.units.pick_str(&mut ctx.rng, 1, 1, I_UNITS).0;
        skip_scd(&mut flags);

        // The stream of the table number, `CC_HOURS`, as in the original.
        r.container = self.container.pick_str(&mut ctx.rng, 1, 1, ITEM).0;
        skip_scd(&mut flags);

        mk_word(
            &mut r.product_name,
            ctx.dists.syllables,
            i64::from(index as i32),
            RS_I_PRODUCT_NAME,
        );

        r.promo_sk = ctx.mk_join(I_PROMO_SK, PROMOTION, 1);
        let temp = ctx.rng.genrand_integer(DIST_UNIFORM, 1, 100, 0, I_PROMO_SK);
        if temp > I_PROMO_PERCENTAGE {
            r.promo_sk = -1;
        }

        // The first of a set of revisions is the baseline of the old values.
        if first_record || index == 1 {
            self.old.clone_from(r);
        }

        let row = out.row(ITEM).start(&ctx.null_bit_maps);
        row.key(r.item_sk)?;
        row.varchar(bkey_str(&r.keys.business_key))?;
        row.date(r.keys.begin_date)?;
        row.date(r.keys.end_date)?;
        row.varchar(&r.item_desc)?;
        row.decimal(&r.current_price)?;
        row.decimal(&r.wholesale_cost)?;
        row.key(r.brand_id)?;
        row.varchar(&r.brand)?;
        row.key(r.class_id)?;
        row.varchar(r.class)?;
        row.key(r.category_id)?;
        row.varchar(r.category)?;
        row.key(r.manufact_id)?;
        row.varchar(&r.manufact)?;
        row.varchar(r.size)?;
        row.varchar(&r.formulation)?;
        row.varchar(r.color)?;
        row.varchar(r.units)?;
        row.varchar(r.container)?;
        row.key(r.manager_id)?;
        row.varchar(&r.product_name)?;
        out.end(ITEM)
    }
}
