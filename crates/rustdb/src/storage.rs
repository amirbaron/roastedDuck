//! Opening, checkpointing and closing databases (DuckDB's `AttachedDatabase`,
//! `StorageManager::LoadDatabase` and `CreateCheckpoint`).

use std::sync::Arc;

use rustdb_catalog::checkpoint::{checkpoint_in_memory, load_checkpoint, write_checkpoint};
use rustdb_catalog::{Catalog, Transaction};
use rustdb_common::profiling::MetricType;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::SerializationCompatibility;
use rustdb_storage::storage_manager::{InMemoryStorageManager, StorageManager, StorageOptions};
use rustdb_types::Value;

use crate::Config;
use crate::attached::AttachOptions;

/// The catalog name of a database file (`AttachedDatabase::ExtractDatabaseName`): the
/// file name up to its first dot; reserved names get a `_db` suffix.
pub(crate) fn database_name(path: impl AsRef<[u8]>) -> rustdb_common::ByteString {
    let path = path.as_ref();
    if path.is_empty() || path == b":memory:" {
        return "memory".into();
    }
    let file = path
        .rsplit(|byte| matches!(byte, b'/' | b'\\'))
        .next()
        .unwrap_or(path);
    let file = file.split(|byte| *byte == b'?').next().unwrap_or(file);
    let name = file.split(|byte| *byte == b'.').next().unwrap_or(file);
    if [b"main".as_slice(), b"temp", b"system"].contains(&name) {
        rustdb_common::byte_format!("{name}_db")
    } else {
        name.into()
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
        db_type: None,
        compress: None,
        vacuum_rebuild_indexes: None,
        encryption_key: None,
        encryption_cipher: rustdb_crypto::CipherType::Invalid,
        encryption_version: rustdb_crypto::EncryptionVersion::None,
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
        let size = unsigned(size, "row_group_size")?;
        if size == 0 {
            return Err(Error::not_implemented(format!(
                "Invalid row group size: {size} - row group size must be bigger than 0"
            )));
        }
        let vector_size = rustdb_common::STANDARD_VECTOR_SIZE as u64;
        if size % vector_size != 0 {
            return Err(Error::not_implemented(format!(
                "Invalid row group size: {size} - row group size must be divisible by the vector size ({vector_size})"
            )));
        }
        options.row_group_size = size;
    }
    if let Some(key) = &attach.encryption_key {
        options.encryption = Some(rustdb_storage::encryption::EncryptionOptions {
            user_key: key.clone(),
            cipher: attach.encryption_cipher,
            version: attach.encryption_version,
        });
        let given = attach
            .storage_version
            .as_ref()
            .map(Value::to_varchar)
            .transpose()?;
        options.set_encryption_version(given.as_deref())?;
    }
    Ok(options)
}

/// Opens the database file at `path` and loads its catalog into `catalog`.
pub(crate) fn open_storage(
    path: &[u8],
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
    if !force && transactions.retains_changes(current.transaction_id, database.name()) {
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
/// The checkpoint lock is taken first (no transaction updates rows or changes the catalog
/// while it is held, and no other checkpoint runs). Under the WAL lock, the WAL records
/// the checkpoint's root and commits switch to the checkpoint WAL; commits then continue
/// while the data is written (see [`write_checkpoint`]). Once the new header is in place,
/// the checkpoint WAL, if commits wrote to it, replaces the WAL; otherwise the WAL is
/// removed. When no transaction needs data older than the last commit (`transactions`),
/// the in-memory table data is replaced by the checkpointed data.
///
/// A failure invalidates the database (`failed` returns the error reported); commits
/// are refused from then on with the message `refused` returns.
///
/// With `commit`, the checkpoint runs for a commit that did not log its changes
/// (`CheckpointAction::ALWAYS_CHECKPOINT` with `CheckpointOptions::wal_lock`): it is
/// written even if the WAL is empty, under the checkpoint lock and the WAL lock the
/// commit holds.
pub(crate) fn create_checkpoint(
    catalog: &Catalog,
    storage: &StorageManager,
    mode: CheckpointMode,
    transactions: Option<&rustdb_catalog::TransactionManager>,
    failed: &dyn Fn(Error) -> Error,
    refused: &dyn Fn() -> rustdb_common::ByteString,
    commit: Option<CommitLocks<'_, '_>>,
) -> Result<()> {
    if storage.is_read_only() {
        return Ok(());
    }
    let Some(wal) = storage.wal() else {
        return Ok(());
    };
    let always = commit.is_some();
    // A shutdown/automatic checkpoint has no connection to bind expressions. Keep
    // the original checkpoint plus its WAL until binding can apply all key changes.
    // A commit that skips WAL writes is excluded by `skip_wal_checkpoint` instead.
    if !always
        && mode == CheckpointMode::Automatic
        && rustdb_catalog::checkpoint::has_pending_index_replays(catalog)
    {
        return Ok(());
    }
    let locks = catalog.locks();
    let (exclusive, mut held_wal) = if let Some(CommitLocks { checkpoint, wal }) = commit {
        (Some(checkpoint), Some(wal))
    } else {
        // `FORCE CHECKPOINT` waits until the write transactions finished: a checkpoint
        // written beside them would store rows they appended but had not committed (as
        // deleted rows), so the row ids of their WAL entries would no longer match.
        let exclusive = if mode == CheckpointMode::Force {
            Some(locks.checkpoint.exclusive())
        } else {
            locks.checkpoint.try_exclusive()
        };
        (exclusive, None)
    };
    // Without the lock, another checkpoint runs, or a transaction updates rows or changes
    // the catalog: there is no checkpoint (`CanCheckpoint`), except when the database
    // closes (without `transactions`).
    if exclusive.is_none() && transactions.is_some() {
        if mode == CheckpointMode::Manual {
            return Err(Error::new(
                ErrorKind::Transaction,
                "Cannot CHECKPOINT: there are other write transactions active. Try using FORCE CHECKPOINT to \
                 wait until all active transactions are finished",
            ));
        }
        return Ok(());
    }
    // A database that closes waits for a checkpoint that runs.
    let _running = storage.checkpoint_running();
    let replace_in_memory = exclusive.is_some()
        && transactions
            .is_none_or(|transactions| transactions.last_commit() < transactions.lowest_active_start());
    // A full checkpoint only vacuums when no transaction inserting or deleting rows runs
    // (`TryGetVacuumLock`; otherwise it is a concurrent one), and no committed
    // transaction is being cleaned up (see `vacuum_allowed`).
    let vacuum_lock = if replace_in_memory && vacuum_allowed(transactions) {
        locks.vacuum.try_exclusive()
    } else {
        None
    };
    if !always && with_wal(wal, &mut held_wal, |guard| guard.size()) == 0 {
        return Ok(());
    }
    let mut has_wal = false;
    // `SingleFileStorageManager::CreateCheckpoint` times writing the checkpoint.
    let mut timer = rustdb_common::profiling::start_timer(MetricType::CheckpointLatency);
    let written = write_checkpoint(
        catalog,
        storage,
        &Transaction::system(),
        &mut |meta_block, hold_off_commits| {
            // `WALStartCheckpoint`: the checkpoint includes the commits before commits
            // switch to the checkpoint WAL.
            with_wal(wal, &mut held_wal, |guard| {
                let checkpoint_id =
                    transactions.map_or(u64::MAX, rustdb_catalog::TransactionManager::last_commit);
                storage.io().set_active_checkpoint(Some(checkpoint_id));
                hold_off_commits();
                has_wal = guard.start_checkpoint(meta_block)?;
                Ok(checkpoint_id)
            })
        },
        replace_in_memory,
        vacuum_lock.is_some(),
        transactions.map_or(u64::MAX, rustdb_catalog::TransactionManager::lowest_active_start),
    );
    drop(vacuum_lock);
    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "the method is not general over the guard's lifetime"
    )]
    let finished = written.and_then(|()| {
        if has_wal {
            with_wal(wal, &mut held_wal, |guard| guard.finish_checkpoint())
        } else {
            Ok(())
        }
    });
    storage.io().set_active_checkpoint(None);
    drop(exclusive);
    if let Err(error) = finished {
        let error = failed(error);
        with_wal(wal, &mut held_wal, |guard| guard.invalidate(refused()));
        return Err(error);
    }
    timer.end();
    Ok(())
}

/// The locks a commit that checkpoints instead of logging its changes holds through the
/// checkpoint: the database's checkpoint lock, exclusively, and the WAL lock.
pub(crate) struct CommitLocks<'a, 'w> {
    pub checkpoint: rustdb_storage::checkpoint_lock::ExclusiveCheckpointGuard,
    pub wal: &'a mut rustdb_storage::wal::WalGuard<'w>,
}

/// Calls `f` with the WAL lock: `held` if the caller holds it, else taken for the call.
fn with_wal<R>(
    wal: &rustdb_storage::wal::WriteAheadLog,
    held: &mut Option<&mut rustdb_storage::wal::WalGuard<'_>>,
    f: impl FnOnce(&mut rustdb_storage::wal::WalGuard<'_>) -> R,
) -> R {
    match held {
        Some(guard) => f(guard),
        None => f(&mut wal.lock()),
    }
}

/// Whether a checkpoint may vacuum: in DuckDB a committed transaction keeps its locks
/// until its versions are cleaned up, so no vacuum sees their update chains.
fn vacuum_allowed(transactions: Option<&rustdb_catalog::TransactionManager>) -> bool {
    transactions.is_none_or(|transactions| !transactions.cleanup_in_progress())
}

/// Checkpoints an in-memory database if changes were committed since the last checkpoint
/// (`SingleFileStorageManager::CreateCheckpoint` with an `InMemoryCheckpointer`): its
/// tables are vacuumed and compressed, or only vacuumed by an automatic checkpoint of a
/// database without `COMPRESS` (`CheckpointType::VACUUM_ONLY`). An in-memory checkpoint
/// replaces the table data, so it only runs while no transaction writes to the database
/// (it holds the checkpoint and vacuum locks); while a transaction needs data older than
/// the last commit (`transactions`) it does not vacuum (`CONCURRENT_CHECKPOINT`).
pub(crate) fn create_in_memory_checkpoint(
    catalog: &Catalog,
    storage: &InMemoryStorageManager,
    mode: CheckpointMode,
    transactions: Option<&rustdb_catalog::TransactionManager>,
    failed: &dyn Fn(Error) -> Error,
) -> Result<()> {
    let locks = catalog.locks();
    let exclusive = if mode == CheckpointMode::Force {
        Some(locks.checkpoint.exclusive())
    } else {
        locks.checkpoint.try_exclusive()
    };
    if mode == CheckpointMode::Manual && exclusive.is_none() {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot CHECKPOINT: there are other write transactions active. Try using FORCE CHECKPOINT to wait \
             until all active transactions are finished",
        ));
    }
    // `TryGetVacuumLock`: no transaction inserts or deletes rows.
    let vacuum_lock = exclusive.as_ref().and_then(|_| locks.vacuum.try_exclusive());
    if vacuum_lock.is_none() || storage.wal_size() == 0 {
        return Ok(());
    }
    let full_checkpoint = vacuum_allowed(transactions)
        && transactions
            .is_none_or(|transactions| transactions.last_commit() < transactions.lowest_active_start());
    // Committed updates older transactions must not see hold the checkpoint lock in
    // DuckDB (`recently_committed_transactions`): compressing would fold them in.
    if !full_checkpoint
        && transactions.is_some_and(|transactions| transactions.retains_changes(u64::MAX, catalog.name()))
    {
        return Ok(());
    }
    let vacuum_only = mode == CheckpointMode::Automatic && !storage.compression_is_enabled();
    // `SingleFileStorageManager::CreateCheckpoint` times writing the checkpoint.
    let mut timer = rustdb_common::profiling::start_timer(MetricType::CheckpointLatency);
    checkpoint_in_memory(
        catalog,
        storage,
        full_checkpoint,
        vacuum_only,
        transactions.map_or(u64::MAX, rustdb_catalog::TransactionManager::lowest_active_start),
    )
    .map_err(failed)?;
    timer.end();
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
