//! Table-set signatures, sampled outside the caller's transaction and promoted on commit.
use super::{PgCatalog, PgTransaction};
use roastedduck_catalog::{CatalogType, Transaction};
use roastedduck_common::{Error, Result};
use roastedduck_function::FunctionClient;
use std::sync::{Arc, PoisonError};
#[derive(Debug, Default)]
pub(super) struct Signatures {
    pub generation: u64,
    pub values: std::collections::HashMap<String, Vec<u8>>,
}

pub(super) fn information_schema(client: &FunctionClient) -> bool {
    client
        .settings()
        .and_then(|s| s.current_setting("pg_use_information_schema_introspection").ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
fn query(client: &FunctionClient, schema: &str) -> Result<Option<String>> {
    let settings = client.settings();
    let enabled = settings
        .as_ref()
        .and_then(|s| s.current_setting("pg_staleness_query_enabled").ok())
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| !information_schema(client));
    if !enabled {
        return Ok(None);
    }
    let custom = settings
        .as_ref()
        .and_then(|s| s.current_setting("pg_staleness_query").ok())
        .filter(|v| !v.is_null())
        .map(|v| v.to_varchar())
        .transpose()?
        .unwrap_or_default();
    let sql = if custom.is_empty() {
        "SELECT pg_class.oid, relname, pg_class.xmin FROM pg_class JOIN pg_namespace ON relnamespace = pg_namespace.oid WHERE relkind IN ('r', 'v', 'm', 'f', 'p') AND pg_namespace.nspname = ${SCHEMA} ORDER BY pg_class.oid;".to_owned()
    } else {
        if !custom.contains("${SCHEMA}") {
            return Err(Error::invalid_input(
                "pg_staleness_query must contain a ${SCHEMA} placeholder",
            ));
        }
        custom
    };
    Ok(Some(sql.replace("${SCHEMA}", &crate::postgres::literal(schema))))
}
fn signature(result: roastedduck_postgres::TextResult) -> Result<Vec<u8>> {
    if result.columns.len() < 3 {
        return Err(Error::invalid_input(format!(
            "pg_staleness_query must return at least 3 columns (identity, name, revision marker), got {}",
            result.columns.len()
        )));
    }
    let mut signature = Vec::new();
    for row in result.rows {
        for (index, field) in row.into_iter().take(3).enumerate() {
            signature.extend_from_slice(field.as_deref().unwrap_or_default().as_bytes());
            signature.push(u8::from(index == 2));
        }
    }
    Ok(signature)
}

impl PgCatalog {
    /// A signature mismatch invalidates only this schema's table/view set. Types,
    /// indexes and other schemas retain their independent cache lifetimes.
    pub(super) fn check_staleness(
        &self,
        client: &Arc<FunctionClient>,
        schema: &str,
    ) -> Result<Option<Vec<u8>>> {
        let Some(sql) = query(client, schema)? else {
            return Ok(None);
        };
        let lease = self.pool.acquire(client.interrupt_flag().as_deref())?;
        let result = crate::postgres::logging::query(client, &sql, || {
            lease
                .session()?
                .text_query(&sql, client.interrupt_flag().as_deref())
                .map_err(|e| crate::postgres::transaction::query_error(&sql, e))
        })?;
        let signature = signature(result)?;
        drop(lease);
        let known = self.signatures.lock().unwrap_or_else(PoisonError::into_inner);
        if known.values.get(schema) == Some(&signature) {
            return Ok(None);
        }
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(kind, s, _, _), _| {
                s != schema || !matches!(kind, CatalogType::Table | CatalogType::View)
            });
        self.listings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(kind, s), _| s != schema || !matches!(kind, CatalogType::Table | CatalogType::View));
        Ok(Some(signature))
    }
    pub(super) fn stage_loaded_signature(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
        signature: Option<Vec<u8>>,
    ) -> Result<()> {
        if let Some(signature) = signature {
            self.transaction(client, transaction)?.stage_signature(
                self.own.clone(),
                schema.to_owned(),
                signature,
            );
        }
        Ok(())
    }
    pub(super) fn refresh_signature(
        &self,
        client: &Arc<FunctionClient>,
        transaction: &Transaction,
        schema: &str,
    ) -> Result<()> {
        let Some(sql) = query(client, schema)? else {
            return Ok(());
        };
        let pg = self.transaction(client, transaction)?;
        let value = pg.staleness_signature(&sql)?;
        pg.stage_signature(self.own.clone(), schema.to_owned(), value);
        Ok(())
    }
    pub(crate) fn signature_generation(&self) -> u64 {
        self.signatures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
    }
    pub(crate) fn promote_signature(&self, schema: String, signature: Vec<u8>, generation: u64) {
        let mut state = self.signatures.lock().unwrap_or_else(PoisonError::into_inner);
        // pg_clear_cache replaces upstream schema/table sets. A transaction
        // referencing an old set must not promote into its replacement.
        if state.generation == generation {
            state.values.insert(schema, signature);
        }
    }
}
impl PgTransaction {
    pub(super) fn staleness_signature(&self, sql: &str) -> Result<Vec<u8>> {
        signature(self.text_query(sql)?)
    }
}
