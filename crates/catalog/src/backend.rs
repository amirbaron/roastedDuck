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
    fn lookup(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        kind: CatalogType,
        schema: &[u8],
        name: &[u8],
    ) -> Result<Option<Arc<CatalogEntry>>>;
}

/// A real table entry can bind a provider scan while keeping its table identity
/// for dependencies and DML. Its rows are never copied into a native DataTable.
#[doc(hidden)]
pub trait TableBackend: std::fmt::Debug + Send + Sync {
    fn scan(&self, client: &Arc<FunctionClient>, transaction: &Transaction) -> Result<BackendScan>;
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct BackendScan {
    pub function: TableFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// Virtual columns supplied by the provider, including its row identity.
    pub virtual_columns: Vec<(u64, ByteString, roastedduck_types::LogicalType)>,
}
