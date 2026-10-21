use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rustdb_common::Result;
use rustdb_planner::Expression;
use rustdb_row::{AppendState, PartitionedTupleData, PinProperties, TupleDataCollection};
use rustdb_row::hash::hash_columns;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::aggregate_hashtable::{GroupedAggregateHashTable, capacity_for_count, initial_capacity};
use crate::operators::hash_aggregate::{repartitioned, table_data};
use crate::operators::radix_sink::initial_radix_bits;
use crate::column_data_collection::ColumnDataCollection;
use rustdb_common::tree_renderer::RenderParams;

use crate::operator::{
    ErasedSink, ErasedSource, OrderPreservationType, PhysicalOperator, PhysicalRef, SinkData, SinkState,
    set_estimated_cardinality,
};
use crate::plan_generator::order_preservation::order_preservation_recursive;
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Data produced by a delim join's sink and read by its scans: the buffered left input
/// and the distinct values of its duplicate-eliminated columns.
///
/// A delim join inside a recursive CTE runs once per iteration, so the data is replaced
/// on every finalize.
#[derive(Debug, Default)]
pub struct DelimShared {
    cached: Mutex<Arc<BufferedRows>>,
    distinct: Mutex<Arc<BufferedRows>>,
    /// The sink filling this data, so scans can make their pipelines wait for it.
    sink: OnceLock<Weak<PhysicalDelimJoin>>,
}

/// The sink half of a duplicate-eliminated join (DuckDB's `PhysicalLeftDelimJoin`):
/// buffers its input and computes the distinct correlated values that the subquery side
/// receives through [`PhysicalDelimScan`]s. The join itself reads the buffered input back
/// through a cached [`PhysicalDelimScan`].
#[derive(Debug)]
pub struct PhysicalDelimJoin {
    types: Vec<LogicalType>,
    groups: Vec<Expression>,
    shared: Arc<DelimShared>,
    children: [PhysicalRef; 1],
}

impl PhysicalDelimJoin {
    pub fn new(groups: Vec<Expression>, shared: &Arc<DelimShared>, child: PhysicalRef) -> Arc<Self> {
        let sink = Arc::new(Self {
            types: Vec::new(),
            groups,
            shared: Arc::clone(shared),
            children: [child],
        });
        // The first delim join registered for this shared data owns it.
        let _ = shared.sink.set(Arc::downgrade(&sink));
        sink
    }
}

impl PhysicalOperator for PhysicalDelimJoin {
    fn name(&self) -> &'static str {
        "LEFT_DELIM_JOIN"
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
    fn parallel_sink(&self) -> bool {
        true
    }
}

/// Rows a delim join's sink produced: buffer-managed collections, read back chunk by chunk
/// in order (the input in the order of the threads' combines, DuckDB's `lhs_data`; the
/// distinct values partition by partition).
#[derive(Debug, Default)]
pub struct BufferedRows {
    collections: Vec<TupleDataCollection>,
    /// The columns of the rows to read.
    columns: Vec<usize>,
    /// The index of the first chunk of every collection, plus the total chunk count.
    chunk_starts: Vec<usize>,
}

impl BufferedRows {
    fn new(collections: Vec<TupleDataCollection>, columns: Vec<usize>) -> Self {
        let mut chunk_starts = Vec::with_capacity(collections.len() + 1);
        let mut total = 0;
        chunk_starts.push(0);
        for collection in &collections {
            total += collection.chunk_count();
            chunk_starts.push(total);
        }
        Self {
            collections,
            columns,
            chunk_starts,
        }
    }

    fn chunk_count(&self) -> usize {
        self.chunk_starts.last().copied().unwrap_or(0)
    }

    /// Reads chunk `index` (counting over all collections) into `result`.
    fn fetch_chunk(&self, index: usize, result: &mut DataChunk) -> Result<()> {
        let collection = self.chunk_starts.partition_point(|&start| start <= index) - 1;
        let rows = &self.collections[collection];
        let mut scan = rows.initialize_scan_columns(self.columns.clone(), PinProperties::UnpinAfterDone);
        rows.seek(&mut scan, index - self.chunk_starts[collection]);
        rows.scan(&mut scan, result)?;
        Ok(())
    }
}

/// Buffered chunks and the distinct values (DuckDB's `DelimJoinGlobalState` with the
/// distinct aggregate's radix partitioned table).
#[derive(Debug, Default)]
pub struct DelimGlobal {
    buffered: Mutex<Vec<ColumnDataCollection>>,
    /// The threads' radix partitioned distinct values.
    distinct_parts: Mutex<Vec<PartitionedTupleData>>,
    /// After finalize: each partition's rows from every thread, deduplicated by one
    /// finalize task each.
    partitions: Vec<Mutex<Vec<TupleDataCollection>>>,
    /// The distinct values each finalize task found.
    distinct: Vec<Mutex<Option<TupleDataCollection>>>,
    /// Whether the partitions were handed out as finalize tasks.
    scheduled: AtomicBool,
}

/// One thread's buffered input and distinct values.
#[derive(Debug)]
pub struct DelimLocal {
    groups: ExpressionExecutor,
    /// The input rows, copied: the chunks themselves may be small selections of large
    /// vectors that must not be kept alive (DuckDB's `lhs_data` collection).
    data: ColumnDataCollection,
    append: AppendState,
    distinct: GroupedAggregateHashTable,
}

impl PhysicalDelimJoin {
    /// An empty table of the distinct duplicate-eliminated values.
    fn distinct_table(
        &self,
        ctx: &ExecutionContext,
        capacity: usize,
        radix_bits: usize,
    ) -> Result<GroupedAggregateHashTable> {
        let group_types = self
            .groups
            .iter()
            .map(|group| group.return_type.clone())
            .collect();
        GroupedAggregateHashTable::new(ctx.env.buffer_manager.clone(), group_types, &[], capacity, radix_bits)
    }
}

/// Adds the groups `keys` to `table`.
fn add_groups(table: &mut GroupedAggregateHashTable, keys: &DataChunk) -> Result<()> {
    let hashes = if keys.column_count() == 0 {
        Vector::constant(&Value::ubigint(0))?
    } else {
        let columns: Vec<&Vector> = keys.columns().iter().collect();
        hash_columns(&columns, keys.len())?
    };
    table.add_chunk(keys, &hashes, &[], &[])?;
    Ok(())
}

impl Sink for PhysicalDelimJoin {
    type Global = DelimGlobal;
    type Local = DelimLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<DelimGlobal> {
        Ok(DelimGlobal::default())
    }

    fn local_sink_state(&self, ctx: &ExecutionContext, _: &DelimGlobal) -> Result<DelimLocal> {
        let mut data =
            ColumnDataCollection::new(ctx.env.buffer_manager.clone(), self.children[0].types().to_vec())?;
        let append = data.initialize_append();
        Ok(DelimLocal {
            groups: ExpressionExecutor::new(self.groups.clone()),
            data,
            append,
            distinct: self.distinct_table(ctx, initial_capacity(), initial_radix_bits(ctx))?,
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &DelimGlobal,
        local: &mut DelimLocal,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        local.groups.execute(chunk, &mut keys)?;
        let keys = DataChunk::new(keys.into_columns(), chunk.len());
        add_groups(&mut local.distinct, &keys)?;
        local.data.append(&mut local.append, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }

    /// Hands the thread's buffered input and distinct values to the global state.
    fn combine(&self, _: &ExecutionContext, global: &DelimGlobal, mut local: DelimLocal) -> Result<()> {
        local.data.finalize_append(&mut local.append);
        global
            .buffered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(local.data);
        // The partitions are merged by the finalize tasks, not here under a lock.
        let distinct = table_data(&mut local.distinct, None)?;
        global
            .distinct_parts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(distinct);
        Ok(())
    }

    /// Publishes the buffered input and splits the threads' distinct values into their
    /// radix partitions, which the finalize tasks deduplicate in parallel (DuckDB's
    /// distinct aggregate finalize).
    fn finalize(&self, _: &ExecutionContext, global: &mut DelimGlobal) -> Result<SinkFinalize> {
        let buffered: Vec<TupleDataCollection> =
            std::mem::take(global.buffered.get_mut().unwrap_or_else(PoisonError::into_inner))
                .into_iter()
                .map(ColumnDataCollection::into_rows)
                .collect();
        let columns = (0..self.children[0].types().len()).collect();
        *self.shared.cached.lock().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(BufferedRows::new(buffered, columns));
        let parts = std::mem::take(global.distinct_parts.get_mut().unwrap_or_else(PoisonError::into_inner));
        let radix_bits = parts.iter().map(PartitionedTupleData::radix_bits).max().unwrap_or(0);
        let mut partitions: Vec<Vec<TupleDataCollection>> = (0..1usize << radix_bits).map(|_| Vec::new()).collect();
        for part in parts {
            let mut part = repartitioned(part, radix_bits)?;
            for (partition, collection) in part.take_partitions().into_iter().enumerate() {
                if collection.count() > 0 {
                    partitions[partition].push(collection);
                }
            }
        }
        partitions.retain(|data| !data.is_empty());
        global.distinct = partitions.iter().map(|_| Mutex::default()).collect();
        global.partitions = partitions.into_iter().map(Mutex::new).collect();
        Ok(SinkFinalize::Ready)
    }

    /// One task per partition of distinct values; once they ran, the values are
    /// published to the delim scans.
    fn finalize_tasks(&self, _: &ExecutionContext, global: &DelimGlobal) -> Result<usize> {
        if !global.scheduled.swap(true, Ordering::AcqRel) && !global.partitions.is_empty() {
            return Ok(global.partitions.len());
        }
        let distinct = global
            .distinct
            .iter()
            .filter_map(|rows| rows.lock().unwrap_or_else(PoisonError::into_inner).take())
            .collect();
        let columns = (0..self.groups.len()).collect();
        *self
            .shared
            .distinct
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(BufferedRows::new(distinct, columns));
        Ok(0)
    }

    /// Deduplicates one partition of the threads' distinct values.
    fn execute_finalize_task(&self, ctx: &ExecutionContext, global: &DelimGlobal, task: usize) -> Result<()> {
        let (Some(partition), Some(result)) = (global.partitions.get(task), global.distinct.get(task)) else {
            return Err(rustdb_common::Error::internal("delim join finalize task out of range"));
        };
        let data = std::mem::take(&mut *partition.lock().unwrap_or_else(PoisonError::into_inner));
        let rows = data.iter().map(TupleDataCollection::count).sum();
        let mut table = self.distinct_table(ctx, capacity_for_count(rows), 0)?;
        for collection in &data {
            table.combine(collection)?;
        }
        drop(data);
        // The groups have no aggregate states: the table's rows are the distinct values.
        let rows = table_data(&mut table, None)?.take_partitions().into_iter().next();
        *result.lock().unwrap_or_else(PoisonError::into_inner) = rows;
        Ok(())
    }
}

/// Which delim join data a scan reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelimData {
    /// The buffered left input (the join's left side).
    Cached,
    /// The distinct duplicate-eliminated values (DuckDB's `DELIM_SCAN`).
    Distinct,
}

/// Scans the data of a delim join once its sink has finished.
#[derive(Debug)]
pub struct PhysicalDelimScan {
    types: Vec<LogicalType>,
    shared: Arc<DelimShared>,
    data: DelimData,
    /// The delim sink, owned by the scan of its buffered input (the plan's only strong
    /// reference to it; `DelimShared` points back weakly to avoid a cycle).
    owner: Option<Arc<PhysicalDelimJoin>>,
    /// Whether the scan is inside a recursive CTE that the delim join is outside of: the
    /// recursive CTE then waits for the delim join, and re-running the scan must not
    /// re-run the join.
    external: bool,
    /// The delim join's number in the plan, shown by `EXPLAIN` (DuckDB's `delim_index`).
    delim_index: OnceLock<u64>,
    /// For the buffered input of a flipped (right) delim join: `EXPLAIN` shows it as
    /// DuckDB's `PhysicalDummyScan`.
    shown_as_dummy_scan: bool,
}

impl PhysicalDelimScan {
    /// A scan of the distinct duplicate-eliminated values (`DELIM_SCAN`).
    pub fn new(types: Vec<LogicalType>, shared: Arc<DelimShared>, external: bool) -> Self {
        Self {
            types,
            shared,
            data: DelimData::Distinct,
            owner: None,
            external,
            delim_index: OnceLock::new(),
            shown_as_dummy_scan: false,
        }
    }

    /// The scan of the delim join's buffered input, owning the delim sink; `flipped` for
    /// a right delim join.
    pub fn buffered_input(
        types: Vec<LogicalType>,
        shared: Arc<DelimShared>,
        owner: Arc<PhysicalDelimJoin>,
        flipped: bool,
    ) -> Self {
        Self {
            types,
            shared,
            data: DelimData::Cached,
            owner: Some(owner),
            external: false,
            delim_index: OnceLock::new(),
            shown_as_dummy_scan: flipped,
        }
    }

    /// Records the number of the delim join this scan reads (DuckDB's
    /// `GatherDelimScans`).
    pub fn set_delim_index(&self, delim_index: u64) {
        let _ = self.delim_index.set(delim_index);
    }
}

impl PhysicalOperator for PhysicalDelimScan {
    fn name(&self) -> &'static str {
        match self.data {
            DelimData::Cached if self.shown_as_dummy_scan => "DUMMY_SCAN",
            DelimData::Cached => "COLUMN_DATA_SCAN",
            DelimData::Distinct => "DELIM_SCAN",
        }
    }
    /// DuckDB's `PhysicalColumnDataScan::ParamsToString` (`PhysicalDummyScan` has none).
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        if self.shown_as_dummy_scan {
            return result;
        }
        if let (DelimData::Distinct, Some(index)) = (&self.data, self.delim_index.get()) {
            result.insert("Delim Index".to_owned(), index.to_string());
        }
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &[]
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn parallel_source(&self) -> bool {
        true
    }
    /// The buffered input keeps the order of the delim join's input (DuckDB's plan
    /// reaches the delim join's input there).
    fn source_order(&self) -> OrderPreservationType {
        match (&self.data, &self.owner) {
            (DelimData::Cached, Some(owner)) => owner
                .children()
                .first()
                .map_or(OrderPreservationType::InsertionOrder, |input| {
                    order_preservation_recursive(input.as_ref())
                }),
            _ => OrderPreservationType::InsertionOrder,
        }
    }
    fn source_dependencies(&self) -> Vec<PhysicalRef> {
        if let Some(owner) = &self.owner {
            return vec![Arc::clone(owner) as PhysicalRef];
        }
        if self.external {
            return Vec::new();
        }
        self.shared.sink().into_iter().collect()
    }
}

impl SinkData for DelimShared {
    fn sink(&self) -> Option<PhysicalRef> {
        self.sink
            .get()
            .and_then(Weak::upgrade)
            .map(|sink| sink as PhysicalRef)
    }
}

impl Source for PhysicalDelimScan {
    /// The data when the scan started and the next chunk to hand out.
    type Global = (Arc<BufferedRows>, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Self::Global> {
        let lock = match self.data {
            DelimData::Cached => &self.shared.cached,
            DelimData::Distinct => &self.shared.distinct,
        };
        let rows = Arc::clone(&lock.lock().unwrap_or_else(PoisonError::into_inner));
        Ok((rows, AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (rows, next): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let index = next.fetch_add(1, Ordering::Relaxed);
        let count = rows.chunk_count();
        if index < count {
            rows.fetch_chunk(index, chunk)?;
        }
        Ok(if index + 1 < count {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }

    fn max_threads(&self, (rows, _): &Self::Global) -> usize {
        rows.chunk_count().max(1)
    }
}
