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
//! - [`topn`]: `ORDER BY` + `LIMIT` as a Top-N;
//! - [`expression_util`]: helpers on bound expressions shared by the passes.

pub mod build_probe_side;
pub mod column_binding_replacer;
pub mod column_lifetime_analyzer;
pub mod expression_rewriter;
pub mod expression_util;
pub mod filter_combiner;
pub mod filter_pullup;
pub mod filter_pushdown;
pub mod join_order;
mod optimizer;
pub mod remove_unused_columns;
pub mod rule;
pub mod topn;

pub use optimizer::{Optimizer, OptimizerConfig, OptimizerType};
