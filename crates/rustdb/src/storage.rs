//! Opening, checkpointing and closing database files (DuckDB's `AttachedDatabase`,
//! `StorageManager::LoadDatabase` and `CreateCheckpoint`).

use std::sync::Arc;

use rustdb_ast::CreateIndexInfo;
use rustdb_catalog::checkpoint::{IndexExpressionLoader, load_checkpoint, write_checkpoint};
use rustdb_catalog::{Catalog, Transaction};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::SerializationCompatibility;
use rustdb_storage::DataTable;
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
fn verify_block_alloc_size(size: u64) -> Result<()> {
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

/// Builds expression indexes of a loaded database (needs the binder; not supported yet).
struct NoExpressionIndexes;

impl IndexExpressionLoader for NoExpressionIndexes {
    fn load_expression_index(&self, _table: &Arc<DataTable>, info: &CreateIndexInfo) -> Result<()> {
        Err(Error::not_implemented(format!(
            "loading the expression index \"{}\" from a database file",
            info.name
        )))
    }
}

/// Opens the database file at `path` and loads its catalog into `catalog`.
pub(crate) fn open_storage(
    path: &str,
    attach: &AttachOptions,
    buffer_manager: rustdb_buffer::BufferManager,
    catalog: &Catalog,
) -> Result<Arc<StorageManager>> {
    let options = storage_options(attach)?;
    let storage = StorageManager::open(path, buffer_manager, options)?;
    load_checkpoint(catalog, &storage, &NoExpressionIndexes)?;
    Ok(Arc::new(storage))
}

/// `CHECKPOINT` / `FORCE CHECKPOINT` from a connection whose transaction is `current`
/// (`DuckTransactionManager::Checkpoint`).
pub(crate) fn checkpoint(
    catalog: &Catalog,
    storage: Option<&Arc<StorageManager>>,
    current: &Transaction,
    _force: bool,
) -> Result<()> {
    let Some(storage) = storage else {
        return Ok(());
    };
    if current.changes_made() {
        return Err(Error::new(
            ErrorKind::Transaction,
            "Cannot CHECKPOINT: the current transaction has transaction local changes",
        ));
    }
    create_checkpoint(catalog, storage, false)
}

/// The checkpoint written when a database is closed (`AttachedDatabase::Close`).
pub(crate) fn checkpoint_on_close(catalog: &Catalog, storage: &StorageManager) -> Result<()> {
    create_checkpoint(catalog, storage, false)
}

/// Writes a checkpoint if the WAL has entries, or `always`
/// (`SingleFileStorageManager::CreateCheckpoint`). The WAL lock is held throughout, so
/// commits wait; the WAL records the checkpoint's root before anything is written and is
/// removed once the new header is in place.
pub(crate) fn create_checkpoint(catalog: &Catalog, storage: &StorageManager, always: bool) -> Result<()> {
    if storage.is_read_only() {
        return Ok(());
    }
    let mut guard = storage.wal().map(|wal| wal.lock());
    let wal_size = guard.as_ref().map_or(0, rustdb_storage::wal::WalGuard::size);
    if wal_size == 0 && !always {
        return Ok(());
    }
    let mut has_wal = false;
    write_checkpoint(catalog, storage, &Transaction::system(), &mut |meta_block| {
        if let Some(guard) = guard.as_mut() {
            has_wal = guard.start_checkpoint(meta_block)?;
        }
        Ok(())
    })?;
    if has_wal && let Some(guard) = guard.as_mut() {
        guard.finish_checkpoint()?;
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
