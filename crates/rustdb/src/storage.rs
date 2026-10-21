//! Opening, checkpointing and closing database files (DuckDB's `AttachedDatabase`,
//! `StorageManager::LoadDatabase` and `CreateCheckpoint`).

use std::sync::Arc;

use rustdb_catalog::checkpoint::{load_checkpoint, write_checkpoint};
use rustdb_catalog::{Catalog, Transaction};
use rustdb_common::profiling::MetricType;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::SerializationCompatibility;
use rustdb_storage::storage_manager::{StorageManager, StorageOptions};
use rustdb_types::Value;

use crate::Config;
use crate::attached::AttachOptions;

/// The catalog name of a database file (`AttachedDatabase::ExtractDatabaseName`): the
/// file name up to its first dot; reserved names get a `_db` suffix.
pub(crate) fn database_name(path: &str) -> String {
    if path.is_empty() || path == ":memory:" {
        return "memory".to_owned();
    }
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let file = file.split('?').next().unwrap_or(file);
    let name = file.split('.').next().unwrap_or(file).to_owned();
    if matches!(name.as_str(), "main" | "temp" | "system") {
        format!("{name}_db")
    } else {
        name
    }
}

fn option<'a>(config: &'a Config, name: &str) -> Option<&'a Value> {
    config
        .options
        .iter()
        .rev()
        .find(|(option, _)| option.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// The attach options of the database opened at start-up.
pub(crate) fn startup_options(config: &Config) -> AttachOptions {
    AttachOptions {
        access: config.access_mode,
        hidden: false,
        storage_version: option(config, "storage_compatibility_version").cloned(),
        block_size: option(config, "default_block_size").cloned(),
        row_group_size: None,
        kept: Vec::new(),
        default_table: None,
        no_wal_writes: false,
    }
}

fn unsigned(value: &Value, name: &str) -> Result<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.to_varchar().ok().and_then(|text| text.parse().ok()))
        .ok_or_else(|| Error::invalid_input(format!("{name} must be an integer")))
}

/// `Storage::VerifyBlockAllocSize`.
pub(crate) fn verify_block_alloc_size(size: u64) -> Result<()> {
    const MIN_BLOCK_ALLOC_SIZE: u64 = 16_384;
    const MAX_BLOCK_ALLOC_SIZE: u64 = 262_144;
    if !size.is_power_of_two() {
        return Err(Error::invalid_input(format!(
            "the block size must be a power of two, got {size}"
        )));
    }
    if size < MIN_BLOCK_ALLOC_SIZE {
        return Err(Error::invalid_input(format!(
            "the block size must be greater or equal than the minimum block size of {MIN_BLOCK_ALLOC_SIZE}, got {size}"
        )));
    }
    if size > MAX_BLOCK_ALLOC_SIZE {
        return Err(Error::invalid_input(format!(
            "the block size must be lesser or equal than the maximum block size of {MAX_BLOCK_ALLOC_SIZE}, got {size}"
        )));
    }
    Ok(())
}

/// The storage options of an attach.
fn storage_options(attach: &AttachOptions) -> Result<StorageOptions> {
    let mut options = StorageOptions {
        read_only: attach.read_only(),
        ..StorageOptions::default()
    };
    if let Some(version) = &attach.storage_version {
        let name = version.to_varchar()?;
        options.storage_version =
            Some(SerializationCompatibility::from_version_name(&name)?.serialization_version);
    }
    if let Some(size) = &attach.block_size {
        let size = unsigned(size, "block_size")?;
        verify_block_alloc_size(size)?;
        options.block_alloc_size =
            usize::try_from(size).map_err(|_| Error::invalid_input("block size too large"))?;
    }
    if let Some(size) = &attach.row_group_size {
        options.row_group_size = unsigned(size, "row_group_size")?;
    }
    Ok(options)
}

/// Opens the database file at `path` and loads its catalog into `catalog`.
pub(crate) fn open_storage(
    path: &str,
    attach: &AttachOptions,
    buffer_manager: rustdb_buffer::BufferManager,
    catalog: &Catalog,
) -> Result<Arc<StorageManager>> {
    let options = storage_options(attach)?;
    let requested_block_size = options.block_alloc_size;
    let requested_block_size_set = attach.block_size.is_some();
    let storage = StorageManager::open(path, buffer_manager, options)?;
    // `SingleFileStorageManager::LoadDatabase`: an existing file keeps its block size.
    let file_block_size = rustdb_buffer::BlockManager::block_alloc_size(storage.block_manager().as_ref());
    if requested_block_size_set && !storage.is_new() && requested_block_size != file_block_size {
        return Err(Error::invalid_input(format!(
            "block size parameter does not match the file's block size, got {requested_block_size}, expected \
             {file_block_size}"
        )));
    }
    let row_group_size = storage.io().row_group_size();
    if row_group_size > 122_880 && storage.block_manager().storage_version() < 4 {
        return Err(Error::invalid_input(format!(
            "Unsupported row group size {row_group_size} - row group sizes >= 122_880 are only supported with \
             STORAGE_VERSION '1.2.0' or above.\nExplicitly specify a newer storage version when creating the \
             database to enable larger row groups"
        )));
    }
    // `SingleFileStorageManager::LoadDatabase` times loading the checkpoint.
    let timer = rustdb_common::profiling::start_timer(MetricType::AttachLoadStorageLatency);
    load_checkpoint(catalog, &storage)?;
    drop(timer);
    Ok(Arc::new(storage))
}

/// `CHECKPOINT` / `FORCE CHECKPOINT` of `database` from a connection whose transaction
/// is `current`; `started` tells whether that transaction was started for the database
/// (`DuckTransactionManager::Checkpoint`).
pub(crate) fn checkpoint(
    database: &crate::attached::AttachedDatabase,
    current: &Transaction,
    started: bool,
    force: bool,
    transactions: &rustdb_catalog::TransactionManager,
) -> Result<()> {
    database.check_valid()?;
    if database.storage.is_none() {
        if !current.changes_made() {
            checkpoint_in_memory(&database.catalog, transactions)?;
        }
        return Ok(());
    }
    if started && force {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot FORCE CHECKPOINT: the current transaction has been started for this database",
        ));
    }
    if current.changes_made() {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot CHECKPOINT: the current transaction has transaction local changes",
        ));
    }
    // A committed transaction with updates or catalog changes that older transactions
    // still need holds the checkpoint lock (`recently_committed_transactions`).
    if !force && transactions.retains_changes(current.transaction_id) {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot CHECKPOINT: there are other write transactions active. Try using FORCE CHECKPOINT to wait \
             until all active transactions are finished",
        ));
    }
    let mode = if force {
        CheckpointMode::Force
    } else {
        CheckpointMode::Manual
    };
    database.create_checkpoint(mode, Some(transactions))
}

/// Checkpoints an in-memory database (DuckDB's `InMemoryCheckpointer::CreateCheckpoint`):
/// every table compresses its version information and, when no other transaction runs
/// that may still need older data, vacuums its deleted rows.
fn checkpoint_in_memory(catalog: &Catalog, transactions: &rustdb_catalog::TransactionManager) -> Result<()> {
    let lowest_active_start = transactions.lowest_active_start();
    let settings = rustdb_storage::checkpoint::table_writer::TableWriteSettings {
        full_checkpoint: transactions.active_count() <= 1 && transactions.last_commit() < lowest_active_start,
        lowest_active_start,
        ..Default::default()
    };
    let system = Transaction::system();
    for schema in catalog.schemas().scan(&system) {
        let Some(schema_entry) = schema.as_schema() else {
            continue;
        };
        for entry in schema_entry.tables.scan(&system) {
            if let rustdb_catalog::EntryKind::Table(table) = &entry.kind
                && let Some(storage) = &table.storage
            {
                rustdb_storage::checkpoint::table_writer::checkpoint_in_memory_table(storage, settings)?;
            }
        }
    }
    Ok(())
}

/// Why a checkpoint is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckpointMode {
    /// `CHECKPOINT`: fails while other transactions write to the database.
    Manual,
    /// `FORCE CHECKPOINT`.
    Force,
    /// After a commit or when the database closes.
    Automatic,
}

/// Writes a checkpoint if the WAL has entries (`SingleFileStorageManager::CreateCheckpoint`).
/// The checkpoint lock is taken first (no transaction writes to the database while it is
/// held), then the WAL lock, so commits wait; the WAL records the checkpoint's root
/// before anything is written and is removed once the new header is in place. When no
/// transaction writes to the database or needs data older than the last commit
/// (`transactions`), the in-memory table data is replaced by the checkpointed data.
pub(crate) fn create_checkpoint(
    catalog: &Catalog,
    storage: &StorageManager,
    mode: CheckpointMode,
    transactions: Option<&rustdb_catalog::TransactionManager>,
    failed: &dyn Fn(Error) -> Error,
) -> Result<()> {
    if storage.is_read_only() {
        return Ok(());
    }
    // `FORCE CHECKPOINT` waits until the write transactions finished: a checkpoint
    // written beside them would store rows they appended but had not committed (as
    // deleted rows), so the row ids of their WAL entries would no longer match.
    let exclusive = storage.wal().and_then(|wal| {
        if mode == CheckpointMode::Force {
            Some(wal.checkpoint_lock().exclusive())
        } else {
            wal.checkpoint_lock().try_exclusive()
        }
    });
    if mode == CheckpointMode::Manual && storage.wal().is_some() && exclusive.is_none() {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot CHECKPOINT: there are other write transactions active. Try using FORCE CHECKPOINT to wait \
             until all active transactions are finished",
        ));
    }
    let replace_in_memory = exclusive.is_some()
        && transactions
            .is_none_or(|transactions| transactions.last_commit() < transactions.lowest_active_start());
    // A full checkpoint only vacuums when no transaction inserting or deleting rows runs
    // (`TryGetVacuumLock`; otherwise it is a concurrent one).
    let vacuum_lock = if replace_in_memory {
        storage.wal().and_then(|wal| wal.vacuum_lock().try_exclusive())
    } else {
        None
    };
    let mut guard = storage.wal().map(|wal| wal.lock());
    let wal_size = guard.as_ref().map_or(0, rustdb_storage::wal::WalGuard::size);
    if wal_size == 0 {
        return Ok(());
    }
    let mut has_wal = false;
    // `SingleFileStorageManager::CreateCheckpoint` times writing the checkpoint.
    let mut timer = rustdb_common::profiling::start_timer(MetricType::CheckpointLatency);
    write_checkpoint(
        catalog,
        storage,
        &Transaction::system(),
        &mut |meta_block| {
            if let Some(guard) = guard.as_mut() {
                has_wal = guard.start_checkpoint(meta_block)?;
            }
            Ok(())
        },
        replace_in_memory,
        vacuum_lock.is_some(),
        transactions.map_or(u64::MAX, rustdb_catalog::TransactionManager::lowest_active_start),
    )
    .map_err(|error| {
        let error = failed(error);
        if let Some(guard) = guard.as_mut() {
            guard.invalidate(error.raw_message().to_owned());
        }
        error
    })?;
    timer.end();
    drop(exclusive);
    if has_wal && let Some(guard) = guard.as_mut() {
        guard.finish_checkpoint().map_err(|error| {
            let error = failed(error);
            guard.invalidate(error.raw_message().to_owned());
            error
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_names_follow_duckdb() {
        assert_eq!(database_name(""), "memory");
        assert_eq!(database_name("/tmp/dir/test.db"), "test");
        assert_eq!(database_name("x.duckdb.wal"), "x");
        assert_eq!(database_name("main.db"), "main_db");
    }
}
