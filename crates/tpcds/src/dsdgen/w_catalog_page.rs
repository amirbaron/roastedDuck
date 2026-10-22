//! The `catalog_page` table (`w_catalog_page.c`): catalogs are issued monthly, quarterly
//! or bi-annually, with one of each type circulating at all times.

use rustdb_common::Result;

use super::Output;
use super::build_support::{bkey_str, mk_bkey};
use super::columns::{CP_DESCRIPTION, CP_NULLS};
use super::constants::{CP_CATALOGS_PER_YEAR, DATA_START_DATE, RS_CP_DESCRIPTION};
use super::context::Context;
use super::date::Date;
use super::dist::{Dist, dist};
use super::tables::CATALOG_PAGE;

/// The generator of `catalog_page` (`mk_w_catalog_page` and its statics).
#[derive(Debug)]
pub(crate) struct CatalogPage {
    start_date: i64,
    /// `cp_department`, which is never randomized.
    department: &'static str,
    description: String,
    page_types: &'static Dist,
}

impl CatalogPage {
    pub(crate) fn new() -> Self {
        Self {
            start_date: i64::from(Date::parse(DATA_START_DATE).julian),
            department: "DEPARTMENT",
            description: String::new(),
            page_types: dist("catalog_page_type"),
        }
    }

    /// `mk_w_catalog_page`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        let page_max = i64::from(ctx.pages_per_catalog);
        ctx.null_set(CATALOG_PAGE, CP_NULLS);
        let catalog_page_id = mk_bkey(index);
        let catalog_number = ((index - 1) / page_max + 1) as i32;
        let catalog_page_number = ((index - 1) % page_max + 1) as i32;
        let interval = (catalog_number - 1) % CP_CATALOGS_PER_YEAR;
        let (kind, duration, offset) = match interval {
            // Bi-annual.
            0 | 1 => (1, 182, interval * 182),
            // Quarterly.
            2..=5 => (2, 91, (interval - 2) * 91),
            // Monthly.
            _ => (3, 30, (interval - 6) * 30),
        };
        let mut start_date_id = self.start_date + i64::from(offset);
        start_date_id += i64::from((catalog_number - 1) / CP_CATALOGS_PER_YEAR) * 365;
        let end_date_id = start_date_id + i64::from(duration) - 1;
        let kind = self.page_types.member_str(kind, 1);
        ctx.gen_text(
            &mut self.description,
            RS_CP_DESCRIPTION / 2,
            RS_CP_DESCRIPTION - 1,
            CP_DESCRIPTION,
        );

        let row = out.row(CATALOG_PAGE).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(bkey_str(&catalog_page_id))?;
        row.key(start_date_id)?;
        row.key(end_date_id)?;
        row.varchar(self.department)?;
        row.integer(catalog_number)?;
        row.integer(catalog_page_number)?;
        row.varchar(&self.description)?;
        row.varchar(kind)?;
        out.end(CATALOG_PAGE)
    }
}
