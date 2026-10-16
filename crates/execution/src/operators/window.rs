//! Window functions over partitions and orderings (DuckDB's `PhysicalWindow`).
//!
//! The sink hands its input (plus computed PARTITION BY / ORDER BY keys) to a
//! [`SortStrategy`], which radix-partitions it into hash groups and sorts each group. The
//! source then processes one hash group at a time: sort it, compute its partition and peer
//! masks, materialise the shared argument expressions, let every executor sink and
//! finalize, and finally evaluate the functions chunk by chunk.
//!
//! Hash groups are processed largest first (ties: highest bin first), as DuckDB schedules
//! them; with one thread this is DuckDB's output order. DuckDB's per-group task stages
//! (sort, materialize, mask, sink, finalize, get data) run back to back here; each group's
//! state is self-contained so groups can be spread over threads later.

use rustdb_common::tree_renderer::RenderParams;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_row::sort::{SortColumn, SortConfig, SortStrategy, StrategyGlobalState, StrategyLocalState};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::window::WindowExpression;
use crate::window::collection::{WindowCollection, delta_flags};
use crate::window::executor::{ExecutorGlobalState, ExecutorLocalState, WindowExecutor, WindowGroup};
use crate::window::mask::WindowMask;
use crate::window::order_modifiers;
use crate::window::shared_expressions::WindowSharedExpressions;
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// The window operator: computes window functions sharing one partitioning and ordering.
#[derive(Debug)]
pub struct PhysicalWindow {
    types: Vec<LogicalType>,
    /// The window functions, in output order.
    select_list: Vec<WindowExpression>,
    /// The function with the longest ORDER BY: it defines the sort.
    order_idx: usize,
    /// Whether some function has neither PARTITION BY nor ORDER BY (input order matters).
    is_order_dependent: bool,
    estimated_cardinality: usize,
    children: [PhysicalRef; 1],
    /// Columns of the input.
    input_width: usize,
    /// Key expressions that are not plain input columns; computed into extra columns.
    sort_exprs: Vec<Expression>,
    /// The sorted data: input columns, then the computed keys.
    payload_types: Vec<LogicalType>,
    /// Payload columns of the PARTITION BY keys followed by the ORDER BY keys.
    sort_ids: Vec<usize>,
    /// The executors and their shared expressions.
    executors: Vec<WindowExecutor>,
    shared: WindowSharedExpressions,
}

impl PhysicalWindow {
    /// A window operator over `child` computing `select_list` (all with the same
    /// partitioning and prefix-compatible orderings).
    pub fn new(
        types: Vec<LogicalType>,
        select_list: Vec<Expression>,
        estimated_cardinality: usize,
        child: PhysicalRef,
    ) -> Result<Self> {
        let select_list = select_list
            .into_iter()
            .map(WindowExpression::from_expression)
            .collect::<Result<Vec<_>>>()?;
        let mut order_idx = 0;
        let mut max_orders = 0;
        let mut is_order_dependent = false;
        for (index, wexpr) in select_list.iter().enumerate() {
            if wexpr.window.partitions.is_empty() && wexpr.window.orders.is_empty() {
                is_order_dependent = true;
            }
            if wexpr.window.orders.len() > max_orders {
                order_idx = index;
                max_orders = wexpr.window.orders.len();
            }
        }
        let over = select_list
            .get(order_idx)
            .ok_or_else(|| Error::internal("window operator without window functions"))?;
        let input_types = child.types().to_vec();
        let input_width = input_types.len();
        // Key expressions that are not references are materialised as payload columns
        // (DuckDB's `HashedSort` / `FullSort` constructors).
        let mut payload_types = input_types;
        let mut sort_exprs = Vec::new();
        let mut sort_ids = Vec::new();
        let keys = over
            .window
            .partitions
            .iter()
            .chain(over.window.orders.iter().map(|order| &order.expression));
        for key in keys {
            if let ExpressionKind::Reference(reference) = &key.kind {
                sort_ids.push(reference.index as usize);
            } else {
                sort_ids.push(payload_types.len());
                payload_types.push(key.return_type.clone());
                sort_exprs.push(key.clone());
            }
        }
        let mut shared = WindowSharedExpressions::default();
        let executors = select_list
            .iter()
            .map(|wexpr| WindowExecutor::new(wexpr.clone(), &mut shared))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            types,
            select_list,
            order_idx,
            is_order_dependent,
            estimated_cardinality,
            children: [child],
            input_width,
            sort_exprs,
            payload_types,
            sort_ids,
            executors,
            shared,
        })
    }

    /// The window functions computed by this operator.
    pub fn select_list(&self) -> &[WindowExpression] {
        &self.select_list
    }

    /// Whether the result depends on the input order (DuckDB's `is_order_dependent`).
    pub fn is_order_dependent(&self) -> bool {
        self.is_order_dependent
    }

    fn over(&self) -> &WindowExpression {
        &self.select_list[self.order_idx]
    }

    /// The sort strategy for the function with the longest ordering
    /// (DuckDB's `SortStrategy::Factory`).
    fn create_strategy(&self, ctx: &ExecutionContext) -> Result<SortStrategy> {
        let over = &self.over().window;
        let partition_count = over.partitions.len();
        let partition_columns = self.sort_ids[..partition_count].to_vec();
        let orders = over
            .orders
            .iter()
            .zip(&self.sort_ids[partition_count..])
            .map(|(order, &column)| {
                Ok(SortColumn {
                    column,
                    modifiers: order_modifiers(order)?,
                    stats: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        SortStrategy::new(
            ctx.env.buffer_manager.clone(),
            partition_columns,
            &[],
            orders,
            self.payload_types.clone(),
            self.estimated_cardinality,
            false,
        )
    }
}

impl PhysicalOperator for PhysicalWindow {
    fn name(&self) -> &'static str {
        "WINDOW"
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

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        let projections: Vec<&str> = self.select_list.iter().map(|w| w.alias.as_str()).collect();
        result.insert("Projections".to_owned(), projections.join("\n"));
        result
    }
}

/// The sink's shared state (DuckDB's `WindowGlobalSinkState`).
#[derive(Debug)]
pub struct WindowGlobalSinkState {
    strategy: SortStrategy,
    sink: StrategyGlobalState,
}

/// A thread's sink state: its part of the strategy and the key expression executor.
#[derive(Debug)]
pub struct WindowLocalSinkState {
    strategy: StrategyLocalState,
    keys: Option<ExpressionExecutor>,
}

impl Sink for PhysicalWindow {
    type Global = WindowGlobalSinkState;
    type Local = WindowLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<WindowGlobalSinkState> {
        let strategy = self.create_strategy(ctx)?;
        let sink = strategy.global_state(SortConfig {
            threads: ctx.env.threads(),
            connections: 1,
            ..SortConfig::default()
        })?;
        Ok(WindowGlobalSinkState { strategy, sink })
    }

    fn local_sink_state(
        &self,
        _: &ExecutionContext,
        global: &WindowGlobalSinkState,
    ) -> Result<WindowLocalSinkState> {
        Ok(WindowLocalSinkState {
            strategy: global.strategy.local_state(),
            keys: (!self.sort_exprs.is_empty()).then(|| ExpressionExecutor::new(self.sort_exprs.clone())),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &WindowGlobalSinkState,
        local: &mut WindowLocalSinkState,
    ) -> Result<SinkResult> {
        match &mut local.keys {
            None => global.strategy.sink(chunk, &global.sink, &mut local.strategy)?,
            Some(executor) => {
                let mut keys = DataChunk::default();
                executor.execute(chunk, &mut keys)?;
                let mut columns = chunk.columns().to_vec();
                columns.extend(keys.into_columns());
                let payload = DataChunk::new(columns, chunk.len());
                global
                    .strategy
                    .sink(&payload, &global.sink, &mut local.strategy)?;
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &WindowGlobalSinkState,
        local: WindowLocalSinkState,
    ) -> Result<()> {
        global.strategy.combine(&global.sink, local.strategy)
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut WindowGlobalSinkState) -> Result<SinkFinalize> {
        Ok(if global.strategy.finalize(&mut global.sink)? {
            SinkFinalize::Ready
        } else {
            SinkFinalize::NoOutputPossible
        })
    }
}

/// The source's shared state (DuckDB's `WindowGlobalSourceState`): the hash groups in
/// processing order.
#[derive(Debug)]
pub struct WindowGlobalSourceState {
    sink: SinkState,
    /// Non-empty hash groups, largest first (DuckDB's `partition_blocks`).
    groups: Vec<usize>,
    next_group: AtomicUsize,
}

/// One hash group being evaluated (DuckDB's `WindowHashGroup`).
#[derive(Debug)]
struct WindowHashGroup {
    /// The sorted rows, chunk by chunk.
    chunks: Vec<DataChunk>,
    count: usize,
    partition_mask: WindowMask,
    /// Peer masks by key width (partition plus ORDER BY columns).
    order_masks: BTreeMap<usize, WindowMask>,
    collection: WindowCollection,
    global_states: Vec<ExecutorGlobalState>,
    local_states: Vec<ExecutorLocalState>,
    /// The next chunk to output and its first row.
    next_chunk: usize,
    next_row: usize,
}

/// A thread's source state: the hash group it is evaluating.
#[derive(Debug, Default)]
pub struct WindowLocalSourceState {
    group: Option<Box<WindowHashGroup>>,
    eval: Option<ExpressionExecutor>,
}

impl WindowGlobalSourceState {
    /// The finalized sink state.
    fn sink_state(&self) -> Result<&WindowGlobalSinkState> {
        self.sink
            .downcast_ref::<WindowGlobalSinkState>()
            .ok_or_else(|| Error::internal("WINDOW sink state has the wrong type"))
    }
}

impl PhysicalWindow {
    /// Partition and peer masks (DuckDB's `WindowHashGroup::ComputeMasks`).
    fn compute_masks(
        &self,
        chunks: &[DataChunk],
        count: usize,
    ) -> Result<(WindowMask, BTreeMap<usize, WindowMask>)> {
        let over = &self.over().window;
        let partition_count = over.partitions.len();
        let mut widths: Vec<usize> = self.executors.iter().map(WindowExecutor::key_count).collect();
        widths.sort_unstable();
        widths.dedup();
        let lens: Vec<usize> = chunks.iter().map(DataChunk::len).collect();
        // Without PARTITION BY / ORDER BY only the first row starts a group.
        let mut flags = vec![false; count];
        if let Some(first) = flags.first_mut() {
            *first = true;
        }
        let sorted = !(over.partitions.is_empty() && over.orders.is_empty());
        // The masks of wider keys add the boundaries of further key columns.
        let mut masks: BTreeMap<usize, WindowMask> = BTreeMap::new();
        let mut width = 0;
        for target in std::iter::once(partition_count).chain(widths.iter().copied()) {
            let target = target.max(partition_count);
            if sorted && target > width {
                let keys: Vec<Vec<&Vector>> = chunks
                    .iter()
                    .map(|chunk| {
                        self.sort_ids[width..target]
                            .iter()
                            .map(|&column| chunk.column(column))
                            .collect()
                    })
                    .collect();
                delta_flags(&keys, &lens, &mut flags)?;
                width = target;
            }
            masks
                .entry(target)
                .or_insert_with(|| WindowMask::from_flags(&flags));
        }
        let partition_mask = masks
            .get(&partition_count)
            .cloned()
            .ok_or_else(|| Error::internal("window partition mask missing"))?;
        let order_masks = widths
            .into_iter()
            .map(|width| {
                masks
                    .get(&width.max(partition_count))
                    .cloned()
                    .map(|mask| (width, mask))
                    .ok_or_else(|| Error::internal("window peer mask missing"))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok((partition_mask, order_masks))
    }

    /// Sorts, materialises and finalizes a hash group (DuckDB's SORT, MATERIALIZE, MASK,
    /// SINK and FINALIZE stages).
    fn prepare_group(
        &self,
        ctx: &ExecutionContext,
        sink: &WindowGlobalSinkState,
        bin: usize,
    ) -> Result<WindowHashGroup> {
        sink.strategy.sort_hash_group(&sink.sink, bin)?;
        let chunks = sink.strategy.group_chunks(&sink.sink, bin)?;
        let count = chunks.iter().map(DataChunk::len).sum();
        let (partition_mask, order_masks) = self.compute_masks(&chunks, count)?;

        let mut collection = WindowCollection::new(&self.shared.coll.types(), count);
        let mut global_states = self
            .executors
            .iter()
            .map(|executor| executor.global_state(ctx, count, &partition_mask))
            .collect::<Result<Vec<_>>>()?;
        let mut local_states = self
            .executors
            .iter()
            .zip(&global_states)
            .map(|(executor, global)| executor.local_state(ctx, global, count))
            .collect::<Result<Vec<_>>>()?;

        // Sink: materialise the collection and feed the executors.
        let mut coll_exec = (!self.shared.coll.is_empty()).then(|| self.shared.coll.executor());
        let mut sink_exec = (!self.shared.sink.is_empty()).then(|| self.shared.sink.executor());
        let mut input_idx = 0;
        for chunk in &chunks {
            let input = self.input_chunk(chunk);
            let mut coll_chunk = DataChunk::new(Vec::new(), input.len());
            if let Some(executor) = &mut coll_exec {
                executor.execute(&input, &mut coll_chunk)?;
                collection.append(&coll_chunk, input_idx)?;
            }
            let mut sink_chunk = DataChunk::new(Vec::new(), input.len());
            if let Some(executor) = &mut sink_exec {
                executor.execute(&input, &mut sink_chunk)?;
            }
            for ((executor, global), local) in self
                .executors
                .iter()
                .zip(&mut global_states)
                .zip(&mut local_states)
            {
                executor.sink(global, local, &sink_chunk, &coll_chunk, input_idx)?;
            }
            input_idx += input.len();
        }

        // Finalize: complete the collection, then the executors.
        collection.combine(&self.shared.coll_validity);
        for ((executor, global), local) in self
            .executors
            .iter()
            .zip(&mut global_states)
            .zip(&mut local_states)
        {
            let group = WindowGroup {
                count,
                partition_mask: &partition_mask,
                order_mask: order_masks
                    .get(&executor.key_count())
                    .ok_or_else(|| Error::internal("window peer mask missing"))?,
                collection: &collection,
            };
            executor.finalize(global, local, group)?;
        }
        Ok(WindowHashGroup {
            chunks,
            count,
            partition_mask,
            order_masks,
            collection,
            global_states,
            local_states,
            next_chunk: 0,
            next_row: 0,
        })
    }

    /// The input columns of a sorted chunk (dropping computed keys).
    fn input_chunk(&self, chunk: &DataChunk) -> DataChunk {
        DataChunk::new(chunk.columns()[..self.input_width].to_vec(), chunk.len())
    }

    /// Evaluates the functions for the group's next chunk (DuckDB's GETDATA stage).
    fn evaluate_chunk(
        &self,
        group: &mut WindowHashGroup,
        eval: &mut Option<ExpressionExecutor>,
        output: &mut DataChunk,
    ) -> Result<()> {
        let chunk = &group.chunks[group.next_chunk];
        let input = self.input_chunk(chunk);
        let mut eval_chunk = DataChunk::new(Vec::new(), input.len());
        if let Some(executor) = eval {
            executor.execute(&input, &mut eval_chunk)?;
        }
        let mut columns: Vec<Vector> = input.columns().to_vec();
        for ((executor, global), local) in self
            .executors
            .iter()
            .zip(&group.global_states)
            .zip(&mut group.local_states)
        {
            let window_group = WindowGroup {
                count: group.count,
                partition_mask: &group.partition_mask,
                order_mask: group
                    .order_masks
                    .get(&executor.key_count())
                    .ok_or_else(|| Error::internal("window peer mask missing"))?,
                collection: &group.collection,
            };
            columns.push(executor.evaluate(global, local, window_group, group.next_row, &eval_chunk)?);
        }
        output.set_columns(columns, input.len());
        group.next_row += input.len();
        group.next_chunk += 1;
        Ok(())
    }
}

impl Source for PhysicalWindow {
    type Global = WindowGlobalSourceState;
    type Local = Mutex<WindowLocalSourceState>;

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<WindowGlobalSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("WINDOW source requires its sink state"))?;
        let state = sink
            .downcast_ref::<WindowGlobalSinkState>()
            .ok_or_else(|| Error::internal("WINDOW sink state has the wrong type"))?;
        // Largest groups first; equal sizes by descending bin (DuckDB sorts
        // (blocks, bin) pairs with std::greater).
        let mut blocks: Vec<(usize, usize)> = state
            .strategy
            .hash_groups(&state.sink)
            .iter()
            .enumerate()
            .filter(|(_, group)| group.chunks > 0)
            .map(|(bin, group)| (group.chunks, bin))
            .collect();
        blocks.sort_unstable_by(|a, b| b.cmp(a));
        Ok(WindowGlobalSourceState {
            sink: Arc::clone(sink),
            groups: blocks.into_iter().map(|(_, bin)| bin).collect(),
            next_group: AtomicUsize::new(0),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &WindowGlobalSourceState) -> Result<Self::Local> {
        Ok(Mutex::new(WindowLocalSourceState {
            group: None,
            eval: (!self.shared.eval.is_empty()).then(|| self.shared.eval.executor()),
        }))
    }

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &WindowGlobalSourceState,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let local = local.get_mut().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(group) = &mut local.group
                && group.next_chunk < group.chunks.len()
            {
                self.evaluate_chunk(group, &mut local.eval, chunk)?;
                return Ok(SourceResult::HaveMoreOutput);
            }
            // Release the finished group before preparing the next one.
            local.group = None;
            let next = global.next_group.fetch_add(1, Ordering::Relaxed);
            let Some(&bin) = global.groups.get(next) else {
                return Ok(SourceResult::Finished);
            };
            let sink = global.sink_state()?;
            local.group = Some(Box::new(self.prepare_group(ctx, sink, bin)?));
        }
    }
}
