//! The expression rewrite rules (DuckDB's `optimizer/rule/*`).
//!
//! DuckDB describes where a rule applies with expression matchers and passes the matched
//! sub-expressions to the rule. Here each rule matches its pattern itself and returns
//! what it did; the semantics of DuckDB's matcher policies (which child a "some child"
//! matcher binds, the order of an unordered match) are reproduced in each rule.

#![allow(
    clippy::unnecessary_wraps,
    reason = "every rule has the `Rule` signature, fallible or not"
)]

mod aggregate;
pub use aggregate::apply_ordered_aggregate;
mod arithmetic;
mod case;
mod comparison;
mod conjunction;
mod constant_folding;
mod date_part;
mod date_trunc;
mod distributivity;
mod enum_comparison;
mod join_dependent_filter;
mod like;
mod move_constants;
mod predicate_factoring;
mod regex;
mod timestamp_comparison;

use roastedduck_common::Result;
use roastedduck_planner::Expression;

use crate::expression_rewriter::RuleContext;

/// What applying a rule did (DuckDB returns the replacement or sets `changes_made`).
#[derive(Debug)]
pub enum RuleOutcome {
    /// The rule did not apply (or applied without a change worth re-running the rules).
    Unchanged,
    /// The expression changed in place.
    Changed,
    /// The expression is to be replaced.
    Replaced(Expression),
}

/// A rewrite rule: inspects (and possibly rewrites) one expression node.
pub type Rule = fn(&RuleContext<'_>, &mut Expression, bool) -> Result<RuleOutcome>;

/// The rules in the order DuckDB's `Optimizer` registers them.
pub const RULES: &[Rule] = &[
    constant_folding::constant_order_normalization,
    constant_folding::constant_folding,
    distributivity::distributivity,
    arithmetic::arithmetic_simplification,
    case::case_simplification,
    conjunction::conjunction_simplification,
    date_part::date_part_simplification,
    date_trunc::date_trunc_simplification,
    comparison::comparison_simplification,
    comparison::in_clause_simplification,
    conjunction::equal_or_null_simplification,
    move_constants::move_constants,
    like::like_optimization,
    aggregate::ordered_aggregate_optimizer,
    aggregate::distinct_aggregate_optimizer,
    aggregate::distinct_windowed_optimizer,
    regex::regex_optimization,
    like::empty_needle_removal,
    enum_comparison::enum_comparison,
    join_dependent_filter::join_dependent_filter,
    timestamp_comparison::timestamp_comparison,
    predicate_factoring::predicate_factoring,
];
