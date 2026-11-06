//! Probe rows an external hash join cannot probe yet (DuckDB's `ProbeSpill`,
//! `join_hashtable.cpp`): rows whose hash falls in a partition that is not in the current
//! build round are set aside, partitioned like the build side and spillable, and probed
//! in the round that builds their partition.

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result};
use rustdb_row::{
    PartitionedAppendState, PartitionedTupleData, PinProperties, TupleDataCollection, TupleDataLayout,
    ValidityType,
};
use rustdb_types::{DataChunk, LogicalType};

/// One thread's spilled probe rows.
#[derive(Debug)]
pub(crate) struct LocalProbeSpill {
    data: PartitionedTupleData,
    append: PartitionedAppendState,
}

impl LocalProbeSpill {
    /// Sets `chunk` (the probe input followed by the key hashes) aside (`Append`).
    pub fn append(&mut self, chunk: &DataChunk) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.data.append(&mut self.append, chunk)
    }
}

/// The spilled probe rows of every thread (DuckDB's `ProbeSpill`).
#[derive(Debug)]
pub(crate) struct ProbeSpill {
    buffer_manager: BufferManager,
    layout: Arc<TupleDataLayout>,
    radix_bits: usize,
    /// The threads' spills, merged by [`ProbeSpill::finalize`].
    locals: Mutex<Vec<Arc<Mutex<LocalProbeSpill>>>>,
    /// All spilled rows, by build partition.
    global: Mutex<Option<PartitionedTupleData>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProbeSpill {
    /// A spill for probe rows of `probe_types` (the probe input followed by the `UBIGINT`
    /// key hash), partitioned on `radix_bits` bits like the build side.
    pub fn new(
        buffer_manager: BufferManager,
        probe_types: Vec<LogicalType>,
        radix_bits: usize,
    ) -> Result<Self> {
        Ok(Self {
            buffer_manager,
            layout: Arc::new(TupleDataLayout::new(probe_types, ValidityType::CanHaveNull)?),
            radix_bits,
            locals: Mutex::default(),
            global: Mutex::new(None),
        })
    }

    fn partitioned(&self) -> Result<PartitionedTupleData> {
        PartitionedTupleData::new(
            self.buffer_manager.clone(),
            Arc::clone(&self.layout),
            MemoryTag::HashTable,
            self.radix_bits,
            self.layout.column_count() - 1,
        )
    }

    /// A thread's spill (`RegisterThread`).
    pub fn register_thread(&self) -> Result<Arc<Mutex<LocalProbeSpill>>> {
        let mut data = self.partitioned()?;
        let append = data.initialize_append_state(PinProperties::UnpinAfterDone);
        let local = Arc::new(Mutex::new(LocalProbeSpill { data, append }));
        lock(&self.locals).push(Arc::clone(&local));
        Ok(local)
    }

    /// Merges the threads' spills once the probe pipeline is done (`Finalize`).
    pub fn finalize(&self) -> Result<()> {
        let locals = std::mem::take(&mut *lock(&self.locals));
        let mut global = self.partitioned()?;
        for local in locals {
            let mut local = lock(&local);
            let LocalProbeSpill { data, append } = &mut *local;
            data.flush_append_state(append);
            let data = std::mem::replace(data, data.with_radix_bits(self.radix_bits)?);
            global.combine(data)?;
        }
        global.unpin();
        *lock(&self.global) = Some(global);
        Ok(())
    }

    /// Takes the rows of the partitions built in the current round, to probe them
    /// (`PrepareNextProbe`).
    pub fn prepare_next_probe(&self, current_partitions: &[bool]) -> Result<TupleDataCollection> {
        let mut global = lock(&self.global);
        let global = global
            .as_mut()
            .ok_or_else(|| Error::internal("probing a probe spill before finalizing it"))?;
        let mut result = TupleDataCollection::new(
            self.buffer_manager.clone(),
            Arc::clone(&self.layout),
            MemoryTag::HashTable,
        )?;
        for (partition, &current) in global.partitions_mut().iter_mut().zip(current_partitions) {
            if current {
                let rows = std::mem::replace(partition, partition.empty_like());
                result.combine(rows)?;
            }
        }
        Ok(result)
    }
}
