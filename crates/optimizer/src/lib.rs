//! The logical plan optimizer (DuckDB's `src/optimizer`).
//!
//! [`Optimizer::optimize`] runs DuckDB's passes in DuckDB's order on a bound logical
//! plan, before column bindings are resolved and the physical plan is generated. Each
//! pass can be switched off with the `disabled_optimizers` setting, all of them with
//! `PRAGMA disable_optimizer`.
//!
//! - [`expression_rewriter`] and [`rule`]: expression simplification rules;
//! - [`filter_pullup`], [`filter_pushdown`] and [`filter_combiner`]: moving filters to
//!   where they are cheapest, into joins and table scans;
//! - [`join_order`]: join ordering with cardinality estimation (`DPhyp`);
//! - [`remove_unused_columns`] and [`column_lifetime_analyzer`]: reading and keeping
//!   only the columns that are used;
//! - [`build_probe_side`]: picking the cheaper hash join build side;
//! - [`statistics_propagator`]: folding what column statistics decide;
//! - [`topn`]: `ORDER BY` + `LIMIT` as a Top-N;
//! - [`expression_util`]: helpers on bound expressions shared by the passes.

pub mod build_probe_side;
pub mod column_binding_replacer;
pub mod column_lifetime_analyzer;
pub mod common_aggregate;
pub mod cse_optimizer;
pub mod cte_filter_pusher;
pub mod cte_inlining;
pub mod deliminator;
pub mod empty_result_pullup;
pub mod expression_heuristics;
pub mod expression_nullability;
pub mod expression_rewriter;
pub mod expression_util;
pub mod filter_combiner;
pub mod filter_pullup;
pub mod filter_pushdown;
pub mod in_clause_rewriter;
pub mod join_elimination;
pub mod join_filter_pushdown;
pub mod join_order;
pub mod late_materialization;
pub mod limit_pushdown;
pub mod logical_operator_deep_copy;
mod optimizer;
pub mod remove_duplicate_groups;
pub mod remove_unused_columns;
pub mod rule;
pub mod sampling_pushdown;
pub mod statistics_propagator;
pub mod sum_rewriter;
pub mod topn;
pub mod topn_window_elimination;
pub mod window_self_join;

pub use optimizer::{Optimizer, OptimizerConfig, OptimizerType};
