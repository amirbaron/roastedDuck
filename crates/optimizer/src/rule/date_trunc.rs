//! `date_trunc(part, x) <op> constant` as a range on `x` (DuckDB's
//! `DateTruncSimplificationRule`). Not ported yet: the rule leaves expressions unchanged,
//! which only forgoes the rewrite.

use rustdb_common::Result;
use rustdb_planner::Expression;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;

pub fn date_trunc_simplification(_: &RuleContext<'_>, _: &mut Expression, _: bool) -> Result<RuleOutcome> {
    Ok(RuleOutcome::Unchanged)
}
