//! The storage of an attached database file (DuckDB's `SingleFileStorageManager`): the
//! block manager, the metadata manager and the I/O manager of its tables.

use std::sync::Arc;

use rustdb_buffer::{BlockManager, BufferManager};
use rustdb_common::Result;
use rustdb_serde::{SerializationCompatibility, SerializationOptions};

use crate::data_table::TableIoManager;
use crate::metadata::{MetaBlockPointer, MetadataManager};
use crate::single_file_block_manager::{BlockManagerOptions, SingleFileBlockManager};
use crate::storage_info::DEFAULT_BLOCK_ALLOC_SIZE;

/// How a database file is opened (DuckDB's `StorageManagerOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageOptions {
    pub read_only: bool,
    pub block_alloc_size: usize,
    /// The serialization version to write; `None` keeps the file's (or the default).
    pub storage_version: Option<u64>,
    pub row_group_size: u64,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            read_only: false,
            block_alloc_size: DEFAULT_BLOCK_ALLOC_SIZE,
            storage_version: None,
            row_group_size: crate::DEFAULT_ROW_GROUP_SIZE,
        }
    }
}

/// The storage of one database file.
#[derive(Debug)]
pub struct StorageManager {
    path: String,
    block_manager: Arc<SingleFileBlockManager>,
    metadata: MetadataManager,
    io: Arc<TableIoManager>,
    /// Whether the file was created by this open.
    created: bool,
}

impl StorageManager {
    /// Opens the database file at `path`, creating it if it does not exist
    /// (`SingleFileStorageManager::LoadDatabase`).
    pub fn open(path: &str, buffer_manager: BufferManager, options: StorageOptions) -> Result<Self> {
        let block_options = BlockManagerOptions {
            read_only: options.read_only,
            block_alloc_size: options.block_alloc_size,
            storage_version: options.storage_version,
        };
        let exists = std::path::Path::new(path).exists();
        let created = !exists && !options.read_only;
        let block_manager = Arc::new(if created {
            SingleFileBlockManager::create_new(path, block_options)?
        } else {
            SingleFileBlockManager::load_existing(path, block_options)?
        });
        let metadata = MetadataManager::new(Arc::clone(&block_manager), buffer_manager.clone());
        if !created {
            block_manager.load_free_list(&metadata)?;
        }
        let dyn_manager: Arc<dyn BlockManager> = Arc::clone(&block_manager) as Arc<dyn BlockManager>;
        let io = Arc::new(TableIoManager::with_row_group_size(
            buffer_manager,
            dyn_manager,
            options.row_group_size,
        ));
        Ok(Self {
            path: path.to_owned(),
            block_manager,
            metadata,
            io,
            created,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn block_manager(&self) -> &Arc<SingleFileBlockManager> {
        &self.block_manager
    }

    pub fn metadata(&self) -> &MetadataManager {
        &self.metadata
    }

    /// Where tables of this database keep their data.
    pub fn io(&self) -> &Arc<TableIoManager> {
        &self.io
    }

    /// Whether the file did not exist before it was opened.
    pub fn is_new(&self) -> bool {
        self.created
    }

    pub fn is_read_only(&self) -> bool {
        self.block_manager.is_read_only()
    }

    /// The root of the catalog written by the last checkpoint (`GetMetaBlock`).
    pub fn meta_block(&self) -> MetaBlockPointer {
        MetaBlockPointer::new(self.block_manager.meta_block(), 0)
    }

    /// The serialization options of checkpoints and the WAL (`SerializationOptions(db)`).
    pub fn serialization_options(&self) -> SerializationOptions {
        SerializationOptions {
            serialize_enum_as_string: false,
            serialize_default_values: false,
            compatibility: SerializationCompatibility::from_index(self.block_manager.storage_version()),
        }
    }
}
