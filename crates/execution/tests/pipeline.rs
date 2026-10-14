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
