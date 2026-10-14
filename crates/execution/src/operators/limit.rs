use std::sync::atomic::{AtomicU64, Ordering};

use rustdb_common::Result;
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, Operator, OperatorResult};

/// LIMIT/OFFSET with constant bounds over an ordered stream (DuckDB's
/// `PhysicalStreamingLimit`).
#[derive(Debug)]
pub struct PhysicalStreamingLimit {
    types: Vec<LogicalType>,
    limit: u64,
    offset: u64,
    children: [PhysicalRef; 1],
}

impl PhysicalStreamingLimit {
    pub fn new(types: Vec<LogicalType>, limit: u64, offset: u64, child: PhysicalRef) -> Self {
        Self {
            types,
            limit,
            offset,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalStreamingLimit {
    fn name(&self) -> &'static str {
        "STREAMING_LIMIT"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }
}

impl Operator for PhysicalStreamingLimit {
    /// Rows seen so far across all threads.
    type Global = AtomicU64;
    type Local = ();

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<AtomicU64> {
        Ok(AtomicU64::new(0))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &AtomicU64) -> Result<()> {
        Ok(())
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        seen: &AtomicU64,
        (): &mut (),
    ) -> Result<OperatorResult> {
        let count = input.len() as u64;
        let start = seen.fetch_add(count, Ordering::Relaxed);
        let end = self.offset.saturating_add(self.limit);
        if start >= end {
            return Ok(OperatorResult::Finished);
        }
        // Rows [first, last) of this chunk fall inside [offset, offset + limit).
        let first = self.offset.saturating_sub(start).min(count);
        let last = (end - start).min(count);
        if first == 0 && last == count {
            output.set_columns(input.columns().to_vec(), input.len());
        } else if first < last {
            let sel = SelectionVector::new((first as u32..last as u32).collect::<Vec<_>>());
            output.set_columns(
                input.columns().iter().map(|c| c.slice(&sel)).collect(),
                (last - first) as usize,
            );
        }
        Ok(if start + count >= end {
            OperatorResult::Finished
        } else {
            OperatorResult::NeedMoreInput
        })
    }
}
