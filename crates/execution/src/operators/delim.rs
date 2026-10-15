use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_row::hash::hash_columns;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::aggregate_hashtable::{GroupedAggregateHashTable, initial_capacity};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Data produced by a delim join's sink and read by its scans: the buffered left input
/// and the distinct values of its duplicate-eliminated columns.
#[derive(Debug, Default)]
pub struct DelimShared {
    cached: OnceLock<Vec<DataChunk>>,
    distinct: OnceLock<Vec<DataChunk>>,
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
}

/// Buffered chunks and the distinct-value table.
#[derive(Debug)]
pub struct DelimGlobal {
    chunks: Mutex<Vec<DataChunk>>,
    distinct: Mutex<GroupedAggregateHashTable>,
}

impl Sink for PhysicalDelimJoin {
    type Global = DelimGlobal;
    type Local = ExpressionExecutor;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<DelimGlobal> {
        let group_types = self
            .groups
            .iter()
            .map(|group| group.return_type.clone())
            .collect();
        let distinct = GroupedAggregateHashTable::new(
            ctx.env.buffer_manager.clone(),
            group_types,
            &[],
            initial_capacity(),
            0,
        )?;
        Ok(DelimGlobal {
            chunks: Mutex::default(),
            distinct: Mutex::new(distinct),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &DelimGlobal) -> Result<ExpressionExecutor> {
        Ok(ExpressionExecutor::new(self.groups.clone()))
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &DelimGlobal,
        groups: &mut ExpressionExecutor,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        groups.execute(chunk, &mut keys)?;
        let hashes = if keys.column_count() == 0 {
            Vector::constant(&Value::ubigint(0))?
        } else {
            let columns: Vec<&Vector> = keys.columns().iter().collect();
            hash_columns(&columns, chunk.len())?
        };
        global
            .distinct
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add_chunk(&keys, &hashes, &[], &[])?;
        global
            .chunks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(chunk.clone());
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &DelimGlobal, _: ExpressionExecutor) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut DelimGlobal) -> Result<SinkFinalize> {
        let chunks = std::mem::take(global.chunks.get_mut().unwrap_or_else(PoisonError::into_inner));
        let mut distinct = Vec::new();
        global
            .distinct
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .finalize_into(|chunk| {
                distinct.push(chunk);
                Ok(())
            })?;
        let already = || Error::internal("delim join data finalized twice");
        self.shared.cached.set(chunks).map_err(|_| already())?;
        self.shared.distinct.set(distinct).map_err(|_| already())?;
        Ok(SinkFinalize::Ready)
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
}

impl PhysicalDelimScan {
    /// A scan of the distinct duplicate-eliminated values (`DELIM_SCAN`).
    pub fn new(types: Vec<LogicalType>, shared: Arc<DelimShared>, data: DelimData) -> Self {
        Self {
            types,
            shared,
            data,
            owner: None,
        }
    }

    /// The scan of the delim join's buffered left input, owning the delim sink.
    pub fn buffered_input(
        types: Vec<LogicalType>,
        shared: Arc<DelimShared>,
        owner: Arc<PhysicalDelimJoin>,
    ) -> Self {
        Self {
            types,
            shared,
            data: DelimData::Cached,
            owner: Some(owner),
        }
    }
}

impl PhysicalOperator for PhysicalDelimScan {
    fn name(&self) -> &'static str {
        match self.data {
            DelimData::Cached => "COLUMN_DATA_SCAN",
            DelimData::Distinct => "DELIM_SCAN",
        }
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
    fn source_dependency(&self) -> Option<PhysicalRef> {
        if let Some(owner) = &self.owner {
            return Some(Arc::clone(owner) as PhysicalRef);
        }
        self.shared
            .sink
            .get()
            .and_then(Weak::upgrade)
            .map(|sink| sink as PhysicalRef)
    }
}

impl Source for PhysicalDelimScan {
    type Global = AtomicUsize;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<AtomicUsize> {
        Ok(AtomicUsize::new(0))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AtomicUsize) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        next: &AtomicUsize,
        (): &mut (),
    ) -> Result<SourceResult> {
        let lock = match self.data {
            DelimData::Cached => &self.shared.cached,
            DelimData::Distinct => &self.shared.distinct,
        };
        let chunks = lock
            .get()
            .ok_or_else(|| Error::internal("delim scan ran before its delim join"))?;
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
}
