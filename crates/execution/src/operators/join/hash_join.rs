//! The hash join (DuckDB's `PhysicalHashJoin`, `physical_hash_join.cpp`): the right input
//! is built into a [`JoinHashTable`], the left input probes it, and RIGHT/FULL joins scan
//! the table for unmatched rows afterwards.

use rustdb_common::tree_renderer::RenderParams;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

mod source;
pub use source::{HashJoinGlobalSourceState, HashJoinLocalSourceState};

use super::probe_spill::{LocalProbeSpill, ProbeSpill};

use rustdb_ast::JoinType;
use rustdb_buffer::{TemporaryMemoryContext, TemporaryMemoryState};
use rustdb_common::{Error, Result};
use rustdb_planner::operator::JoinCondition;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_row::{
    PartitionedAppendState, RadixPartitioning, TupleDataCollection, TupleDataLayout, ValidityType,
};
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE};

use super::join_filter::{
    JoinFilterInput, KeyMinMax, clear_join_filters, push_bloom_filter, set_join_filters,
};
use super::perfect_hash_join::{
    KeyRange, PerfectHashJoinExecutor, PerfectHashJoinState, is_perfect_hash_join_candidate,
};
use super::{construct_empty_join_result, empty_result_if_rhs_is_empty, reorder_conditions};
use crate::join_hashtable::{
    JoinHashTable, JoinHashTableConditions, ProbeState, ScanStructure, flatten_sequences,
};
use crate::operator::{
    ErasedOperator, ErasedSink, ErasedSource, OrderPreservationType, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult};

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
    /// The run-time filters set on the probe side's scans from the build keys.
    join_filters: Vec<rustdb_planner::operator::JoinFilter>,
    /// `dynamic_or_filter_threshold`.
    dynamic_or_filter_threshold: u64,
    /// The probe side's estimated cardinality, which decides on bloom filters.
    probe_cardinality: u64,
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
            join_filters: Vec::new(),
            dynamic_or_filter_threshold: 0,
            probe_cardinality: 0,
            children: [left, right],
        })
    }

    /// Sets the run-time filters on the probe side (DuckDB's `filter_pushdown`); the
    /// conditions are referenced by their position with equalities first. Up to
    /// `dynamic_or_filter_threshold` build rows, they include the IN list of the keys;
    /// against a probe side of `probe_cardinality` rows, a bloom filter.
    #[must_use]
    pub fn with_join_filters(
        mut self,
        join_filters: Vec<rustdb_planner::operator::JoinFilter>,
        dynamic_or_filter_threshold: u64,
        probe_cardinality: u64,
    ) -> Self {
        self.join_filters = join_filters;
        self.dynamic_or_filter_threshold = dynamic_or_filter_threshold;
        self.probe_cardinality = probe_cardinality;
        self
    }

    /// The key identifying this join's filters in the scans' run-time filter sets.
    fn filter_key(&self) -> usize {
        std::ptr::from_ref(self).addr()
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
    /// Always a source: RIGHT/FULL joins emit unmatched build rows, and external joins
    /// probe the rest of their partitions afterwards (DuckDB's `IsSource`).
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn parallel_operator(&self) -> bool {
        true
    }
    fn parallel_sink(&self) -> bool {
        true
    }
    fn parallel_source(&self) -> bool {
        true
    }
    fn source_order(&self) -> OrderPreservationType {
        OrderPreservationType::NoOrder
    }
    fn operator_order(&self) -> OrderPreservationType {
        OrderPreservationType::NoOrder
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
    /// The range of the build keys of each join filter.
    filter_ranges: Mutex<Vec<KeyMinMax>>,
    /// The parallel finalize of the pointer table, after `finalize`.
    table_finalize: TableFinalize,
    /// Whether the build side does not fit in memory: its partitions are built and probed
    /// in rounds (DuckDB's `external`).
    external: AtomicBool,
    /// The join's share of memory, negotiated with the other operators of the query
    /// (`temporary_memory_state`).
    temporary_memory: TemporaryMemoryState,
    memory_context: TemporaryMemoryContext,
    /// The memory the probe side needs to spill its rows (`probe_side_requirement`).
    probe_side_requirement: usize,
    /// Probe rows of partitions not built yet, for external joins (`probe_spill`).
    probe_spill: OnceLock<ProbeSpill>,
}

/// The stages of building the pointer table in parallel (DuckDB's
/// `HashJoinTableInitEvent` and `HashJoinFinalizeEvent`).
#[derive(Debug, Default)]
struct TableFinalize {
    /// The stage whose tasks are running.
    stage: AtomicUsize,
    /// How many tasks each stage has.
    tasks: AtomicUsize,
    /// Whether an insert task created a chain longer than one entry.
    chains_longer_than_one: std::sync::atomic::AtomicBool,
    /// The join filters to push bloom filters into once the table is complete.
    bloom_targets: Mutex<Vec<(usize, usize)>>,
}

impl TableFinalize {
    const NONE: usize = 0;
    const INITIALIZE: usize = 1;
    const INSERT: usize = 2;
    const DONE: usize = 3;
}

/// Pointer table entries one initialization task clears.
const ENTRIES_PER_INIT_TASK: usize = 1 << 20;
/// Below this many rows the table is built by one task, so chains keep their order
/// (DuckDB's `PARALLEL_CONSTRUCT_THRESHOLD`).
const PARALLEL_CONSTRUCT_THRESHOLD: usize = 1_048_576;

/// One thread's build state (DuckDB's `HashJoinLocalSinkState`).
#[derive(Debug)]
pub struct HashJoinLocalSinkState {
    join_key_executor: ExpressionExecutor,
    hash_table: JoinHashTable,
    append_state: PartitionedAppendState,
    key_range: KeyRange,
    filter_ranges: Vec<KeyMinMax>,
}

impl Sink for PhysicalHashJoin {
    type Global = HashJoinGlobalSinkState;
    type Local = HashJoinLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<HashJoinGlobalSinkState> {
        clear_join_filters(&self.join_filters, self.filter_key());
        let memory_context =
            ctx.env
                .buffer_manager
                .temporary_memory_context(ctx.env.threads().max(1) as u64, 1, false);
        Ok(HashJoinGlobalSinkState {
            hash_table: RwLock::new(self.initialize_hash_table(ctx)?),
            local_hash_tables: Mutex::default(),
            key_range: Mutex::default(),
            perfect_join_executor: None,
            filter_ranges: Mutex::new(vec![KeyMinMax::default(); self.join_filters.len()]),
            table_finalize: TableFinalize::default(),
            external: AtomicBool::new(false),
            temporary_memory: ctx
                .env
                .buffer_manager
                .temporary_memory_manager()
                .register(&memory_context),
            memory_context,
            probe_side_requirement: 0,
            probe_spill: OnceLock::new(),
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
            filter_ranges: vec![KeyMinMax::default(); self.join_filters.len()],
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
        for (filter, range) in self.join_filters.iter().zip(&mut local.filter_ranges) {
            if filter.condition < keys.column_count() {
                range.update(keys.column(filter.condition), keys.len());
            }
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
        if !self.join_filters.is_empty() {
            let mut ranges = global
                .filter_ranges
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for (range, local) in ranges.iter_mut().zip(local.filter_ranges) {
                range.merge(local);
            }
        }
        global
            .local_hash_tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(local.hash_table);
        Ok(())
    }

    fn finalize(&self, ctx: &ExecutionContext, global: &mut HashJoinGlobalSinkState) -> Result<SinkFinalize> {
        let locals = std::mem::take(
            global
                .local_hash_tables
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        if self.prepare_external(ctx, global, &locals)? {
            return self.finalize_external(ctx, global, locals);
        }
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
        let mut bloom_targets = Vec::new();
        if !self.join_filters.is_empty() && table.count() > 0 {
            let ranges = global
                .filter_ranges
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner);
            let key_types: Vec<LogicalType> = self
                .conditions
                .iter()
                .map(|condition| condition.right.return_type.clone())
                .collect();
            let probe_types: Vec<LogicalType> = self
                .conditions
                .iter()
                .map(|condition| condition.left.return_type.clone())
                .collect();
            bloom_targets = set_join_filters(&JoinFilterInput {
                key: self.filter_key(),
                filters: &self.join_filters,
                ranges,
                comparisons: &self.hash_conditions.comparisons,
                key_types: &key_types,
                probe_types: &probe_types,
                table,
                in_filter_threshold: self.dynamic_or_filter_threshold,
                probe_cardinality: self.probe_cardinality,
                is_perfect_hashtable: global.perfect_join_executor.is_some(),
            })?;
            table.set_build_bloom_filter(!bloom_targets.is_empty());
        }
        let count = table.count();
        if count > 0 && global.perfect_join_executor.is_none() {
            // The pointer table is cleared and filled by finalize tasks
            // (`ScheduleFinalize`).
            table.allocate_pointer_table()?;
            *global
                .table_finalize
                .bloom_targets
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = bloom_targets;
            global
                .table_finalize
                .stage
                .store(TableFinalize::NONE, Ordering::Relaxed);
        } else {
            global
                .table_finalize
                .stage
                .store(TableFinalize::DONE, Ordering::Relaxed);
            table.mark_finalized();
            self.push_bloom_filters(table, bloom_targets);
        }
        if count == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }

    /// The parallel stages of the pointer table: clear it in ranges of entries, then
    /// insert the rows in ranges of chunks (DuckDB's `HashJoinTableInitEvent` and
    /// `HashJoinFinalizeEvent`); then mark the table finalized.
    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &HashJoinGlobalSinkState) -> Result<usize> {
        let finalize = &global.table_finalize;
        let threads = ctx.env.threads().max(1);
        let (next, tasks) = match finalize.stage.load(Ordering::Acquire) {
            TableFinalize::NONE => {
                let capacity = global
                    .hash_table
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .capacity();
                (
                    TableFinalize::INITIALIZE,
                    capacity.div_ceil(ENTRIES_PER_INIT_TASK).clamp(1, threads),
                )
            }
            TableFinalize::INITIALIZE => {
                let table = global.hash_table.read().unwrap_or_else(PoisonError::into_inner);
                let chunks = table.data_collection().chunk_count();
                let tasks = if table.count() < PARALLEL_CONSTRUCT_THRESHOLD {
                    1
                } else {
                    threads.min(chunks).max(1)
                };
                (TableFinalize::INSERT, tasks)
            }
            TableFinalize::INSERT => {
                let mut table = global.hash_table.write().unwrap_or_else(PoisonError::into_inner);
                table.finish_parallel_finalize(finalize.chains_longer_than_one.load(Ordering::Acquire));
                table.mark_finalized();
                let bloom_targets = std::mem::take(
                    &mut *finalize
                        .bloom_targets
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner),
                );
                self.push_bloom_filters(&table, bloom_targets);
                (TableFinalize::DONE, 0)
            }
            _ => return Ok(0),
        };
        finalize.tasks.store(tasks, Ordering::Release);
        finalize.stage.store(next, Ordering::Release);
        Ok(tasks)
    }

    fn execute_finalize_task(
        &self,
        _: &ExecutionContext,
        global: &HashJoinGlobalSinkState,
        task: usize,
    ) -> Result<()> {
        let finalize = &global.table_finalize;
        let tasks = finalize.tasks.load(Ordering::Acquire).max(1);
        let table = global.hash_table.read().unwrap_or_else(PoisonError::into_inner);
        match finalize.stage.load(Ordering::Acquire) {
            TableFinalize::INITIALIZE => {
                let per_task = table.capacity().div_ceil(tasks);
                table.initialize_pointer_table(task * per_task, (task + 1) * per_task);
            }
            TableFinalize::INSERT => {
                let chunks = table.data_collection().chunk_count();
                let per_task = chunks.div_ceil(tasks);
                let range = (task * per_task).min(chunks)..((task + 1) * per_task).min(chunks);
                if table.insert_chunks(range, tasks > 1)? {
                    finalize.chains_longer_than_one.store(true, Ordering::Release);
                }
            }
            stage => {
                return Err(Error::internal(format!(
                    "hash join finalize task in stage {stage}"
                )));
            }
        }
        Ok(())
    }
}

impl PhysicalHashJoin {
    /// The memory a thread needs to spill probe rows over the partitions of `radix_bits`
    /// bits (`GetPartitioningSpaceRequirement`).
    fn partitioning_space_requirement(&self, ctx: &ExecutionContext, radix_bits: usize) -> Result<usize> {
        let layout = TupleDataLayout::new(self.children[0].types().to_vec(), ValidityType::CanHaveNull)?;
        let tuple_width = layout.row_width();
        if tuple_width == 0 {
            return Err(Error::internal(
                "GetPartitioningSpaceRequirement: tuple width should not be 0",
            ));
        }
        let buffer_manager = &ctx.env.buffer_manager;
        let tuples_per_block = (buffer_manager.block_size() / tuple_width).max(1);
        let mut blocks_per_chunk = (STANDARD_VECTOR_SIZE + tuples_per_block) / tuples_per_block + 1;
        if !layout.all_constant() {
            blocks_per_chunk += 2;
        }
        let size_per_partition = blocks_per_chunk * buffer_manager.block_alloc_size();
        Ok(ctx.env.threads().max(1)
            * RadixPartitioning::number_of_partitions(radix_bits)
            * size_per_partition)
    }

    /// Sizes the build side and negotiates the join's memory; returns whether the join
    /// must go external (DuckDB's `PrepareFinalize` and the start of `Finalize`).
    fn prepare_external(
        &self,
        ctx: &ExecutionContext,
        global: &mut HashJoinGlobalSinkState,
        locals: &[JoinHashTable],
    ) -> Result<bool> {
        let table = global
            .hash_table
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let (total_size, max_partition_size, max_partition_count) = table.total_size_of(locals);
        let probe_side_requirement = self.partitioning_space_requirement(ctx, table.radix_bits())?;
        let max_partition_ht_size = max_partition_size + table.pointer_table_size(max_partition_count);
        let memory = &global.temporary_memory;
        memory.set_minimum_reservation((max_partition_ht_size + probe_side_requirement) as u64);
        let probe_layout =
            TupleDataLayout::new(self.children[0].types().to_vec(), ValidityType::CanHaveNull)?;
        memory.set_materialization_penalty(probe_layout.row_width() as u64);
        memory.set_remaining_size(total_size as u64);
        memory.update_reservation(&global.memory_context);
        global.probe_side_requirement = probe_side_requirement;
        Ok(memory.reservation() < total_size as u64)
    }

    /// Finalizes an external join: repartitions the build side if its largest partition
    /// does not fit, then puts the first partitions that fit into the table and schedules
    /// building it; the probe spills the other partitions' rows (DuckDB's external
    /// `Finalize` and `HashJoinRepartitionEvent`).
    fn finalize_external(
        &self,
        ctx: &ExecutionContext,
        global: &mut HashJoinGlobalSinkState,
        locals: Vec<JoinHashTable>,
    ) -> Result<SinkFinalize> {
        global.external.store(true, Ordering::Release);
        global.perfect_join_executor = None;
        let table = global
            .hash_table
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let (total_size, max_partition_size, max_partition_count) = table.total_size_of(&locals);
        let max_partition_ht_size = max_partition_size + table.pointer_table_size(max_partition_count);
        // No point in repartitioning if one partition holds almost everything.
        let very_very_skewed = max_partition_ht_size as f64 >= 0.8 * total_size as f64;
        let reservation = usize::try_from(global.temporary_memory.reservation()).unwrap_or(usize::MAX);
        if !very_very_skewed && max_partition_ht_size + global.probe_side_requirement > reservation {
            table.set_repartition_radix_bits(reservation, max_partition_size, max_partition_count)?;
            for local in locals {
                local.repartition_into(table)?;
            }
            let sizes = table
                .sink_collection()
                .partitions()
                .iter()
                .map(TupleDataCollection::size_in_bytes);
            let counts = table
                .sink_collection()
                .partitions()
                .iter()
                .map(TupleDataCollection::count);
            let (sizes, counts): (Vec<usize>, Vec<usize>) = (sizes.collect(), counts.collect());
            let (total_size, max_partition_size, max_partition_count) = table.total_size(&sizes, &counts);
            global.probe_side_requirement = self.partitioning_space_requirement(ctx, table.radix_bits())?;
            let memory = &global.temporary_memory;
            memory.set_minimum_reservation(
                (max_partition_size
                    + table.pointer_table_size(max_partition_count)
                    + global.probe_side_requirement) as u64,
            );
            memory.set_remaining_size_and_update_reservation(
                &global.memory_context,
                (total_size + global.probe_side_requirement) as u64,
            );
        } else {
            for local in locals {
                table.merge(local)?;
            }
        }
        let budget = usize::try_from(global.temporary_memory.reservation())
            .unwrap_or(usize::MAX)
            .saturating_sub(global.probe_side_requirement);
        table.prepare_external_finalize(budget)?;
        let mut probe_types = self.children[0].types().to_vec();
        probe_types.push(LogicalType::UBIGINT);
        let spill = ProbeSpill::new(ctx.env.buffer_manager.clone(), probe_types, table.radix_bits())?;
        global
            .probe_spill
            .set(spill)
            .map_err(|_| Error::internal("hash join probe spill initialized twice"))?;
        // The first round's table is built by the finalize tasks (`ScheduleFinalize`).
        if table.count() > 0 {
            table.allocate_pointer_table()?;
            global
                .table_finalize
                .stage
                .store(TableFinalize::NONE, Ordering::Relaxed);
        } else {
            table.mark_finalized();
            global
                .table_finalize
                .stage
                .store(TableFinalize::DONE, Ordering::Relaxed);
        }
        Ok(SinkFinalize::Ready)
    }

    /// Pushes the build side's bloom filter into the probe side's scans, once every row is
    /// in the table (DuckDB pushes them in `HashJoinFinalizeEvent::FinishEvent`).
    fn push_bloom_filters(&self, table: &JoinHashTable, bloom_targets: Vec<(usize, usize)>) {
        if let (Some(bloom_filter), Some(condition)) = (table.bloom_filter(), self.conditions.first()) {
            let key_column_name = condition.right.name();
            for (filter, target) in bloom_targets {
                if let Some(target) = self
                    .join_filters
                    .get(filter)
                    .and_then(|filter| filter.targets.get(target))
                {
                    push_bloom_filter(target, self.filter_key(), bloom_filter, &key_column_name);
                }
            }
        }
    }
}

impl PhysicalHashJoin {
    /// Probes the input rows of the partitions built in this round and spills the others
    /// (`ProbeAndSpill`).
    fn probe_and_spill(
        global: &HashJoinGlobalSinkState,
        table: &JoinHashTable,
        input: &DataChunk,
        state: &mut HashJoinProbeState,
    ) -> Result<()> {
        if state.spill.is_none() {
            let probe_spill = global
                .probe_spill
                .get()
                .ok_or_else(|| Error::internal("external hash join without a probe spill"))?;
            state.spill = Some(probe_spill.register_thread()?);
        }
        let spill = state
            .spill
            .as_ref()
            .ok_or_else(|| Error::internal("external hash join probe without a spill"))?;
        let hashes = table.hash_probe_keys(&state.join_keys)?;
        let (probe_now, spill_rows): (Vec<u32>, Vec<u32>) = {
            let unified = hashes.unified::<u64>();
            (0..input.len() as u32).partition(|&row| {
                table.hash_in_current_partitions(unified.values[unified.slot(row as usize)])
            })
        };
        let slice = |chunk: &DataChunk, rows: &[u32]| {
            let sel = rustdb_types::SelectionVector::new(rows.to_vec());
            DataChunk::new(
                chunk.columns().iter().map(|column| column.slice(&sel)).collect(),
                rows.len(),
            )
        };
        if !spill_rows.is_empty() {
            let mut columns = input.columns().to_vec();
            columns.push(hashes.clone());
            let spilled = slice(&DataChunk::new(columns, input.len()), &spill_rows);
            spill
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .append(&spilled)?;
        }
        let probe_input = slice(input, &probe_now);
        state.join_keys = slice(&state.join_keys, &probe_now);
        let probe_hashes = hashes.slice(&rustdb_types::SelectionVector::new(probe_now));
        table.probe(
            &mut state.scan_structure,
            &state.join_keys,
            &mut state.probe_state,
            Some(&probe_hashes),
        )?;
        state.probe_input = Some(probe_input);
        Ok(())
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
    /// External joins: this thread's spilled probe rows (`spill_state`).
    spill: Option<Arc<Mutex<LocalProbeSpill>>>,
    /// External joins: the input rows probed in this round, while the scan structure
    /// walks them.
    probe_input: Option<DataChunk>,
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
            spill: None,
            probe_input: None,
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
        let external = global.external.load(Ordering::Acquire);
        if state.scan_structure.is_null {
            state.probe_executor.execute(input, &mut state.join_keys)?;
            flatten_sequences(&mut state.join_keys);
            if external {
                Self::probe_and_spill(global, &table, input, state)?;
            } else {
                table.probe(
                    &mut state.scan_structure,
                    &state.join_keys,
                    &mut state.probe_state,
                    None,
                )?;
            }
        }
        let lhs_output = self.lhs_output(state.probe_input.as_ref().filter(|_| external).unwrap_or(input));
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
