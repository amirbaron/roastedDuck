//! The hash join (DuckDB's `PhysicalHashJoin`, `physical_hash_join.cpp`): the right input
//! is built into a [`JoinHashTable`], the left input probes it, and RIGHT/FULL joins scan
//! the table for unmatched rows afterwards.

use rustdb_common::tree_renderer::RenderParams;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_planner::operator::JoinCondition;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_row::PartitionedAppendState;
use rustdb_types::{DataChunk, LogicalType};

use super::perfect_hash_join::{
    KeyRange, PerfectHashJoinExecutor, PerfectHashJoinState, is_perfect_hash_join_candidate,
};
use super::{
    construct_empty_join_result, empty_result_if_rhs_is_empty, propagates_build_side, reorder_conditions,
};
use crate::join_hashtable::{
    FullOuterScanState, JoinHashTable, JoinHashTableConditions, ProbeState, ScanStructure, flatten_sequences,
};
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

/// An equi-join through a hash table built on the right input; see the module docs.
#[derive(Debug)]
pub struct PhysicalHashJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, equalities first.
    conditions: Vec<JoinCondition>,
    hash_conditions: JoinHashTableConditions,
    /// Left input columns in the output (`lhs_output_columns`).
    lhs_output_columns: Vec<usize>,
    /// Right input columns stored besides the keys (`payload_columns`).
    payload_columns: Vec<usize>,
    payload_types: Vec<LogicalType>,
    /// Hash table layout columns in the output (`rhs_output_columns`).
    rhs_output_columns: Vec<usize>,
    /// Types of the correlated columns of a correlated MARK join (`delim_types`).
    delim_types: Vec<LogicalType>,
    single_join_error_on_multiple_rows: bool,
    /// Whether the join may use a perfect hash table, depending on its build keys.
    perfect_hash_candidate: bool,
    children: [PhysicalRef; 2],
}

impl PhysicalHashJoin {
    /// A hash join of `left` and `right` (DuckDB's `PhysicalHashJoin` constructor).
    /// Empty projection maps keep every column; `delim_types` are the MARK join's
    /// correlated column types.
    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's constructor")]
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        mut conditions: Vec<JoinCondition>,
        left_projection_map: &[u64],
        right_projection_map: &[u64],
        delim_types: Vec<LogicalType>,
        single_join_error_on_multiple_rows: bool,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Result<Self> {
        reorder_conditions(&mut conditions);
        let mut build_columns_in_conditions = HashMap::new();
        for (index, condition) in conditions.iter().enumerate() {
            if let ExpressionKind::Reference(reference) = &condition.right.kind {
                build_columns_in_conditions.insert(reference.index as usize, index);
            }
        }
        let hash_conditions = JoinHashTableConditions {
            comparisons: conditions.iter().map(|condition| condition.comparison).collect(),
            types: conditions
                .iter()
                .map(|condition| condition.left.return_type.clone())
                .collect(),
        };
        let lhs_output_columns = if left_projection_map.is_empty() {
            (0..left.types().len()).collect()
        } else {
            left_projection_map.iter().map(|&index| index as usize).collect()
        };
        let mut payload_columns = Vec::new();
        let mut payload_types = Vec::new();
        let mut rhs_output_columns = Vec::new();
        if !matches!(join_type, JoinType::Anti | JoinType::Semi | JoinType::Mark) {
            let rhs_types = right.types();
            let right_columns: Vec<usize> = if right_projection_map.is_empty() {
                (0..rhs_types.len()).collect()
            } else {
                right_projection_map.iter().map(|&index| index as usize).collect()
            };
            for column in right_columns {
                if let Some(&condition) = build_columns_in_conditions.get(&column) {
                    rhs_output_columns.push(condition);
                } else {
                    payload_columns.push(column);
                    payload_types.push(rhs_types[column].clone());
                    rhs_output_columns.push(conditions.len() + payload_types.len() - 1);
                }
            }
        }
        let perfect_hash_candidate = conditions.first().is_some_and(|condition| {
            is_perfect_hash_join_candidate(
                join_type,
                &hash_conditions.comparisons,
                &condition.left.return_type,
                right.types(),
            )
        });
        Ok(Self {
            types,
            join_type,
            conditions,
            hash_conditions,
            lhs_output_columns,
            payload_columns,
            payload_types,
            rhs_output_columns,
            delim_types,
            single_join_error_on_multiple_rows,
            perfect_hash_candidate,
            children: [left, right],
        })
    }

    /// A hash table for this join (`InitializeHashTable`).
    fn initialize_hash_table(&self, ctx: &ExecutionContext) -> Result<JoinHashTable> {
        let mut table = JoinHashTable::new(
            ctx.env.buffer_manager.clone(),
            &self.hash_conditions,
            &self.payload_types,
            self.join_type,
            self.rhs_output_columns.clone(),
            self.single_join_error_on_multiple_rows,
        )?;
        // A correlated ANY() has one condition more than correlated columns: count rows
        // and non-NULL values per correlated group.
        if self.join_type == JoinType::Mark
            && !self.delim_types.is_empty()
            && self.delim_types.len() + 1 == self.conditions.len()
        {
            table.enable_correlated_mark_join(self.delim_types.clone())?;
        }
        Ok(table)
    }

    fn left_expressions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.left.clone())
            .collect()
    }

    fn right_expressions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.right.clone())
            .collect()
    }

    /// The left input columns that appear in the output.
    fn lhs_output(&self, input: &DataChunk) -> DataChunk {
        let columns = self
            .lhs_output_columns
            .iter()
            .map(|&column| input.column(column).clone())
            .collect();
        DataChunk::new(columns, input.len())
    }
}

impl PhysicalOperator for PhysicalHashJoin {
    fn name(&self) -> &'static str {
        "HASH_JOIN"
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
        propagates_build_side(self.join_type).then_some(self as &dyn ErasedSource)
    }

    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        super::comparison_join_params(self.join_type, &self.conditions, estimated_cardinality)
    }
}

/// The shared build state (DuckDB's `HashJoinGlobalSinkState`).
#[derive(Debug)]
pub struct HashJoinGlobalSinkState {
    /// The global table, finalized for probing.
    hash_table: RwLock<JoinHashTable>,
    /// Tables built by each thread, merged when finalizing.
    local_hash_tables: Mutex<Vec<JoinHashTable>>,
    /// The range of the build keys, for the perfect hash join decision.
    key_range: Mutex<KeyRange>,
    /// The perfect hash table, used instead of the hash table's pointer table when the
    /// build keys allow it (`perfect_join_executor`).
    perfect_join_executor: Option<PerfectHashJoinExecutor>,
}

/// One thread's build state (DuckDB's `HashJoinLocalSinkState`).
#[derive(Debug)]
pub struct HashJoinLocalSinkState {
    join_key_executor: ExpressionExecutor,
    hash_table: JoinHashTable,
    append_state: PartitionedAppendState,
    key_range: KeyRange,
}

impl Sink for PhysicalHashJoin {
    type Global = HashJoinGlobalSinkState;
    type Local = HashJoinLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<HashJoinGlobalSinkState> {
        Ok(HashJoinGlobalSinkState {
            hash_table: RwLock::new(self.initialize_hash_table(ctx)?),
            local_hash_tables: Mutex::default(),
            key_range: Mutex::default(),
            perfect_join_executor: None,
        })
    }

    fn local_sink_state(
        &self,
        ctx: &ExecutionContext,
        _: &HashJoinGlobalSinkState,
    ) -> Result<HashJoinLocalSinkState> {
        let mut hash_table = self.initialize_hash_table(ctx)?;
        let append_state = hash_table.initialize_append_state();
        Ok(HashJoinLocalSinkState {
            join_key_executor: ExpressionExecutor::new(self.right_expressions()),
            hash_table,
            append_state,
            key_range: KeyRange::default(),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &HashJoinGlobalSinkState,
        local: &mut HashJoinLocalSinkState,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        local.join_key_executor.execute(chunk, &mut keys)?;
        flatten_sequences(&mut keys);
        if self.perfect_hash_candidate {
            local.key_range.update(keys.column(0), keys.len());
        }
        let mut payload = DataChunk::new(
            self.payload_columns
                .iter()
                .map(|&column| chunk.column(column).clone())
                .collect(),
            chunk.len(),
        );
        flatten_sequences(&mut payload);
        local.hash_table.build(&mut local.append_state, &keys, &payload)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &HashJoinGlobalSinkState,
        mut local: HashJoinLocalSinkState,
    ) -> Result<()> {
        local
            .hash_table
            .sink_collection_mut()
            .flush_append_state(&mut local.append_state);
        global
            .key_range
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(local.key_range);
        global
            .local_hash_tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(local.hash_table);
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut HashJoinGlobalSinkState) -> Result<SinkFinalize> {
        let locals = std::mem::take(
            global
                .local_hash_tables
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let table = global
            .hash_table
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for local in locals {
            table.merge(local)?;
        }
        table.unpartition()?;
        if table.count() > 0 && self.perfect_hash_candidate {
            let range = *global.key_range.get_mut().unwrap_or_else(PoisonError::into_inner);
            let rhs_types = &self.types[self.lhs_output_columns.len()..];
            global.perfect_join_executor = PerfectHashJoinExecutor::try_build(table, range, rhs_types)?;
        }
        if table.count() > 0 && global.perfect_join_executor.is_none() {
            table.allocate_pointer_table()?;
            table.initialize_pointer_table(0, table.capacity());
            table.finalize(0..table.data_collection().chunk_count(), false)?;
        }
        table.mark_finalized();
        if table.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// One thread's probe state (DuckDB's `HashJoinOperatorState`).
#[derive(Debug)]
pub struct HashJoinProbeState {
    probe_executor: ExpressionExecutor,
    join_keys: DataChunk,
    scan_structure: ScanStructure,
    probe_state: ProbeState,
    perfect_hash_join_state: PerfectHashJoinState,
}

impl Operator for PhysicalHashJoin {
    type Global = SinkState;
    type Local = HashJoinProbeState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal("hash join probe requires its sink state"))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("hash join probe requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<HashJoinProbeState> {
        Ok(HashJoinProbeState {
            probe_executor: ExpressionExecutor::new(self.left_expressions()),
            join_keys: DataChunk::default(),
            scan_structure: ScanStructure::default(),
            probe_state: ProbeState::default(),
            perfect_hash_join_state: PerfectHashJoinState::default(),
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        state: &mut HashJoinProbeState,
    ) -> Result<OperatorResult> {
        let global = super::sink_state::<HashJoinGlobalSinkState>(sink, "hash join")?;
        let table = global.hash_table.read().unwrap_or_else(PoisonError::into_inner);
        if table.count() == 0 {
            if empty_result_if_rhs_is_empty(self.join_type) {
                return Ok(OperatorResult::Finished);
            }
            let lhs_output = self.lhs_output(input);
            let right_types = &self.types[self.lhs_output_columns.len().min(self.types.len())..];
            construct_empty_join_result(self.join_type, table.has_null, &lhs_output, right_types, output)?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        if let Some(perfect) = &global.perfect_join_executor {
            state.probe_executor.execute(input, &mut state.join_keys)?;
            flatten_sequences(&mut state.join_keys);
            let lhs_output = self.lhs_output(input);
            perfect.probe(
                &state.join_keys,
                &lhs_output,
                &mut state.perfect_hash_join_state,
                output,
            )?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        if state.scan_structure.is_null {
            state.probe_executor.execute(input, &mut state.join_keys)?;
            flatten_sequences(&mut state.join_keys);
            table.probe(
                &mut state.scan_structure,
                &state.join_keys,
                &mut state.probe_state,
                None,
            )?;
        }
        let lhs_output = self.lhs_output(input);
        state
            .scan_structure
            .next(&table, &state.join_keys, &lhs_output, &self.types, output)?;
        if state.scan_structure.pointers_exhausted() && output.is_empty() {
            state.scan_structure.is_null = true;
            return Ok(OperatorResult::NeedMoreInput);
        }
        Ok(OperatorResult::HaveMoreOutput)
    }
}

/// The scan of unmatched build rows (DuckDB's `HashJoinGlobalSourceState` in its
/// `SCAN_HT` stage).
#[derive(Debug)]
pub struct HashJoinGlobalSourceState {
    sink: SinkState,
    /// The next data collection chunk to hand out.
    next_chunk: AtomicUsize,
    chunks_per_thread: usize,
    chunk_count: usize,
}

impl Source for PhysicalHashJoin {
    type Global = HashJoinGlobalSourceState;
    type Local = Option<FullOuterScanState>;

    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<HashJoinGlobalSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("hash join source requires its sink state"))?;
        let global = super::sink_state::<HashJoinGlobalSinkState>(sink, "hash join")?;
        let chunk_count = global
            .hash_table
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .data_collection()
            .chunk_count();
        let threads = ctx.env.threads.max(1);
        Ok(HashJoinGlobalSourceState {
            sink: Arc::clone(sink),
            next_chunk: AtomicUsize::new(0),
            chunks_per_thread: chunk_count.div_ceil(threads).max(1),
            chunk_count,
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &HashJoinGlobalSourceState) -> Result<Self::Local> {
        Ok(None)
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &HashJoinGlobalSourceState,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let sink = super::sink_state::<HashJoinGlobalSinkState>(&global.sink, "hash join")?;
        let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
        loop {
            if local.is_none() {
                let from = global
                    .next_chunk
                    .fetch_add(global.chunks_per_thread, Ordering::Relaxed);
                if from >= global.chunk_count {
                    return Ok(SourceResult::Finished);
                }
                let to = (from + global.chunks_per_thread).min(global.chunk_count);
                *local = Some(table.full_outer_scan_state(from..to));
            }
            if let Some(scan) = local.as_mut() {
                table.scan_full_outer(scan, &self.types, chunk)?;
            }
            if !chunk.is_empty() {
                return Ok(SourceResult::HaveMoreOutput);
            }
            *local = None;
        }
    }
}
