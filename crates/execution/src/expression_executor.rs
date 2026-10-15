//! Evaluation of bound expressions over data chunks (DuckDB's `ExpressionExecutor`).
//!
//! Conventions follow DuckDB exactly because they decide which rows an expression is
//! evaluated on (and therefore which errors are raised):
//! - `execute(.., sel, count)` evaluates rows `sel[0..count]` of the input (all rows when
//!   `sel` is `None`) and returns a *compact* vector whose row `i` is input row `sel[i]`.
//! - `select(..)` partitions input rows into `true_sel` / `false_sel`, holding input row
//!   positions, and evaluates later conjuncts of an AND only on rows that still pass.
//! - `CASE` and `COALESCE` evaluate each branch only on the rows routed to it.

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::FunctionState;
use rustdb_function::cast::CastParameters;
use rustdb_planner::expression::{
    BoundBetweenExpression, BoundCaseExpression, BoundCastExpression, BoundFunctionExpression,
};
use rustdb_planner::{Expression, ExpressionEvaluator, ExpressionKind, ExpressionType};
use rustdb_types::compare::{CompareOp, compare};
use rustdb_types::kernels::{scatter, unary};
use rustdb_types::{
    DataChunk, LogicalType, SelectionVector, Unified, ValidityMask, Value, Vector, VectorKind,
};

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
            children: expression
                .children()
                .into_iter()
                .map(Self::for_expression)
                .collect(),
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

    /// Evaluates an expression without column references on a single row (DuckDB's
    /// `ExpressionExecutor::EvaluateScalar`).
    pub fn evaluate_scalar(expression: &Expression) -> Result<Value> {
        let mut executor = Self::new(vec![expression.clone()]);
        let input = DataChunk::new(Vec::new(), 1);
        let mut output = DataChunk::default();
        executor.execute(&input, &mut output)?;
        Ok(output.column(0).get_value(0))
    }
}

/// The planner's constant folding hook, evaluating with [`ExpressionExecutor`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ScalarEvaluator;

impl ExpressionEvaluator for ScalarEvaluator {
    fn evaluate_scalar(&self, expression: &Expression) -> Result<Value> {
        ExpressionExecutor::evaluate_scalar(expression)
    }
}

/// The input row for output row `i` under an optional selection.
#[inline]
fn row(sel: Option<&[u32]>, i: usize) -> u32 {
    sel.map_or_else(|| u32::try_from(i).unwrap_or(u32::MAX), |sel| sel[i])
}

fn slice_selection(sel: &[u32]) -> SelectionVector {
    SelectionVector::new(sel.to_vec())
}

/// Compacts a vector holding values at input-row positions to the rows of `sel`.
fn compact(result: Vector, sel: Option<&[u32]>) -> Vector {
    match sel {
        Some(sel) => result.slice(&slice_selection(sel)),
        None => result,
    }
}

/// The number of input rows a vector indexed by input-row position must hold.
fn position_capacity(sel: Option<&[u32]>, count: usize) -> usize {
    (0..count).map(|i| row(sel, i) as usize + 1).max().unwrap_or(0)
}

fn execute(
    expression: &Expression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    match &expression.kind {
        ExpressionKind::Constant(constant) => Vector::constant(&constant.value),
        ExpressionKind::Parameter(parameter) => Vector::constant(&parameter.value()),
        ExpressionKind::Reference(reference) => {
            let index =
                usize::try_from(reference.index).map_err(|_| Error::internal("reference index overflow"))?;
            if index >= input.column_count() {
                return Err(Error::internal(format!(
                    "reference #{index} out of range of a chunk with {} columns",
                    input.column_count()
                )));
            }
            Ok(compact(input.column(index).clone(), sel))
        }
        ExpressionKind::Function(function) => execute_function(function, state, input, sel, count),
        ExpressionKind::Comparison(comparison) => {
            let [left_state, right_state] = &mut state.children[..] else {
                return Err(Error::internal("comparison state must have two children"));
            };
            let left = execute(&comparison.left, left_state, input, sel, count)?;
            let right = execute(&comparison.right, right_state, input, sel, count)?;
            compare_vectors(expression.expression_type, &left, &right, count)
        }
        ExpressionKind::Conjunction(conjunction) => {
            let mut result: Option<Vector> = None;
            for (child, child_state) in conjunction.children.iter().zip(&mut state.children) {
                let value = execute(child, child_state, input, sel, count)?;
                result = Some(match result {
                    None => value,
                    Some(previous) => conjoin(expression.expression_type, &previous, &value, count)?,
                });
            }
            result.ok_or_else(|| Error::internal("conjunction without children"))
        }
        ExpressionKind::Operator(operator) => {
            execute_operator(expression, &operator.children, state, input, sel, count)
        }
        ExpressionKind::Case(case) => execute_case(case, &expression.return_type, state, input, sel, count),
        ExpressionKind::Between(between) => execute_between(between, state, input, sel, count),
        ExpressionKind::Cast(cast) => execute_cast(expression, cast, state, input, sel, count),
        ExpressionKind::ColumnRef(column) => Err(Error::internal(format!(
            "Failed to bind column reference \"{}\" [{}] (bindings: {{}})",
            expression.alias, column.binding
        ))),
        ExpressionKind::Aggregate(aggregate) => Err(Error::internal(format!(
            "Attempting to execute expression of unknown type! (aggregate {})",
            aggregate.function.name
        ))),
        ExpressionKind::Default => Err(Error::internal(
            "Attempting to execute expression of unknown type!",
        )),
        ExpressionKind::Subquery(_)
        | ExpressionKind::Window(_)
        | ExpressionKind::Unnest(_)
        | ExpressionKind::Lambda(_)
        | ExpressionKind::LambdaRef(_)
        | ExpressionKind::Expanded(_) => Err(Error::not_implemented(format!(
            "Attempting to execute expression of unknown type! ({})",
            expression.expression_type.name()
        ))),
    }
}

fn execute_function(
    function: &BoundFunctionExpression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let mut arguments = Vec::with_capacity(function.children.len());
    for (child, child_state) in function.children.iter().zip(&mut state.children) {
        arguments.push(execute(child, child_state, input, sel, count)?);
    }
    state.arguments.set_columns(arguments, count);
    state.function.bind_data.clone_from(&function.bind_info);
    let mut result = Vector::flat(function.function.return_type.clone(), 0);
    (function.function.kernel)(&state.arguments, &mut state.function, &mut result)?;
    Ok(result)
}

/// DuckDB's `ExecuteCast`: casts through the bound cast function, reporting conversion
/// errors at the cast's query location.
fn execute_cast(
    expression: &Expression,
    cast: &BoundCastExpression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let [child_state] = &mut state.children[..] else {
        return Err(Error::internal("cast state must have one child"));
    };
    let child = execute(&cast.child, child_state, input, sel, count)?;
    let mut parameters = if cast.try_cast {
        CastParameters::try_cast()
    } else {
        CastParameters::new()
    };
    let mut result = Vector::flat(expression.return_type.clone(), count);
    cast.bound_cast
        .execute(&child, &mut result, count, &mut parameters)
        .map_err(|error| {
            if error.kind() == ErrorKind::Conversion {
                error.with_position(expression.query_location)
            } else {
                error
            }
        })?;
    Ok(result)
}

fn execute_between(
    between: &BoundBetweenExpression,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let [value_state, lower_state, upper_state] = &mut state.children[..] else {
        return Err(Error::internal("between state must have three children"));
    };
    let value = execute(&between.input, value_state, input, sel, count)?;
    let lower = execute(&between.lower, lower_state, input, sel, count)?;
    let upper = execute(&between.upper, upper_state, input, sel, count)?;
    let above = compare_vectors(between.lower_comparison_type(), &value, &lower, count)?;
    let below = compare_vectors(between.upper_comparison_type(), &value, &upper, count)?;
    conjoin(ExpressionType::ConjunctionAnd, &above, &below, count)
}

/// DuckDB's `CASE` execution: route rows through the WHEN checks and evaluate each THEN
/// (and the ELSE) only on the rows that reach it.
fn execute_case(
    case: &BoundCaseExpression,
    ty: &LogicalType,
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    // Results are first written at input-row positions, then compacted to `sel`.
    let mut result = Vector::flat(ty.clone(), position_capacity(sel, count));
    let mut current: Option<Vec<u32>> = sel.map(<[u32]>::to_vec);
    let mut current_count = count;
    let (check_states, else_state) = state.children.split_at_mut(case.case_checks.len() * 2);
    let (check_pairs, _) = check_states.as_chunks_mut::<2>();
    for (check, [when_state, then_state]) in case.case_checks.iter().zip(check_pairs) {
        let mut true_sel = Vec::new();
        let mut false_sel = Vec::new();
        let true_count = select(
            &check.when_expr,
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
            return execute(&check.then_expr, then_state, input, sel, count);
        }
        let values = execute(&check.then_expr, then_state, input, Some(&true_sel), true_count)?;
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
            return execute(&case.else_expr, else_state, input, sel, count);
        }
        let remaining = current.unwrap_or_default();
        let values = execute(
            &case.else_expr,
            else_state,
            input,
            Some(&remaining),
            current_count,
        )?;
        scatter(&values, &mut result, &remaining)?;
    }
    Ok(compact(result, sel))
}

/// DuckDB's `ExecuteOperator`: `NOT`, `IS [NOT] NULL`, `[NOT] IN`, `COALESCE` and `TRY`.
fn execute_operator(
    expression: &Expression,
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    match expression.expression_type {
        ExpressionType::CompareIn | ExpressionType::CompareNotIn => {
            let (Some(left_expression), Some((left_state, child_states))) =
                (children.first(), state.children.split_first_mut())
            else {
                return Err(Error::internal("IN without a left side"));
            };
            let left = execute(left_expression, left_state, input, sel, count)?;
            let mut intermediate = Vector::constant(&Value::boolean(false))?;
            for (index, (child, child_state)) in children[1..].iter().zip(child_states).enumerate() {
                let right = execute(child, child_state, input, sel, count)?;
                let equal = compare_vectors(ExpressionType::CompareEqual, &left, &right, count)?;
                intermediate = if index == 0 {
                    equal
                } else {
                    conjoin(ExpressionType::ConjunctionOr, &intermediate, &equal, count)?
                };
            }
            if expression.expression_type == ExpressionType::CompareNotIn {
                return Ok(not(&intermediate, count));
            }
            Ok(intermediate)
        }
        ExpressionType::OperatorCoalesce => {
            execute_coalesce(&expression.return_type, children, state, input, sel, count)
        }
        ExpressionType::OperatorTry => execute_try(children, state, input, sel, count),
        ExpressionType::OperatorNot => {
            let value = execute_only_child(children, state, input, sel, count)?;
            Ok(not(&value, count))
        }
        ExpressionType::OperatorIsNull | ExpressionType::OperatorIsNotNull => {
            let value = execute_only_child(children, state, input, sel, count)?;
            let want_null = expression.expression_type == ExpressionType::OperatorIsNull;
            if value.kind() == VectorKind::Constant {
                return Vector::constant(&Value::boolean(value.is_null(0) == want_null));
            }
            let mut out = Vector::flat(LogicalType::BOOLEAN, count);
            for (i, slot) in out.values_mut::<bool>().iter_mut().enumerate().take(count) {
                *slot = value.is_null(i) == want_null;
            }
            Ok(out)
        }
        other => Err(Error::not_implemented(format!(
            "Unsupported operator type with 0 children: {}",
            other.name()
        ))),
    }
}

fn execute_only_child(
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let ([child], [child_state]) = (children, &mut state.children[..]) else {
        return Err(Error::internal("operator expects exactly one child"));
    };
    execute(child, child_state, input, sel, count)
}

fn not(value: &Vector, count: usize) -> Vector {
    let mut out = Vector::flat(LogicalType::BOOLEAN, 0);
    unary::<bool, bool>(value, count, &mut out, |value| !value);
    out
}

/// DuckDB's `COALESCE` execution: each argument is evaluated only on the rows for which
/// all previous arguments were NULL.
fn execute_coalesce(
    ty: &LogicalType,
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let mut result = Vector::flat(ty.clone(), position_capacity(sel, count));
    let mut remaining: Vec<u32> = (0..count).map(|i| row(sel, i)).collect();
    for (child, child_state) in children.iter().zip(&mut state.children) {
        let values = execute(child, child_state, input, Some(&remaining), remaining.len())?;
        let mut found_rows = Vec::new();
        let mut found_positions = Vec::new();
        let mut next = Vec::new();
        for (i, &position) in remaining.iter().enumerate() {
            if values.is_null(i) {
                next.push(position);
            } else {
                found_rows.push(u32::try_from(i).map_err(|_| Error::internal("row index overflow"))?);
                found_positions.push(position);
            }
        }
        if !found_rows.is_empty() {
            let found = values.slice(&slice_selection(&found_rows));
            scatter(&found, &mut result, &found_positions)?;
        }
        remaining = next;
        if remaining.is_empty() {
            break;
        }
    }
    for &position in &remaining {
        result
            .validity_mut()
            .set_invalid(position as usize, result_len(sel, count));
    }
    Ok(compact(result, sel))
}

fn result_len(sel: Option<&[u32]>, count: usize) -> usize {
    position_capacity(sel, count)
}

/// DuckDB's `TRY` execution: on an error, rows are re-evaluated one at a time and the
/// failing ones become NULL.
fn execute_try(
    children: &[Expression],
    state: &mut ExpressionState,
    input: &DataChunk,
    sel: Option<&[u32]>,
    count: usize,
) -> Result<Vector> {
    let ([child], [child_state]) = (children, &mut state.children[..]) else {
        return Err(Error::internal("TRY expects exactly one child"));
    };
    match execute(child, child_state, input, sel, count) {
        Ok(result) => return Ok(result),
        Err(error) if error.kind() == ErrorKind::Internal => return Err(error),
        Err(_) => {}
    }
    let mut result = Vector::flat(child.return_type.clone(), position_capacity(sel, count));
    for i in 0..count {
        let position = [row(sel, i)];
        match execute(child, child_state, input, Some(&position), 1) {
            Ok(value) => scatter(&value, &mut result, &position)?,
            Err(error) if error.kind() == ErrorKind::Internal => return Err(error),
            Err(_) => {
                let len = result_len(sel, count);
                result.validity_mut().set_invalid(position[0] as usize, len);
            }
        }
    }
    Ok(compact(result, sel))
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
    match (&expression.kind, expression.expression_type) {
        (ExpressionKind::Conjunction(conjunction), ExpressionType::ConjunctionAnd) => select_and(
            &conjunction.children,
            state,
            input,
            sel,
            count,
            true_sel,
            false_sel,
        ),
        (ExpressionKind::Conjunction(conjunction), ExpressionType::ConjunctionOr) => select_or(
            &conjunction.children,
            state,
            input,
            sel,
            count,
            true_sel,
            false_sel,
        ),
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
        let remaining = current.unwrap_or_else(|| (0..count).map(|i| row(None, i)).collect());
        false_sel.extend_from_slice(&remaining[..current_count]);
    }
    Ok(result_count)
}

/// Compares two vectors row by row (DuckDB's `VectorOperations::Equals` and friends,
/// plus the NULL-aware `[NOT] DISTINCT FROM`).
fn compare_vectors(
    expression_type: ExpressionType,
    left: &Vector,
    right: &Vector,
    count: usize,
) -> Result<Vector> {
    let mut result = Vector::flat(LogicalType::BOOLEAN, 0);
    let simple = match expression_type {
        ExpressionType::CompareEqual => Some(CompareOp::Equal),
        ExpressionType::CompareNotequal => Some(CompareOp::NotEqual),
        ExpressionType::CompareLessthan => Some(CompareOp::LessThan),
        ExpressionType::CompareGreaterthan => Some(CompareOp::GreaterThan),
        ExpressionType::CompareLessthanorequalto => Some(CompareOp::LessThanOrEqual),
        ExpressionType::CompareGreaterthanorequalto => Some(CompareOp::GreaterThanOrEqual),
        ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom => None,
        other => {
            return Err(Error::internal(format!(
                "Unknown comparison type {}",
                other.name()
            )));
        }
    };
    if let Some(op) = simple {
        compare(op, left, right, count, &mut result)?;
        return Ok(result);
    }
    // NULL-aware: NULL IS NOT DISTINCT FROM NULL is true; results are never NULL.
    let mut equal = Vector::flat(LogicalType::BOOLEAN, 0);
    compare(CompareOp::Equal, left, right, count, &mut equal)?;
    let distinct = expression_type == ExpressionType::CompareDistinctFrom;
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
fn conjoin(expression_type: ExpressionType, left: &Vector, right: &Vector, count: usize) -> Result<Vector> {
    let is_and = match expression_type {
        ExpressionType::ConjunctionAnd => true,
        ExpressionType::ConjunctionOr => false,
        other => {
            return Err(Error::internal(format!(
                "Unknown conjunction type {}",
                other.name()
            )));
        }
    };
    let (l, r) = (left.unified::<bool>(), right.unified::<bool>());
    let mut out = Vector::flat(LogicalType::BOOLEAN, count);
    let mut nulls = Vec::new();
    {
        let values = out.values_mut::<bool>();
        for (i, value) in values.iter_mut().enumerate().take(count) {
            let combined = if is_and {
                match (l.get(i), r.get(i)) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            } else {
                match (l.get(i), r.get(i)) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }
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
    Ok(out)
}
