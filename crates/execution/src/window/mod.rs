//! Window function evaluation (DuckDB's `src/function/window`).
//!
//! The window operator ([`crate::operators::PhysicalWindow`]) partitions and sorts its
//! input into hash groups. For each hash group it computes the partition and peer
//! boundary masks, materialises the shared argument expressions into a
//! [`collection::WindowCollection`], and lets one [`executor::WindowExecutor`] per window
//! function sink the rows, build its acceleration structures (segment trees, merge sort
//! trees) and evaluate the function chunk by chunk.
//!
//! Module map (DuckDB file in parentheses):
//! - [`boundaries_state`] (`window_boundaries_state.cpp`): partition, peer and frame bounds.
//! - [`collection`] (`window_collection.cpp`): materialised arguments, row deltas.
//! - [`shared_expressions`] (`window_shared_expressions.cpp`).
//! - [`executor`] (`window_executor.cpp`): the executor interface and dispatch.
//! - [`rownumber_function`], [`rank_function`], [`value_function`]: non-aggregate functions.
//! - [`aggregate_function`] (`window_aggregate_function.cpp`) and the aggregators
//!   [`constant_aggregator`], [`custom_aggregator`], [`segment_tree`],
//!   [`distinct_aggregator`], [`naive_aggregator`] on top of [`aggregator`] and
//!   [`aggregate_states`].
//! - [`merge_sort_tree`] (`merge_sort_tree.hpp`, `window_merge_sort_tree.cpp`,
//!   `window_index_tree.cpp`, `window_token_tree.cpp`): secondary orderings.

pub(crate) mod aggregate_function;
pub(crate) mod aggregate_states;
pub(crate) mod aggregator;
pub(crate) mod boundaries_state;
pub(crate) mod collection;
pub(crate) mod constant_aggregator;
pub(crate) mod custom_aggregator;
pub(crate) mod distinct_aggregator;
pub(crate) mod executor;
pub(crate) mod mask;
pub(crate) mod merge_sort_tree;
pub(crate) mod naive_aggregator;
pub(crate) mod rank_function;
pub(crate) mod rownumber_function;
pub(crate) mod segment_tree;
pub(crate) mod shared_expressions;
pub(crate) mod value_function;

use roastedduck_ast::{OrderByNullType, OrderType};
use roastedduck_common::{Error, Result};
use roastedduck_planner::expression::{BoundOrderByNode, BoundWindowExpression};
use roastedduck_planner::{Expression, ExpressionKind, ExpressionType};
use roastedduck_row::sort::{NullOrder, OrderModifiers, OrderType as SortOrder};
use roastedduck_types::LogicalType;

/// A window function call taken out of its [`Expression`] (DuckDB's
/// `BoundWindowExpression`, which is itself an expression).
#[derive(Clone, Debug)]
pub struct WindowExpression {
    /// `WINDOW_ROW_NUMBER`, `WINDOW_AGGREGATE`, ...
    pub expression_type: ExpressionType,
    pub return_type: LogicalType,
    /// The alias of the call.
    pub alias: roastedduck_common::ByteString,
    /// The name of the call (`Expression::GetName`: its alias, else its SQL), for plan
    /// rendering.
    pub name: roastedduck_common::ByteString,
    pub window: BoundWindowExpression,
}

impl WindowExpression {
    /// Unwraps a bound window expression.
    pub fn from_expression(expression: Expression) -> Result<Self> {
        let name = expression.name();
        let Expression {
            expression_type,
            return_type,
            alias,
            kind,
            ..
        } = expression;
        match kind {
            ExpressionKind::Window(window) => Ok(Self {
                expression_type,
                return_type,
                alias,
                name,
                window: *window,
            }),
            _ => Err(Error::internal("expected a window expression")),
        }
    }

    /// Wraps the call back into an expression.
    pub fn to_expression(&self) -> Expression {
        let mut expression = Expression::new(
            self.expression_type,
            self.return_type.clone(),
            ExpressionKind::Window(Box::new(self.window.clone())),
        );
        expression.alias.clone_from(&self.alias);
        expression
    }

    /// Whether the call ignores NULL arguments (`IGNORE NULLS`).
    pub(crate) const fn ignore_nulls(&self) -> bool {
        self.window.ignore_nulls
    }
}

/// Whether two ORDER BY terms are the same (DuckDB's `BoundOrderByNode::Equals`).
pub(crate) fn orders_equal(left: &BoundOrderByNode, right: &BoundOrderByNode) -> bool {
    left.order_type == right.order_type
        && left.null_order == right.null_order
        && left.expression.equals(&right.expression)
}

/// The length of the common prefix of two orderings, or 0 if they diverge within the
/// shorter one (DuckDB's `BoundWindowExpression::GetSharedOrders`).
pub(crate) fn shared_orders(lhs: &[BoundOrderByNode], rhs: &[BoundOrderByNode]) -> usize {
    let overlap = lhs.len().min(rhs.len());
    if lhs.iter().zip(rhs).take(overlap).all(|(l, r)| orders_equal(l, r)) {
        overlap
    } else {
        0
    }
}

/// Whether two calls partition by the same expressions, in any order (DuckDB's
/// `BoundWindowExpression::PartitionsAreEquivalent`).
pub(crate) fn partitions_are_equivalent(lhs: &BoundWindowExpression, rhs: &BoundWindowExpression) -> bool {
    lhs.partitions.len() == rhs.partitions.len()
        && lhs
            .partitions
            .iter()
            .all(|partition| rhs.partitions.iter().any(|other| other.equals(partition)))
}

/// The sort modifiers of a resolved ORDER BY term.
pub(crate) fn order_modifiers(order: &BoundOrderByNode) -> Result<OrderModifiers> {
    let order_type = match order.order_type {
        OrderType::Ascending => SortOrder::Ascending,
        OrderType::Descending => SortOrder::Descending,
        other => return Err(Error::internal(format!("unresolved order type {other:?}"))),
    };
    let nulls = match order.null_order {
        OrderByNullType::NullsFirst => NullOrder::NullsFirst,
        OrderByNullType::NullsLast => NullOrder::NullsLast,
        other => return Err(Error::internal(format!("unresolved null order {other:?}"))),
    };
    Ok(OrderModifiers::new(order_type, nulls))
}
