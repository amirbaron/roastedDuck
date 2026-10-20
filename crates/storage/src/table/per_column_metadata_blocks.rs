//! The metadata blocks a row group's columns span beyond their start blocks (DuckDB's
//! `PerColumnMetadataBlocks`).
//!
//! The entries are a flat list: a column marker (the column index with the top bit set)
//! followed by the metadata block ids of that column, columns in ascending order. A
//! column whose metadata fits in its start block has no entry.

/// The top bit marks an entry as a column index (`COLUMN_INDEX_BIT`).
const COLUMN_INDEX_BIT: u64 = 1 << 63;

/// One entry: a column marker or a metadata block id (DuckDB's `PerColumnMetadataBlock`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerColumnMetadataBlock {
    pub is_column_index: bool,
    pub index: u64,
}

impl PerColumnMetadataBlock {
    /// `GetPacked`: the serialized form.
    pub const fn packed(self) -> u64 {
        if self.is_column_index {
            self.index | COLUMN_INDEX_BIT
        } else {
            self.index
        }
    }

    /// `Unpack`.
    pub const fn unpack(packed: u64) -> Self {
        Self {
            is_column_index: packed & COLUMN_INDEX_BIT != 0,
            index: packed & !COLUMN_INDEX_BIT,
        }
    }
}

/// The extra metadata blocks of each column of a row group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerColumnMetadataBlocks {
    pub data: Vec<PerColumnMetadataBlock>,
}

impl PerColumnMetadataBlocks {
    /// `GetBlocksForColumns`: the block ids of each of `columns` (ascending).
    pub fn blocks_for_columns(&self, columns: &[usize]) -> Vec<Vec<u64>> {
        let mut result = vec![Vec::new(); columns.len()];
        if columns.is_empty() {
            return result;
        }
        let mut position = 0;
        let mut collecting = false;
        for entry in &self.data {
            if entry.is_column_index {
                while position < columns.len() && (columns[position] as u64) < entry.index {
                    position += 1;
                }
                if position >= columns.len() {
                    break;
                }
                collecting = columns[position] as u64 == entry.index;
            } else if collecting {
                result[position].push(entry.index);
            }
        }
        result
    }

    /// `AddColumn`: columns are added in ascending order; a column without extra blocks
    /// gets no entry.
    pub fn add_column(&mut self, column: usize, blocks: &[u64]) {
        if blocks.is_empty() {
            return;
        }
        self.data.push(PerColumnMetadataBlock {
            is_column_index: true,
            index: column as u64,
        });
        self.data
            .extend(blocks.iter().map(|&block| PerColumnMetadataBlock {
                is_column_index: false,
                index: block,
            }));
    }

    /// `Merge`: the entries of two maps with disjoint columns, by column.
    pub fn merge(a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(a.data.len() + b.data.len());
        let (mut ai, mut bi) = (0, 0);
        while ai < a.data.len() && bi < b.data.len() {
            let take_a = a.data[ai].index < b.data[bi].index;
            let (source, position) = if take_a {
                (&a.data, &mut ai)
            } else {
                (&b.data, &mut bi)
            };
            data.push(source[*position]);
            *position += 1;
            while *position < source.len() && !source[*position].is_column_index {
                data.push(source[*position]);
                *position += 1;
            }
        }
        data.extend_from_slice(&a.data[ai..]);
        data.extend_from_slice(&b.data[bi..]);
        Self { data }
    }

    /// `ClearColumn`: drops the entry of `column`, keeping the other column indexes.
    pub fn clear_column(&mut self, column: usize) {
        let Some(start) = self
            .data
            .iter()
            .position(|entry| entry.is_column_index && entry.index == column as u64)
        else {
            return;
        };
        let end = self.data[start + 1..]
            .iter()
            .position(|entry| entry.is_column_index)
            .map_or(self.data.len(), |offset| start + 1 + offset);
        self.data.drain(start..end);
    }

    /// `RemoveColumn`: drops the entry of `column` and shifts the later columns down.
    pub fn remove_column(&mut self, column: usize) {
        self.clear_column(column);
        for entry in &mut self.data {
            if entry.is_column_index && entry.index > column as u64 {
                entry.index -= 1;
            }
        }
    }

    /// `ForEachBlock`: calls `f` with `(column index, block id)` for every block.
    pub fn for_each_block(&self, mut f: impl FnMut(usize, u64)) {
        let mut column = 0;
        for entry in &self.data {
            if entry.is_column_index {
                column = entry.index as usize;
            } else {
                f(column, entry.index);
            }
        }
    }

    /// Every block id, in order.
    pub fn blocks(&self) -> Vec<u64> {
        let mut result = Vec::new();
        self.for_each_block(|_, block| result.push(block));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_merge_clear_and_remove() {
        let mut a = PerColumnMetadataBlocks::default();
        a.add_column(0, &[10, 11]);
        a.add_column(3, &[12]);
        let mut b = PerColumnMetadataBlocks::default();
        b.add_column(1, &[]);
        b.add_column(2, &[20]);
        let merged = PerColumnMetadataBlocks::merge(&a, &b);
        assert_eq!(merged.blocks(), vec![10, 11, 20, 12]);
        assert_eq!(
            merged.blocks_for_columns(&[0, 2, 3]),
            vec![vec![10, 11], vec![20], vec![12]]
        );
        let mut removed = merged.clone();
        removed.remove_column(2);
        assert_eq!(removed.blocks_for_columns(&[2]), vec![vec![12]]);
        let mut cleared = merged;
        cleared.clear_column(0);
        assert_eq!(cleared.blocks(), vec![20, 12]);
        let packed = PerColumnMetadataBlock::unpack(
            PerColumnMetadataBlock {
                is_column_index: true,
                index: 5,
            }
            .packed(),
        );
        assert!(packed.is_column_index && packed.index == 5);
    }
}
