//! `regexp_matches` with a pattern that is a plain string or a simple LIKE pattern
//! (DuckDB's `RegexOptimizationRule`). Not ported yet: the rule leaves expressions
//! unchanged, which only forgoes the rewrite.

use rustdb_common::Result;
use rustdb_planner::Expression;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;

pub fn regex_optimization(_: &RuleContext<'_>, _: &mut Expression, _: bool) -> Result<RuleOutcome> {
    Ok(RuleOutcome::Unchanged)
}
