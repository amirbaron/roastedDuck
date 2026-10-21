use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rustdb_common::{Error, Result};
use rustdb_row::hash::hash_columns;
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, SelectionVector, Value, Vector};

use crate::aggregate_hashtable::{GroupedAggregateHashTable, initial_capacity};
use crate::window::collection::copy_into;
use rustdb_common::tree_renderer::RenderParams;

use crate::operator::{
    ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkData, SinkState, set_estimated_cardinality,
};
use crate::operators::{CollectedResult, PhysicalResultCollector};
use crate::{ClientEnv, ExecutionContext, Executor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// The rows a CTE reference reads: the materialised CTE, or a recursive CTE's working
/// table for the current iteration.
#[derive(Debug, Default)]
pub struct CteData {
    chunks: Mutex<Arc<Vec<DataChunk>>>,
    /// The CTE operator materialising these rows (unset for recursive working tables),
    /// so scans can make their pipelines wait for it.
    sink: OnceLock<Weak<PhysicalCte>>,
}

impl SinkData for CteData {
    fn sink(&self) -> Option<PhysicalRef> {
        self.sink
            .get()
            .and_then(Weak::upgrade)
            .map(|sink| sink as PhysicalRef)
    }
}

impl CteData {
    fn set(&self, chunks: Vec<DataChunk>) {
        *self.chunks.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(chunks);
    }

    fn snapshot(&self) -> Arc<Vec<DataChunk>> {
        Arc::clone(&self.chunks.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// A materialised CTE (DuckDB's `PhysicalCTE`): sinks the CTE definition (its first
/// input) into [`CteData`], then the query using it (its second input) runs in the
/// pipeline above, after the materialisation.
#[derive(Debug)]
pub struct PhysicalCte {
    types: Vec<LogicalType>,
    data: Arc<CteData>,
    children: [PhysicalRef; 2],
    ctename: String,
    table_index: u64,
}

impl PhysicalCte {
    pub fn new(
        ctename: String,
        table_index: u64,
        data: &Arc<CteData>,
        definition: PhysicalRef,
        query: PhysicalRef,
    ) -> Arc<Self> {
        let cte = Arc::new(Self {
            types: query.types().to_vec(),
            data: Arc::clone(data),
            children: [definition, query],
            ctename,
            table_index,
        });
        let _ = data.sink.set(Arc::downgrade(&cte));
        cte
    }
}

impl PhysicalOperator for PhysicalCte {
    fn name(&self) -> &'static str {
        "CTE"
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
    fn is_cte(&self) -> bool {
        true
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("CTE Name".to_owned(), self.ctename.clone());
        result.insert("Table Index".to_owned(), self.table_index.to_string());
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

/// The chunks one thread materialised. Small input chunks (e.g. behind a selective
/// join) are packed into chunks of up to `STANDARD_VECTOR_SIZE` rows, as DuckDB's
/// `ColumnDataCollection` does, so that the CTE's scans do not produce tiny chunks.
#[derive(Debug, Default)]
pub struct CteLocal {
    chunks: Vec<DataChunk>,
    /// The chunk being packed.
    packing: Option<DataChunk>,
}

impl CteLocal {
    /// Adds the rows of `chunk`, whose columns have `types`.
    fn push(&mut self, chunk: &DataChunk, types: &[LogicalType]) -> Result<()> {
        let count = chunk.len();
        if count == 0 {
            return Ok(());
        }
        let packed = self.packing.as_ref().map_or(0, DataChunk::len);
        if count > STANDARD_VECTOR_SIZE / 2 || packed + count > STANDARD_VECTOR_SIZE {
            self.flush();
        }
        if count > STANDARD_VECTOR_SIZE / 2 {
            self.chunks.push(chunk.compacted());
            return Ok(());
        }
        let packing = self.packing.get_or_insert_with(|| {
            DataChunk::new(
                types
                    .iter()
                    .map(|ty| Vector::flat(ty.clone(), STANDARD_VECTOR_SIZE))
                    .collect(),
                0,
            )
        });
        let offset = packing.len();
        // Compacting first copies the rows' strings into a heap of their own, which the
        // packed chunk then references (rather than the input's, e.g. a table block's).
        let compacted = chunk.compacted();
        for (index, column) in compacted.columns().iter().enumerate() {
            copy_into(column, count, packing.column_mut(index), offset)?;
        }
        packing.set_len(offset + count);
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(packing) = self.packing.take() {
            self.chunks.push(packing);
        }
    }
}

impl Sink for PhysicalCte {
    type Global = Mutex<Vec<DataChunk>>;
    type Local = CteLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<CteLocal> {
        Ok(CteLocal::default())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut CteLocal,
    ) -> Result<SinkResult> {
        local.push(chunk, self.children[0].types())?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: CteLocal) -> Result<()> {
        local.flush();
        global
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append(&mut local.chunks);
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        self.data.set(std::mem::take(
            global.get_mut().unwrap_or_else(PoisonError::into_inner),
        ));
        Ok(SinkFinalize::Ready)
    }
}

/// Scans a CTE's rows (DuckDB's `CTE_SCAN` / `RECURSIVE_CTE_SCAN`).
#[derive(Debug)]
pub struct PhysicalCteScan {
    types: Vec<LogicalType>,
    data: Arc<CteData>,
    /// Whether the scan is inside a recursive CTE that the CTE it reads is outside of
    /// (the recursive CTE then waits for the materialisation instead).
    external: bool,
    cte_index: u64,
    /// Whether it reads a recursive CTE's working table (`RECURSIVE_CTE_SCAN`).
    recursive: bool,
}

impl PhysicalCteScan {
    pub fn new(types: Vec<LogicalType>, data: Arc<CteData>, external: bool, cte_index: u64) -> Self {
        Self {
            types,
            data,
            external,
            cte_index,
            recursive: false,
        }
    }

    /// A scan of a recursive CTE's working table.
    pub fn working_table(types: Vec<LogicalType>, data: Arc<CteData>, cte_index: u64) -> Self {
        Self {
            recursive: true,
            ..Self::new(types, data, false, cte_index)
        }
    }
}

impl PhysicalOperator for PhysicalCteScan {
    fn name(&self) -> &'static str {
        if self.recursive {
            "REC_CTE_SCAN"
        } else {
            "CTE_SCAN"
        }
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("CTE Index".to_owned(), self.cte_index.to_string());
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
    fn source_dependencies(&self) -> Vec<PhysicalRef> {
        if self.external {
            return Vec::new();
        }
        self.data.sink().into_iter().collect()
    }
}

impl Source for PhysicalCteScan {
    /// The rows at the time the scan started and the next chunk to hand out.
    type Global = (Arc<Vec<DataChunk>>, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Self::Global> {
        Ok((self.data.snapshot(), AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (chunks, next): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if let Some(source) = chunks.get(index) {
            chunk.set_columns(source.columns().to_vec(), source.len());
        }
        Ok(if index + 1 < chunks.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }

    fn max_threads(&self, (chunks, _): &Self::Global) -> usize {
        chunks.len().max(1)
    }
}

/// `WITH RECURSIVE` (DuckDB's `PhysicalRecursiveCTE`): runs the anchor once, then the
/// recursive part repeatedly over the rows the previous iteration produced (the working
/// table), until an iteration produces no new rows. Without `ALL`, rows already produced
/// are discarded.
///
/// With `USING KEY`, every produced row replaces the row of its key in a keyed table (the
/// other columns keep their last value, DuckDB's `LAST` aggregates), which is the result
/// once the recursion ends. `recurring.<name>` reads the recurring table: the rows of
/// all iterations so far, or the keyed table with `USING KEY`.
///
/// Both parts run as nested executions; data they read from outside the CTE (delim
/// joins, materialised CTEs) is produced by the outer plan first.
#[derive(Debug)]
pub struct PhysicalRecursiveCte {
    types: Vec<LogicalType>,
    union_all: bool,
    working_table: Arc<CteData>,
    /// The anchor and the recursive part.
    parts: [PhysicalRef; 2],
    external: Vec<Arc<dyn SinkData>>,
    ctename: String,
    table_index: u64,
    /// The key columns of `USING KEY` (`distinct_idx`), if any.
    key_columns: Option<Vec<usize>>,
    /// The table `recurring.<name>` reads, when the recursive part reads it.
    recurring_table: Option<Arc<CteData>>,
}

impl PhysicalRecursiveCte {
    #[allow(clippy::too_many_arguments, reason = "mirrors the fields of DuckDB's operator")]
    pub fn new(
        (ctename, table_index): (String, u64),
        types: Vec<LogicalType>,
        union_all: bool,
        working_table: Arc<CteData>,
        parts: [PhysicalRef; 2],
        external: Vec<Arc<dyn SinkData>>,
        key_columns: Option<Vec<usize>>,
        recurring_table: Option<Arc<CteData>>,
    ) -> Self {
        Self {
            types,
            union_all,
            working_table,
            parts,
            external,
            ctename,
            table_index,
            key_columns,
            recurring_table,
        }
    }

    /// Runs `plan` to completion in a nested executor and returns its chunks.
    fn run(plan: &PhysicalRef, env: &Arc<ClientEnv>) -> Result<Vec<DataChunk>> {
        let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(Arc::clone(plan)));
        let state = Executor::execute(&root, Arc::clone(env))?;
        let collected = state
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("recursive CTE result has the wrong type"))?;
        Ok(std::mem::take(
            &mut collected.lock().unwrap_or_else(PoisonError::into_inner).chunks,
        ))
    }

    /// Keeps only rows not produced before (UNION without ALL).
    fn new_rows(seen: &mut GroupedAggregateHashTable, chunks: Vec<DataChunk>) -> Result<Vec<DataChunk>> {
        let mut result = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            if chunk.is_empty() {
                continue;
            }
            let hashes = if chunk.column_count() == 0 {
                Vector::constant(&Value::ubigint(0))?
            } else {
                let columns: Vec<&Vector> = chunk.columns().iter().collect();
                hash_columns(&columns, chunk.len())?
            };
            if seen.find_or_create_groups(&chunk, &hashes)? == 0 {
                continue;
            }
            let kept = seen.new_group_rows().to_vec();
            let len = kept.len();
            let sel = SelectionVector::new(kept);
            result.push(DataChunk::new(
                chunk.columns().iter().map(|c| c.slice(&sel)).collect(),
                len,
            ));
        }
        Ok(result)
    }
}

impl PhysicalOperator for PhysicalRecursiveCte {
    fn name(&self) -> &'static str {
        "REC_CTE"
    }
    fn explain_children(&self) -> &[PhysicalRef] {
        &self.parts
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("CTE Name".to_owned(), self.ctename.clone());
        result.insert("Table Index".to_owned(), self.table_index.to_string());
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
    fn source_dependencies(&self) -> Vec<PhysicalRef> {
        self.external.iter().filter_map(|data| data.sink()).collect()
    }
}

/// The rows of the current iteration and those produced so far (without `ALL`).
#[derive(Debug, Default)]
pub struct RecursiveCteState {
    iteration: Mutex<Option<Iteration>>,
}

/// An iteration's result being returned (DuckDB's `RecursiveCTEState`).
#[derive(Debug)]
struct Iteration {
    /// Every row produced so far, to discard duplicates (UNION without ALL).
    seen: GroupedAggregateHashTable,
    /// The non-empty chunks this iteration produced; the next iteration's working table.
    chunks: Vec<DataChunk>,
    /// The next chunk of `chunks` to return.
    next: usize,
    /// The rows of the recurring table without `USING KEY`.
    recurring: Vec<DataChunk>,
    /// The keyed table of `USING KEY`.
    keyed: KeyedRows,
    /// Whether the recursion ended; `chunks` then holds the keyed table's rows.
    finished: bool,
}

/// The rows of a `USING KEY` recursion, one per key in the order the keys first
/// appeared (DuckDB's hash table grouped on the keys with `LAST` of the other columns).
#[derive(Debug, Default)]
struct KeyedRows {
    index: HashMap<Vec<Value>, usize>,
    rows: Vec<Vec<Value>>,
}

impl KeyedRows {
    /// Every row of `chunk` replaces the row with its key.
    fn upsert(&mut self, chunk: &DataChunk, keys: &[usize]) {
        for row in 0..chunk.len() {
            let values: Vec<Value> = chunk.columns().iter().map(|column| column.get_value(row)).collect();
            let key: Vec<Value> = keys.iter().map(|&column| values[column].clone()).collect();
            if let Some(&slot) = self.index.get(&key) {
                self.rows[slot] = values;
            } else {
                self.index.insert(key, self.rows.len());
                self.rows.push(values);
            }
        }
    }

    /// The rows as chunks of `types`.
    fn chunks(&self, types: &[LogicalType]) -> Result<Vec<DataChunk>> {
        let mut result = Vec::with_capacity(self.rows.len().div_ceil(STANDARD_VECTOR_SIZE));
        for rows in self.rows.chunks(STANDARD_VECTOR_SIZE) {
            let mut columns: Vec<Vector> = types.iter().map(|ty| Vector::flat(ty.clone(), rows.len())).collect();
            for (row, values) in rows.iter().enumerate() {
                for (column, value) in columns.iter_mut().zip(values) {
                    column.set_value(row, value)?;
                }
            }
            result.push(DataChunk::new(columns, rows.len()));
        }
        Ok(result)
    }
}

impl PhysicalRecursiveCte {
    /// The non-empty chunks of an iteration's output, without rows produced before
    /// unless `ALL`.
    fn iteration_rows(
        &self,
        seen: &mut GroupedAggregateHashTable,
        keyed: &mut KeyedRows,
        produced: Vec<DataChunk>,
    ) -> Result<Vec<DataChunk>> {
        if let Some(keys) = &self.key_columns {
            let produced: Vec<DataChunk> = produced.into_iter().filter(|chunk| !chunk.is_empty()).collect();
            for chunk in &produced {
                keyed.upsert(chunk, keys);
            }
            return Ok(produced);
        }
        if self.union_all {
            Ok(produced.into_iter().filter(|chunk| !chunk.is_empty()).collect())
        } else {
            Self::new_rows(seen, produced)
        }
    }
}

impl Source for PhysicalRecursiveCte {
    type Global = RecursiveCteState;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<RecursiveCteState> {
        Ok(RecursiveCteState::default())
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &RecursiveCteState) -> Result<()> {
        Ok(())
    }

    /// Returns each iteration's rows before running the next one, so a `LIMIT` above
    /// stops an unbounded recursion (`PhysicalRecursiveCTE::GetData`).
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        state: &RecursiveCteState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let mut guard = state.iteration.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            let mut seen = GroupedAggregateHashTable::new(
                ctx.env.buffer_manager.clone(),
                self.types.clone(),
                &[],
                initial_capacity(),
                0,
            )?;
            let mut keyed = KeyedRows::default();
            let anchor = Self::run(&self.parts[0], &ctx.env)?;
            let chunks = self.iteration_rows(&mut seen, &mut keyed, anchor)?;
            *guard = Some(Iteration {
                seen,
                chunks,
                next: 0,
                recurring: Vec::new(),
                keyed,
                finished: false,
            });
        }
        let Some(iteration) = guard.as_mut() else {
            return Err(Error::internal("recursive CTE without an iteration"));
        };
        // With `USING KEY` the rows are only returned once the recursion ends.
        let returns_iterations = self.key_columns.is_none();
        loop {
            if (returns_iterations || iteration.finished)
                && let Some(source) = iteration.chunks.get(iteration.next)
            {
                iteration.next += 1;
                chunk.set_columns(source.columns().to_vec(), source.len());
                return Ok(SourceResult::HaveMoreOutput);
            }
            if iteration.finished {
                return Ok(SourceResult::Finished);
            }
            if iteration.chunks.is_empty() {
                iteration.finished = true;
                if !returns_iterations {
                    iteration.chunks = iteration.keyed.chunks(&self.types)?;
                    iteration.next = 0;
                }
                continue;
            }
            if let Some(recurring) = &self.recurring_table {
                if returns_iterations {
                    iteration.recurring.extend(iteration.chunks.iter().cloned());
                    recurring.set(iteration.recurring.clone());
                } else {
                    recurring.set(iteration.keyed.chunks(&self.types)?);
                }
            }
            self.working_table.set(std::mem::take(&mut iteration.chunks));
            let produced = Self::run(&self.parts[1], &ctx.env)?;
            iteration.chunks = self.iteration_rows(&mut iteration.seen, &mut iteration.keyed, produced)?;
            iteration.next = 0;
        }
    }
}
