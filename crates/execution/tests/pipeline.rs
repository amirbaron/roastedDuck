//! End-to-end execution of small hand-built logical plans.
#![allow(clippy::unwrap_used)]

use rustdb_execution::operators::{CollectedResult, PhysicalResultCollector};
use rustdb_execution::{Executor, PhysicalRef, create_physical_plan};
use rustdb_planner::{BoundFunction, Expression, LogicalKind, LogicalOperator};
use rustdb_types::{LogicalType, Value};
use std::sync::{Arc, Mutex};

fn plus(left: Expression, right: Expression) -> Expression {
    let set = rustdb_functions::builtin_scalar_functions()
        .into_iter()
        .find(|set| &*set.name == "+")
        .unwrap();
    let function = set
        .functions
        .into_iter()
        .find(|f| f.arguments[0] == LogicalType::INTEGER)
        .unwrap();
    Expression::Function(BoundFunction {
        function,
        children: vec![left, right],
        bind_data: None,
        is_operator: true,
    })
}

fn run(plan: LogicalOperator) -> rustdb_common::Result<Vec<Vec<Value>>> {
    let physical = create_physical_plan(plan)?;
    let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(physical));
    let state = Executor::execute(&root)?;
    let collected = state
        .downcast_ref::<Mutex<CollectedResult>>()
        .unwrap()
        .lock()
        .unwrap();
    Ok(collected
        .chunks
        .iter()
        .flat_map(|chunk| {
            (0..chunk.len()).map(move |row| {
                (0..chunk.column_count())
                    .map(|c| chunk.column(c).get_value(row))
                    .collect()
            })
        })
        .collect())
}

fn select(expressions: Vec<Expression>) -> LogicalOperator {
    let scan = LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }, vec![], vec![]);
    LogicalOperator::new(
        LogicalKind::Projection { table_index: 1 },
        vec![scan],
        expressions,
    )
}

#[test]
fn select_constants_and_arithmetic() {
    let plan = select(vec![
        Expression::Constant(Value::integer(42)),
        plus(
            Expression::Constant(Value::integer(1)),
            Expression::Constant(Value::integer(2)),
        ),
        Expression::Constant(Value::varchar("a")),
    ]);
    let rows = run(plan).unwrap();
    assert_eq!(
        rows,
        vec![vec![Value::integer(42), Value::integer(3), Value::varchar("a")]]
    );
}

#[test]
fn errors_propagate_out_of_the_pipeline() {
    let plan = select(vec![plus(
        Expression::Constant(Value::integer(i32::MAX)),
        Expression::Constant(Value::integer(1)),
    )]);
    let error = run(plan).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Out of Range Error: Overflow in addition of INT32 (2147483647 + 1)!"
    );
}

fn function(name: &str, ty: &LogicalType, children: Vec<Expression>) -> Expression {
    let set = rustdb_functions::builtin_scalar_functions()
        .into_iter()
        .find(|set| &*set.name == name)
        .unwrap();
    let function = set.functions.into_iter().find(|f| &f.arguments[0] == ty).unwrap();
    Expression::Function(BoundFunction {
        function,
        children,
        bind_data: None,
        is_operator: true,
    })
}

/// A one-column INTEGER table.
fn ints(values: &[Option<i32>]) -> LogicalOperator {
    use rustdb_types::{DataChunk, Vector};
    let mut vector = Vector::flat(LogicalType::INTEGER, values.len());
    for (row, value) in values.iter().enumerate() {
        vector
            .set_value(
                row,
                &value.map_or(Value::null(LogicalType::INTEGER), Value::integer),
            )
            .unwrap();
    }
    let chunk = DataChunk::new(vec![vector], values.len());
    LogicalOperator::new(
        LogicalKind::ColumnDataGet {
            table_index: 0,
            types: vec![LogicalType::INTEGER],
            chunks: Arc::from(vec![chunk]),
        },
        vec![],
        vec![],
    )
}

fn column() -> Expression {
    Expression::Reference {
        index: 0,
        ty: LogicalType::INTEGER,
    }
}

fn int(value: i32) -> Expression {
    Expression::Constant(Value::integer(value))
}

fn cmp(op: rustdb_planner::ComparisonOp, left: Expression, right: Expression) -> Expression {
    Expression::Comparison {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

#[test]
fn filters_drop_null_and_false_rows() {
    use rustdb_planner::ComparisonOp::GreaterThan;
    let filter = LogicalOperator::new(
        LogicalKind::Filter,
        vec![ints(&[Some(1), None, Some(5), Some(3)])],
        vec![cmp(GreaterThan, column(), int(2))],
    );
    let rows = run(filter).unwrap();
    assert_eq!(rows, vec![vec![Value::integer(5)], vec![Value::integer(3)]]);
}

#[test]
fn and_filters_only_evaluate_surviving_rows() {
    use rustdb_planner::ComparisonOp::{GreaterThan, LessThan};
    // x * 1000 overflows for x = 2^30, but that row fails the first conjunct.
    let big = 1 << 30;
    let filter = LogicalOperator::new(
        LogicalKind::Filter,
        vec![ints(&[Some(1), Some(big), Some(7)])],
        vec![
            cmp(LessThan, column(), int(100)),
            cmp(
                GreaterThan,
                function("*", &LogicalType::INTEGER, vec![column(), int(1000)]),
                int(5000),
            ),
        ],
    );
    assert_eq!(run(filter).unwrap(), vec![vec![Value::integer(7)]]);
}

#[test]
fn case_evaluates_branches_only_on_their_rows() {
    use rustdb_planner::ComparisonOp::LessThan;
    let big = 1 << 30;
    let case = Expression::Case {
        checks: vec![(
            cmp(LessThan, column(), int(100)),
            function("*", &LogicalType::INTEGER, vec![column(), int(1000)]),
        )],
        otherwise: Box::new(int(-1)),
        ty: LogicalType::INTEGER,
    };
    let projection = LogicalOperator::new(
        LogicalKind::Projection { table_index: 1 },
        vec![ints(&[Some(2), Some(big), None])],
        vec![case],
    );
    assert_eq!(
        run(projection).unwrap(),
        vec![
            vec![Value::integer(2000)],
            vec![Value::integer(-1)],
            vec![Value::integer(-1)]
        ]
    );
}
