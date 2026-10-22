//! The `household_demographics` and `income_band` tables (`w_household_demographics.c`,
//! `w_income_band.c`).

use rustdb_common::Result;

use super::Output;
use super::build_support::{bitmap_to_dist_int, bitmap_to_dist_str};
use super::columns::{HD_NULLS, IB_NULLS};
use super::context::Context;
use super::dist::{Dist, dist};
use super::tables::{HOUSEHOLD_DEMOGRAPHICS, INCOME_BAND};

/// The generator of `household_demographics` (`mk_w_household_demographics`).
#[derive(Debug)]
pub(crate) struct HouseholdDemographics {
    income_band: &'static Dist,
    buy_potential: &'static Dist,
    dependent_count: &'static Dist,
    vehicle_count: &'static Dist,
}

impl HouseholdDemographics {
    pub(crate) fn new() -> Self {
        Self {
            income_band: dist("income_band"),
            buy_potential: dist("buy_potential"),
            dependent_count: dist("dependent_count"),
            vehicle_count: dist("vehicle_count"),
        }
    }

    /// `mk_w_household_demographics`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(HOUSEHOLD_DEMOGRAPHICS, HD_NULLS);
        let mut temp = index;
        let bands = self.income_band.size() as i64;
        let income_band_id = (temp % bands) + 1;
        temp /= bands;
        let buy_potential = bitmap_to_dist_str(self.buy_potential, &mut temp, 1);
        let dep_count = bitmap_to_dist_int(self.dependent_count, &mut temp, 1);
        let vehicle_count = bitmap_to_dist_int(self.vehicle_count, &mut temp, 1);

        let row = out.row(HOUSEHOLD_DEMOGRAPHICS).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.key(income_band_id)?;
        row.varchar(buy_potential)?;
        row.integer(dep_count)?;
        row.integer(vehicle_count)?;
        out.end(HOUSEHOLD_DEMOGRAPHICS)
    }
}

/// The generator of `income_band` (`mk_w_income_band`).
#[derive(Debug)]
pub(crate) struct IncomeBand {
    income_band: &'static Dist,
}

impl IncomeBand {
    pub(crate) fn new() -> Self {
        Self {
            income_band: dist("income_band"),
        }
    }

    /// `mk_w_income_band`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(INCOME_BAND, IB_NULLS);
        let lower_bound = self.income_band.member_int(index as i32, 1);
        let upper_bound = self.income_band.member_int(index as i32, 2);

        let row = out.row(INCOME_BAND).start(&ctx.null_bit_maps);
        row.integer(index as i32)?;
        row.integer(lower_bound)?;
        row.integer(upper_bound)?;
        out.end(INCOME_BAND)
    }
}
