use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rustdb_common::{Error, Result};
use rustdb_row::hash::hash_columns;
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use crate::aggregate_hashtable::{GroupedAggregateHashTable, initial_capacity};
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

impl Sink for PhysicalCte {
    type Global = Mutex<Vec<DataChunk>>;
    /// The chunks one thread materialised.
    type Local = Vec<DataChunk>;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Vec<DataChunk>> {
        Ok(Vec::new())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut Vec<DataChunk>,
    ) -> Result<SinkResult> {
        local.push(chunk.clone());
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: Vec<DataChunk>) -> Result<()> {
        global
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append(&mut local);
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
}

impl PhysicalRecursiveCte {
    pub fn new(
        (ctename, table_index): (String, u64),
        types: Vec<LogicalType>,
        union_all: bool,
        working_table: Arc<CteData>,
        parts: [PhysicalRef; 2],
        external: Vec<Arc<dyn SinkData>>,
    ) -> Self {
        Self {
            types,
            union_all,
            working_table,
            parts,
            external,
            ctename,
            table_index,
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

/// The CTE's result, computed on the first `get_data`.
#[derive(Debug, Default)]
pub struct RecursiveCteState {
    computed: AtomicBool,
    chunks: Mutex<Vec<DataChunk>>,
    next: AtomicUsize,
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

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        state: &RecursiveCteState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let mut chunks = state.chunks.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.computed.swap(true, Ordering::AcqRel) {
            let mut seen = GroupedAggregateHashTable::new(
                ctx.env.buffer_manager.clone(),
                self.types.clone(),
                &[],
                initial_capacity(),
                0,
            )?;
            let mut working = Self::run(&self.parts[0], &ctx.env)?;
            if !self.union_all {
                working = Self::new_rows(&mut seen, working)?;
            }
            while working.iter().any(|chunk| !chunk.is_empty()) {
                chunks.extend(working.iter().cloned());
                self.working_table.set(working);
                let produced = Self::run(&self.parts[1], &ctx.env)?;
                working = if self.union_all {
                    produced
                } else {
                    Self::new_rows(&mut seen, produced)?
                };
            }
        }
        let index = state.next.fetch_add(1, Ordering::Relaxed);
        if let Some(source) = chunks.get(index) {
            chunk.set_columns(source.columns().to_vec(), source.len());
        }
        Ok(if index + 1 < chunks.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
