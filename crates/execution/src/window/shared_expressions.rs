//! Expressions shared by the window functions of one operator (DuckDB's
//! `WindowSharedExpressions`): each distinct, non-volatile expression is computed once per
//! row, in one of three places.

use roastedduck_planner::Expression;
use roastedduck_types::LogicalType;

use crate::ExpressionExecutor;

/// One set of shared expressions: the expression of each column, in column order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shared {
    columns: Vec<Expression>,
}

impl Shared {
    /// Registers `expression` and returns its column; equal non-volatile expressions share
    /// a column (DuckDB's `RegisterExpr`).
    fn register(&mut self, expression: Option<&Expression>) -> Option<usize> {
        let expression = expression?;
        if !expression.is_volatile()
            && let Some(column) = self.columns.iter().position(|other| other.equals(expression))
        {
            return Some(column);
        }
        self.columns.push(expression.clone());
        Some(self.columns.len() - 1)
    }

    /// The column types.
    pub(crate) fn types(&self) -> Vec<LogicalType> {
        self.columns
            .iter()
            .map(|expression| expression.return_type.clone())
            .collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// An executor computing the columns (DuckDB's `PrepareExecutors`).
    pub(crate) fn executor(&self) -> ExpressionExecutor {
        ExpressionExecutor::new(self.columns.clone())
    }
}

/// The shared expressions of a window operator.
#[derive(Clone, Debug, Default)]
pub(crate) struct WindowSharedExpressions {
    /// Fully materialised for the whole hash group (the [`super::collection::WindowCollection`]).
    pub coll: Shared,
    /// Computed while sinking each chunk.
    pub sink: Shared,
    /// Computed while evaluating each output chunk.
    pub eval: Shared,
    /// Collection columns whose NULL mask is requested.
    pub coll_validity: Vec<usize>,
}

impl WindowSharedExpressions {
    /// Registers a fully materialised expression; `build_validity` requests its NULL mask.
    pub(crate) fn register_collection(
        &mut self,
        expression: Option<&Expression>,
        build_validity: bool,
    ) -> Option<usize> {
        let column = self.coll.register(expression)?;
        if build_validity && !self.coll_validity.contains(&column) {
            self.coll_validity.push(column);
        }
        Some(column)
    }

    /// Registers an expression computed while sinking.
    pub(crate) fn register_sink(&mut self, expression: Option<&Expression>) -> Option<usize> {
        self.sink.register(expression)
    }

    /// Registers an expression computed while evaluating.
    pub(crate) fn register_evaluate(&mut self, expression: Option<&Expression>) -> Option<usize> {
        self.eval.register(expression)
    }
}
