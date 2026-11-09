//! `MERGE INTO` (DuckDB's `PhysicalMergeInto`): each input row is classified as matched,
//! not matched by the target or not matched by the source, and goes to the first action
//! of its class whose condition holds. Actions sink into their own INSERT, UPDATE or
//! DELETE operator, which are not part of the pipeline tree.

use std::any::Any;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use roastedduck_ast::{MergeActionCondition, MergeActionType};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_planner::Expression;
use roastedduck_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use super::modify::count_types;
use crate::expression_executor::ExpressionExecutor;
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// One action of a merge (DuckDB's `MergeIntoOperator`).
#[derive(Debug)]
pub struct MergeIntoOperator {
    pub action_type: MergeActionType,
    /// The `AND ...` condition, if any.
    pub condition: Option<Expression>,
    /// The operator the action's rows are sunk into (INSERT, UPDATE, DELETE).
    pub op: Option<PhysicalRef>,
    /// Computes the operator's input (or the error message) from the merge's input.
    pub expressions: Vec<Expression>,
}

/// The classes of rows, in the order they are processed (`match_actions`).
const MATCH_ACTIONS: [MergeActionCondition; 3] = [
    MergeActionCondition::WhenMatched,
    MergeActionCondition::WhenNotMatchedByTarget,
    MergeActionCondition::WhenNotMatchedBySource,
];

/// `MergeIntoStatement::ActionConditionToString`.
const fn action_condition_to_string(condition: MergeActionCondition) -> &'static str {
    match condition {
        MergeActionCondition::WhenMatched => "WHEN MATCHED",
        MergeActionCondition::WhenNotMatchedBySource => "WHEN NOT MATCHED BY SOURCE",
        MergeActionCondition::WhenNotMatchedByTarget => "WHEN NOT MATCHED",
    }
}

/// Runs the actions of a `MERGE INTO` (DuckDB's `PhysicalMergeInto`).
#[derive(Debug)]
pub struct PhysicalMergeInto {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    actions: Vec<MergeIntoOperator>,
    /// The actions of each class of [`MATCH_ACTIONS`].
    action_ranges: [Range<usize>; 3],
    row_id_index: usize,
    source_marker: Option<usize>,
    return_chunk: bool,
}

impl PhysicalMergeInto {
    /// `actions` lists, per condition, the actions in order.
    pub fn new(
        child: PhysicalRef,
        table_types: Vec<LogicalType>,
        actions: Vec<(MergeActionCondition, Vec<MergeIntoOperator>)>,
        row_id_index: usize,
        source_marker: Option<usize>,
        return_chunk: bool,
    ) -> Self {
        let mut flat = Vec::new();
        let mut action_ranges = [0..0, 0..0, 0..0];
        for (condition, condition_actions) in actions {
            let start = flat.len();
            flat.extend(condition_actions);
            if let Some(slot) = MATCH_ACTIONS.iter().position(|c| *c == condition) {
                action_ranges[slot] = start..flat.len();
            }
        }
        let types = if return_chunk {
            let mut types = table_types;
            types.push(LogicalType::VARCHAR);
            types
        } else {
            count_types()
        };
        Self {
            types,
            children: [child],
            actions: flat,
            action_ranges,
            row_id_index,
            source_marker,
            return_chunk,
        }
    }

    fn sink_of(&self, index: usize) -> Option<&dyn ErasedSink> {
        self.actions[index].op.as_ref().and_then(|op| op.as_sink())
    }

    /// `ComputeMatches`: the rows of each class of [`MATCH_ACTIONS`].
    fn compute_matches(&self, chunk: &DataChunk) -> [Vec<u32>; 3] {
        let mut matches: [Vec<u32>; 3] = Default::default();
        let row_ids = chunk.column(self.row_id_index);
        let marker = self.source_marker.map(|index| chunk.column(index));
        for row in 0..chunk.len() {
            let class = if marker.is_some_and(|marker| marker.is_null(row)) {
                2
            } else {
                usize::from(row_ids.is_null(row))
            };
            matches[class].push(row as u32);
        }
        matches
    }

    /// The error of an `ERROR` action reached by `input`.
    fn merge_error(condition: MergeActionCondition, action: &MergeIntoOperator, input: &DataChunk) -> Error {
        let mut message = action_condition_to_string(condition).to_owned();
        if let Some(action_condition) = &action.condition {
            message.push_str(" AND ");
            message.push_str(&action_condition.to_string());
        }
        if !action.expressions.is_empty() && !input.is_empty() {
            message.push_str(": ");
            message.push_str(&input.column(0).get_value(0).to_varchar().unwrap_or_default());
        }
        Error::new(ErrorKind::Constraint, format!("Merge error condition {message}"))
    }

    /// Runs the actions of one class over its rows (`MergeIntoGlobalState::Sink`).
    fn sink_class(
        &self,
        ctx: &ExecutionContext,
        class: usize,
        chunk: &DataChunk,
        global: &MergeGlobal,
        local: &mut MergeLocal,
    ) -> Result<()> {
        let condition = MATCH_ACTIONS[class];
        // The rows of `chunk` no action has taken yet.
        let mut remaining: Option<Vec<u32>> = None;
        for index in self.action_ranges[class].clone() {
            let input = match &remaining {
                None => chunk.clone(),
                Some(rows) if rows.is_empty() => break,
                Some(rows) => slice(chunk, rows),
            };
            let action = &self.actions[index];
            let state = &mut local.actions[index];
            let selected = if let Some(executor) = &mut state.condition {
                let mut selection = Vec::new();
                let count = executor.select(&input, &mut selection)?;
                let current: Vec<u32> = remaining
                    .take()
                    .unwrap_or_else(|| (0..chunk.len() as u32).collect());
                let mut taken = vec![false; current.len()];
                for &row in &selection {
                    taken[row as usize] = true;
                }
                remaining = Some(
                    current
                        .iter()
                        .zip(&taken)
                        .filter(|&(_, taken)| !taken)
                        .map(|(&row, _)| row)
                        .collect(),
                );
                if count == 0 {
                    continue;
                }
                if count == input.len() {
                    input
                } else {
                    slice(&input, &selection)
                }
            } else {
                remaining = Some(Vec::new());
                input
            };
            let action_input = match &mut state.executor {
                Some(executor) => {
                    let mut output = DataChunk::default();
                    executor.execute(&selected, &mut output)?;
                    output
                }
                None => selected,
            };
            let Some(sink) = self.sink_of(index) else {
                if action.action_type == MergeActionType::MergeError {
                    return Err(Self::merge_error(condition, action, &action_input));
                }
                continue;
            };
            local.merged_count += action_input.len() as u64;
            let (Some(op_global), Some(op_local)) = (&global.states[index], &mut state.local) else {
                return Err(Error::internal("merge action without sink state"));
            };
            sink.sink(ctx, &action_input, op_global.as_ref(), op_local.as_mut())?;
        }
        Ok(())
    }
}

/// The rows `rows` of `chunk`.
fn slice(chunk: &DataChunk, rows: &[u32]) -> DataChunk {
    let sel = SelectionVector::new(rows.to_vec());
    DataChunk::new(
        chunk.columns().iter().map(|column| column.slice(&sel)).collect(),
        rows.len(),
    )
}

impl PhysicalOperator for PhysicalMergeInto {
    fn name(&self) -> &'static str {
        "MERGE_INTO"
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
}

/// The shared state of a merge: the sink states of the actions' operators.
#[derive(Debug)]
pub struct MergeGlobal {
    states: Vec<Option<Box<dyn Any + Send + Sync>>>,
    /// The finalized states, for the operators' source role (RETURNING).
    finalized: Vec<Option<SinkState>>,
    merged_count: AtomicU64,
}

/// A thread's state of one action.
#[derive(Debug)]
struct ActionLocal {
    local: Option<Box<dyn Any + Send>>,
    condition: Option<ExpressionExecutor>,
    executor: Option<ExpressionExecutor>,
}

/// A thread's merge state.
#[derive(Debug)]
pub struct MergeLocal {
    actions: Vec<ActionLocal>,
    merged_count: u64,
}

impl Sink for PhysicalMergeInto {
    type Global = MergeGlobal;
    type Local = MergeLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<Self::Global> {
        let states = (0..self.actions.len())
            .map(|index| {
                self.sink_of(index)
                    .map(|sink| sink.global_sink_state(ctx))
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(MergeGlobal {
            states,
            finalized: Vec::new(),
            merged_count: AtomicU64::new(0),
        })
    }

    fn local_sink_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local> {
        let mut actions = Vec::with_capacity(self.actions.len());
        for (index, action) in self.actions.iter().enumerate() {
            let local = match (self.sink_of(index), &global.states[index]) {
                (Some(sink), Some(state)) => Some(sink.local_sink_state(ctx, state.as_ref())?),
                _ => None,
            };
            actions.push(ActionLocal {
                local,
                condition: action
                    .condition
                    .as_ref()
                    .map(|condition| ExpressionExecutor::new(vec![condition.clone()])),
                executor: (!action.expressions.is_empty())
                    .then(|| ExpressionExecutor::new(action.expressions.clone())),
            });
        }
        Ok(MergeLocal {
            actions,
            merged_count: 0,
        })
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        let matches = self.compute_matches(chunk);
        for (class, rows) in matches.iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            let class_chunk = if rows.len() == chunk.len() {
                chunk.clone()
            } else {
                slice(chunk, rows)
            };
            self.sink_class(ctx, class, &class_chunk, global, local)?;
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, ctx: &ExecutionContext, global: &Self::Global, local: Self::Local) -> Result<()> {
        for (index, action) in local.actions.into_iter().enumerate() {
            if let (Some(sink), Some(state), Some(op_local)) =
                (self.sink_of(index), &global.states[index], action.local)
            {
                sink.combine(ctx, state.as_ref(), op_local)?;
            }
        }
        global
            .merged_count
            .fetch_add(local.merged_count, Ordering::Relaxed);
        Ok(())
    }

    fn finalize(&self, ctx: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        for index in 0..self.actions.len() {
            if let (Some(sink), Some(state)) = (self.sink_of(index), &mut global.states[index]) {
                sink.finalize(ctx, state.as_mut())?;
            }
        }
        global.finalized = global
            .states
            .iter_mut()
            .map(|state| state.take().map(SinkState::from))
            .collect();
        Ok(SinkFinalize::Ready)
    }
}

/// The source states of the actions' operators (RETURNING).
#[derive(Debug)]
pub struct MergeSourceGlobal {
    sink: SinkState,
    states: Vec<Option<Box<dyn Any + Send + Sync>>>,
}

/// The action being returned and the source states of the actions' operators.
#[derive(Debug)]
pub struct MergeSourceLocal {
    index: usize,
    states: Vec<Option<Box<dyn Any + Send>>>,
    count_emitted: bool,
}

impl PhysicalMergeInto {
    fn source_of(&self, index: usize) -> Option<&dyn ErasedSource> {
        self.actions[index].op.as_ref().and_then(|op| op.as_source())
    }
}

impl Source for PhysicalMergeInto {
    type Global = MergeSourceGlobal;
    type Local = MergeSourceLocal;

    fn global_source_state(&self, ctx: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("merge source requires its sink state"))?;
        let global = sink
            .downcast_ref::<MergeGlobal>()
            .ok_or_else(|| Error::internal("merge sink state has the wrong type"))?;
        let mut states = Vec::with_capacity(self.actions.len());
        for index in 0..self.actions.len() {
            let state = match (
                self.return_chunk,
                self.source_of(index),
                global.finalized.get(index),
            ) {
                (true, Some(source), Some(Some(finalized))) => {
                    Some(source.global_source_state(ctx, Some(finalized))?)
                }
                _ => None,
            };
            states.push(state);
        }
        Ok(MergeSourceGlobal {
            sink: SinkState::clone(sink),
            states,
        })
    }

    fn local_source_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local> {
        let mut states = Vec::with_capacity(self.actions.len());
        for (index, state) in global.states.iter().enumerate() {
            states.push(match (self.source_of(index), state) {
                (Some(source), Some(state)) => Some(source.local_source_state(ctx, state.as_ref())?),
                _ => None,
            });
        }
        Ok(MergeSourceLocal {
            index: 0,
            states,
            count_emitted: false,
        })
    }

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        if !self.return_chunk {
            if local.count_emitted {
                chunk.set_len(0);
                return Ok(SourceResult::Finished);
            }
            local.count_emitted = true;
            let merge = global
                .sink
                .downcast_ref::<MergeGlobal>()
                .ok_or_else(|| Error::internal("merge sink state has the wrong type"))?;
            let count = i64::try_from(merge.merged_count.load(Ordering::Relaxed))
                .map_err(|_| Error::internal("row count overflow"))?;
            let column = Vector::constant(&Value::bigint(count))?.flattened(1);
            chunk.set_columns(vec![column], 1);
            return Ok(SourceResult::Finished);
        }
        while local.index < self.actions.len() {
            let index = local.index;
            let (Some(source), Some(state), Some(op_local)) = (
                self.source_of(index),
                &global.states[index],
                &mut local.states[index],
            ) else {
                local.index += 1;
                continue;
            };
            let mut scan = DataChunk::default();
            let result = source.get_data(ctx, &mut scan, state.as_ref(), op_local.as_mut())?;
            if result == SourceResult::Finished {
                local.index += 1;
            }
            if scan.is_empty() {
                continue;
            }
            let name = match self.actions[index].action_type {
                MergeActionType::MergeUpdate => "UPDATE",
                MergeActionType::MergeInsert => "INSERT",
                MergeActionType::MergeDelete => "DELETE",
                _ => return Err(Error::internal("Unsupported merge action for RETURNING")),
            };
            let len = scan.len();
            let mut columns = scan.into_columns();
            columns.push(Vector::constant(&Value::varchar(name))?.flattened(len));
            chunk.set_columns(columns, len);
            return Ok(SourceResult::HaveMoreOutput);
        }
        chunk.set_len(0);
        Ok(SourceResult::Finished)
    }
}
