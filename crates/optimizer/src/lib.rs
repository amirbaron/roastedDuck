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
//! - [`expression_util`]: helpers on bound expressions shared by the passes.

pub mod expression_rewriter;
pub mod expression_util;
pub mod filter_combiner;
pub mod filter_pullup;
pub mod filter_pushdown;
pub mod join_order;
mod optimizer;
pub mod rule;

pub use optimizer::{Optimizer, OptimizerConfig, OptimizerType};
