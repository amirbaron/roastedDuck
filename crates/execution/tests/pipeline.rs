//! End-to-end execution of small hand-built logical plans.
#![allow(clippy::unwrap_used)]

use rustdb_execution::operators::{CollectedResult, PhysicalResultCollector};
use rustdb_execution::{Executor, PhysicalRef, create_physical_plan};
use rustdb_planner::expression::{
    AggregateType, BoundAggregateExpression, BoundCaseCheck, BoundCaseExpression,
};
use rustdb_planner::operator::LogicalAggregate;
use rustdb_planner::{
    BoundLimitNode, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
};
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
        .find(|f| f.arguments == [LogicalType::INTEGER, LogicalType::INTEGER])
        .unwrap();
    Expression::function(function, vec![left, right], None, true)
}

fn run(mut plan: LogicalOperator) -> rustdb_common::Result<Vec<Vec<Value>>> {
    plan.resolve_operator_types()?;
    let physical = create_physical_plan(plan)?;
    let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(physical));
    let state = Executor::execute(&root, rustdb_execution::ClientEnv::standalone()?)?;
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
        group_stats: vec![],
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

/// A two-column INTEGER table `(g, x)`, split into chunks of at most 2048 rows.
fn pairs(rows: &[(Option<i32>, Option<i32>)]) -> LogicalOperator {
    use rustdb_types::{DataChunk, Vector};
    let to_value = |value: Option<i32>| value.map_or(Value::null(LogicalType::INTEGER), Value::integer);
    let chunks: Vec<DataChunk> = rows
        .chunks(2048)
        .map(|batch| {
            let mut g = Vector::flat(LogicalType::INTEGER, batch.len());
            let mut x = Vector::flat(LogicalType::INTEGER, batch.len());
            for (row, (gv, xv)) in batch.iter().enumerate() {
                g.set_value(row, &to_value(*gv)).unwrap();
                x.set_value(row, &to_value(*xv)).unwrap();
            }
            DataChunk::new(vec![g, x], batch.len())
        })
        .collect();
    LogicalOperator::new(LogicalKind::ChunkGet {
        table_index: 0,
        chunk_types: vec![LogicalType::INTEGER, LogicalType::INTEGER],
        collection: Arc::from(chunks),
    })
}

fn sum_of_x(filter: Option<Expression>) -> Expression {
    let mut expression = sum(filter);
    if let ExpressionKind::Aggregate(aggregate) = &mut expression.kind {
        aggregate.children = vec![Expression::reference("x", LogicalType::INTEGER, 1)];
    }
    expression
}

fn group_by(child: LogicalOperator, sets: Vec<Vec<u64>>, aggregates: Vec<Expression>) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Aggregate(Box::new(LogicalAggregate {
        group_index: 1,
        aggregate_index: 2,
        groupings_index: 3,
        groups: vec![Expression::reference("g", LogicalType::INTEGER, 0)],
        grouping_sets: sets.into_iter().map(|set| set.into_iter().collect()).collect(),
        grouping_functions: vec![],
        group_stats: vec![],
    })))
    .with_expressions(aggregates)
    .with_children(vec![child])
}

fn sorted(mut rows: Vec<Vec<Value>>) -> Vec<String> {
    let mut text: Vec<String> = rows
        .drain(..)
        .map(|row| {
            row.iter()
                .map(|v| v.to_varchar().unwrap())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    text.sort();
    text
}

#[test]
fn hash_aggregate_groups_nulls_and_filters() {
    let input = pairs(&[
        (Some(1), Some(10)),
        (None, Some(5)),
        (Some(2), Some(7)),
        (Some(1), Some(1)),
        (None, None),
    ]);
    let plan = group_by(
        input,
        vec![],
        vec![
            sum_of_x(None),
            sum_of_x(Some(cmp(
                GREATER_THAN,
                Expression::reference("x", LogicalType::INTEGER, 1),
                int(6),
            ))),
        ],
    );
    assert_eq!(sorted(run(plan).unwrap()), ["1|11|10", "2|7|7", "NULL|5|NULL"]);
}

#[test]
fn hash_aggregate_many_groups_across_chunks() {
    // 5000 groups, each seen three times across several chunks: forces table resizes.
    let rows: Vec<_> = (0..15_000).map(|i| (Some(i % 5000), Some(1))).collect();
    let plan = group_by(pairs(&rows), vec![], vec![sum_of_x(None)]);
    let result = run(plan).unwrap();
    assert_eq!(result.len(), 5000);
    assert!(result.iter().all(|row| row[1] == Value::bigint(3)));
}

#[test]
fn hash_aggregate_grouping_sets_include_the_empty_set() {
    let input = pairs(&[(Some(1), Some(10)), (Some(2), Some(7)), (Some(1), Some(1))]);
    // GROUP BY ROLLUP (g): sets {g} and {}.
    let plan = group_by(input, vec![vec![0], vec![]], vec![sum_of_x(None)]);
    assert_eq!(sorted(run(plan).unwrap()), ["1|11", "2|7", "NULL|18"]);
    let empty = group_by(pairs(&[]), vec![vec![0], vec![]], vec![sum_of_x(None)]);
    assert_eq!(sorted(run(empty).unwrap()), ["NULL|NULL"]);
}

#[test]
fn operators_with_buffered_output_do_not_repeat_rows() {
    // A cross product produces its output over several passes; each row must reach the
    // sink exactly once.
    let left = ints(&[Some(1), Some(2), Some(3)]);
    let right = ints(&[Some(10), Some(20)]);
    let product = LogicalOperator::new(LogicalKind::CrossProduct).with_children(vec![left, right]);
    let rows = run(product).unwrap();
    assert_eq!(rows.len(), 6);
    let mut pairs: Vec<(i128, i128)> = rows
        .iter()
        .map(|row| (row[0].as_i128().unwrap(), row[1].as_i128().unwrap()))
        .collect();
    pairs.sort_unstable();
    assert_eq!(pairs, [(1, 10), (1, 20), (2, 10), (2, 20), (3, 10), (3, 20)]);
}
