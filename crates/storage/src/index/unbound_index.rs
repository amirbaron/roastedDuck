//! An index read from a checkpoint or replayed from the WAL whose key expressions are not
//! bound yet (DuckDB's `UnboundIndex`).
//!
//! Binding needs the binder of a connection, so it happens the first time a connection
//! changes the table (`TableIndexList::Bind`). rustdb then builds the index from the
//! table's rows, which already hold every change replayed from the WAL, so nothing is
//! buffered (`BufferChunk`). Until then the stored index data is kept so a checkpoint can
//! write it again (`TableIndexList::SerializeToDisk` of an unbound index).

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::BlockId;
use rustdb_common::{Error, Result};

use super::IndexConstraintType;
use super::art::ArtStorageInfo;
use super::fixed_size_allocator::IndexBufferWriter;

/// The stored data of an unbound index.
#[derive(Debug)]
pub enum UnboundIndexStorage {
    /// Data in blocks of the database file (`IndexStorageInfo` of a checkpoint).
    Disk(ArtStorageInfo),
    /// Data replayed from the WAL: the description and the bytes of each buffer per
    /// allocator. DuckDB writes them to new blocks at once (`ReplayIndexData`); rustdb
    /// does so when a checkpoint writes the still unbound index.
    Wal(ArtStorageInfo, Vec<Vec<Vec<u8>>>),
    /// Data in the layout of DuckDB versions before v0.10, behind a root block pointer.
    Deprecated,
}

/// An index whose key expressions are not bound yet.
#[derive(Debug)]
pub struct UnboundIndex {
    name: String,
    index_type: String,
    constraint: IndexConstraintType,
    /// The physical table columns the key expressions read.
    column_ids: Vec<usize>,
    /// The `CreateInfo` of the index; its type is known to the binder only.
    create_info: Arc<dyn Any + Send + Sync>,
    storage: Mutex<UnboundIndexStorage>,
}

impl UnboundIndex {
    pub fn new(
        name: impl Into<String>,
        index_type: impl Into<String>,
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
        }
    }

    /// `GetIndexName`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `GetIndexType`.
    pub fn index_type(&self) -> &str {
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
            // `ReplayIndexData`: every buffer becomes a block of the file.
            for (allocator, buffers) in info.allocator_infos.iter_mut().zip(buffers.iter()) {
                for (position, bytes) in buffers.iter().enumerate() {
                    let pointer = writer.write(bytes)?;
                    match allocator.block_pointers.get_mut(position) {
                        Some(slot) => *slot = pointer,
                        None => allocator.block_pointers.push(pointer),
                    }
                }
            }
            // Keep the storage lock across the flush, before publishing Disk metadata
            // that a concurrent binder can load.
            writer.flush()?;
            let info = std::mem::take(info);
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
            UnboundIndexStorage::Deprecated => Err(Error::not_implemented(format!(
                "checkpointing the unbound index \"{}\" stored by an old DuckDB version",
                self.name
            ))),
        }
    }
}
