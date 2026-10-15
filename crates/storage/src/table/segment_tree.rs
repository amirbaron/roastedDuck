//! An ordered list of segments covering consecutive rows (DuckDB's `SegmentTree`).

use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Something that covers a number of rows.
pub trait Segment {
    fn row_count(&self) -> u64;
}

/// A segment and the first row it covers.
#[derive(Debug)]
pub struct SegmentNode<T> {
    pub row_start: u64,
    pub node: Arc<T>,
}

impl<T> Clone for SegmentNode<T> {
    fn clone(&self) -> Self {
        Self {
            row_start: self.row_start,
            node: Arc::clone(&self.node),
        }
    }
}

impl<T: Segment> SegmentNode<T> {
    /// One past the last row of the segment.
    pub fn row_end(&self) -> u64 {
        self.row_start + self.node.row_count()
    }
}

/// Segments sorted by start row. Readers clone the node handles they need, so the lock
/// is only held for lookups.
#[derive(Debug)]
pub struct SegmentTree<T> {
    nodes: RwLock<Vec<SegmentNode<T>>>,
}

impl<T> Default for SegmentTree<T> {
    fn default() -> Self {
        Self {
            nodes: RwLock::default(),
        }
    }
}

impl<T: Segment> SegmentTree<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<SegmentNode<T>>> {
        self.nodes.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, Vec<SegmentNode<T>>> {
        self.nodes.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// The first segment.
    pub fn root(&self) -> Option<SegmentNode<T>> {
        self.read().first().cloned()
    }

    pub fn last(&self) -> Option<SegmentNode<T>> {
        self.read().last().cloned()
    }

    /// The segment at position `index`.
    pub fn get(&self, index: usize) -> Option<SegmentNode<T>> {
        self.read().get(index).cloned()
    }

    /// Appends a segment starting at `row_start`.
    pub fn push(&self, row_start: u64, node: Arc<T>) {
        self.write().push(SegmentNode { row_start, node });
    }

    /// The position of the segment containing `row`, if any (`TryGetSegmentIndex`).
    pub fn index_of(&self, row: u64) -> Option<usize> {
        index_of(&self.read(), row)
    }

    /// The segment containing `row` and its position (`GetSegment`); the last segment
    /// also matches the row just past its end.
    pub fn find(&self, row: u64) -> Option<(usize, SegmentNode<T>)> {
        let nodes = self.read();
        let index = index_of(&nodes, row).or_else(|| {
            let last = nodes.len().checked_sub(1)?;
            (nodes[last].row_end() == row).then_some(last)
        })?;
        Some((index, nodes[index].clone()))
    }

    /// Removes every segment at position `index` and after.
    pub fn truncate(&self, index: usize) {
        self.write().truncate(index);
    }

    /// Snapshot of all segments.
    pub fn nodes(&self) -> Vec<SegmentNode<T>> {
        self.read().clone()
    }

    /// Replaces all segments.
    pub fn replace(&self, nodes: Vec<SegmentNode<T>>) {
        *self.write() = nodes;
    }
}

fn index_of<T: Segment>(nodes: &[SegmentNode<T>], row: u64) -> Option<usize> {
    let index = nodes
        .partition_point(|node| node.row_start <= row)
        .checked_sub(1)?;
    (row < nodes[index].row_end()).then_some(index)
}
