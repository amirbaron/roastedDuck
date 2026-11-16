//! Internal adapters for engine-owned catalogs and tables with non-native storage.
//! These Rust traits are not an extension loading ABI or a public plugin protocol.
use std::sync::Arc;

use roastedduck_common::{ByteString, Result};
use roastedduck_function::{FunctionClient, FunctionData, TableFunction};

use crate::{CatalogEntry, CatalogType, Transaction};

/// Metadata lookup may perform I/O and must preserve provider errors. In particular,
/// an authentication or connection failure is not a missing catalog entry.
#[doc(hidden)]
pub trait CatalogBackend: std::fmt::Debug + Send + Sync {
    fn catalog_type(&self) -> &str;
    fn default_schema(&self) -> &[u8];
    /// Enumerate provider metadata without populating native catalog sets.
    fn scan(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _kind: CatalogType,
        _schema: &[u8],
    ) -> Result<Vec<Arc<CatalogEntry>>> {
        Ok(Vec::new())
    }
    fn lookup(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &[u8],
        name: &[u8],
    ) -> Result<Option<Arc<CatalogEntry>>>;
    fn create(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _info: &roastedduck_ast::CreateInfo,
    ) -> Result<Option<Arc<CatalogEntry>>> {
        Err(roastedduck_common::Error::not_implemented(
            "CREATE is not implemented for this catalog",
        ))
    }
    fn drop_entry(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _info: &roastedduck_ast::DropInfo,
    ) -> Result<()> {
        Err(roastedduck_common::Error::not_implemented(
            "DROP is not implemented for this catalog",
        ))
    }
    fn alter(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _info: &roastedduck_ast::AlterInfo,
    ) -> Result<()> {
        Err(roastedduck_common::Error::not_implemented(
            "ALTER is not implemented for this catalog",
        ))
    }
}

/// A real table entry can bind a provider scan while keeping its table identity
/// for dependencies and DML. Its rows are never copied into a native DataTable.
#[doc(hidden)]
pub trait TableBackend: std::fmt::Debug + Send + Sync {
    fn scan(&self, client: &Arc<FunctionClient>, transaction: &Transaction) -> Result<BackendScan>;
    fn validate_insert(&self, returning: bool) -> Result<()> {
        if returning {
            Err(roastedduck_common::Error::not_implemented(
                "RETURNING is not implemented for this table",
            ))
        } else {
            Ok(())
        }
    }
    fn validate_update(&self, _returning: bool, _defaults: bool) -> Result<()> {
        Err(roastedduck_common::Error::not_implemented(
            "UPDATE is not implemented for this table",
        ))
    }
    fn validate_delete(&self, _returning: bool) -> Result<()> {
        Err(roastedduck_common::Error::not_implemented(
            "DELETE is not implemented for this table",
        ))
    }
    fn validate_merge(&self, _returning: bool) -> Result<()> {
        Err(roastedduck_common::Error::not_implemented(
            "MERGE is not implemented for this table",
        ))
    }
    fn begin_insert(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _columns: &[usize],
    ) -> Result<Box<dyn TableMutation>> {
        Err(roastedduck_common::Error::not_implemented(
            "Streaming INSERT is not implemented for this table",
        ))
    }
    fn begin_update(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _columns: &[usize],
        _defaults: bool,
    ) -> Result<Box<dyn TableMutation>> {
        Err(roastedduck_common::Error::not_implemented(
            "UPDATE is not implemented for this table",
        ))
    }
    fn begin_delete(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _row_id: usize,
    ) -> Result<Box<dyn TableMutation>> {
        Err(roastedduck_common::Error::not_implemented(
            "DELETE is not implemented for this table",
        ))
    }
    fn insert(
        &self,
        _client: &Arc<FunctionClient>,
        _transaction: &Transaction,
        _columns: &[usize],
        _chunks: &mut dyn Iterator<Item = Result<roastedduck_types::DataChunk>>,
    ) -> Result<u64> {
        Err(roastedduck_common::Error::not_implemented(
            "INSERT is not implemented for this table",
        ))
    }
}

/// State of one remote mutation, including chunk boundaries and deferred work.
#[doc(hidden)]
pub trait TableMutation: std::fmt::Debug + Send {
    fn sink(&mut self, chunk: &roastedduck_types::DataChunk) -> Result<()>;
    fn finish(&mut self) -> Result<u64>;
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct BackendScan {
    pub function: TableFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// Virtual columns supplied by the provider, including its row identity.
    pub virtual_columns: Vec<(u64, ByteString, roastedduck_types::LogicalType)>,
}
