use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{ErasedOperator, ErasedSink, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Operator, OperatorResult, Sink, SinkFinalize, SinkResult};

/// `CROSS JOIN` (DuckDB's `PhysicalCrossProduct`): the right input is materialised by
/// the sink; every left chunk is then combined with it by the `CrossProductExecutor`.
#[derive(Debug)]
pub struct PhysicalCrossProduct {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalCrossProduct {
    pub fn new(types: Vec<LogicalType>, left: PhysicalRef, right: PhysicalRef) -> Self {
        Self {
            types,
            children: [left, right],
        }
    }
}

impl PhysicalOperator for PhysicalCrossProduct {
    fn name(&self) -> &'static str {
        "CROSS_PRODUCT"
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
    fn parallel_operator(&self) -> bool {
        true
    }
    fn parallel_sink(&self) -> bool {
        true
    }
}

/// The materialised right input (`CrossProductGlobalState::rhs_materialized`).
#[derive(Debug, Default)]
pub struct CrossProductGlobal {
    chunks: Mutex<Vec<DataChunk>>,
}

impl Sink for PhysicalCrossProduct {
    type Global = CrossProductGlobal;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<CrossProductGlobal> {
        Ok(CrossProductGlobal::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &CrossProductGlobal) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &CrossProductGlobal,
        (): &mut (),
    ) -> Result<SinkResult> {
        if !chunk.is_empty() {
            global
                .chunks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(chunk.compacted());
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &CrossProductGlobal, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, _: &mut CrossProductGlobal) -> Result<SinkFinalize> {
        Ok(SinkFinalize::Ready)
    }
}

/// The right chunks and the position of a thread's `CrossProductExecutor`.
#[derive(Debug)]
pub struct CrossProductExecutor {
    rhs: Arc<Vec<DataChunk>>,
    /// The right chunk being combined with the current input, once started.
    chunk: Option<usize>,
    position_in_chunk: usize,
    /// Whether the input chunk is scanned one row at a time against the whole right
    /// chunk (the input is the smaller one); otherwise the right chunk is scanned.
    scan_input_chunk: bool,
}

impl CrossProductExecutor {
    /// Advances to the next (input row or right row, right chunk) pair; `false` once
    /// the right side is exhausted for this input (`NextValue`).
    fn next_value(&mut self, input: &DataChunk) -> bool {
        if let Some(chunk) = self.chunk {
            self.position_in_chunk += 1;
            let chunk_size = if self.scan_input_chunk {
                input.len()
            } else {
                self.rhs[chunk].len()
            };
            if self.position_in_chunk < chunk_size {
                return true;
            }
        }
        let next = self.chunk.map_or(0, |chunk| chunk + 1);
        let Some(scan_chunk) = self.rhs.get(next) else {
            self.chunk = None;
            return false;
        };
        self.chunk = Some(next);
        self.position_in_chunk = 0;
        // Keep the larger chunk referenced whole so the output chunks are large.
        self.scan_input_chunk = input.len() < scan_chunk.len();
        true
    }
}

impl Operator for PhysicalCrossProduct {
    type Global = Arc<Vec<DataChunk>>;
    type Local = CrossProductExecutor;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Err(Error::internal("cross product probe requires its sink state"))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<Self::Global> {
        let global = sink
            .and_then(|state| state.downcast_ref::<CrossProductGlobal>())
            .ok_or_else(|| Error::internal("cross product probe requires its sink state"))?;
        let chunks = global
            .chunks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Ok(Arc::new(chunks))
    }

    fn local_operator_state(&self, _: &ExecutionContext, rhs: &Self::Global) -> Result<CrossProductExecutor> {
        Ok(CrossProductExecutor {
            rhs: Arc::clone(rhs),
            chunk: None,
            position_in_chunk: 0,
            scan_input_chunk: false,
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        _: &Self::Global,
        executor: &mut CrossProductExecutor,
    ) -> Result<OperatorResult> {
        if executor.rhs.is_empty() {
            return Ok(OperatorResult::Finished);
        }
        if !executor.next_value(input) {
            return Ok(OperatorResult::NeedMoreInput);
        }
        let Some(chunk) = executor.chunk else {
            return Err(Error::internal("cross product without a right chunk"));
        };
        let scan_chunk = &executor.rhs[chunk];
        let position = executor.position_in_chunk;
        // One side is referenced whole, the other contributes a single (constant) row.
        let (whole, single) = if executor.scan_input_chunk {
            (scan_chunk, input)
        } else {
            (input, scan_chunk)
        };
        // One value per output chunk (`ConstantVector::Reference`).
        let constants = single
            .columns()
            .iter()
            .map(|column| column.reference_row(position, whole.len()))
            .collect::<Result<Vec<Vector>>>()?;
        let columns = if executor.scan_input_chunk {
            constants
                .into_iter()
                .chain(whole.columns().iter().cloned())
                .collect()
        } else {
            whole.columns().iter().cloned().chain(constants).collect()
        };
        output.set_columns(columns, whole.len());
        Ok(OperatorResult::HaveMoreOutput)
    }
}
