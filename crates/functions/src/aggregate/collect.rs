//! Rows collected by an aggregate state without copying them (the role of DuckDB's
//! `LinkedList` of `ListSegment`s in `list_segment.cpp`).
//!
//! Input vectors are reference counted, so a state keeps the chunks it took rows from and
//! the selected row numbers; the values are copied once, when the result is built.

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::kernels::scatter;
use rustdb_types::{SelectionVector, Vector};

/// The rows a state collected, in insertion order.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowCollection {
    segments: Vec<(Arc<Vector>, Vec<u32>)>,
    len: usize,
}

impl RowCollection {
    /// Appends row `row` of `vector` (logical row numbering of that vector).
    pub(crate) fn push(&mut self, vector: &Arc<Vector>, row: usize) {
        match self.segments.last_mut() {
            Some((last, rows)) if Arc::ptr_eq(last, vector) => rows.push(row as u32),
            _ => self.segments.push((Arc::clone(vector), vec![row as u32])),
        }
        self.len += 1;
    }

    /// Appends rows `0..count` of `vector`.
    pub(crate) fn push_all(&mut self, vector: &Arc<Vector>, count: usize) {
        if count == 0 {
            return;
        }
        let rows = (0..count as u32).collect();
        self.segments.push((Arc::clone(vector), rows));
        self.len += count;
    }

    /// Appends every row of `other`.
    pub(crate) fn extend(&mut self, other: &Self) {
        self.segments.extend(other.segments.iter().cloned());
        self.len += other.len;
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies the rows into `target` (flat, of the collected type) starting at row `offset`;
    /// `target` grows as needed.
    pub(crate) fn write_to(&self, target: &mut Vector, offset: usize) -> Result<()> {
        if target.capacity() < offset + self.len {
            target.resize((offset + self.len).max(target.capacity() * 2));
        }
        let mut position = offset;
        for (vector, rows) in &self.segments {
            let sliced = vector.slice(&SelectionVector::new(rows.clone()));
            let positions: Vec<u32> = (position..position + rows.len()).map(|p| p as u32).collect();
            scatter(&sliced, target, &positions)?;
            position += rows.len();
        }
        Ok(())
    }

    /// The rows as one flat vector of type `ty`.
    pub(crate) fn to_vector(&self, ty: rustdb_types::LogicalType) -> Result<Vector> {
        let mut vector = Vector::flat(ty, self.len.max(1));
        self.write_to(&mut vector, 0)?;
        Ok(vector)
    }
}
