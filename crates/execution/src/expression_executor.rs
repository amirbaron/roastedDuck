//! Evaluation of bound expressions over data chunks (DuckDB's `ExpressionExecutor`).
//!
//! Conventions follow DuckDB exactly because they decide which rows an expression is
//! evaluated on (and therefore which errors are raised):
//! - `execute(.., sel, count)` evaluates rows `sel[0..count]` of the input (all rows when
//!   `sel` is `None`) and returns a *compact* vector whose row `i` is input row `sel[i]`.
//! - `select(..)` partitions input rows into `true_sel` / `false_sel`, holding input row
//!   positions, and evaluates later conjuncts of an AND only on rows that still pass.
//! - `CASE` evaluates each branch only on the rows routed to it.

use rustdb_common::{Error, Result};
use rustdb_function::FunctionState;
use rustdb_planner::{ComparisonOp, ConjunctionOp, Expression, OperatorOp};
use rustdb_types::compare::{CompareOp, compare};
use rustdb_types::kernels::{scatter, unary};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Unified, ValidityMask, Value, Vector};

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

/// Evaluates bound expressions over data chunks.
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
            columns.push(execute(expression, state, input, None, count)?);
        }
        output.set_columns(columns, count);
        Ok(())
    }

    /// Fills `true_sel` with the rows of `input` for which the first expression (a
    /// boolean predicate) is true; returns how many there are.
    pub fn select(&mut self, input: &DataChunk, true_sel: &mut Vec<u32>) -> Result<usize> {
        let (Some(expression), Some(state)) = (self.expressions.first(), self.states.first_mut()) else {
            return Err(Error::internal("select on an executor without expressions"));
        };
        true_sel.clear();
        select(expression, state, input, None, input.len(), Some(true_sel), None)
    }
}

/// The input row for output row `i` under an optional selection.
#[inline]
fn row(sel: Option<&[u32]>, i: usize) -> u32 {
    sel.map_or(i as u32, |sel| sel[i])
}

fn slice_selection(sel: &[u32]) -> SelectionVector {
    SelectionVector::new(sel.to_vec())
}

fn execute(
    expression: &Expression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    match expression {
        Expression::Constant(value) => Vector::constant(value),
        Expression::Reference { index, .. } => {
            let column = input.column(*index);
            Ok(match sel {
                Some(sel) => column.slice(&slice_selection(sel)),
                None => column.clone(),
            })
        }
        Expression::Function(function) => {
            let mut arguments = Vec::with_capacity(function.children.len());
            for (child, child_state) in function.children.iter().zip(&mut state.children) {
                arguments.push(execute(child, child_state, input, sel, count)?);
            }
            state.arguments.set_columns(arguments, count);
            state.function.bind_data.clone_from(&function.bind_data);
            let mut result = Vector::flat(function.function.return_type.clone(), 0);
            (function.function.kernel)(&state.arguments, &mut state.function, &mut result)?;
            Ok(result)
        }
        Expression::Comparison { op, left, right } => {
            let [left_state, right_state] = &mut state.children[..] else {
                return Err(Error::internal("comparison state must have two children"));
            };
            let left = execute(left, left_state, input, sel, count)?;
            let right = execute(right, right_state, input, sel, count)?;
            comparison(*op, &left, &right, count)
        }
        Expression::Conjunction { op, children } => {
            let mut result: Option<Vector> = None;
            for (child, child_state) in children.iter().zip(&mut state.children) {
                let value = execute(child, child_state, input, sel, count)?;
                result = Some(match result {
                    None => value,
                    Some(previous) => conjunction(*op, &previous, &value, count),
                });
            }
            result.ok_or_else(|| Error::internal("conjunction without children"))
        }
        Expression::Operator { op, children, .. } => {
            let mut values = Vec::with_capacity(children.len());
            for (child, child_state) in children.iter().zip(&mut state.children) {
                values.push(execute(child, child_state, input, sel, count)?);
            }
            operator(*op, &values, count)
        }
        Expression::Case {
            checks,
            otherwise,
            ty,
        } => execute_case(checks, otherwise, ty, state, input, sel, count),
        Expression::Between {
            input: value,
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
        } => {
            let [value_state, lower_state, upper_state] = &mut state.children[..] else {
                return Err(Error::internal("between state must have three children"));
            };
            let value = execute(value, value_state, input, sel, count)?;
            let lower = execute(lower, lower_state, input, sel, count)?;
            let upper = execute(upper, upper_state, input, sel, count)?;
            let lower_op = if *lower_inclusive {
                ComparisonOp::GreaterThanOrEqual
            } else {
                ComparisonOp::GreaterThan
            };
            let upper_op = if *upper_inclusive {
                ComparisonOp::LessThanOrEqual
            } else {
                ComparisonOp::LessThan
            };
            let above = comparison(lower_op, &value, &lower, count)?;
            let below = comparison(upper_op, &value, &upper, count)?;
            Ok(conjunction(ConjunctionOp::And, &above, &below, count))
        }
        Expression::Cast(cast) => Err(Error::not_implemented(format!(
            "execution of casts to {} (cast functions are being ported)",
            cast.target
        ))),
        Expression::ColumnRef { binding, .. } => Err(Error::internal(format!(
            "unresolved column reference {binding} reached the executor"
        ))),
    }
}

/// DuckDB's `CASE` execution: route rows through the WHEN checks and evaluate each THEN
/// (and the ELSE) only on the rows that reach it.
fn execute_case(
    checks: &[(Expression, Expression)],
    otherwise: &Expression,
    ty: &LogicalType,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    // Results are first written at input-row positions, then compacted to `sel`.
    let capacity = (0..count).map(|i| row(sel, i) as usize + 1).max().unwrap_or(0);
    let mut result = Vector::flat(ty.clone(), capacity);
    let mut current: Option<Vec<u32>> = sel.map(<[u32]>::to_vec);
    let mut current_count = count;
    let (check_states, else_state) = state.children.split_at_mut(checks.len() * 2);
    for ((when, then), pair) in checks.iter().zip(check_states.as_chunks_mut::<2>().0) {
        let [when_state, then_state] = pair;
        let mut true_sel = Vec::new();
        let mut false_sel = Vec::new();
        let true_count = select(
            when,
            when_state,
            input,
            current.as_deref(),
            current_count,
            Some(&mut true_sel),
            Some(&mut false_sel),
        )?;
        if true_count == 0 {
            continue;
        }
        if true_count == count && current_count == count {
            // Every row takes this branch: evaluate it directly on the original rows.
            return execute(then, then_state, input, sel, count);
        }
        let values = execute(then, then_state, input, Some(&true_sel), true_count)?;
        scatter(&values, &mut result, &true_sel)?;
        current_count -= true_count;
        current = Some(false_sel);
        if current_count == 0 {
            break;
        }
    }
    if current_count > 0 {
        let [else_state] = else_state else {
            return Err(Error::internal("case state lacks an else child"));
        };
        if current_count == count {
            return execute(otherwise, else_state, input, sel, count);
        }
        let remaining = current.unwrap_or_default();
        let values = execute(otherwise, else_state, input, Some(&remaining), current_count)?;
        scatter(&values, &mut result, &remaining)?;
    }
    Ok(match sel {
        Some(sel) => result.slice(&slice_selection(sel)),
        None => result,
    })
}

/// Partitions rows by a boolean expression (DuckDB's `ExpressionExecutor::Select`).
fn select(
    expression: &Expression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
    true_sel: Option<&mut Vec<u32>>,
    false_sel: Option<&mut Vec<u32>>,
) -> Result<usize> {
    if count == 0 {
        return Ok(0);
    }
    match expression {
        Expression::Conjunction {
            op: ConjunctionOp::And,
            children,
        } => select_and(children, state, input, sel, count, true_sel, false_sel),
        Expression::Conjunction {
            op: ConjunctionOp::Or,
            children,
        } => select_or(children, state, input, sel, count, true_sel, false_sel),
        _ => {
            let result = execute(expression, state, input, sel, count)?;
            Ok(partition(
                &result.unified::<bool>(),
                sel,
                count,
                true_sel,
                false_sel,
            ))
        }
    }
}

fn partition(
    values: &Unified<'_, bool>,
    sel: Option<&[u32]>,
    count: usize,
    mut true_sel: Option<&mut Vec<u32>>,
    mut false_sel: Option<&mut Vec<u32>>,
) -> usize {
    let mut true_count = 0;
    for i in 0..count {
        let position = row(sel, i);
        if values.get(i) == Some(true) {
            true_count += 1;
            if let Some(true_sel) = true_sel.as_deref_mut() {
                true_sel.push(position);
            }
        } else if let Some(false_sel) = false_sel.as_deref_mut() {
            false_sel.push(position);
        }
    }
    true_count
}

fn select_and(
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
    true_sel: Option<&mut Vec<u32>>,
    mut false_sel: Option<&mut Vec<u32>>,
) -> Result<usize> {
    let mut current: Option<Vec<u32>> = sel.map(<[u32]>::to_vec);
    let mut current_count = count;
    for (child, child_state) in children.iter().zip(&mut state.children) {
        let mut passing = Vec::with_capacity(current_count);
        let mut failing = Vec::new();
        let passed = select(
            child,
            child_state,
            input,
            current.as_deref(),
            current_count,
            Some(&mut passing),
            Some(&mut failing),
        )?;
        if let Some(false_sel) = false_sel.as_deref_mut() {
            false_sel.extend_from_slice(&failing);
        }
        current_count = passed;
        current = Some(passing);
        if current_count == 0 {
            break;
        }
    }
    if let Some(true_sel) = true_sel {
        true_sel.extend_from_slice(current.as_deref().unwrap_or_default());
    }
    Ok(current_count)
}

fn select_or(
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
    true_sel: Option<&mut Vec<u32>>,
    false_sel: Option<&mut Vec<u32>>,
) -> Result<usize> {
    let mut current: Option<Vec<u32>> = sel.map(<[u32]>::to_vec);
    let mut current_count = count;
    let mut passed_rows = Vec::new();
    for (child, child_state) in children.iter().zip(&mut state.children) {
        let mut passing = Vec::new();
        let mut failing = Vec::with_capacity(current_count);
        let passed = select(
            child,
            child_state,
            input,
            current.as_deref(),
            current_count,
            Some(&mut passing),
            Some(&mut failing),
        )?;
        if passed > 0 {
            passed_rows.extend_from_slice(&passing);
            current_count -= passed;
            current = Some(failing);
        }
        if current_count == 0 {
            break;
        }
    }
    let result_count = passed_rows.len();
    if let Some(true_sel) = true_sel {
        passed_rows.sort_unstable();
        true_sel.extend_from_slice(&passed_rows);
    }
    if let Some(false_sel) = false_sel {
        let remaining = match current {
            Some(rows) => rows,
            None => (0..count as u32).collect(),
        };
        false_sel.extend_from_slice(&remaining[..current_count]);
    }
    Ok(result_count)
}

fn comparison(op: ComparisonOp, left: &Vector, right: &Vector, count: usize) -> Result<Vector> {
    let mut result = Vector::flat(LogicalType::BOOLEAN, 0);
    let simple = match op {
        ComparisonOp::Equal => Some(CompareOp::Equal),
        ComparisonOp::NotEqual => Some(CompareOp::NotEqual),
        ComparisonOp::LessThan => Some(CompareOp::LessThan),
        ComparisonOp::GreaterThan => Some(CompareOp::GreaterThan),
        ComparisonOp::LessThanOrEqual => Some(CompareOp::LessThanOrEqual),
        ComparisonOp::GreaterThanOrEqual => Some(CompareOp::GreaterThanOrEqual),
        ComparisonOp::IsDistinctFrom | ComparisonOp::IsNotDistinctFrom => None,
    };
    if let Some(op) = simple {
        compare(op, left, right, count, &mut result)?;
        return Ok(result);
    }
    // NULL-aware: NULL IS NOT DISTINCT FROM NULL is true; results are never NULL.
    let mut equal = Vector::flat(LogicalType::BOOLEAN, 0);
    compare(CompareOp::Equal, left, right, count, &mut equal)?;
    let distinct = op == ComparisonOp::IsDistinctFrom;
    let mut out = Vector::flat(LogicalType::BOOLEAN, count);
    let values = out.values_mut::<bool>();
    for (i, value) in values.iter_mut().enumerate().take(count) {
        let not_distinct = match (left.is_null(i), right.is_null(i)) {
            (true, true) => true,
            (false, false) => equal.get_value(i).as_bool() == Some(true),
            _ => false,
        };
        *value = not_distinct != distinct;
    }
    Ok(out)
}

/// Three-valued AND/OR (DuckDB's `VectorOperations::And` / `Or`).
fn conjunction(op: ConjunctionOp, left: &Vector, right: &Vector, count: usize) -> Vector {
    let (l, r) = (left.unified::<bool>(), right.unified::<bool>());
    let mut out = Vector::flat(LogicalType::BOOLEAN, count);
    let mut nulls = Vec::new();
    {
        let values = out.values_mut::<bool>();
        for (i, value) in values.iter_mut().enumerate().take(count) {
            let combined = match op {
                ConjunctionOp::And => match (l.get(i), r.get(i)) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                },
                ConjunctionOp::Or => match (l.get(i), r.get(i)) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                },
            };
            match combined {
                Some(result) => *value = result,
                None => nulls.push(i),
            }
        }
    }
    if !nulls.is_empty() {
        let mut validity = ValidityMask::all_valid();
        for i in nulls {
            validity.set_invalid(i, count);
        }
        *out.validity_mut() = validity;
    }
    out
}

fn operator(op: OperatorOp, values: &[Vector], count: usize) -> Result<Vector> {
    let input = values
        .first()
        .ok_or_else(|| Error::internal("operator without arguments"))?;
    let mut out = Vector::flat(LogicalType::BOOLEAN, 0);
    match op {
        OperatorOp::Not => unary::<bool, bool>(input, count, &mut out, |value| !value),
        OperatorOp::IsNull | OperatorOp::IsNotNull => {
            let want_null = op == OperatorOp::IsNull;
            if input.kind() == rustdb_types::VectorKind::Constant {
                return Vector::constant(&Value::boolean(input.is_null(0) == want_null));
            }
            out = Vector::flat(LogicalType::BOOLEAN, count);
            let data = out.values_mut::<bool>();
            for (i, value) in data.iter_mut().enumerate().take(count) {
                *value = input.is_null(i) == want_null;
            }
        }
        OperatorOp::In | OperatorOp::NotIn | OperatorOp::Coalesce | OperatorOp::TryExpression => {
            return Err(Error::not_implemented(format!("execution of operator {op:?}")));
        }
    }
    Ok(out)
}
