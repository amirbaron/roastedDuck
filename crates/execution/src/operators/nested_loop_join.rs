use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, SelectionVector, Value, Vector};

use crate::expression_executor::compare_vectors;
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

const SCALAR_SUBQUERY_ERROR: &str = "More than one row returned by a subquery used as an expression - scalar \
subqueries can only return a single row.\n\nUse \"SET scalar_subquery_error_on_multiple_rows=false\" to revert to \
previous behavior of returning a random row.";

/// A join evaluating its conditions on every pair of rows (DuckDB's
/// `PhysicalNestedLoopJoin`, generalised to every join type). The right input is
/// materialised by the sink; the left input streams through the operator; RIGHT and FULL
/// joins emit unmatched right rows from the source afterwards.
#[derive(Debug)]
pub struct PhysicalNestedLoopJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    conditions: Vec<JoinCondition>,
    /// Extra predicate over the joined row (left columns, then right columns).
    predicate: Option<Expression>,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalNestedLoopJoin {
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        conditions: Vec<JoinCondition>,
        predicate: Option<Expression>,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Result<Self> {
        match join_type {
            JoinType::Inner
            | JoinType::Left
            | JoinType::Right
            | JoinType::Outer
            | JoinType::Semi
            | JoinType::Anti
            | JoinType::Mark
            | JoinType::Single => {}
            other => return Err(Error::not_implemented(format!("{other:?} join"))),
        }
        Ok(Self {
            types,
            join_type,
            conditions,
            predicate,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            children: [left, right],
        })
    }

    fn empty_result_if_right_is_empty(&self) -> bool {
        matches!(self.join_type, JoinType::Inner | JoinType::Right | JoinType::Semi)
    }

    fn emits_unmatched_right(&self) -> bool {
        matches!(self.join_type, JoinType::Right | JoinType::Outer)
    }

    fn right_conditions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.right.clone())
            .collect()
    }

    fn left_conditions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.left.clone())
            .collect()
    }

    /// NULL constant columns of `types`, `count` rows.
    fn null_columns(types: &[LogicalType]) -> Result<Vec<Vector>> {
        types
            .iter()
            .map(|ty| Vector::constant(&Value::null(ty.clone())))
            .collect()
    }
}

impl PhysicalOperator for PhysicalNestedLoopJoin {
    fn name(&self) -> &'static str {
        "NESTED_LOOP_JOIN"
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
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        self.emits_unmatched_right().then_some(self as &dyn ErasedSource)
    }
}

/// One materialised chunk of the right input with its evaluated join keys.
#[derive(Debug)]
struct RightChunk {
    payload: DataChunk,
    keys: DataChunk,
    /// Row number of the chunk's first row in the whole right input.
    offset: usize,
}

/// The materialised right input (sink state).
#[derive(Debug, Default)]
pub struct NestedLoopJoinGlobal {
    collected: Mutex<Vec<(DataChunk, DataChunk)>>,
    has_null: AtomicBool,
    chunks: Vec<RightChunk>,
    /// Whether each right row found a partner (RIGHT/FULL joins).
    right_found: Vec<AtomicBool>,
}

impl NestedLoopJoinGlobal {
    fn right_count(&self) -> usize {
        self.chunks
            .last()
            .map_or(0, |chunk| chunk.offset + chunk.payload.len())
    }
}

/// Per-thread sink state: the right-key executor.
#[derive(Debug)]
pub struct NestedLoopJoinSinkLocal {
    keys: ExpressionExecutor,
}

fn chunk_has_nulls(chunk: &DataChunk) -> bool {
    chunk
        .columns()
        .iter()
        .any(|column| (0..chunk.len()).any(|row| column.is_null(row)))
}

impl Sink for PhysicalNestedLoopJoin {
    type Global = NestedLoopJoinGlobal;
    type Local = NestedLoopJoinSinkLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<NestedLoopJoinGlobal> {
        Ok(NestedLoopJoinGlobal::default())
    }

    fn local_sink_state(
        &self,
        _: &ExecutionContext,
        _: &NestedLoopJoinGlobal,
    ) -> Result<NestedLoopJoinSinkLocal> {
        Ok(NestedLoopJoinSinkLocal {
            keys: ExpressionExecutor::new(self.right_conditions()),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &NestedLoopJoinGlobal,
        local: &mut NestedLoopJoinSinkLocal,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        local.keys.execute(chunk, &mut keys)?;
        if self.join_type == JoinType::Mark
            && !global.has_null.load(Ordering::Relaxed)
            && chunk_has_nulls(&keys)
        {
            global.has_null.store(true, Ordering::Relaxed);
        }
        global
            .collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((chunk.clone(), keys));
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        _: &NestedLoopJoinGlobal,
        _: NestedLoopJoinSinkLocal,
    ) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut NestedLoopJoinGlobal) -> Result<SinkFinalize> {
        let collected = std::mem::take(global.collected.get_mut().unwrap_or_else(PoisonError::into_inner));
        let mut offset = 0;
        for (payload, keys) in collected {
            let len = payload.len();
            global.chunks.push(RightChunk {
                payload,
                keys,
                offset,
            });
            offset += len;
        }
        global.right_found = (0..offset).map(|_| AtomicBool::new(false)).collect();
        if offset == 0 && self.empty_result_if_right_is_empty() {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// Per-thread probe state: where the current left chunk is in the right input.
#[derive(Debug)]
pub struct NestedLoopJoinProbe {
    keys: ExpressionExecutor,
    predicate: Option<ExpressionExecutor>,
    left_keys: DataChunk,
    /// Whether `left_keys` belongs to the current input chunk.
    fresh_input: bool,
    right_chunk: usize,
    left_row: usize,
    left_found: Vec<bool>,
    /// LEFT/FULL: the unmatched left rows still need to be emitted.
    pending_left_outer: bool,
}

/// The rows of `right` (rows `0..right.len()`) matching left row `left_row` on every
/// condition.
fn matching_rows(
    conditions: &[JoinCondition],
    left_keys: &DataChunk,
    left_row: usize,
    right_keys: &DataChunk,
) -> Result<Vec<u32>> {
    let count = right_keys.len();
    let mut matches: Vec<u32> = (0..count as u32).collect();
    let repeat = SelectionVector::new(vec![left_row as u32; count]);
    for (index, condition) in conditions.iter().enumerate() {
        if matches.is_empty() {
            break;
        }
        let left = left_keys.column(index).slice(&repeat);
        let result = compare_vectors(condition.comparison, &left, right_keys.column(index), count)?;
        let values = result.unified::<bool>();
        matches.retain(|&row| values.get(row as usize) == Some(true));
    }
    Ok(matches)
}

impl Operator for PhysicalNestedLoopJoin {
    type Global = SinkState;
    type Local = NestedLoopJoinProbe;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal("nested loop join probe requires its sink state"))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("nested loop join probe requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<NestedLoopJoinProbe> {
        Ok(NestedLoopJoinProbe {
            keys: ExpressionExecutor::new(self.left_conditions()),
            predicate: self
                .predicate
                .as_ref()
                .map(|p| ExpressionExecutor::new(vec![p.clone()])),
            left_keys: DataChunk::default(),
            fresh_input: true,
            right_chunk: 0,
            left_row: 0,
            left_found: Vec::new(),
            pending_left_outer: false,
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        state: &mut NestedLoopJoinProbe,
    ) -> Result<OperatorResult> {
        let global = sink
            .downcast_ref::<NestedLoopJoinGlobal>()
            .ok_or_else(|| Error::internal("nested loop join sink state has the wrong type"))?;
        if global.right_count() == 0 {
            if self.empty_result_if_right_is_empty() {
                return Ok(OperatorResult::Finished);
            }
            self.empty_right_result(input, output, global)?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        if state.fresh_input {
            state.keys.execute(input, &mut state.left_keys)?;
            state.fresh_input = false;
            state.right_chunk = 0;
            state.left_row = 0;
            state.left_found = vec![false; input.len()];
            state.pending_left_outer = matches!(self.join_type, JoinType::Left | JoinType::Outer);
        }
        match self.join_type {
            JoinType::Semi | JoinType::Anti | JoinType::Mark | JoinType::Single => {
                self.simple_join(input, output, global, state)?;
                state.fresh_input = true;
                Ok(OperatorResult::NeedMoreInput)
            }
            _ => self.complex_join(input, output, global, state),
        }
    }
}

impl PhysicalNestedLoopJoin {
    /// DuckDB's `ConstructEmptyJoinResult` for joins whose right input is empty.
    fn empty_right_result(
        &self,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &NestedLoopJoinGlobal,
    ) -> Result<()> {
        let mut columns = input.columns().to_vec();
        match self.join_type {
            JoinType::Anti => {}
            JoinType::Mark => {
                let mark = if global.has_null.load(Ordering::Relaxed) {
                    Value::null(LogicalType::BOOLEAN)
                } else {
                    Value::boolean(false)
                };
                columns.push(Vector::constant(&mark)?);
            }
            _ => columns.extend(Self::null_columns(&self.right_types)?),
        }
        output.set_columns(columns, input.len());
        Ok(())
    }

    /// SEMI, ANTI, MARK and SINGLE joins: one output row per input row at most.
    fn simple_join(
        &self,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &NestedLoopJoinGlobal,
        state: &mut NestedLoopJoinProbe,
    ) -> Result<()> {
        let count = input.len();
        let mut found = vec![false; count];
        // SINGLE: the matching right row of each left row.
        let mut single: Vec<Option<(usize, u32)>> = vec![None; count];
        for row in 0..count {
            for (chunk_index, chunk) in global.chunks.iter().enumerate() {
                let mut matches = matching_rows(&self.conditions, &state.left_keys, row, &chunk.keys)?;
                if let Some(predicate) = &mut state.predicate {
                    matches = Self::apply_predicate(predicate, input, row, chunk, matches)?;
                }
                if matches.is_empty() {
                    continue;
                }
                if self.join_type == JoinType::Single {
                    if found[row] || matches.len() > 1 {
                        return Err(Error::invalid_input(SCALAR_SUBQUERY_ERROR));
                    }
                    single[row] = Some((chunk_index, matches[0]));
                }
                found[row] = true;
                if self.join_type != JoinType::Single {
                    break;
                }
            }
        }
        match self.join_type {
            JoinType::Semi | JoinType::Anti => {
                let want = self.join_type == JoinType::Semi;
                let selection: Vec<u32> = (0..count as u32)
                    .filter(|&row| found[row as usize] == want)
                    .collect();
                let len = selection.len();
                let sel = SelectionVector::new(selection);
                output.set_columns(input.columns().iter().map(|c| c.slice(&sel)).collect(), len);
            }
            JoinType::Mark => {
                let has_null = global.has_null.load(Ordering::Relaxed);
                let mut mark = Vector::flat(LogicalType::BOOLEAN, count);
                for (row, &matched) in found.iter().enumerate() {
                    let left_null =
                        (0..state.left_keys.column_count()).any(|c| state.left_keys.column(c).is_null(row));
                    let value = if left_null || (!matched && has_null) {
                        Value::null(LogicalType::BOOLEAN)
                    } else {
                        Value::boolean(matched)
                    };
                    mark.set_value(row, &value)?;
                }
                let mut columns = input.columns().to_vec();
                columns.push(mark);
                output.set_columns(columns, count);
            }
            _ => {
                // SINGLE: left row with its (only) partner, or NULLs.
                let mut columns = input.columns().to_vec();
                for (column, ty) in self.right_types.iter().enumerate() {
                    let mut vector = Vector::flat(ty.clone(), count);
                    for (row, partner) in single.iter().enumerate() {
                        let value = match partner {
                            Some((chunk, right_row)) => global.chunks[*chunk]
                                .payload
                                .column(column)
                                .get_value(*right_row as usize),
                            None => Value::null(ty.clone()),
                        };
                        vector.set_value(row, &value)?;
                    }
                    columns.push(vector);
                }
                output.set_columns(columns, count);
            }
        }
        Ok(())
    }

    /// Keeps the matches of left row `row` with `chunk` that satisfy the extra predicate.
    fn apply_predicate(
        predicate: &mut ExpressionExecutor,
        input: &DataChunk,
        row: usize,
        chunk: &RightChunk,
        matches: Vec<u32>,
    ) -> Result<Vec<u32>> {
        if matches.is_empty() {
            return Ok(matches);
        }
        let repeat = SelectionVector::new(vec![row as u32; matches.len()]);
        let right_sel = SelectionVector::new(matches.clone());
        let mut columns: Vec<Vector> = input.columns().iter().map(|c| c.slice(&repeat)).collect();
        columns.extend(chunk.payload.columns().iter().map(|c| c.slice(&right_sel)));
        let pairs = DataChunk::new(columns, matches.len());
        let mut passing = Vec::new();
        predicate.select(&pairs, &mut passing)?;
        Ok(passing.into_iter().map(|index| matches[index as usize]).collect())
    }

    /// INNER, LEFT, RIGHT and FULL joins: emits matching pairs one right chunk at a time,
    /// then (LEFT/FULL) the unmatched left rows.
    fn complex_join(
        &self,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &NestedLoopJoinGlobal,
        state: &mut NestedLoopJoinProbe,
    ) -> Result<OperatorResult> {
        while let Some(chunk) = global.chunks.get(state.right_chunk) {
            let mut left_sel = Vec::new();
            let mut right_sel = Vec::new();
            while state.left_row < input.len() {
                let row = state.left_row;
                let mut matches = matching_rows(&self.conditions, &state.left_keys, row, &chunk.keys)?;
                if let Some(predicate) = &mut state.predicate {
                    matches = Self::apply_predicate(predicate, input, row, chunk, matches)?;
                }
                if !left_sel.is_empty() && left_sel.len() + matches.len() > STANDARD_VECTOR_SIZE {
                    break;
                }
                for &right_row in &matches {
                    global.right_found[chunk.offset + right_row as usize].store(true, Ordering::Relaxed);
                }
                if !matches.is_empty() {
                    state.left_found[row] = true;
                }
                left_sel.extend(std::iter::repeat_n(row as u32, matches.len()));
                right_sel.extend(matches);
                state.left_row += 1;
            }
            if state.left_row >= input.len() {
                state.left_row = 0;
                state.right_chunk += 1;
            }
            if !left_sel.is_empty() {
                let len = left_sel.len();
                let left = SelectionVector::new(left_sel);
                let right = SelectionVector::new(right_sel);
                let mut columns: Vec<Vector> = input.columns().iter().map(|c| c.slice(&left)).collect();
                columns.extend(chunk.payload.columns().iter().map(|c| c.slice(&right)));
                output.set_columns(columns, len);
                return Ok(OperatorResult::HaveMoreOutput);
            }
        }
        if state.pending_left_outer {
            state.pending_left_outer = false;
            let unmatched: Vec<u32> = (0..input.len() as u32)
                .filter(|&row| !state.left_found[row as usize])
                .collect();
            if !unmatched.is_empty() {
                let len = unmatched.len();
                let sel = SelectionVector::new(unmatched);
                let mut columns: Vec<Vector> = input.columns().iter().map(|c| c.slice(&sel)).collect();
                columns.extend(Self::null_columns(&self.right_types)?);
                output.set_columns(columns, len);
            }
        }
        state.fresh_input = true;
        Ok(OperatorResult::NeedMoreInput)
    }
}

impl Source for PhysicalNestedLoopJoin {
    /// The finalized sink state and the next right chunk to scan for unmatched rows.
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("nested loop join source requires its sink state"))?;
        Ok((Arc::clone(sink), AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    /// Emits the right rows that never found a partner, with NULL left columns.
    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, next): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let global = sink
            .downcast_ref::<NestedLoopJoinGlobal>()
            .ok_or_else(|| Error::internal("nested loop join sink state has the wrong type"))?;
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(right) = global.chunks.get(index) else {
                return Ok(SourceResult::Finished);
            };
            let unmatched: Vec<u32> = (0..right.payload.len() as u32)
                .filter(|&row| !global.right_found[right.offset + row as usize].load(Ordering::Relaxed))
                .collect();
            if unmatched.is_empty() {
                continue;
            }
            let len = unmatched.len();
            let sel = SelectionVector::new(unmatched);
            let mut columns = Self::null_columns(&self.left_types)?;
            columns.extend(right.payload.columns().iter().map(|c| c.slice(&sel)));
            chunk.set_columns(columns, len);
            return Ok(SourceResult::HaveMoreOutput);
        }
    }
}
