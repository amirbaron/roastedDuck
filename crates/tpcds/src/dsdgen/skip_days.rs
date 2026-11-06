//! `skip_days.c`: the first date of a fact table.

use super::constants::DATA_START_DATE;
use super::context::Context;
use super::date::Date;

/// `skipDays`: the first date of `table`, and in `remainder` the last row of that date
/// plus one. The date would move forward to the first row of a part of a parallel build;
/// DuckDB's generation is never one (`split_work` starts it at row 1).
pub(crate) fn skip_days(ctx: &Context, table: usize, remainder: &mut i64) -> i64 {
    let date = i64::from(Date::parse(DATA_START_DATE).julian);
    *remainder = ctx.date_scaling(table, date) + 1;
    date
}
