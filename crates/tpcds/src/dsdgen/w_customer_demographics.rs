//! The `customer_demographics` table (`w_customer_demographics.c`): every combination of
//! the demographic values, spelled by the row number.

use rustdb_common::Result;

use super::Output;
use super::build_support::{bitmap_to_dist_int, bitmap_to_dist_str};
use super::columns::CD_NULLS;
use super::context::Context;
use super::dist::{Dist, dist};
use super::tables::CUSTOMER_DEMOGRAPHICS;

/// `CD_MAX_CHILDREN`.
const CD_MAX_CHILDREN: i64 = 7;
/// `CD_MAX_EMPLOYED`.
const CD_MAX_EMPLOYED: i64 = 7;
/// `CD_MAX_COLLEGE`.
const CD_MAX_COLLEGE: i64 = 7;

/// The generator of `customer_demographics` (`mk_w_customer_demographics`).
#[derive(Debug)]
pub(crate) struct CustomerDemographics {
    gender: &'static Dist,
    marital_status: &'static Dist,
    education: &'static Dist,
    purchase_band: &'static Dist,
    credit_rating: &'static Dist,
}

impl CustomerDemographics {
    pub(crate) fn new() -> Self {
        Self {
            gender: dist("gender"),
            marital_status: dist("marital_status"),
            education: dist("education"),
            purchase_band: dist("purchase_band"),
            credit_rating: dist("credit_rating"),
        }
    }

    /// `mk_w_customer_demographics`.
    pub(crate) fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        ctx.null_set(CUSTOMER_DEMOGRAPHICS, CD_NULLS);
        let mut temp = index - 1;
        let gender = bitmap_to_dist_str(self.gender, &mut temp, 1);
        let marital_status = bitmap_to_dist_str(self.marital_status, &mut temp, 1);
        let education_status = bitmap_to_dist_str(self.education, &mut temp, 1);
        let purchase_estimate = bitmap_to_dist_int(self.purchase_band, &mut temp, 1);
        let credit_rating = bitmap_to_dist_str(self.credit_rating, &mut temp, 1);
        let dep_count = (temp % CD_MAX_CHILDREN) as i32;
        temp /= CD_MAX_CHILDREN;
        let dep_employed_count = (temp % CD_MAX_EMPLOYED) as i32;
        temp /= CD_MAX_EMPLOYED;
        let dep_college_count = (temp % CD_MAX_COLLEGE) as i32;

        let row = out.row(CUSTOMER_DEMOGRAPHICS).start(&ctx.null_bit_maps);
        row.key(index)?;
        row.varchar(gender)?;
        row.varchar(marital_status)?;
        row.varchar(education_status)?;
        row.integer(purchase_estimate)?;
        row.varchar(credit_rating)?;
        row.integer(dep_count)?;
        row.integer(dep_employed_count)?;
        row.integer(dep_college_count)?;
        out.end(CUSTOMER_DEMOGRAPHICS)
    }
}
