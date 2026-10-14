use std::sync::Arc;

/// Row positions used to select from or reorder a vector (DuckDB's `SelectionVector`).
///
/// Positions are `u32` like DuckDB's `sel_t`. Shared selections are reference counted so
/// that slicing many vectors with the same selection does not copy it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionVector {
    indices: Arc<[u32]>,
}

impl SelectionVector {
    pub fn new(indices: impl Into<Arc<[u32]>>) -> Self {
        Self {
            indices: indices.into(),
        }
    }

    /// `0, 1, .., count - 1`.
    pub fn incremental(count: usize) -> Self {
        Self::new((0..count as u32).collect::<Vec<_>>())
    }

    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    #[inline]
    pub fn get(&self, row: usize) -> usize {
        self.indices[row] as usize
    }

    pub fn as_slice(&self) -> &[u32] {
        &self.indices
    }

    /// The selection `self ∘ inner`: row `i` maps to `inner[self[i]]`.
    #[must_use]
    pub fn compose(&self, inner: &SelectionVector) -> Self {
        Self::new(
            self.indices
                .iter()
                .map(|&i| inner.indices[i as usize])
                .collect::<Vec<_>>(),
        )
    }
}

/// A borrowed view of how logical rows map to physical slots.
#[derive(Clone, Copy, Debug)]
pub enum Sel<'a> {
    /// Row `i` is slot `i`.
    Identity,
    /// Every row is slot `0`.
    Constant,
    /// Row `i` is slot `indices[i]`.
    Indices(&'a [u32]),
}

impl Sel<'_> {
    #[inline]
    pub fn slot(self, row: usize) -> usize {
        match self {
            Self::Identity => row,
            Self::Constant => 0,
            Self::Indices(indices) => indices[row] as usize,
        }
    }
}
