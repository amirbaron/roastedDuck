//! `row_number` and `ntile` (DuckDB's `window_rownumber_function.cpp`).

use rustdb_common::{Error, Result};
use rustdb_planner::ExpressionType;
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::WindowExpression;
use super::boundaries_state::{WindowBound, WindowInputExpression};
use super::executor::EvaluateInput;
use super::merge_sort_tree::WindowMergeSortTree;
use super::shared_expressions::WindowSharedExpressions;
use super::shared_orders;
use crate::ExecutionContext;

/// `row_number()` / `ntile(n)`, optionally with a secondary `ORDER BY` argument.
#[derive(Debug)]
pub(crate) struct RowNumberExecutor {
    /// Evaluation column of NTILE's argument.
    ntile_idx: Option<usize>,
    /// Sink columns of the `ORDER BY` arguments.
    arg_order_idx: Vec<usize>,
}

/// Per hash group: whether rows are numbered within frames, and the token tree ranking
/// rows by the secondary order.
#[derive(Debug)]
pub(crate) struct RowNumberGlobalState {
    use_framing: bool,
    token_tree: Option<WindowMergeSortTree>,
}

impl RowNumberExecutor {
    pub(crate) fn new(wexpr: &WindowExpression, shared: &mut WindowSharedExpressions) -> Self {
        let arg_order_idx = wexpr
            .window
            .arg_orders
            .iter()
            .filter_map(|order| shared.register_sink(Some(&order.expression)))
            .collect();
        let ntile_idx = if wexpr.expression_type == ExpressionType::WindowNtile {
            shared.register_evaluate(wexpr.window.children.first())
        } else {
            None
        };
        Self {
            ntile_idx,
            arg_order_idx,
        }
    }

    pub(crate) fn global_state(
        &self,
        ctx: &ExecutionContext,
        wexpr: &WindowExpression,
        count: usize,
    ) -> Result<RowNumberGlobalState> {
        let mut state = RowNumberGlobalState {
            use_framing: false,
            token_tree: None,
        };
        if !self.arg_order_idx.is_empty() {
            state.use_framing = true;
            // A secondary order that is a prefix of the partition order needs no tree.
            let arg_orders = &wexpr.window.arg_orders;
            if shared_orders(&wexpr.window.orders, arg_orders) != arg_orders.len() {
                // Row numbers are ranks with ties broken by position.
                state.token_tree = Some(WindowMergeSortTree::new(
                    ctx,
                    arg_orders,
                    &self.arg_order_idx,
                    count,
                    true,
                )?);
            }
        }
        Ok(state)
    }

    pub(crate) fn sink(
        global: &mut RowNumberGlobalState,
        sink_chunk: &DataChunk,
        input_idx: usize,
    ) -> Result<()> {
        if let Some(tree) = &mut global.token_tree {
            tree.sink(sink_chunk, input_idx, None)?;
        }
        Ok(())
    }

    pub(crate) fn finalize(global: &mut RowNumberGlobalState) -> Result<()> {
        if let Some(tree) = &mut global.token_tree {
            tree.build_token_tree()?;
        }
        Ok(())
    }

    pub(crate) fn evaluate(
        &self,
        wexpr: &WindowExpression,
        global: &RowNumberGlobalState,
        input: &EvaluateInput<'_>,
    ) -> Result<Vector> {
        if wexpr.expression_type == ExpressionType::WindowNtile {
            return self.evaluate_ntile(global, input);
        }
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            ..
        } = *input;
        let mut result = Vector::flat(LogicalType::BIGINT, count);
        let data = &mut result.values_mut::<i64>()[..count];
        if global.use_framing {
            let begins = &bounds[WindowBound::FrameBegin];
            let ends = &bounds[WindowBound::FrameEnd];
            for (i, value) in data.iter_mut().enumerate() {
                let row = row_idx + i;
                *value = match &global.token_tree {
                    // Row numbers are unique ranks.
                    Some(tree) => tree.rank(begins[i], ends[i], row) as i64,
                    None => (row - begins[i] + 1) as i64,
                };
            }
        } else {
            let begins = &bounds[WindowBound::PartitionBegin];
            for (i, value) in data.iter_mut().enumerate() {
                *value = (row_idx + i - begins[i] + 1) as i64;
            }
        }
        Ok(result)
    }

    /// NTILE is a scaled row number (with thanks to SQLite's `ntileValueFunc`).
    fn evaluate_ntile(&self, global: &RowNumberGlobalState, input: &EvaluateInput<'_>) -> Result<Vector> {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            eval_chunk,
            ..
        } = *input;
        let (begins, ends) = if global.use_framing {
            // With secondary sorts the frame takes the place of the partition.
            (&bounds[WindowBound::FrameBegin], &bounds[WindowBound::FrameEnd])
        } else {
            (
                &bounds[WindowBound::PartitionBegin],
                &bounds[WindowBound::PartitionEnd],
            )
        };
        let ntile = WindowInputExpression::new(eval_chunk, self.ntile_idx);
        let mut result = Vector::flat(LogicalType::BIGINT, count);
        let mut nulls = Vec::new();
        {
            let data = result.values_mut::<i64>();
            for i in 0..count {
                if ntile.cell_is_null(i) {
                    nulls.push(i);
                    continue;
                }
                let mut n_param = ntile.cell::<i64>(i);
                if n_param < 1 {
                    return Err(Error::invalid_input(
                        "Argument for ntile must be greater than zero",
                    ));
                }
                let row = row_idx + i;
                let n_total = (ends[i] - begins[i]) as i64;
                // More groups than values: one group per value.
                n_param = n_param.min(n_total);
                let n_size = n_total / n_param;
                let position = match &global.token_tree {
                    Some(tree) => tree.rank(begins[i], ends[i], row) - 1,
                    None => row - begins[i],
                } as i64;
                let n_large = n_total - n_param * n_size;
                let i_small = n_large * (n_size + 1);
                data[i] = if position < i_small {
                    1 + position / (n_size + 1)
                } else {
                    1 + n_large + (position - i_small) / n_size
                };
            }
        }
        let validity = result.validity_mut();
        for row in nulls {
            validity.set_invalid(row, count);
        }
        Ok(result)
    }
}
