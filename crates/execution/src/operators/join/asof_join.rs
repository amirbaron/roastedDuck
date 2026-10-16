//! The as-of join (DuckDB's `PhysicalAsOfJoin`): each left row joins the right row with
//! the same partition values whose ordering value is the closest one satisfying the
//! inequality.
//!
//! Both inputs are hash-partitioned on the equality conditions and sorted on the
//! partition values and the inequality value. For every left row in sort order, an
//! exponential plus binary search finds the last right row sorting before it; it is a
//! match if its partition values are equal.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::sort::{ChunkRow, NullOrder, OrderModifiers, OrderType, SortColumn, SortConfig, SortStrategy};
use rustdb_row::sort::{StrategyGlobalState, StrategyLocalState};
use rustdb_types::{DataChunk, LogicalType, ValidityMask};

use super::range_join::{KeyBytes, SortedRows, order_modifiers, sorts_before};
use super::{
    OuterJoinMarker, construct_semi_or_anti_join_result, is_left_outer_join, null_columns, reorder_conditions,
    slice_columns,
};
use crate::join_hashtable::flatten_sequences;
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// An as-of join; see the module docs. Both inputs are sinks of this operator, which
/// produces the result as a source.
#[derive(Debug)]
pub struct PhysicalAsOfJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, equalities first.
    conditions: Vec<JoinCondition>,
    /// The equality conditions (the partitions), by index.
    partitions: Vec<usize>,
    /// The inequality condition.
    inequality: usize,
    /// Whether the inequality sorts descending (`<`, `<=`).
    descending: bool,
    /// Whether the inequality excludes equality.
    strict: bool,
    /// Left key columns whose NULLs never match (DuckDB's `null_sensitive`, which
    /// indexes the keys by the position of the order among the orders).
    null_sensitive: Vec<usize>,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    estimated_cardinality: [usize; 2],
    children: [PhysicalRef; 2],
}

impl PhysicalAsOfJoin {
    /// An as-of join of `left` and `right` on equality conditions and one inequality;
    /// `estimated_cardinality` are the inputs' estimates.
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        mut conditions: Vec<JoinCondition>,
        left: PhysicalRef,
        right: PhysicalRef,
        estimated_cardinality: [usize; 2],
    ) -> Result<Self> {
        reorder_conditions(&mut conditions);
        let mut partitions = Vec::new();
        let mut inequality = None;
        let mut descending = false;
        let mut strict = false;
        let mut null_sensitive = Vec::new();
        for (index, condition) in conditions.iter().enumerate() {
            let orders = usize::from(inequality.is_some());
            match condition.comparison {
                ExpressionType::CompareGreaterthanorequalto | ExpressionType::CompareGreaterthan => {
                    null_sensitive.push(orders);
                    inequality = Some(index);
                    strict = condition.comparison == ExpressionType::CompareGreaterthan;
                }
                ExpressionType::CompareLessthanorequalto | ExpressionType::CompareLessthan => {
                    null_sensitive.push(orders);
                    inequality = Some(index);
                    descending = true;
                    strict = condition.comparison == ExpressionType::CompareLessthan;
                }
                ExpressionType::CompareEqual => {
                    null_sensitive.push(orders);
                    partitions.push(index);
                }
                ExpressionType::CompareNotDistinctFrom => partitions.push(index),
                _ => return Err(Error::not_implemented("Unsupported join condition for ASOF join")),
            }
        }
        let inequality = inequality.ok_or_else(|| Error::internal("ASOF join without an inequality"))?;
        Ok(Self {
            types,
            join_type,
            conditions,
            partitions,
            inequality,
            descending,
            strict,
            null_sensitive,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            estimated_cardinality,
            children: [left, right],
        })
    }

    fn side_types(&self, child: usize) -> &[LogicalType] {
        if child == 0 { &self.left_types } else { &self.right_types }
    }

    /// The partition values and then the ordering value of one side.
    fn key_expressions(&self, child: usize) -> Vec<Expression> {
        self.partitions
            .iter()
            .chain(std::iter::once(&self.inequality))
            .map(|&index| {
                let condition = &self.conditions[index];
                if child == 0 {
                    condition.left.clone()
                } else {
                    condition.right.clone()
                }
            })
            .collect()
    }

    /// The modifiers of the full sort key: partitions ASC NULLS FIRST, then the order.
    fn key_modifiers(&self) -> Vec<OrderModifiers> {
        let mut modifiers = vec![OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst); self.partitions.len()];
        modifiers.push(order_modifiers(self.descending));
        modifiers
    }

    /// The sort strategy of one side (`SortStrategy::Factory`): the input columns
    /// followed by the key values, grouped on the partitions, sorted on the order.
    fn sort_strategy(&self, ctx: &ExecutionContext, child: usize) -> Result<SortStrategy> {
        let input_width = self.side_types(child).len();
        let mut input_types = self.side_types(child).to_vec();
        input_types.extend(self.key_expressions(child).iter().map(|key| key.return_type.clone()));
        let partition_columns = (input_width..input_width + self.partitions.len()).collect();
        let orders = vec![SortColumn {
            column: input_width + self.partitions.len(),
            modifiers: order_modifiers(self.descending),
            stats: None,
        }];
        SortStrategy::new(
            ctx.env.buffer_manager.clone(),
            partition_columns,
            &[],
            orders,
            input_types,
            self.estimated_cardinality[child],
            true,
        )
    }

    fn sink_state<'a>(&self, state: &'a SinkState) -> Result<&'a AsOfGlobalSinkState> {
        state
            .downcast_ref::<AsOfGlobalSinkState>()
            .ok_or_else(|| Error::internal("ASOF join sink state has the wrong type"))
    }
}

impl PhysicalOperator for PhysicalAsOfJoin {
    fn name(&self) -> &'static str {
        "ASOF_JOIN"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn sinks_all_children(&self) -> bool {
        true
    }
}

/// Both inputs, partitioned and sorted (DuckDB's `AsOfGlobalSinkState`).
#[derive(Debug)]
pub struct AsOfGlobalSinkState {
    strategies: [SortStrategy; 2],
    states: [StrategyGlobalState; 2],
    /// The input being sunk: the right one first.
    child: AtomicUsize,
}

/// One thread's sink of one input (DuckDB's `AsOfLocalSinkState`).
#[derive(Debug)]
pub struct AsOfLocalSinkState {
    child: usize,
    executor: ExpressionExecutor,
    state: StrategyLocalState,
}

impl Sink for PhysicalAsOfJoin {
    type Global = AsOfGlobalSinkState;
    type Local = AsOfLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<AsOfGlobalSinkState> {
        let config = SortConfig {
            threads: ctx.env.threads,
            connections: 1,
            ..SortConfig::default()
        };
        let strategies = [self.sort_strategy(ctx, 0)?, self.sort_strategy(ctx, 1)?];
        let states = [
            strategies[0].global_state(config.clone())?,
            strategies[1].global_state(config)?,
        ];
        Ok(AsOfGlobalSinkState {
            strategies,
            states,
            child: AtomicUsize::new(1),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, global: &AsOfGlobalSinkState) -> Result<AsOfLocalSinkState> {
        let child = global.child.load(Ordering::Acquire);
        Ok(AsOfLocalSinkState {
            child,
            executor: ExpressionExecutor::new(self.key_expressions(child)),
            state: global.strategies[child].local_state(),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &AsOfGlobalSinkState,
        local: &mut AsOfLocalSinkState,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        local.executor.execute(chunk, &mut keys)?;
        let mut columns = chunk.columns().to_vec();
        columns.extend(keys.into_columns());
        let mut input = DataChunk::new(columns, chunk.len());
        flatten_sequences(&mut input);
        global.strategies[local.child].sink(&input, &global.states[local.child], &mut local.state)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &AsOfGlobalSinkState, local: AsOfLocalSinkState) -> Result<()> {
        let child = local.child;
        global.strategies[child].combine(&global.states[child], local.state)?;
        if child == 1 {
            // The left side partitions like the right side, so hash groups correspond.
            global.strategies[0].synchronize(&global.states[1], &global.states[0])?;
            global.child.store(0, Ordering::Release);
        }
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut AsOfGlobalSinkState) -> Result<SinkFinalize> {
        let [left, right] = &global.strategies;
        let [left_state, right_state] = &mut global.states;
        right.finalize(right_state)?;
        left.finalize(left_state)?;
        Ok(SinkFinalize::Ready)
    }
}

/// One side of a hash group, sorted: rows with their full keys and partition keys.
#[derive(Debug)]
struct SortedSide {
    rows: SortedRows,
    /// The normalized partition values of each row.
    partition_keys: KeyBytes,
}

impl SortedSide {
    fn count(&self) -> usize {
        self.rows.count()
    }
}

/// A hash group of both inputs (DuckDB's `AsOfHashGroup`).
#[derive(Debug)]
struct AsOfHashGroup {
    left: Option<SortedSide>,
    right: Option<SortedSide>,
    /// Which right rows found a match (RIGHT and FULL joins).
    right_outer: Vec<AtomicBool>,
}

/// A unit of source work (`AsOfSourceTask`).
#[derive(Debug)]
enum AsOfTask {
    /// Sort and materialize both sides of a group.
    Prepare(usize),
    /// Probe the left blocks in the range.
    Left { group: usize, begin: usize, end: usize },
    /// Emit the unmatched right rows of the blocks in the range.
    Right { group: usize, begin: usize, end: usize },
}

/// The probe of a range of left blocks (DuckDB's `AsOfProbeBuffer`).
#[derive(Debug)]
struct AsOfProbe {
    group: usize,
    /// The next left block and the end of the range.
    begin: usize,
    end: usize,
    /// The current right position: the search starts here.
    right_pos: usize,
    lhs_payload: DataChunk,
    lhs_match_sel: Vec<u32>,
    matches: Vec<usize>,
    left_outer: OuterJoinMarker,
    fetch_next_left: bool,
}

/// The task in progress.
#[derive(Debug)]
enum ActiveTask {
    Left(Box<AsOfProbe>),
    Right { group: usize, begin: usize, end: usize },
}

/// The source's progress (DuckDB's `AsOfGlobalSourceState`, run as one task sequence).
#[derive(Debug)]
struct AsOfSourceInner {
    groups: Vec<AsOfHashGroup>,
    tasks: VecDeque<AsOfTask>,
    active: Option<ActiveTask>,
    lhs_executor: ExpressionExecutor,
}

/// The source state: the sink's sorted inputs and the task queue.
#[derive(Debug)]
pub struct AsOfSourceState {
    sink: SinkState,
    inner: Mutex<AsOfSourceInner>,
}

impl PhysicalAsOfJoin {
    /// Sorts both sides of hash group `group` (stages `SORT`, `MATERIALIZE` and `GET`).
    fn prepare_group(&self, ctx: &ExecutionContext, sink: &AsOfGlobalSinkState, group: usize) -> Result<AsOfHashGroup> {
        let mut sides = [None, None];
        for (child, side) in sides.iter_mut().enumerate() {
            let strategy = &sink.strategies[child];
            let state = &sink.states[child];
            let rows = strategy.hash_groups(state);
            if rows.get(group).is_none_or(|rows| rows.count == 0) {
                continue;
            }
            strategy.sort_hash_group(state, group)?;
            let chunks = strategy.group_chunks(state, group)?;
            let width = self.side_types(child).len();
            let key_columns: Vec<usize> = (width..=width + self.partitions.len()).collect();
            let payload_columns: Vec<usize> = (0..width).collect();
            let rows = SortedRows::materialize(
                &ctx.env.buffer_manager,
                &chunks,
                &key_columns,
                &self.key_modifiers(),
                &payload_columns,
                self.side_types(child).to_vec(),
            )?;
            let mut partition_keys = KeyBytes::default();
            let partition_modifiers = &self.key_modifiers()[..self.partitions.len()];
            for chunk in &chunks {
                let columns: Vec<_> = (width..width + self.partitions.len())
                    .map(|column| chunk.column(column))
                    .collect();
                partition_keys.push_columns(&columns, partition_modifiers, chunk.len())?;
            }
            *side = Some(SortedSide { rows, partition_keys });
        }
        let [left, right] = sides;
        let right_count = right.as_ref().map_or(0, SortedSide::count);
        let right_outer = if matches!(self.join_type, JoinType::Right | JoinType::Outer) {
            (0..right_count).map(|_| AtomicBool::new(false)).collect()
        } else {
            Vec::new()
        };
        Ok(AsOfHashGroup {
            left,
            right,
            right_outer,
        })
    }

    /// Finds the match of every row of the next left block: the last right row sorting
    /// before it, if its partition values are equal (`ResolveJoin`).
    fn resolve_join(&self, group: &AsOfHashGroup, probe: &mut AsOfProbe) {
        probe.lhs_match_sel.clear();
        probe.matches.clear();
        let (Some(left), Some(right)) = (&group.left, &group.right) else {
            return;
        };
        let right_count = right.count();
        if right_count == 0 {
            return;
        }
        let base = probe.begin * STANDARD_VECTOR_SIZE;
        let count = left.count().saturating_sub(base).min(STANDARD_VECTOR_SIZE);
        probe.matches.resize(count, 0);
        let before = |right_row: usize, left_row: usize| {
            sorts_before(right.rows.key(right_row), left.rows.key(left_row), self.strict)
        };
        for i in 0..count {
            let left_pos = base + i;
            if !before(probe.right_pos, left_pos) {
                continue;
            }
            // Exponential search for a right row that no longer sorts before.
            let begin = probe.right_pos;
            let mut bound = 1;
            while begin + bound < right_count && before(begin + bound, left_pos) {
                bound *= 2;
            }
            // Binary search for the first such row; the one before it is the match.
            let mut first = begin + bound / 2;
            let mut last = (begin + bound).min(right_count);
            while first < last {
                let mid = first + (last - first) / 2;
                if before(mid, left_pos) {
                    first = mid + 1;
                } else {
                    last = mid;
                }
            }
            first -= 1;
            probe.right_pos = first;
            if !self.partitions.is_empty()
                && left.partition_keys.get(left_pos) != right.partition_keys.get(first)
            {
                continue;
            }
            probe.matches[i] = first;
            probe.lhs_match_sel.push(i as u32);
        }
    }

    /// Reads the next left block and drops matches of rows with NULL keys (`ScanLeft`).
    fn scan_left(&self, group: &AsOfHashGroup, probe: &mut AsOfProbe, executor: &mut ExpressionExecutor) -> Result<()> {
        let left = group
            .left
            .as_ref()
            .ok_or_else(|| Error::internal("ASOF probe without left rows"))?;
        let base = probe.begin * STANDARD_VECTOR_SIZE;
        let end = (base + STANDARD_VECTOR_SIZE).min(left.count());
        let rows: Vec<u32> = (base as u32..end as u32).collect();
        left.rows.gather(&rows, &mut probe.lhs_payload)?;
        probe.begin += 1;
        let mut keys = DataChunk::default();
        executor.execute(&probe.lhs_payload, &mut keys)?;
        let count = probe.lhs_payload.len();
        let mut valid = ValidityMask::all_valid();
        for &column in &self.null_sensitive {
            let key = keys.column(column);
            let validity = key.validity();
            if validity.is_all_valid() {
                continue;
            }
            let sel = key.sel();
            for row in 0..count {
                if !validity.is_valid(sel.slot(row)) {
                    valid.set_invalid(row, count);
                }
            }
        }
        if !valid.is_all_valid() {
            probe.lhs_match_sel.retain(|&row| valid.is_valid(row as usize));
        }
        Ok(())
    }

    /// The output of the next left block, or of its unmatched rows
    /// (`AsOfProbeBuffer::GetData`). May leave `chunk` empty.
    fn probe_next(
        &self,
        groups: &[AsOfHashGroup],
        probe: &mut AsOfProbe,
        executor: &mut ExpressionExecutor,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        if !probe.fetch_next_left {
            // A LEFT/FULL join emits the unmatched rows of the previous block.
            probe.fetch_next_left = true;
            probe
                .left_outer
                .construct_left_join_result(&probe.lhs_payload, &self.right_types, chunk);
            probe.left_outer.reset();
            return Ok(());
        }
        if probe.begin >= probe.end {
            return Ok(());
        }
        let group = &groups[probe.group];
        self.resolve_join(group, probe);
        self.scan_left(group, probe, executor)?;
        match self.join_type {
            JoinType::Semi | JoinType::Anti => {
                let mut found = vec![false; probe.lhs_payload.len()];
                for &row in &probe.lhs_match_sel {
                    found[row as usize] = true;
                }
                construct_semi_or_anti_join_result(&probe.lhs_payload, chunk, &found, self.join_type == JoinType::Semi);
            }
            JoinType::Left | JoinType::Inner | JoinType::Right | JoinType::Outer => {
                let right = group
                    .right
                    .as_ref()
                    .map(|right| &right.rows);
                let matched: Vec<u32> = probe
                    .lhs_match_sel
                    .iter()
                    .map(|&row| probe.matches[row as usize] as u32)
                    .collect();
                let mut columns = slice_columns(&probe.lhs_payload, &probe.lhs_match_sel);
                let mut rhs_input = DataChunk::default();
                match right {
                    Some(right) => right.gather(&matched, &mut rhs_input)?,
                    None => rhs_input.set_columns(null_columns(&self.right_types), 0),
                }
                columns.extend(rhs_input.into_columns());
                chunk.set_columns(columns, matched.len());
                probe.left_outer.reset();
                for &row in &probe.lhs_match_sel {
                    probe.left_outer.set_match(row as usize);
                }
                if !group.right_outer.is_empty() {
                    for &row in &matched {
                        group.right_outer[row as usize].store(true, Ordering::Relaxed);
                    }
                }
                probe.fetch_next_left = !probe.left_outer.enabled();
            }
            _ => return Err(Error::not_implemented("Unimplemented type for as-of join!")),
        }
        Ok(())
    }

    /// The unmatched right rows of the next block of a right task (`ExecuteRightTask`).
    fn right_next(&self, group: &AsOfHashGroup, begin: &mut usize, end: usize, chunk: &mut DataChunk) -> Result<()> {
        let Some(right) = &group.right else {
            *begin = end;
            return Ok(());
        };
        while *begin < end {
            let base = *begin * STANDARD_VECTOR_SIZE;
            *begin += 1;
            let rows: Vec<u32> = (base..(base + STANDARD_VECTOR_SIZE).min(right.count()))
                .filter(|&row| !group.right_outer[row].load(Ordering::Relaxed))
                .map(|row| row as u32)
                .collect();
            if rows.is_empty() {
                continue;
            }
            let mut payload = DataChunk::default();
            right.rows.gather(&rows, &mut payload)?;
            let mut columns = null_columns(&self.left_types);
            columns.extend(payload.into_columns());
            chunk.set_columns(columns, rows.len());
            return Ok(());
        }
        Ok(())
    }
}

impl Source for PhysicalAsOfJoin {
    type Global = AsOfSourceState;
    type Local = ();

    fn global_source_state(&self, ctx: &ExecutionContext, sink: Option<&SinkState>) -> Result<AsOfSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("ASOF join source requires its sink state"))?;
        let global = self.sink_state(sink)?;
        let left_rows = global.strategies[0].hash_groups(&global.states[0]);
        let right_rows = global.strategies[1].hash_groups(&global.states[1]);
        let group_count = left_rows.len().max(right_rows.len());
        let stats = |rows: &[ChunkRow], group: usize| rows.get(group).copied().unwrap_or_default();
        // Largest groups first (`CreateTaskList`).
        let mut blocks: Vec<(usize, usize)> = (0..group_count)
            .map(|group| {
                let chunks = stats(&left_rows, group).chunks.max(stats(&right_rows, group).chunks);
                (chunks, group)
            })
            .collect();
        blocks.sort_by(|a, b| b.cmp(a));
        let threads = ctx.env.threads.max(1);
        let per_thread = blocks.first().map_or(0, |&(chunks, _)| chunks.div_ceil(threads));
        let right_outer = matches!(self.join_type, JoinType::Right | JoinType::Outer);
        let mut tasks = VecDeque::new();
        if per_thread > 0 {
            for &(_, group) in &blocks {
                let left_chunks = stats(&left_rows, group).chunks;
                let right_chunks = if right_outer {
                    stats(&right_rows, group).chunks
                } else {
                    0
                };
                if left_chunks + stats(&right_rows, group).chunks == 0 {
                    continue;
                }
                tasks.push_back(AsOfTask::Prepare(group));
                for begin in (0..left_chunks).step_by(per_thread) {
                    tasks.push_back(AsOfTask::Left {
                        group,
                        begin,
                        end: (begin + per_thread).min(left_chunks),
                    });
                }
                for begin in (0..right_chunks).step_by(per_thread) {
                    tasks.push_back(AsOfTask::Right {
                        group,
                        begin,
                        end: (begin + per_thread).min(right_chunks),
                    });
                }
            }
        }
        let lhs_expressions = self.conditions.iter().map(|condition| condition.left.clone()).collect();
        Ok(AsOfSourceState {
            sink: Arc::clone(sink),
            inner: Mutex::new(AsOfSourceInner {
                groups: (0..group_count)
                    .map(|_| AsOfHashGroup {
                        left: None,
                        right: None,
                        right_outer: Vec::new(),
                    })
                    .collect(),
                tasks,
                active: None,
                lhs_executor: ExpressionExecutor::new(lhs_expressions),
            }),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AsOfSourceState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &AsOfSourceState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let sink = self.sink_state(&global.sink)?;
        let mut guard = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *guard;
        loop {
            if state.active.is_none() {
                match state.tasks.pop_front() {
                    None => return Ok(SourceResult::Finished),
                    Some(AsOfTask::Prepare(group)) => {
                        state.groups[group] = self.prepare_group(ctx, sink, group)?;
                        continue;
                    }
                    Some(AsOfTask::Left { group, begin, end }) => {
                        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
                        left_outer.initialize(STANDARD_VECTOR_SIZE);
                        state.active = Some(ActiveTask::Left(Box::new(AsOfProbe {
                            group,
                            begin,
                            end,
                            right_pos: 0,
                            lhs_payload: DataChunk::default(),
                            lhs_match_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
                            matches: Vec::with_capacity(STANDARD_VECTOR_SIZE),
                            left_outer,
                            fetch_next_left: true,
                        })));
                    }
                    Some(AsOfTask::Right { group, begin, end }) => {
                        state.active = Some(ActiveTask::Right { group, begin, end });
                    }
                }
            }
            match &mut state.active {
                Some(ActiveTask::Left(probe)) => {
                    while !probe.fetch_next_left || probe.begin < probe.end {
                        self.probe_next(&state.groups, probe, &mut state.lhs_executor, chunk)?;
                        if !chunk.is_empty() {
                            return Ok(SourceResult::HaveMoreOutput);
                        }
                    }
                }
                Some(ActiveTask::Right { group, begin, end }) => {
                    self.right_next(&state.groups[*group], begin, *end, chunk)?;
                    if !chunk.is_empty() {
                        return Ok(SourceResult::HaveMoreOutput);
                    }
                }
                None => {}
            }
            state.active = None;
        }
    }
}
