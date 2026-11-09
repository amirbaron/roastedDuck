//! Window aggregates through hand-built plans, with test aggregates (the SQL tests in
//! `crates/roastedduck/tests/query.rs` cover the non-aggregate functions).
#![allow(clippy::unwrap_used, clippy::float_cmp, clippy::cast_precision_loss)]
// The test aggregate's states are allocated with its `u64` layout.
#![allow(clippy::cast_ptr_alignment)]

use std::sync::{Arc, Mutex};

use roastedduck_ast::{OrderByNullType, OrderType, WindowBoundary, WindowExcludeMode};
use roastedduck_common::Result;
use roastedduck_execution::operators::{CollectedResult, PhysicalResultCollector};
use roastedduck_execution::{Executor, PhysicalRef, create_physical_plan};
use roastedduck_function::aggregate::{
    AggregateFunction, AggregateInput, OrderDependence, Unary, UnaryAggregate, WindowAggregate, WindowFrame,
    WindowPartitionInput,
};
use roastedduck_function::{AggregateOps, StatePtr};
use roastedduck_planner::expression::{BoundOrderByNode, BoundWindowExpression};
use roastedduck_planner::{Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

fn run(mut plan: LogicalOperator) -> Result<Vec<Vec<Value>>> {
    plan.resolve_operator_types()?;
    let physical = create_physical_plan(plan)?;
    let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(physical));
    let state = Executor::execute(&root, roastedduck_execution::ClientEnv::standalone()?)?;
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

/// A table with columns `i BIGINT` (0..count) and `x DOUBLE` (`x(i)`).
fn table(count: usize, x: impl Fn(usize) -> Option<f64>) -> LogicalOperator {
    let chunks: Vec<DataChunk> = (0..count)
        .collect::<Vec<_>>()
        .chunks(2048)
        .map(|rows| {
            let mut i = Vector::flat(LogicalType::BIGINT, rows.len());
            let mut values = Vector::flat(LogicalType::DOUBLE, rows.len());
            for (slot, &row) in rows.iter().enumerate() {
                i.set_value(slot, &Value::bigint(row as i64)).unwrap();
                let value = x(row).map_or(Value::null(LogicalType::DOUBLE), Value::double);
                values.set_value(slot, &value).unwrap();
            }
            DataChunk::new(vec![i, values], rows.len())
        })
        .collect();
    LogicalOperator::new(LogicalKind::ChunkGet {
        table_index: 0,
        chunk_types: vec![LogicalType::BIGINT, LogicalType::DOUBLE],
        collection: Arc::from(chunks),
    })
}

fn column_i() -> Expression {
    Expression::reference("i", LogicalType::BIGINT, 0)
}

fn column_x() -> Expression {
    Expression::reference("x", LogicalType::DOUBLE, 1)
}

fn asc(expression: Expression) -> BoundOrderByNode {
    BoundOrderByNode::new(OrderType::Ascending, OrderByNullType::NullsLast, expression)
}

/// A window function over the whole partition, adjusted by `configure`.
fn window(
    expression_type: ExpressionType,
    return_type: LogicalType,
    configure: impl FnOnce(&mut BoundWindowExpression),
) -> Expression {
    let mut window = BoundWindowExpression {
        aggregate: None,
        bind_info: None,
        children: Vec::new(),
        partitions: Vec::new(),
        orders: Vec::new(),
        filter_expr: None,
        ignore_nulls: false,
        distinct: false,
        start: WindowBoundary::UnboundedPreceding,
        end: WindowBoundary::UnboundedFollowing,
        exclude_clause: WindowExcludeMode::NoOther,
        start_expr: None,
        end_expr: None,
        offset_expr: None,
        default_expr: None,
        arg_orders: Vec::new(),
    };
    configure(&mut window);
    Expression::new(
        expression_type,
        return_type,
        ExpressionKind::Window(Box::new(window)),
    )
}

/// `ROWS BETWEEN before PRECEDING AND after FOLLOWING`.
fn rows_frame(window: &mut BoundWindowExpression, before: i64, after: i64) {
    window.start = WindowBoundary::ExprPrecedingRows;
    window.end = WindowBoundary::ExprFollowingRows;
    window.start_expr = Some(Expression::constant(Value::bigint(before)));
    window.end_expr = Some(Expression::constant(Value::bigint(after)));
}

fn window_plan(child: LogicalOperator, expressions: Vec<Expression>) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Window { window_index: 1 })
        .with_expressions(expressions)
        .with_children(vec![child])
}

/// DuckDB's `sum(DOUBLE)`: plain addition, order dependent (so the segment tree keeps
/// the left-to-right combine order).
struct SumDouble;

#[derive(Default)]
struct SumState {
    isset: bool,
    value: f64,
}

impl UnaryAggregate for SumDouble {
    type Input = f64;
    type State = SumState;
    type Output = f64;

    fn update(state: &mut SumState, value: f64) -> Result<()> {
        state.isset = true;
        state.value += value;
        Ok(())
    }

    fn combine(source: &SumState, target: &mut SumState) -> Result<()> {
        target.isset = target.isset || source.isset;
        target.value += source.value;
        Ok(())
    }

    fn finalize(state: &SumState) -> Result<Option<f64>> {
        Ok(state.isset.then_some(state.value))
    }
}

fn sum_double() -> AggregateFunction {
    AggregateFunction::new(
        "sum",
        vec![LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        Unary::<SumDouble>::default(),
    )
}

fn sum_x(configure: impl FnOnce(&mut BoundWindowExpression)) -> Expression {
    window(ExpressionType::WindowAggregate, LogicalType::DOUBLE, |window| {
        window.aggregate = Some(sum_double());
        window.children = vec![column_x()];
        configure(window);
    })
}

fn doubles(rows: &[Vec<Value>], column: usize) -> Vec<Option<f64>> {
    rows.iter().map(|row| row[column].as_f64()).collect()
}

#[test]
fn segment_tree_combines_in_duckdbs_order() {
    // Expected values from DuckDB 1.5.6: they differ from a left-to-right summation.
    let plan = window_plan(
        table(3000, |i| Some(i as f64 * 0.37)),
        vec![
            sum_x(|window| {
                window.orders = vec![asc(column_i())];
                rows_frame(window, 1000, 1000);
                window.exclude_clause = WindowExcludeMode::CurrentRow;
            }),
            sum_x(|window| {
                window.orders = vec![asc(column_i())];
                rows_frame(window, 1000, 1000);
                window.exclude_clause = WindowExcludeMode::Ties;
            }),
        ],
    );
    let rows = run(plan).unwrap();
    let excluded = doubles(&rows, 2);
    let ties = doubles(&rows, 3);
    for (row, current, tie) in [
        (0, 185_185.000_000_000_03, 185_185.000_000_000_03),
        (3, 186_296.11, 186_297.219_999_999_97),
        (102, 224_830.87, 224_868.610_000_000_04),
        (104, 225_646.720_000_000_03, 225_685.2),
        (1500, 1_110_000.0, 1_110_555.0),
        (2999, 924_445.000_000_000_1, 925_554.630_000_000_1),
    ] {
        assert_eq!(excluded[row], Some(current), "EXCLUDE CURRENT ROW, row {row}");
        assert_eq!(ties[row], Some(tie), "EXCLUDE TIES, row {row}");
    }
}

#[test]
fn constant_frames_aggregate_each_partition_once() {
    // Frames are the whole input (no PARTITION BY / ORDER BY), with and without FILTER.
    let plan = window_plan(
        table(5000, |i| if i % 7 == 0 { None } else { Some(i as f64) }),
        vec![
            sum_x(|_| {}),
            sum_x(|window| {
                window.filter_expr = Some(Expression::comparison(
                    ExpressionType::CompareGreaterthan,
                    column_i(),
                    Expression::constant(Value::bigint(4000)),
                ));
            }),
        ],
    );
    let rows = run(plan).unwrap();
    assert_eq!(rows.len(), 5000);
    let total: f64 = (0..5000).filter(|i| i % 7 != 0).map(|i| i as f64).sum();
    let filtered: f64 = (4001..5000).filter(|i| i % 7 != 0).map(|i| i as f64).sum();
    assert!(rows.iter().all(|row| row[2].as_f64() == Some(total)));
    assert!(rows.iter().all(|row| row[3].as_f64() == Some(filtered)));
}

#[test]
fn distinct_aggregates_count_each_value_once_per_frame() {
    // x cycles through 0..13, so a frame of width w holds min(w, 13) distinct values.
    let plan = window_plan(
        table(6000, |i| Some((i % 13) as f64)),
        vec![sum_x(|window| {
            window.distinct = true;
            window.orders = vec![asc(column_i())];
            rows_frame(window, 7, 2);
        })],
    );
    let rows = run(plan).unwrap();
    for (row, values) in rows.iter().enumerate() {
        let begin = row.saturating_sub(7);
        let end = (row + 3).min(6000);
        let mut distinct: Vec<usize> = (begin..end).map(|i| i % 13).collect();
        distinct.sort_unstable();
        distinct.dedup();
        let expected: f64 = distinct.iter().map(|&v| v as f64).sum();
        assert_eq!(values[2].as_f64(), Some(expected), "row {row}");
    }
}

/// Order dependent: the first value in frame order.
struct FirstDouble;

impl UnaryAggregate for FirstDouble {
    type Input = f64;
    type State = Option<f64>;
    type Output = f64;

    fn update(state: &mut Option<f64>, value: f64) -> Result<()> {
        state.get_or_insert(value);
        Ok(())
    }

    fn combine(source: &Option<f64>, target: &mut Option<f64>) -> Result<()> {
        if target.is_none() {
            *target = *source;
        }
        Ok(())
    }

    fn finalize(state: &Option<f64>) -> Result<Option<f64>> {
        Ok(*state)
    }
}

#[test]
fn ordered_aggregates_follow_the_argument_order() {
    // first(x ORDER BY i DESC) over a sliding frame is the frame's last value.
    let mut first = AggregateFunction::new(
        "first",
        vec![LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        Unary::<FirstDouble>::default(),
    );
    first.order_dependence = OrderDependence::OrderDependent;
    let plan = window_plan(
        table(300, |i| Some(i as f64)),
        vec![window(
            ExpressionType::WindowAggregate,
            LogicalType::DOUBLE,
            |window| {
                window.aggregate = Some(first);
                window.children = vec![column_x()];
                window.orders = vec![asc(column_i())];
                window.arg_orders = vec![BoundOrderByNode::new(
                    OrderType::Descending,
                    OrderByNullType::NullsLast,
                    column_i(),
                )];
                rows_frame(window, 3, 4);
            },
        )],
    );
    let rows = run(plan).unwrap();
    for (row, values) in rows.iter().enumerate() {
        assert_eq!(values[2].as_f64(), Some((row + 4).min(299) as f64), "row {row}");
    }
}

/// An aggregate with its own window evaluation: the number of frame rows, computed from
/// the frames, plus the partition size remembered by `window_init`.
struct FrameRows;

impl AggregateOps for FrameRows {
    fn state_layout(&self) -> std::alloc::Layout {
        std::alloc::Layout::new::<u64>()
    }
    fn needs_destructor(&self) -> bool {
        false
    }
    unsafe fn initialize(&self, state: StatePtr) {
        // SAFETY: the caller provides memory for a u64.
        unsafe { state.as_ptr().cast::<u64>().write(0) };
    }
    unsafe fn update(&self, _: &[Vector], _: AggregateInput<'_>, _: &[StatePtr], _: usize) -> Result<()> {
        Ok(())
    }
    unsafe fn simple_update(&self, _: &[Vector], _: AggregateInput<'_>, _: StatePtr, _: usize) -> Result<()> {
        Ok(())
    }
    unsafe fn combine(&self, _: &[StatePtr], _: &[StatePtr], _: AggregateInput<'_>) -> Result<()> {
        Ok(())
    }
    unsafe fn finalize(&self, _: &[StatePtr], _: AggregateInput<'_>, _: &mut Vector, _: usize) -> Result<()> {
        Ok(())
    }
    unsafe fn destroy(&self, _: &[StatePtr]) {}
    fn window(&self) -> Option<&dyn WindowAggregate> {
        Some(self)
    }
}

impl WindowAggregate for FrameRows {
    fn has_window_init(&self) -> bool {
        true
    }

    unsafe fn window_init(
        &self,
        _: AggregateInput<'_>,
        partition: &WindowPartitionInput<'_>,
        global: StatePtr,
    ) -> Result<()> {
        // SAFETY: the state is an initialised u64.
        unsafe { global.as_ptr().cast::<u64>().write(partition.count as u64) };
        Ok(())
    }

    unsafe fn window(
        &self,
        _: AggregateInput<'_>,
        _: &WindowPartitionInput<'_>,
        global: Option<StatePtr>,
        _: StatePtr,
        frames: &[WindowFrame],
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        // SAFETY: the shared state is an initialised u64.
        let partition = global.map_or(0, |state| unsafe { state.as_ptr().cast::<u64>().read() });
        let width: usize = frames.iter().map(|frame| frame.end - frame.start).sum();
        result.values_mut::<i64>()[row] = (partition * 1000 + width as u64) as i64;
        Ok(())
    }
}

#[test]
fn custom_window_callbacks_see_frames_and_partition() {
    let plan = window_plan(
        table(10, |i| Some(i as f64)),
        vec![window(
            ExpressionType::WindowAggregate,
            LogicalType::BIGINT,
            |window| {
                window.aggregate = Some(AggregateFunction::new(
                    "frame_rows",
                    vec![LogicalType::DOUBLE],
                    LogicalType::BIGINT,
                    FrameRows,
                ));
                window.children = vec![column_x()];
                window.orders = vec![asc(column_i())];
                rows_frame(window, 2, 1);
                window.exclude_clause = WindowExcludeMode::CurrentRow;
            },
        )],
    );
    let widths: Vec<Value> = run(plan).unwrap().into_iter().map(|row| row[2].clone()).collect();
    let expected: Vec<Value> = (0..10usize)
        .map(|row| {
            let width = (row + 2).min(10) - row.saturating_sub(2) - 1;
            Value::bigint(10_000 + width as i64)
        })
        .collect();
    assert_eq!(widths, expected);
}
