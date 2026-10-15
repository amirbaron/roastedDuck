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

use crate::{AccessMode, Config};

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

/// The storage options a configuration asks for.
fn storage_options(config: &Config) -> Result<StorageOptions> {
    let mut options = StorageOptions {
        read_only: config.access_mode == AccessMode::ReadOnly,
        ..StorageOptions::default()
    };
    if let Some(version) = option(config, "storage_compatibility_version") {
        let name = version.to_varchar()?;
        options.storage_version =
            Some(SerializationCompatibility::from_version_name(&name)?.serialization_version);
    }
    if let Some(size) = option(config, "default_block_size") {
        let size = size
            .as_u64()
            .or_else(|| size.as_i64().and_then(|v| u64::try_from(v).ok()))
            .ok_or_else(|| Error::invalid_input("default_block_size must be an integer"))?;
        options.block_alloc_size =
            usize::try_from(size).map_err(|_| Error::invalid_input("block size too large"))?;
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
    config: &Config,
    buffer_manager: rustdb_buffer::BufferManager,
    catalog: &Catalog,
) -> Result<Arc<StorageManager>> {
    let options = storage_options(config)?;
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
    write_checkpoint(catalog, storage, &Transaction::system())
}

/// The checkpoint written when a database is closed (`AttachedDatabase::Close`).
pub(crate) fn checkpoint_on_close(catalog: &Catalog, storage: &StorageManager) -> Result<()> {
    write_checkpoint(catalog, storage, &Transaction::system())
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
