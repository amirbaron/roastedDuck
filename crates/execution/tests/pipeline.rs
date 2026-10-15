//! End-to-end execution of small hand-built logical plans.
#![allow(clippy::unwrap_used)]

use rustdb_execution::operators::{CollectedResult, PhysicalResultCollector};
use rustdb_execution::{Executor, PhysicalRef, create_physical_plan};
use rustdb_planner::expression::{
    AggregateType, BoundAggregateExpression, BoundCaseCheck, BoundCaseExpression,
};
use rustdb_planner::operator::LogicalAggregate;
use rustdb_planner::{BoundLimitNode, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator};
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
    Expression::function(function, vec![left, right], None, true)
}

fn run(mut plan: LogicalOperator) -> rustdb_common::Result<Vec<Vec<Value>>> {
    plan.resolve_operator_types()?;
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
    let scan = LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 });
    LogicalOperator::projection(1, expressions, scan)
}

#[test]
fn select_constants_and_arithmetic() {
    let plan = select(vec![
        Expression::constant(Value::integer(42)),
        plus(
            Expression::constant(Value::integer(1)),
            Expression::constant(Value::integer(2)),
        ),
        Expression::constant(Value::varchar("a")),
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
        Expression::constant(Value::integer(i32::MAX)),
        Expression::constant(Value::integer(1)),
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
    Expression::function(function, children, None, true)
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
    LogicalOperator::new(LogicalKind::ChunkGet {
        table_index: 0,
        chunk_types: vec![LogicalType::INTEGER],
        collection: Arc::from(vec![chunk]),
    })
}

fn column() -> Expression {
    Expression::reference("x", LogicalType::INTEGER, 0)
}

fn int(value: i32) -> Expression {
    Expression::constant(Value::integer(value))
}

fn cmp(op: ExpressionType, left: Expression, right: Expression) -> Expression {
    Expression::comparison(op, left, right)
}

const GREATER_THAN: ExpressionType = ExpressionType::CompareGreaterthan;
const LESS_THAN: ExpressionType = ExpressionType::CompareLessthan;

fn filter(child: LogicalOperator, predicates: Vec<Expression>) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Filter {
        projection_map: vec![],
    })
    .with_expressions(predicates)
    .with_children(vec![child])
}

#[test]
fn filters_drop_null_and_false_rows() {
    let plan = filter(
        ints(&[Some(1), None, Some(5), Some(3)]),
        vec![cmp(GREATER_THAN, column(), int(2))],
    );
    let rows = run(plan).unwrap();
    assert_eq!(rows, vec![vec![Value::integer(5)], vec![Value::integer(3)]]);
}

#[test]
fn and_filters_only_evaluate_surviving_rows() {
    // x * 1000 overflows for x = 2^30, but that row fails the first conjunct.
    let big = 1 << 30;
    let plan = filter(
        ints(&[Some(1), Some(big), Some(7)]),
        vec![
            cmp(LESS_THAN, column(), int(100)),
            cmp(
                GREATER_THAN,
                function("*", &LogicalType::INTEGER, vec![column(), int(1000)]),
                int(5000),
            ),
        ],
    );
    assert_eq!(run(plan).unwrap(), vec![vec![Value::integer(7)]]);
}

#[test]
fn case_evaluates_branches_only_on_their_rows() {
    let big = 1 << 30;
    let case = Expression::new(
        ExpressionType::CaseExpr,
        LogicalType::INTEGER,
        ExpressionKind::Case(Box::new(BoundCaseExpression {
            case_checks: vec![BoundCaseCheck {
                when_expr: cmp(LESS_THAN, column(), int(100)),
                then_expr: function("*", &LogicalType::INTEGER, vec![column(), int(1000)]),
            }],
            else_expr: int(-1),
        })),
    );
    let projection = LogicalOperator::projection(1, vec![case], ints(&[Some(2), Some(big), None]));
    assert_eq!(
        run(projection).unwrap(),
        vec![
            vec![Value::integer(2000)],
            vec![Value::integer(-1)],
            vec![Value::integer(-1)]
        ]
    );
}

/// `sum(INTEGER) -> BIGINT`, enough to exercise the aggregate operators.
struct TestSum;

impl rustdb_function::UnaryAggregate for TestSum {
    type Input = i32;
    type State = Option<i64>;
    type Output = i64;

    fn update(state: &mut Option<i64>, value: i32) -> rustdb_common::Result<()> {
        *state = Some(state.unwrap_or(0) + i64::from(value));
        Ok(())
    }

    fn combine(source: &Option<i64>, target: &mut Option<i64>) -> rustdb_common::Result<()> {
        if let Some(value) = source {
            *target = Some(target.unwrap_or(0) + value);
        }
        Ok(())
    }

    fn finalize(state: &Option<i64>) -> rustdb_common::Result<Option<i64>> {
        Ok(*state)
    }
}

fn sum(filter: Option<Expression>) -> Expression {
    let function = rustdb_function::AggregateFunction::new(
        "sum",
        vec![LogicalType::INTEGER],
        LogicalType::BIGINT,
        rustdb_function::aggregate::Unary::<TestSum>::default(),
    );
    Expression::new(
        ExpressionType::BoundAggregate,
        LogicalType::BIGINT,
        ExpressionKind::Aggregate(Box::new(BoundAggregateExpression {
            function,
            children: vec![column()],
            bind_info: None,
            aggr_type: AggregateType::NonDistinct,
            filter,
            order_bys: None,
        })),
    )
}

fn aggregate(child: LogicalOperator, aggregates: Vec<Expression>) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Aggregate(Box::new(LogicalAggregate {
        group_index: 1,
        aggregate_index: 2,
        groupings_index: 3,
        groups: vec![],
        grouping_sets: vec![],
        grouping_functions: vec![],
    })))
    .with_expressions(aggregates)
    .with_children(vec![child])
}

#[test]
fn ungrouped_aggregates_with_filters() {
    let grouped = aggregate(
        ints(&[Some(1), None, Some(5), Some(3)]),
        vec![sum(None), sum(Some(cmp(GREATER_THAN, column(), int(2))))],
    );
    assert_eq!(
        run(grouped).unwrap(),
        vec![vec![Value::bigint(9), Value::bigint(8)]]
    );

    let empty = aggregate(ints(&[None]), vec![sum(None)]);
    assert_eq!(run(empty).unwrap(), vec![vec![Value::null(LogicalType::BIGINT)]]);
}

#[test]
fn limit_and_offset() {
    let limit = LogicalOperator::new(LogicalKind::Limit {
        limit: BoundLimitNode::ConstantValue(2),
        offset: BoundLimitNode::ConstantValue(1),
    })
    .with_children(vec![ints(&[Some(1), Some(2), Some(3), Some(4)])]);
    assert_eq!(
        run(limit).unwrap(),
        vec![vec![Value::integer(2)], vec![Value::integer(3)]]
    );
}
