//! The positional join (DuckDB's `PhysicalPositionalJoin`): row `i` of the left input is
//! joined with row `i` of the right input; the shorter side is padded with NULLs.

use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use super::null_columns;
use super::range_join::MaterializedRows;
use crate::join_hashtable::flatten_sequences;
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source, SourceResult,
};

/// A join by row position; see the module docs.
#[derive(Debug)]
pub struct PhysicalPositionalJoin {
    types: Vec<LogicalType>,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalPositionalJoin {
    /// A positional join of `left` and `right`.
    pub fn new(types: Vec<LogicalType>, left: PhysicalRef, right: PhysicalRef) -> Self {
        Self {
            types,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            children: [left, right],
        }
    }

    /// Right rows `start..start + count` (NULL past the end) as columns.
    fn right_rows(&self, rows: &MaterializedRows, start: usize, count: usize) -> Result<Vec<Vector>> {
        let available = rows.count().saturating_sub(start).min(count);
        if available == 0 {
            return Ok(null_columns(&self.right_types));
        }
        let indices: Vec<u32> = (start as u32..(start + available) as u32).collect();
        let mut chunk = DataChunk::default();
        rows.gather(&indices, &mut chunk)?;
        if available == count {
            return Ok(chunk.into_columns());
        }
        let padded: Vec<Option<u32>> = (0..count as u32)
            .map(|row| (row < available as u32).then_some(row))
            .collect();
        Ok(chunk
            .columns()
            .iter()
            .map(|column| column.select_rows(&padded))
            .collect())
    }
}

impl PhysicalOperator for PhysicalPositionalJoin {
    fn name(&self) -> &'static str {
        "POSITIONAL_JOIN"
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
        // Positional joins are always outer: the rest of the right side follows.
        Some(self)
    }
}

/// The materialized right side and the position of the next right row
/// (DuckDB's `PositionalJoinGlobalState`).
#[derive(Debug)]
pub struct PositionalJoinGlobalState {
    chunks: Mutex<Vec<DataChunk>>,
    rows: Option<MaterializedRows>,
    /// The next right row to emit.
    source_offset: Mutex<usize>,
}

impl Sink for PhysicalPositionalJoin {
    type Global = PositionalJoinGlobalState;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<PositionalJoinGlobalState> {
        Ok(PositionalJoinGlobalState {
            chunks: Mutex::default(),
            rows: None,
            source_offset: Mutex::new(0),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &PositionalJoinGlobalState) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &PositionalJoinGlobalState,
        (): &mut (),
    ) -> Result<SinkResult> {
        let mut chunk = chunk.clone();
        flatten_sequences(&mut chunk);
        global
            .chunks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(chunk);
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &PositionalJoinGlobalState, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(
        &self,
        ctx: &ExecutionContext,
        global: &mut PositionalJoinGlobalState,
    ) -> Result<SinkFinalize> {
        let chunks = std::mem::take(global.chunks.get_mut().unwrap_or_else(PoisonError::into_inner));
        let columns: Vec<usize> = (0..self.right_types.len()).collect();
        global.rows = Some(MaterializedRows::materialize(
            &ctx.env.buffer_manager,
            &chunks,
            &columns,
            self.right_types.clone(),
        )?);
        Ok(SinkFinalize::Ready)
    }
}

impl Operator for PhysicalPositionalJoin {
    type Global = SinkState;
    type Local = ();

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal("positional join requires its sink state"))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("positional join requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<()> {
        Ok(())
    }

    /// The input with the next right rows (`PositionalJoinGlobalState::Execute`).
    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        (): &mut (),
    ) -> Result<OperatorResult> {
        let global = super::sink_state::<PositionalJoinGlobalState>(sink, "positional join")?;
        let rows = global
            .rows
            .as_ref()
            .ok_or_else(|| Error::internal("positional join probed before its sink finished"))?;
        let mut offset = global
            .source_offset
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = input.len();
        let mut columns = input.columns().to_vec();
        columns.extend(self.right_rows(rows, *offset, count)?);
        *offset += count;
        output.set_columns(columns, count);
        Ok(OperatorResult::NeedMoreInput)
    }
}

impl Source for PhysicalPositionalJoin {
    type Global = SinkState;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("positional join source requires its sink state"))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<()> {
        Ok(())
    }

    /// The right rows past the end of the left input, with NULL left columns
    /// (`PositionalJoinGlobalState::GetData`).
    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        sink: &SinkState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let global = super::sink_state::<PositionalJoinGlobalState>(sink, "positional join")?;
        let rows = global
            .rows
            .as_ref()
            .ok_or_else(|| Error::internal("positional join scanned before its sink finished"))?;
        let mut offset = global
            .source_offset
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = rows.count().saturating_sub(*offset).min(STANDARD_VECTOR_SIZE);
        if count == 0 {
            return Ok(SourceResult::Finished);
        }
        let mut columns = null_columns(&self.left_types);
        columns.extend(self.right_rows(rows, *offset, count)?);
        *offset += count;
        chunk.set_columns(columns, count);
        Ok(SourceResult::HaveMoreOutput)
    }
}
