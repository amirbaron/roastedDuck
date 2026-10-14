use rustdb_common::{Error, Result};
use rustdb_function::FunctionState;
use rustdb_planner::Expression;
use rustdb_types::{DataChunk, Vector};

/// Per-expression execution state, mirroring the expression tree.
#[derive(Debug, Default)]
struct ExpressionState {
    function: FunctionState,
    children: Vec<ExpressionState>,
    /// Scratch chunk holding evaluated arguments of a function call.
    arguments: DataChunk,
}

impl ExpressionState {
    fn for_expression(expression: &Expression) -> Self {
        Self {
            function: FunctionState::default(),
            children: expression.children().map(Self::for_expression).collect(),
            arguments: DataChunk::default(),
        }
    }
}

/// Evaluates bound expressions over data chunks (DuckDB's `ExpressionExecutor`).
#[derive(Debug)]
pub struct ExpressionExecutor {
    expressions: Vec<Expression>,
    states: Vec<ExpressionState>,
}

impl ExpressionExecutor {
    pub fn new(expressions: Vec<Expression>) -> Self {
        let states = expressions.iter().map(ExpressionState::for_expression).collect();
        Self { expressions, states }
    }

    /// Evaluates every expression into the corresponding column of `output`.
    pub fn execute(&mut self, input: &DataChunk, output: &mut DataChunk) -> Result<()> {
        let count = input.len();
        let mut columns = Vec::with_capacity(self.expressions.len());
        for (expression, state) in self.expressions.iter().zip(&mut self.states) {
            columns.push(evaluate(expression, state, input, count)?);
        }
        output.set_columns(columns, count);
        Ok(())
    }
}

fn evaluate(
    expression: &Expression,
    state: &mut ExpressionState,
    input: &DataChunk,
    count: usize,
) -> Result<Vector> {
    match expression {
        Expression::Constant(value) => Vector::constant(value),
        Expression::Reference { index, .. } => Ok(input.column(*index).clone()),
        Expression::Function(function) => {
            let mut arguments = Vec::with_capacity(function.children.len());
            for (child, child_state) in function.children.iter().zip(&mut state.children) {
                arguments.push(evaluate(child, child_state, input, count)?);
            }
            state.arguments.set_columns(arguments, count);
            state.function.bind_data.clone_from(&function.bind_data);
            let mut result = Vector::flat(function.function.return_type.clone(), 0);
            (function.function.kernel)(&state.arguments, &mut state.function, &mut result)?;
            Ok(result)
        }
        Expression::ColumnRef { binding, .. } => Err(Error::internal(format!(
            "unresolved column reference {binding} reached the executor"
        ))),
        other => Err(Error::not_implemented(format!(
            "execution of expression {other:?}"
        ))),
    }
}
