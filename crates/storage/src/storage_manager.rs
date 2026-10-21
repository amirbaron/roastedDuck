//! The storage of an attached database (DuckDB's `SingleFileStorageManager`): of a file,
//! the block manager, the metadata manager and the I/O manager of its tables; of an
//! in-memory database, the I/O manager and the estimated size of its changes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rustdb_buffer::{BlockManager, BufferManager, InMemoryBlockManager};
use rustdb_common::Result;
use rustdb_serde::{SerializationCompatibility, SerializationOptions};

use crate::compression::CompressionConfig;
use crate::data_table::TableIoManager;
use crate::metadata::{MetaBlockPointer, MetadataManager};
use crate::single_file_block_manager::{BlockManagerOptions, SingleFileBlockManager};
use crate::storage_info::DEFAULT_BLOCK_ALLOC_SIZE;
use crate::wal::{WriteAheadLog, wal_path};

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

/// Where a checkpoint is aborted for testing (DuckDB's `CheckpointAbort`, the
/// `debug_checkpoint_abort` setting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CheckpointAbort {
    #[default]
    NoAbort,
    BeforeTruncate,
    BeforeHeader,
    AfterFreeListWrite,
    BeforeWalFinish,
    BeforeMovingRecovery,
    BeforeDeletingCheckpointWal,
    BeforeHeaderNonFatal,
    InMemoryCheckpoint,
}

impl CheckpointAbort {
    /// Every value with its `EnumUtil` name.
    pub const ALL: [(Self, &'static str); 9] = [
        (Self::NoAbort, "NONE"),
        (Self::BeforeTruncate, "BEFORE_TRUNCATE"),
        (Self::BeforeHeader, "BEFORE_HEADER"),
        (Self::AfterFreeListWrite, "AFTER_FREE_LIST_WRITE"),
        (Self::BeforeWalFinish, "BEFORE_WAL_FINISH"),
        (Self::BeforeMovingRecovery, "BEFORE_MOVING_RECOVERY"),
        (
            Self::BeforeDeletingCheckpointWal,
            "BEFORE_DELETING_CHECKPOINT_WAL",
        ),
        (Self::BeforeHeaderNonFatal, "BEFORE_HEADER_NON_FATAL"),
        (Self::InMemoryCheckpoint, "IN_MEMORY_CHECKPOINT"),
    ];

    /// `EnumUtil::FromString<CheckpointAbort>` (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, candidate)| candidate.eq_ignore_ascii_case(name))
            .map(|(value, _)| *value)
    }

    /// The error a checkpoint aborted at `self` fails with.
    pub fn error(self) -> rustdb_common::Error {
        use rustdb_common::{Error, ErrorKind};
        let fatal = |message: &str| Error::new(ErrorKind::Fatal, message.to_owned());
        match self {
            Self::BeforeHeader => {
                fatal("Checkpoint aborted before header write because of PRAGMA checkpoint_abort flag")
            }
            Self::BeforeHeaderNonFatal => Error::new(
                ErrorKind::Io,
                "Checkpoint aborted before header write (non-fatal) because of PRAGMA checkpoint_abort flag",
            ),
            Self::AfterFreeListWrite => {
                fatal("Checkpoint aborted after free list write because of PRAGMA checkpoint_abort flag")
            }
            // DuckDB reports the abort before the WAL finish with the truncate message.
            Self::BeforeTruncate | Self::BeforeWalFinish => {
                fatal("Checkpoint aborted before truncate because of PRAGMA checkpoint_abort flag")
            }
            Self::BeforeMovingRecovery => fatal(
                "Checkpoint aborted before moving recovery file because of PRAGMA checkpoint_abort flag",
            ),
            Self::BeforeDeletingCheckpointWal => fatal(
                "Checkpoint aborted before deleting checkpoint file because of PRAGMA checkpoint_abort flag",
            ),
            Self::InMemoryCheckpoint => Error::new(
                ErrorKind::Io,
                "In-memory checkpoint aborted because of PRAGMA debug_checkpoint_abort flag",
            ),
            Self::NoAbort => Error::internal("no checkpoint abort"),
        }
    }

    /// Fails with [`CheckpointAbort::error`] when the checkpoint aborts at `point`.
    pub fn check(self, point: Self) -> rustdb_common::Result<()> {
        if self == point { Err(self.error()) } else { Ok(()) }
    }
}

/// The settings checkpoints of a database follow (DuckDB's `experimental_metadata_reuse`
/// and `force_column_metadata_reuse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointConfig {
    /// Unchanged row groups and tables keep their metadata.
    pub metadata_reuse: bool,
    /// Unchanged columns of changed row groups keep their metadata (storage version
    /// v1.4.0 or later).
    pub force_column_metadata_reuse: bool,
    /// `max_vacuum_tasks`.
    pub max_vacuum_tasks: u64,
    /// `debug_checkpoint_abort`.
    pub abort: CheckpointAbort,
    /// `geometry_minimum_shredding_size`: the rows a row group needs before its GEOMETRY
    /// columns are shredded; negative disables shredding.
    pub geometry_minimum_shredding_size: i64,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            metadata_reuse: true,
            force_column_metadata_reuse: false,
            max_vacuum_tasks: 100,
            abort: CheckpointAbort::NoAbort,
            geometry_minimum_shredding_size: 30_000,
        }
    }
}

/// The storage of one database file.
#[derive(Debug)]
pub struct StorageManager {
    path: String,
    block_manager: Arc<SingleFileBlockManager>,
    metadata: Arc<MetadataManager>,
    io: Arc<TableIoManager>,
    /// Whether the file was created by this open.
    created: bool,
    /// The WAL, set up once the existing one was replayed.
    wal: OnceLock<Arc<WriteAheadLog>>,
    checkpoint_config: Mutex<CheckpointConfig>,
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
        if created {
            // A WAL without its database file belongs to nothing.
            match std::fs::remove_file(wal_path(path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(rustdb_common::Error::new(
                        rustdb_common::ErrorKind::Io,
                        format!("Could not remove file \"{}\": {error}", wal_path(path)),
                    ));
                }
            }
        }
        let block_manager = Arc::new(if created {
            SingleFileBlockManager::create_new(path, block_options)?
        } else {
            SingleFileBlockManager::load_existing(path, block_options)?
        });
        let metadata = Arc::new(MetadataManager::new(
            Arc::clone(&block_manager),
            buffer_manager.clone(),
        ));
        if !created {
            block_manager.load_free_list(&metadata)?;
        }
        let io = Arc::new(TableIoManager::for_file(
            buffer_manager,
            Arc::clone(&block_manager),
            options.row_group_size,
        ));
        Ok(Self {
            path: path.to_owned(),
            block_manager,
            metadata,
            io,
            created,
            wal: OnceLock::new(),
            checkpoint_config: Mutex::default(),
        })
    }

    /// The path of the WAL (`GetWALPath`).
    pub fn wal_path(&self) -> String {
        wal_path(&self.path)
    }

    /// Sets up the WAL after replay: `size` bytes of it are kept; with
    /// `requires_truncate` anything after them is cut off before the next write.
    pub fn init_wal(&self, size: u64, requires_truncate: bool) -> Result<Arc<WriteAheadLog>> {
        let wal = Arc::new(WriteAheadLog::new(
            self.wal_path(),
            Arc::clone(&self.block_manager),
            self.serialization_options(),
            size,
            requires_truncate,
        ));
        self.wal
            .set(Arc::clone(&wal))
            .map_err(|_| rustdb_common::Error::internal("the WAL is set up once"))?;
        Ok(wal)
    }

    /// The WAL, once set up.
    pub fn wal(&self) -> Option<&Arc<WriteAheadLog>> {
        self.wal.get()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn block_manager(&self) -> &Arc<SingleFileBlockManager> {
        &self.block_manager
    }

    /// Sets how checkpoints of this database compress segments (`force_compression`).
    pub fn set_compression_config(&self, config: crate::compression::CompressionConfig) {
        self.block_manager.set_compression_config(config);
    }

    /// The settings checkpoints follow.
    pub fn checkpoint_config(&self) -> CheckpointConfig {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the settings checkpoints follow.
    pub fn set_checkpoint_config(&self, config: CheckpointConfig) {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = config;
    }

    /// How a checkpoint writes the data of tables: `full_checkpoint` when no transaction
    /// needs older data (`CheckpointType::FULL_CHECKPOINT`), with the oldest active
    /// transaction's start time.
    pub fn table_write_settings(
        &self,
        full_checkpoint: bool,
        lowest_active_start: u64,
    ) -> crate::checkpoint::table_writer::TableWriteSettings {
        let config = self.checkpoint_config();
        crate::checkpoint::table_writer::TableWriteSettings {
            metadata_reuse: config.metadata_reuse,
            force_column_metadata_reuse: config.force_column_metadata_reuse,
            storage_version: self.block_manager.storage_version(),
            full_checkpoint,
            max_vacuum_tasks: config.max_vacuum_tasks,
            lowest_active_start,
            geometry_minimum_shredding_size: config.geometry_minimum_shredding_size,
        }
    }

    pub fn metadata(&self) -> &Arc<MetadataManager> {
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

/// The storage of an in-memory database (DuckDB's `SingleFileStorageManager` of the
/// `:memory:` path): an `InMemoryBlockManager` with the latest storage version and no
/// WAL. Commits add an estimate of their changes to the WAL size instead, which decides
/// when a commit checkpoints (vacuums and, with `COMPRESS`, compresses) the tables
/// (`InMemoryCheckpointer`).
#[derive(Debug)]
pub struct InMemoryStorageManager {
    io: Arc<TableIoManager>,
    /// `ATTACH ... (COMPRESS)` (`CompressInMemory::COMPRESS`).
    compress: bool,
    /// The estimated size of the changes committed since the last checkpoint.
    wal_size: AtomicU64,
    checkpoint_config: Mutex<CheckpointConfig>,
    compression_config: Mutex<CompressionConfig>,
}

impl InMemoryStorageManager {
    /// `SingleFileStorageManager::LoadDatabase` of an in-memory database.
    pub fn new(buffer_manager: BufferManager, compress: bool) -> Self {
        let block_manager: Arc<dyn BlockManager> = Arc::new(InMemoryBlockManager::default());
        Self {
            io: Arc::new(TableIoManager::new(buffer_manager, block_manager)),
            compress,
            wal_size: AtomicU64::new(0),
            checkpoint_config: Mutex::default(),
            compression_config: Mutex::default(),
        }
    }

    /// Where tables of this database keep their data.
    pub fn io(&self) -> &Arc<TableIoManager> {
        &self.io
    }

    /// Whether checkpoints compress the table data (`CompressionIsEnabled`).
    pub fn compression_is_enabled(&self) -> bool {
        self.compress
    }

    /// The estimated size of the changes committed since the last checkpoint
    /// (`GetWALSize`).
    pub fn wal_size(&self) -> u64 {
        self.wal_size.load(Ordering::Acquire)
    }

    /// Adds the estimated size of a commit (`AddWALSize`).
    pub fn add_wal_size(&self, size: u64) {
        self.wal_size.fetch_add(size, Ordering::AcqRel);
    }

    /// `SetWALSize`.
    pub fn set_wal_size(&self, size: u64) {
        self.wal_size.store(size, Ordering::Release);
    }

    /// The settings checkpoints follow.
    pub fn checkpoint_config(&self) -> CheckpointConfig {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the settings checkpoints follow.
    pub fn set_checkpoint_config(&self, config: CheckpointConfig) {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = config;
    }

    /// The compression settings checkpoints follow.
    pub fn compression_config(&self) -> CompressionConfig {
        *self
            .compression_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets how checkpoints compress segments (`force_compression`).
    pub fn set_compression_config(&self, config: CompressionConfig) {
        *self
            .compression_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = config;
    }

    /// How a checkpoint writes the data of tables: `full_checkpoint` when no transaction
    /// needs older data (`CheckpointType::FULL_CHECKPOINT`), with the oldest active
    /// transaction's start time.
    pub fn table_write_settings(
        &self,
        full_checkpoint: bool,
        lowest_active_start: u64,
    ) -> crate::checkpoint::table_writer::TableWriteSettings {
        let config = self.checkpoint_config();
        crate::checkpoint::table_writer::TableWriteSettings {
            metadata_reuse: false,
            force_column_metadata_reuse: false,
            storage_version: self.io.storage_version(),
            full_checkpoint,
            max_vacuum_tasks: config.max_vacuum_tasks,
            lowest_active_start,
            geometry_minimum_shredding_size: config.geometry_minimum_shredding_size,
        }
    }
}
