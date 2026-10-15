//! Insert and delete versions of the rows of one vector (DuckDB's `ChunkVectorInfo`).
//!
//! Ids are kept as a single constant while every row shares one, and expanded to per-row
//! arrays only when rows diverge. Partially deleted vectors whose deletes are committed
//! by one transaction collapse into a bitmask.

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};

use crate::transaction::{NOT_DELETED_ID, TRANSACTION_ID_START, TransactionData};

/// Which inserted rows a scan sees (DuckDB's `InsertedScanType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertedScanType {
    /// Rows visible to the transaction.
    Standard,
    /// Every row, including uncommitted inserts of other transactions.
    AllRows,
}

/// Which deleted rows a scan omits (DuckDB's `DeletedScanType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletedScanType {
    /// Rows deleted as seen by the transaction.
    Standard,
    /// No rows: deleted rows are included.
    IncludeAllDeleted,
    /// Rows whose delete committed before the transaction's start time.
    OmitCommittedDeletes,
}

/// Whether a scan may read updated values (DuckDB's `UpdateScanType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateScanType {
    #[default]
    Standard,
    /// Uncommitted updates are an error (index creation).
    DisallowUpdates,
}

/// How a scan treats row versions (DuckDB's `ScanOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    pub transaction: TransactionData,
    pub insert_type: InsertedScanType,
    pub delete_type: DeletedScanType,
    pub update_type: UpdateScanType,
}

impl ScanOptions {
    pub const fn new(transaction: TransactionData) -> Self {
        Self {
            transaction,
            insert_type: InsertedScanType::Standard,
            delete_type: DeletedScanType::Standard,
            update_type: UpdateScanType::Standard,
        }
    }
}

/// The outcome of [`ChunkVectorInfo::compress_version_ids`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionCompression {
    /// Both sides are constants.
    FullyCompressed,
    /// Some ids are not yet visible to every transaction; retry later.
    Pending,
    /// Per-row data remains that only a further modification can change.
    Settled,
}

/// Rows of the vector that a delete touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletedRows {
    /// Rows `0..count`.
    Consecutive(usize),
    /// Explicit row offsets within the vector.
    Rows(Vec<u16>),
}

impl DeletedRows {
    pub fn len(&self) -> usize {
        match self {
            Self::Consecutive(count) => *count,
            Self::Rows(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The row offsets within the vector.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        let (count, rows) = match self {
            Self::Consecutive(count) => (*count, &[][..]),
            Self::Rows(rows) => (0, rows.as_slice()),
        };
        (0..count).chain(rows.iter().map(|&row| usize::from(row)))
    }
}

type PerRowIds = Box<[u64]>;

fn per_row(value: u64) -> PerRowIds {
    vec![value; STANDARD_VECTOR_SIZE].into_boxed_slice()
}

/// Words of a deleted-rows bitmask (bit set = deleted, like the on-disk `VECTOR_INFO`).
type DeleteMask = Box<[u64]>;

#[derive(Clone, Debug, PartialEq)]
enum InsertIds {
    Constant(u64),
    PerRow(PerRowIds),
}

#[derive(Clone, Debug, PartialEq)]
enum DeleteIds {
    /// Every row has this delete id (`NOT_DELETED_ID` when none is deleted).
    Constant(u64),
    /// Partially deleted; every deleted row has the committed id `id`.
    Masked {
        mask: DeleteMask,
        id: u64,
    },
    PerRow(PerRowIds),
}

#[inline]
const fn mask_bit(mask: &[u64], row: usize) -> bool {
    mask[row / 64] >> (row % 64) & 1 == 1
}

/// The version ids of one vector of a row group.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkVectorInfo {
    /// Row index of the vector's first row within the row group.
    pub start: u64,
    inserted: InsertIds,
    deleted: DeleteIds,
    /// Whether a compression pass could achieve anything.
    recheck_compression: bool,
}

/// `id` is visible to the transaction: committed before it started, or its own.
#[inline]
const fn use_version(transaction: TransactionData, id: u64) -> bool {
    id < transaction.start_time || id == transaction.transaction_id
}

fn conflict_on_delete() -> Error {
    Error::new(ErrorKind::Transaction, "Conflict on tuple deletion!")
}

impl ChunkVectorInfo {
    /// A vector whose rows were all inserted with `insert_id` and not deleted.
    pub fn new(start: u64, insert_id: u64) -> Self {
        Self::with_ids(start, insert_id, NOT_DELETED_ID)
    }

    pub fn with_ids(start: u64, insert_id: u64, delete_id: u64) -> Self {
        Self {
            start,
            inserted: InsertIds::Constant(insert_id),
            deleted: DeleteIds::Constant(delete_id),
            recheck_compression: true,
        }
    }

    /// A partially deleted vector read from disk: `deleted_mask` has a set bit per
    /// deleted row, and the deletes are visible to everyone.
    pub fn from_deleted_mask(start: u64, deleted_mask: Vec<u64>) -> Self {
        Self {
            start,
            inserted: InsertIds::Constant(0),
            deleted: DeleteIds::Masked {
                mask: deleted_mask.into_boxed_slice(),
                id: 0,
            },
            recheck_compression: false,
        }
    }

    /// Selects the rows of the first `max_count` that the scan sees into `sel` and returns
    /// how many there are (`GetSelVector`).
    pub fn sel_vector(&self, options: ScanOptions, sel: &mut Vec<u32>, max_count: usize) -> usize {
        let transaction = options.transaction;
        let insert_visible = |id: u64| match options.insert_type {
            InsertedScanType::Standard => use_version(transaction, id),
            InsertedScanType::AllRows => true,
        };
        let is_deleted = |id: u64| match options.delete_type {
            DeletedScanType::Standard => use_version(transaction, id),
            DeletedScanType::IncludeAllDeleted => false,
            DeletedScanType::OmitCommittedDeletes => id < transaction.start_time,
        };
        sel.clear();
        if options.insert_type == InsertedScanType::AllRows
            && options.delete_type == DeletedScanType::IncludeAllDeleted
        {
            sel.extend(0..max_count as u32);
            return max_count;
        }
        match (&self.inserted, &self.deleted) {
            (InsertIds::Constant(insert), DeleteIds::Constant(delete)) => {
                if is_deleted(*delete) || !insert_visible(*insert) {
                    return 0;
                }
                sel.extend(0..max_count as u32);
            }
            (InsertIds::PerRow(inserted), DeleteIds::Constant(delete)) => {
                if is_deleted(*delete) {
                    return 0;
                }
                sel.extend((0..max_count as u32).filter(|&row| insert_visible(inserted[row as usize])));
            }
            (inserted, DeleteIds::Masked { mask, id }) => {
                let masked_deleted = is_deleted(*id);
                for row in 0..max_count {
                    let insert = match inserted {
                        InsertIds::Constant(id) => *id,
                        InsertIds::PerRow(ids) => ids[row],
                    };
                    if insert_visible(insert) && !(masked_deleted && mask_bit(mask, row)) {
                        sel.push(row as u32);
                    }
                }
            }
            (inserted, DeleteIds::PerRow(deleted)) => {
                for row in 0..max_count {
                    let insert = match inserted {
                        InsertIds::Constant(id) => *id,
                        InsertIds::PerRow(ids) => ids[row],
                    };
                    if insert_visible(insert) && !is_deleted(deleted[row]) {
                        sel.push(row as u32);
                    }
                }
            }
        }
        sel.len()
    }

    /// Number of rows among the first `max_count` whose delete is committed
    /// (`GetCommittedDeletedCount`).
    pub fn committed_deleted_count(&self, max_count: usize) -> usize {
        let mut options = ScanOptions::new(TransactionData::new(0, TRANSACTION_ID_START));
        options.insert_type = InsertedScanType::AllRows;
        options.delete_type = DeletedScanType::OmitCommittedDeletes;
        let mut sel = Vec::new();
        max_count - self.sel_vector(options, &mut sel, max_count)
    }

    /// Whether `row` is visible to the transaction (`Fetch`).
    pub fn fetch(&self, transaction: TransactionData, row: usize) -> bool {
        let insert = match &self.inserted {
            InsertIds::Constant(id) => *id,
            InsertIds::PerRow(ids) => ids[row],
        };
        let delete = match &self.deleted {
            DeleteIds::Constant(id) => *id,
            DeleteIds::Masked { mask, id } => {
                if mask_bit(mask, row) {
                    *id
                } else {
                    NOT_DELETED_ID
                }
            }
            DeleteIds::PerRow(ids) => ids[row],
        };
        use_version(transaction, insert) && !use_version(transaction, delete)
    }

    fn inserted_per_row(&mut self) -> &mut PerRowIds {
        if let InsertIds::Constant(id) = self.inserted {
            self.inserted = InsertIds::PerRow(per_row(id));
        }
        match &mut self.inserted {
            InsertIds::PerRow(ids) => ids,
            InsertIds::Constant(_) => unreachable!("expanded above"),
        }
    }

    fn deleted_per_row(&mut self) -> &mut PerRowIds {
        match &self.deleted {
            DeleteIds::Constant(id) => self.deleted = DeleteIds::PerRow(per_row(*id)),
            DeleteIds::Masked { mask, id } => {
                let ids = (0..STANDARD_VECTOR_SIZE)
                    .map(|row| if mask_bit(mask, row) { *id } else { NOT_DELETED_ID })
                    .collect();
                self.deleted = DeleteIds::PerRow(ids);
            }
            DeleteIds::PerRow(_) => {}
        }
        match &mut self.deleted {
            DeleteIds::PerRow(ids) => ids,
            _ => unreachable!("expanded above"),
        }
    }

    /// Records that rows `start..end` were inserted by `id` (`Append`).
    pub fn append(&mut self, start: usize, end: usize, id: u64) {
        if start == 0 {
            self.inserted = InsertIds::Constant(id);
            return;
        }
        if self.inserted == InsertIds::Constant(id) {
            return;
        }
        self.recheck_compression = true;
        self.inserted_per_row()[start..end].fill(id);
    }

    /// Marks the inserts of rows `start..end` as committed by `commit_id`.
    pub fn commit_append(&mut self, commit_id: u64, start: usize, end: usize) {
        match &mut self.inserted {
            InsertIds::Constant(id) => *id = commit_id,
            InsertIds::PerRow(ids) => {
                self.recheck_compression = true;
                ids[start..end].fill(commit_id);
            }
        }
    }

    /// Whether the vector can drop its version info: no deletes and every insert is
    /// visible to all transactions from `lowest_transaction` on (`Cleanup`).
    pub fn cleanup(&self, lowest_transaction: u64) -> bool {
        if self.any_deleted() {
            return false;
        }
        match &self.inserted {
            InsertIds::PerRow(ids) => ids[1..].iter().all(|&id| id <= lowest_transaction),
            InsertIds::Constant(id) => *id <= lowest_transaction,
        }
    }

    /// Deletes `rows` in transaction `transaction_id` and returns how many were not
    /// already deleted by it; `rows` is compacted to those (`Delete`).
    ///
    /// Rows deleted by another transaction are a write-write conflict.
    pub fn delete(&mut self, transaction_id: u64, rows: &mut Vec<u16>) -> Result<usize> {
        if let DeleteIds::Constant(id) = self.deleted
            && id != NOT_DELETED_ID
        {
            if id == transaction_id {
                rows.clear();
                return Ok(0);
            }
            return Err(conflict_on_delete());
        }
        let deletes_all = rows.len() == STANDARD_VECTOR_SIZE
            && rows
                .iter()
                .enumerate()
                .all(|(index, &row)| usize::from(row) == index);
        if matches!(self.deleted, DeleteIds::Constant(_)) && deletes_all {
            self.deleted = DeleteIds::Constant(transaction_id);
            return Ok(rows.len());
        }
        self.recheck_compression = true;
        let deleted = self.deleted_per_row();
        let mut kept = 0;
        for index in 0..rows.len() {
            let row = usize::from(rows[index]);
            if deleted[row] == transaction_id {
                continue;
            }
            if deleted[row] != NOT_DELETED_ID {
                // Undo the deletes of this call before reporting the conflict.
                for &undo in &rows[..kept] {
                    deleted[usize::from(undo)] = NOT_DELETED_ID;
                }
                return Err(conflict_on_delete());
            }
            deleted[row] = transaction_id;
            rows[kept] = rows[index];
            kept += 1;
        }
        rows.truncate(kept);
        Ok(kept)
    }

    /// Sets the delete id of `rows` to `commit_id` (also used with `NOT_DELETED_ID` to
    /// roll back and with the transaction id to revert a commit) (`CommitDelete`).
    pub fn commit_delete(&mut self, commit_id: u64, rows: &DeletedRows) {
        if *rows == DeletedRows::Consecutive(STANDARD_VECTOR_SIZE) {
            self.deleted = DeleteIds::Constant(commit_id);
            return;
        }
        if self.deleted == DeleteIds::Constant(commit_id) {
            return;
        }
        self.recheck_compression = true;
        let deleted = self.deleted_per_row();
        for row in rows.iter() {
            deleted[row] = commit_id;
        }
        if deleted.iter().all(|&id| id == commit_id) {
            self.deleted = DeleteIds::Constant(commit_id);
        }
    }

    /// Collapses per-row ids that behave identically for every transaction starting at or
    /// after `lowest_active_start` (`CompressVersionIds`).
    pub fn compress_version_ids(&mut self, lowest_active_start: u64) -> VersionCompression {
        if !self.recheck_compression {
            return if self.has_constant_ids() {
                VersionCompression::FullyCompressed
            } else {
                VersionCompression::Settled
            };
        }
        let mut pending = false;
        if let DeleteIds::PerRow(deleted) = &self.deleted {
            let mut rows_alive = false;
            let mut deletes_pending = false;
            let mut deletes_uncommitted = false;
            let mut deletes_equal = true;
            let mut max_delete_id = 0;
            let mut shared_delete_id = NOT_DELETED_ID;
            for &id in &**deleted {
                if id == NOT_DELETED_ID {
                    rows_alive = true;
                    continue;
                }
                if id >= lowest_active_start {
                    deletes_pending = true;
                    deletes_uncommitted |= id >= TRANSACTION_ID_START;
                } else {
                    max_delete_id = max_delete_id.max(id);
                }
                if shared_delete_id == NOT_DELETED_ID {
                    shared_delete_id = id;
                } else if id != shared_delete_id {
                    deletes_equal = false;
                }
            }
            let to_mask = |deleted: &PerRowIds| -> DeleteMask {
                let mut mask = vec![0u64; STANDARD_VECTOR_SIZE / 64];
                for (row, &id) in deleted.iter().enumerate() {
                    if id != NOT_DELETED_ID {
                        mask[row / 64] |= 1 << (row % 64);
                    }
                }
                mask.into_boxed_slice()
            };
            if !rows_alive && !deletes_pending {
                self.deleted = DeleteIds::Constant(max_delete_id);
            } else if !rows_alive {
                pending = true;
            } else if !deletes_pending {
                self.deleted = DeleteIds::Masked {
                    mask: to_mask(deleted),
                    id: 0,
                };
            } else if deletes_equal && !deletes_uncommitted {
                self.deleted = DeleteIds::Masked {
                    mask: to_mask(deleted),
                    id: shared_delete_id,
                };
            } else {
                pending = true;
            }
        }
        if let InsertIds::PerRow(inserted) = &self.inserted {
            if inserted.iter().all(|&id| id < lowest_active_start) {
                self.inserted = InsertIds::Constant(0);
            } else {
                pending = true;
            }
        }
        self.recheck_compression = pending;
        if self.has_constant_ids() {
            VersionCompression::FullyCompressed
        } else if pending {
            VersionCompression::Pending
        } else {
            VersionCompression::Settled
        }
    }

    fn has_constant_ids(&self) -> bool {
        matches!(self.inserted, InsertIds::Constant(_)) && matches!(self.deleted, DeleteIds::Constant(_))
    }

    /// Whether the vector must be written with delete information when checkpointing
    /// as of `transaction_id` (`HasDeletes`).
    pub fn has_deletes(&self, transaction_id: u64) -> bool {
        if let InsertIds::Constant(id) = self.inserted
            && id >= TRANSACTION_ID_START
        {
            return true;
        }
        if !self.any_deleted() {
            return false;
        }
        match &self.deleted {
            DeleteIds::Constant(id) | DeleteIds::Masked { id, .. } => *id <= transaction_id,
            DeleteIds::PerRow(ids) => ids.iter().any(|&id| id <= transaction_id),
        }
    }

    /// Whether any row is (possibly uncommitted) deleted.
    pub fn any_deleted(&self) -> bool {
        match self.deleted {
            DeleteIds::Constant(id) => id != NOT_DELETED_ID,
            DeleteIds::Masked { .. } | DeleteIds::PerRow(_) => true,
        }
    }

    /// The rows visible to a checkpoint as of `checkpoint_id`, as a deleted-rows mask
    /// (`Write`): `None` if no row is deleted, `Some(None)` if all are, otherwise the mask
    /// words with a set bit per deleted row.
    pub fn checkpoint_deletes(&self, checkpoint_id: u64) -> Option<Option<Vec<u64>>> {
        let start_time = if checkpoint_id == u64::MAX {
            TRANSACTION_ID_START - 1
        } else {
            checkpoint_id + 1
        };
        let mut sel = Vec::new();
        let options = ScanOptions::new(TransactionData::new(u64::MAX, start_time));
        let count = self.sel_vector(options, &mut sel, STANDARD_VECTOR_SIZE);
        if count == STANDARD_VECTOR_SIZE {
            return None;
        }
        if count == 0 {
            return Some(None);
        }
        let mut mask = vec![u64::MAX; STANDARD_VECTOR_SIZE / 64];
        for row in sel {
            mask[row as usize / 64] &= !(1 << (row % 64));
        }
        Some(Some(mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: u64 = TRANSACTION_ID_START + 1;
    const T2: u64 = TRANSACTION_ID_START + 2;

    fn visible(info: &ChunkVectorInfo, transaction: TransactionData, max_count: usize) -> Vec<u32> {
        let mut sel = Vec::new();
        info.sel_vector(ScanOptions::new(transaction), &mut sel, max_count);
        sel
    }

    #[test]
    fn inserts_become_visible_after_commit() {
        let mut info = ChunkVectorInfo::new(0, T1);
        let own = TransactionData::new(T1, 5);
        let other = TransactionData::new(T2, 5);
        assert_eq!(visible(&info, own, 4).len(), 4);
        assert_eq!(visible(&info, other, 4), Vec::<u32>::new());
        info.commit_append(7, 0, 4);
        assert!(visible(&info, other, 4).is_empty(), "committed after the start");
        assert_eq!(visible(&info, TransactionData::new(T2, 8), 4).len(), 4);
    }

    #[test]
    fn deletes_conflict_and_roll_back() {
        let mut info = ChunkVectorInfo::new(0, 0);
        let mut rows = vec![1, 3];
        assert_eq!(info.delete(T1, &mut rows).unwrap(), 2);
        let mut again = vec![3];
        assert_eq!(info.delete(T1, &mut again).unwrap(), 0, "already deleted by us");
        let mut other = vec![2, 3];
        let error = info.delete(T2, &mut other).unwrap_err();
        assert_eq!(
            error.to_string(),
            "TransactionContext Error: Conflict on tuple deletion!"
        );
        // uncommitted deletes are invisible to others, and the conflicting call must not
        // leave row 2 deleted
        assert_eq!(visible(&info, TransactionData::new(T2, 5), 4), [0, 1, 2, 3]);
        assert_eq!(visible(&info, TransactionData::new(T1, 5), 4), [0, 2]);
        info.commit_delete(NOT_DELETED_ID, &DeletedRows::Rows(vec![1, 3]));
        assert_eq!(visible(&info, TransactionData::new(T2, 5), 4), [0, 1, 2, 3]);
    }

    #[test]
    fn compression_collapses_settled_ids() {
        let mut info = ChunkVectorInfo::new(0, 3);
        let mut rows = vec![5];
        info.delete(T1, &mut rows).unwrap();
        info.commit_delete(9, &DeletedRows::Rows(vec![5]));
        assert_eq!(info.compress_version_ids(5), VersionCompression::Settled);
        assert!(matches!(info.deleted, DeleteIds::Masked { id: 9, .. }));
        assert!(!info.fetch(TransactionData::new(T2, 10), 5));
        assert!(
            info.fetch(TransactionData::new(T2, 8), 5),
            "older snapshots still see the row"
        );
        let mut expected = vec![0u64; 32];
        expected[0] = 1 << 5;
        assert_eq!(info.checkpoint_deletes(u64::MAX), Some(Some(expected)));
    }
}
