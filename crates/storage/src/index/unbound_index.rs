//! An index read from a checkpoint or replayed from the WAL whose key expressions are not
//! bound yet (DuckDB's `UnboundIndex`).
//!
//! Binding needs the binder of a connection, so it happens the first time a connection
//! changes the table (`TableIndexList::Bind`). Stored ART buffers are restored lazily,
//! then buffered WAL mutations are applied in order. Until binding, original storage
//! stays available for retries and checkpoint serialization.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_buffer::BlockId;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use super::art::ArtStorageInfo;
use super::fixed_size_allocator::IndexBufferWriter;
use super::{Art, IndexAppendMode, IndexConstraintType};

/// The stored data of an unbound index.
#[derive(Debug)]
pub enum UnboundIndexStorage {
    /// Data in blocks of the database file (`IndexStorageInfo` of a checkpoint).
    Disk(ArtStorageInfo),
    /// Data replayed from the WAL: the description and the bytes of each buffer per
    /// allocator. DuckDB writes them to new blocks at once (`ReplayIndexData`); roastedduck
    /// does so when a checkpoint writes the still unbound index.
    Wal(ArtStorageInfo, Vec<Vec<Vec<u8>>>),
    /// Data in the layout of DuckDB versions before v0.10, behind a root block pointer.
    Deprecated,
}

/// Writes the native allocator buffers carried by CREATE INDEX WAL entries.
/// Publish the rewritten locations only after every deferred block is readable.
pub fn persist_wal_index(
    info: &ArtStorageInfo,
    buffers: &[Vec<Vec<u8>>],
    writer: &mut dyn IndexBufferWriter,
) -> Result<ArtStorageInfo> {
    if info.allocator_infos.len() != buffers.len() {
        return Err(Error::serialization("index WAL allocator count mismatch"));
    }
    let mut stored = info.clone();
    for (allocator, buffers) in stored.allocator_infos.iter_mut().zip(buffers) {
        if allocator.allocation_sizes.len() != buffers.len() {
            return Err(Error::serialization("index WAL buffer count mismatch"));
        }
        for (position, bytes) in buffers.iter().enumerate() {
            if allocator.allocation_sizes[position] != bytes.len() as u64 {
                return Err(Error::serialization("index WAL allocation size mismatch"));
            }
            let pointer = writer.write(bytes)?;
            match allocator.block_pointers.get_mut(position) {
                Some(slot) => *slot = pointer,
                None => allocator.block_pointers.push(pointer),
            }
        }
    }
    writer.flush()?;
    Ok(stored)
}

/// An index mutation retained while its key expressions are unbound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferedIndexReplay {
    Insert,
    Delete,
}

/// Copy-on-write vectors retain only this index's physical columns and row IDs.
/// Like the reference's allocator-backed replay collections, these buffers live
/// until binding; retaining vectors avoids flattening dictionary/string payloads.
#[derive(Clone, Debug)]
pub(crate) struct BufferedIndexChunk {
    kind: BufferedIndexReplay,
    columns: Vec<(usize, Vector)>,
    row_ids: Vec<i64>,
}

/// An index whose key expressions are not bound yet.
#[derive(Debug)]
pub struct UnboundIndex {
    name: ByteString,
    index_type: ByteString,
    constraint: IndexConstraintType,
    /// The physical table columns the key expressions read.
    column_ids: Vec<usize>,
    /// The `CreateInfo` of the index; its type is known to the binder only.
    create_info: Arc<dyn Any + Send + Sync>,
    storage: Mutex<UnboundIndexStorage>,
    replays: Mutex<Vec<BufferedIndexChunk>>,
}

impl UnboundIndex {
    pub fn new(
        name: impl Into<ByteString>,
        index_type: impl Into<ByteString>,
        constraint: IndexConstraintType,
        column_ids: Vec<usize>,
        create_info: Arc<dyn Any + Send + Sync>,
        storage: UnboundIndexStorage,
    ) -> Self {
        Self {
            name: name.into(),
            index_type: index_type.into(),
            constraint,
            column_ids,
            create_info,
            storage: Mutex::new(storage),
            replays: Mutex::new(Vec::new()),
        }
    }

    /// `GetIndexName`.
    pub fn name(&self) -> &ByteString {
        &self.name
    }

    /// `GetIndexType`.
    pub fn index_type(&self) -> &ByteString {
        &self.index_type
    }

    /// `GetConstraintType`.
    pub fn constraint_type(&self) -> IndexConstraintType {
        self.constraint
    }

    pub fn is_unique(&self) -> bool {
        matches!(
            self.constraint,
            IndexConstraintType::Unique | IndexConstraintType::Primary
        )
    }

    pub fn is_primary(&self) -> bool {
        self.constraint == IndexConstraintType::Primary
    }

    pub fn is_foreign(&self) -> bool {
        self.constraint == IndexConstraintType::Foreign
    }

    /// `GetColumnIds`.
    pub fn column_ids(&self) -> &[usize] {
        &self.column_ids
    }

    /// `GetCreateInfo`.
    pub fn create_info(&self) -> &Arc<dyn Any + Send + Sync> {
        &self.create_info
    }

    fn storage(&self) -> MutexGuard<'_, UnboundIndexStorage> {
        self.storage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn prepare_replay(
        &self,
        chunk: &DataChunk,
        row_ids: &[i64],
        kind: BufferedIndexReplay,
    ) -> Result<BufferedIndexChunk> {
        if row_ids.len() != chunk.len() {
            return Err(Error::internal("unbound index replay row count mismatch"));
        }
        let mut ids = self.column_ids.clone();
        ids.sort_unstable();
        ids.dedup();
        let columns = ids
            .into_iter()
            .map(|column| {
                chunk
                    .columns()
                    .get(column)
                    .cloned()
                    .map(|vector| (column, vector))
                    .ok_or_else(|| Error::internal("unbound index replay column out of range"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(BufferedIndexChunk {
            kind,
            columns,
            row_ids: row_ids.to_vec(),
        })
    }

    pub(crate) fn push_replay(&self, replay: BufferedIndexChunk) {
        self.replays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(replay);
    }

    /// A checkpoint must bind this index before replacing its original tree.
    pub fn has_buffered_replays(&self) -> bool {
        !self
            .replays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Replay against a newly restored tree. Keep the original buffers on failure,
    /// since the binder discards that partial tree and may retry from storage.
    pub fn apply_buffered_replays(&self, index: &mut Art, types: &[LogicalType]) -> Result<()> {
        let replays = self.replays.lock().unwrap_or_else(PoisonError::into_inner);
        for replay in replays.iter() {
            let mut columns = types
                .iter()
                .map(|ty| Vector::constant_null(ty.clone()))
                .collect::<Vec<_>>();
            for (column, vector) in &replay.columns {
                let target = columns
                    .get_mut(*column)
                    .ok_or_else(|| Error::internal("unbound replay physical column out of range"))?;
                *target = vector.clone();
            }
            let chunk = DataChunk::new(columns, replay.row_ids.len());
            match replay.kind {
                BufferedIndexReplay::Insert => {
                    index.append(&chunk, &replay.row_ids, &[], IndexAppendMode::InsertDuplicates)?;
                }
                BufferedIndexReplay::Delete => index.delete(&chunk, &replay.row_ids)?,
            }
        }
        Ok(())
    }

    /// Native allocator metadata, rewriting WAL buffers to file blocks before use.
    /// An unsupported/missing representation remains eligible for table rebuild.
    pub fn storage_for_binding(&self, writer: &mut dyn IndexBufferWriter) -> Result<Option<ArtStorageInfo>> {
        {
            let storage = self.storage();
            match &*storage {
                UnboundIndexStorage::Deprecated => return Ok(None),
                UnboundIndexStorage::Disk(info) | UnboundIndexStorage::Wal(info, _)
                    if info.allocator_infos.is_empty() =>
                {
                    return Ok(None);
                }
                UnboundIndexStorage::Disk(info) => return Ok(Some(info.clone())),
                UnboundIndexStorage::Wal(..) => {}
            }
        }
        self.serialize_to_disk(writer, &mut |_| {}).map(Some)
    }

    /// The description of the stored index for a checkpoint (`GetStorageInfo`). Buffers
    /// replayed from the WAL are written with `write` first; the index then refers to
    /// them like an index read from disk. `blocks` receives each block the description
    /// references.
    pub fn serialize_to_disk(
        &self,
        writer: &mut dyn IndexBufferWriter,
        blocks: &mut dyn FnMut(BlockId),
    ) -> Result<ArtStorageInfo> {
        let mut storage = self.storage();
        if let UnboundIndexStorage::Wal(info, buffers) = &mut *storage {
            let info = persist_wal_index(info, buffers, writer)?;
            *storage = UnboundIndexStorage::Disk(info);
        } else {
            writer.flush()?;
        }
        match &*storage {
            UnboundIndexStorage::Disk(info) => {
                for allocator in &info.allocator_infos {
                    for pointer in &allocator.block_pointers {
                        if pointer.block_id >= 0 {
                            blocks(pointer.block_id);
                        }
                    }
                }
                Ok(info.clone())
            }
            UnboundIndexStorage::Wal(..) => Err(Error::internal("unbound index buffers were not written")),
            UnboundIndexStorage::Deprecated => Err(Error::new_bytes(
                roastedduck_common::ErrorKind::NotImplemented,
                roastedduck_common::byte_format!(
                    "checkpointing the unbound index \"{}\" stored by an old DuckDB version",
                    self.name
                )
                .into_bytes(),
            )),
        }
    }
}
