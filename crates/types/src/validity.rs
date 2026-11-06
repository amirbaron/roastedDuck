use std::sync::Arc;

/// Per-row NULL bitmap; a set bit means "valid". `None` means every row is valid.
///
/// Same bit layout as DuckDB's `ValidityMask` (64-bit words, LSB first), which is also
/// the on-disk and C API layout.
#[derive(Clone, Debug, Default)]
pub struct ValidityMask {
    words: Option<Arc<Vec<u64>>>,
}

impl ValidityMask {
    pub const fn all_valid() -> Self {
        Self { words: None }
    }

    pub fn is_all_valid(&self) -> bool {
        self.words.is_none()
    }

    #[inline]
    pub fn is_valid(&self, row: usize) -> bool {
        match &self.words {
            None => true,
            // Rows past the materialised words were never invalidated.
            Some(words) => words.get(row / 64).is_none_or(|word| word >> (row % 64) & 1 == 1),
        }
    }

    /// A mask with the first `count` rows NULL.
    pub fn all_invalid(count: usize) -> Self {
        Self::from_words(vec![0; count.div_ceil(64)])
    }

    pub fn set_invalid(&mut self, row: usize, capacity: usize) {
        let words = self
            .words
            .get_or_insert_with(|| Arc::new(vec![u64::MAX; capacity.div_ceil(64)]));
        let words = Arc::make_mut(words);
        if words.len() <= row / 64 {
            words.resize(row / 64 + 1, u64::MAX);
        }
        words[row / 64] &= !(1 << (row % 64));
    }

    pub fn set_valid(&mut self, row: usize) {
        if let Some(words) = &mut self.words
            && words.len() > row / 64
        {
            Arc::make_mut(words)[row / 64] |= 1 << (row % 64);
        }
    }

    /// Sets row `row` to `valid`.
    pub fn set(&mut self, row: usize, valid: bool, capacity: usize) {
        if valid {
            self.set_valid(row);
        } else {
            self.set_invalid(row, capacity);
        }
    }

    /// Builds a mask from raw words (bit set = valid).
    pub fn from_words(words: Vec<u64>) -> Self {
        Self {
            words: Some(Arc::new(words)),
        }
    }

    /// Raw words, if any row is NULL.
    pub fn words(&self) -> Option<&[u64]> {
        self.words.as_deref().map(Vec::as_slice)
    }

    /// The materialised words for writing, covering at least `capacity` rows (new rows
    /// valid), or `None` while no words exist (DuckDB's `ValidityMask::GetData`). Shared
    /// words are copied first, so writes affect this mask only.
    pub fn writable_words(&mut self, capacity: usize) -> Option<&mut [u64]> {
        let words = Arc::make_mut(self.words.as_mut()?);
        if words.len() < capacity.div_ceil(64) {
            words.resize(capacity.div_ceil(64), u64::MAX);
        }
        Some(words)
    }

    /// Materialises the words, all valid, for `capacity` rows (DuckDB's
    /// `ValidityMask::EnsureWritable`).
    pub fn ensure_writable(&mut self, capacity: usize) -> &mut [u64] {
        let words = Arc::make_mut(
            self.words
                .get_or_insert_with(|| Arc::new(vec![u64::MAX; capacity.div_ceil(64)])),
        );
        if words.len() < capacity.div_ceil(64) {
            words.resize(capacity.div_ceil(64), u64::MAX);
        }
        words
    }
}
