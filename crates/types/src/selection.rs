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

    /// Identifies the shared index buffer, so that compositions with the same selection
    /// can be reused (DuckDB's `SelCache` keys on the buffer).
    pub(crate) fn buffer_key(&self) -> usize {
        self.indices.as_ptr() as usize
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

/// Splits rows `0..count` by `matches(row)`, appending the position of each row
/// (`positions[row]`, or `row` itself without positions) to `true_sel` or `false_sel`.
/// Returns the number of matching rows.
///
/// The loop is branchless like DuckDB's `SelectGenericLoop`: every position is written to
/// both outputs and only the output whose condition held advances, so the cost does not
/// depend on how predictable the predicate is.
pub fn partition_rows(
    count: usize,
    positions: Option<&[u32]>,
    true_sel: Option<&mut Vec<u32>>,
    false_sel: Option<&mut Vec<u32>>,
    mut matches: impl FnMut(usize) -> bool,
) -> usize {
    let position = |row: usize| positions.map_or(row as u32, |positions| positions[row]);
    match (true_sel, false_sel) {
        (Some(true_sel), Some(false_sel)) => {
            let (true_start, false_start) = (true_sel.len(), false_sel.len());
            true_sel.resize(true_start + count, 0);
            false_sel.resize(false_start + count, 0);
            let (true_out, false_out) = (&mut true_sel[true_start..], &mut false_sel[false_start..]);
            let (mut true_count, mut false_count) = (0, 0);
            for row in 0..count {
                let position = position(row);
                let matched = matches(row);
                true_out[true_count] = position;
                false_out[false_count] = position;
                true_count += usize::from(matched);
                false_count += usize::from(!matched);
            }
            true_sel.truncate(true_start + true_count);
            false_sel.truncate(false_start + false_count);
            true_count
        }
        (Some(true_sel), None) => append_where(count, true_sel, position, &mut matches),
        (None, Some(false_sel)) => count - append_where(count, false_sel, position, |row| !matches(row)),
        (None, None) => (0..count).filter(|&row| matches(row)).count(),
    }
}

/// Appends the positions of the rows for which `keep` holds to `out`; returns how many.
fn append_where(
    count: usize,
    out: &mut Vec<u32>,
    position: impl Fn(usize) -> u32,
    mut keep: impl FnMut(usize) -> bool,
) -> usize {
    let start = out.len();
    out.resize(start + count, 0);
    let target = &mut out[start..];
    let mut kept = 0;
    for row in 0..count {
        target[kept] = position(row);
        kept += usize::from(keep(row));
    }
    out.truncate(start + kept);
    kept
}

#[cfg(test)]
mod tests {
    use super::partition_rows;

    #[test]
    fn partition_rows_splits_positions() {
        let positions = [3, 5, 8, 9];
        let (mut yes, mut no) = (vec![1], Vec::new());
        let matched = partition_rows(4, Some(&positions), Some(&mut yes), Some(&mut no), |row| {
            row % 2 == 0
        });
        assert_eq!(matched, 2);
        assert_eq!(yes, [1, 3, 8]);
        assert_eq!(no, [5, 9]);

        let mut only_no = Vec::new();
        let matched = partition_rows(3, None, None, Some(&mut only_no), |row| row == 1);
        assert_eq!(matched, 1);
        assert_eq!(only_no, [0, 2]);
        assert_eq!(partition_rows(5, None, None, None, |row| row > 2), 2);
    }
}
