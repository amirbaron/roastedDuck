//! A source → operators → sink chain and its run-time state (DuckDB's `Pipeline`,
//! `pipeline.cpp`).

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};

use crate::operator::{OrderPreservationType, PhysicalRef};

/// A type-erased operator state shared by the threads of a pipeline.
pub(crate) type SharedState = Arc<dyn Any + Send + Sync>;

/// The range of batch indices reserved for each pipeline feeding one sink, so that
/// batches of different pipelines (e.g. `UNION ALL` branches) never collide
/// (`PipelineBuildState::BATCH_INCREMENT`).
pub(crate) const BATCH_INCREMENT: u64 = 10_000_000_000_000;

/// One pipeline of a query (DuckDB's `Pipeline`): its operators are fixed once the plan
/// is built; the source state and the batch indices in flight change while it runs.
#[derive(Debug)]
pub(crate) struct Pipeline {
    pub source: PhysicalRef,
    /// Streaming operators in execution order (source side first).
    pub operators: Vec<PhysicalRef>,
    pub sink: PhysicalRef,
    /// The first batch index of this pipeline's batches.
    pub base_batch_index: u64,
    /// Pipelines of other meta pipelines that must be complete before this one starts.
    pub dependencies: Vec<usize>,
    /// Pipelines of the same meta pipeline that must have pushed all their input into the
    /// sink before this one starts.
    pub intra_dependencies: Vec<usize>,
    /// The source's global state, created when the pipeline is scheduled.
    source_state: Mutex<Option<SharedState>>,
    /// The batch index every thread is working on, with multiplicity (`batch_indexes`).
    batch_indexes: Mutex<BTreeMap<u64, usize>>,
}

impl Pipeline {
    pub fn new(
        source: PhysicalRef,
        operators: Vec<PhysicalRef>,
        sink: PhysicalRef,
        base_batch_index: u64,
        dependencies: Vec<usize>,
        intra_dependencies: Vec<usize>,
    ) -> Self {
        Self {
            source,
            operators,
            sink,
            base_batch_index,
            dependencies,
            intra_dependencies,
            source_state: Mutex::new(None),
            batch_indexes: Mutex::default(),
        }
    }

    /// The source's global state, if the pipeline was scheduled.
    pub fn source_state(&self) -> Option<SharedState> {
        self.source_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Creates the source's global state unless it exists (`ResetSource(false)`).
    pub fn reset_source(&self, create: impl FnOnce() -> Result<SharedState>) -> Result<SharedState> {
        let mut state = self.source_state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(state) = &*state {
            return Ok(Arc::clone(state));
        }
        let created = create()?;
        *state = Some(Arc::clone(&created));
        Ok(created)
    }

    /// Registers a thread starting on the pipeline and returns its first batch index:
    /// the lowest one still in flight (`RegisterNewBatchIndex`).
    pub fn register_new_batch_index(&self) -> u64 {
        let mut indexes = self.batch_indexes.lock().unwrap_or_else(PoisonError::into_inner);
        let minimum = indexes.keys().next().copied().unwrap_or(self.base_batch_index);
        *indexes.entry(minimum).or_default() += 1;
        minimum
    }

    /// Moves a thread from batch `old_index` to `new_index` and returns the lowest batch
    /// index still in flight (`UpdateBatchIndex`).
    pub fn update_batch_index(&self, old_index: u64, new_index: u64) -> Result<u64> {
        let mut indexes = self.batch_indexes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&minimum) = indexes.keys().next()
            && new_index < minimum
        {
            return Err(Error::internal(format!(
                "Processing batch index {new_index}, but previous min batch index was {minimum}"
            )));
        }
        let Some(count) = indexes.get_mut(&old_index) else {
            return Err(Error::internal(format!(
                "Batch index {old_index} was not found in set of active batch indexes"
            )));
        };
        *count -= 1;
        if *count == 0 {
            indexes.remove(&old_index);
        }
        *indexes.entry(new_index).or_default() += 1;
        Ok(indexes.keys().next().copied().unwrap_or(new_index))
    }
}

/// Whether the order in which rows reach the sink matters (`Pipeline::IsOrderDependent`),
/// for a pipeline that may still be under construction.
/// `operators` may be in either order: the first operator that fixes or drops the order
/// decides, as in DuckDB (which calls it both while building and after `Ready`).
pub(crate) fn is_order_dependent(
    source: Option<&PhysicalRef>,
    operators: &[PhysicalRef],
    sink: Option<&PhysicalRef>,
    preserve_insertion_order: bool,
) -> bool {
    if let Some(source) = source {
        match source.source_order() {
            OrderPreservationType::FixedOrder => return true,
            OrderPreservationType::NoOrder => return false,
            OrderPreservationType::InsertionOrder => {}
        }
    }
    for op in operators {
        match op.operator_order() {
            OrderPreservationType::NoOrder => return false,
            OrderPreservationType::FixedOrder => return true,
            OrderPreservationType::InsertionOrder => {}
        }
    }
    if !preserve_insertion_order {
        return false;
    }
    sink.is_some_and(|sink| sink.sink_order_dependent())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operators::PhysicalDummyScan;

    fn pipeline() -> Pipeline {
        let op: PhysicalRef = Arc::new(PhysicalDummyScan::new(Vec::new()));
        Pipeline::new(Arc::clone(&op), Vec::new(), op, 5, Vec::new(), Vec::new())
    }

    #[test]
    fn batch_indexes_track_the_minimum_in_flight() {
        let pipeline = pipeline();
        assert_eq!(pipeline.register_new_batch_index(), 5);
        assert_eq!(pipeline.register_new_batch_index(), 5);
        assert_eq!(pipeline.update_batch_index(5, 7).unwrap(), 5);
        assert_eq!(pipeline.update_batch_index(5, 6).unwrap(), 6);
        assert_eq!(pipeline.update_batch_index(6, 9).unwrap(), 7);
        assert!(pipeline.update_batch_index(6, 10).is_err());
        assert!(pipeline.update_batch_index(7, 3).is_err());
    }
}
