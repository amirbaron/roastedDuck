use std::sync::Mutex;

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSink, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, Sink, SinkResult};

/// The materialised output of a query.
#[derive(Debug, Default)]
pub struct CollectedResult {
    pub chunks: Vec<DataChunk>,
}

/// Collects the final query result (DuckDB's `PhysicalMaterializedCollector`).
#[derive(Debug)]
pub struct PhysicalResultCollector {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
}

impl PhysicalResultCollector {
    pub fn new(child: PhysicalRef) -> Self {
        Self {
            types: child.types().to_vec(),
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalResultCollector {
    fn name(&self) -> &'static str {
        "RESULT_COLLECTOR"
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

fn poisoned() -> Error {
    Error::internal("result collector lock poisoned")
}

impl Sink for PhysicalResultCollector {
    type Global = Mutex<CollectedResult>;
    type Local = Vec<DataChunk>;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        Ok(Vec::new())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        local.push(chunk.clone());
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, local: Self::Local) -> Result<()> {
        global.lock().map_err(|_| poisoned())?.chunks.extend(local);
        Ok(())
    }
}
