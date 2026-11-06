//! Shared pieces of the data-modifying operators (INSERT, DELETE, UPDATE).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_storage::StorageIndex;
use rustdb_types::{DataChunk, LogicalType, Value, Vector, VectorKind};

/// What a modifying statement returns: the affected row count, or with `RETURNING` the
/// affected rows (DuckDB's `return_chunk`).
#[derive(Debug, Default)]
pub struct ModifyResult {
    count: AtomicU64,
    returned: Mutex<Vec<DataChunk>>,
}

impl ModifyResult {
    pub fn add_count(&self, count: u64) {
        self.count.fetch_add(count, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn push_returned(&self, chunk: DataChunk) {
        if !chunk.is_empty() {
            self.returned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(chunk);
        }
    }

    /// Emits the result for the operator's source role: the count as one BIGINT row, or
    /// the returned chunks one per call. `next` tracks the position across calls.
    pub fn emit(&self, return_chunk: bool, next: &AtomicUsize, chunk: &mut DataChunk) -> Result<bool> {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if !return_chunk {
            if index == 0 {
                let count = i64::try_from(self.count()).map_err(|_| Error::internal("row count overflow"))?;
                let column = Vector::constant(&Value::bigint(count))?.flattened(1);
                chunk.set_columns(vec![column], 1);
            } else {
                chunk.set_len(0);
            }
            return Ok(false);
        }
        let returned = self.returned.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(rows) = returned.get(index) else {
            chunk.set_len(0);
            return Ok(false);
        };
        chunk.set_columns(rows.columns().to_vec(), rows.len());
        Ok(index + 1 < returned.len())
    }
}

/// The transaction-local append of a table, shared by the operators of one statement
/// that insert into it (appends must not interleave).
pub type SharedAppend = std::sync::Arc<Mutex<Option<rustdb_storage::LocalAppendState>>>;

/// The result types of a modifying statement without `RETURNING`: one BIGINT `Count`.
pub fn count_types() -> Vec<LogicalType> {
    vec![LogicalType::BIGINT]
}

/// The row ids of a BIGINT row-id vector as a slice of `i64`.
pub fn row_ids(vector: &Vector, count: usize) -> Result<Vec<i64>> {
    if vector.logical_type().physical_type() != rustdb_types::PhysicalType::Int64 {
        return Err(Error::internal("row id column is not a BIGINT"));
    }
    let flat;
    let vector = if vector.kind() == VectorKind::Flat {
        vector
    } else {
        flat = vector.flattened(count);
        &flat
    };
    Ok(vector.values::<i64>()[..count].to_vec())
}

/// Every physical column of a table with `column_count` columns.
pub fn all_columns(column_count: usize) -> Vec<StorageIndex> {
    (0..column_count).map(StorageIndex::from).collect()
}
