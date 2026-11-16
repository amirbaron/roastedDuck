//! Transactional PostgreSQL storage for DuckDB's serialized key/value secrets.
use crate::secrets::{KeyValueSecret, SecretEntry};
use roastedduck_ast::{OnCreateConflict, OnEntryNotFound};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_serde::SerializationOptions;
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_storage::transaction::Transaction;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};

const HEX: &[u8; 16] = b"0123456789abcdef";
static NEXT_OFFSET: AtomicI64 = AtomicI64::new(25);
#[derive(Debug)]
pub(crate) struct PostgresSecretStorage {
    pub name: String,
    pub offset: i64,
    instance: Weak<crate::database::DatabaseInstance>,
    attached: String,
    table: String,
}
impl PostgresSecretStorage {
    pub fn new(
        instance: Weak<crate::database::DatabaseInstance>,
        attached: String,
        table: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: format!("postgres_{attached}"),
            offset: NEXT_OFFSET.fetch_add(1, Ordering::AcqRel),
            instance,
            attached,
            table,
        })
    }
    fn transaction(
        &self,
        transaction: Option<&Transaction>,
    ) -> Result<Option<Arc<super::transaction::PgTransaction>>> {
        let Some(instance) = self.instance.upgrade() else {
            return Ok(None);
        };
        let Some(attached) = instance.databases.get(&self.attached) else {
            return Ok(None);
        };
        let crate::attached::DatabaseStorage::Postgres(catalog) = &attached.storage else {
            return Ok(None);
        };
        let client = roastedduck_function::file_system::opener()
            .ok_or_else(|| Error::internal("Postgres secret storage requires client context"))?;
        let active = client
            .active_transaction()
            .and_then(|t| t.downcast::<Transaction>().ok());
        let transaction = transaction
            .or(active.as_deref())
            .ok_or_else(|| Error::internal("Postgres secret storage requires transaction"))?;
        catalog.transaction(&client, transaction).map(Some)
    }
    fn decode(&self, text: Option<&str>) -> Result<Option<SecretEntry>> {
        let Some(hex) = text.and_then(|s| s.strip_prefix("\\x")) else {
            return Ok(None);
        };
        if hex.len() % 2 != 0 {
            return Err(Error::serialization(
                "Invalid serialized PostgreSQL secret length",
            ));
        }
        let bytes = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let nibble = |c: u8| match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => 0,
                };
                (nibble(pair[0]) << 4) | nibble(pair[1])
            })
            .collect::<Vec<_>>();
        let secret = BinaryDeserializer::deserialize::<KeyValueSecret>(&bytes)?;
        Ok(Some(SecretEntry {
            secret,
            persistent: true,
            storage: self.name.clone(),
        }))
    }
    pub fn get(&self, name: &str, transaction: Option<&Transaction>) -> Result<Option<SecretEntry>> {
        let Some(pg) = self.transaction(transaction)? else {
            return Ok(None);
        };
        let result = pg.text_query(&format!(
            "SELECT serialized_secret FROM {} WHERE secret_name = {}",
            self.table,
            super::literal(name)
        ))?;
        result
            .rows
            .first()
            .map_or(Ok(None), |r| self.decode(r.first().and_then(Option::as_deref)))
    }
    pub fn all(&self, transaction: Option<&Transaction>) -> Result<Vec<SecretEntry>> {
        let Some(pg) = self.transaction(transaction)? else {
            return Ok(Vec::new());
        };
        let result = pg.text_query(&format!(
            "SELECT secret_name, serialized_secret FROM {}",
            self.table
        ))?;
        result
            .rows
            .iter()
            .filter_map(|r| self.decode(r.get(1).and_then(Option::as_deref)).transpose())
            .collect()
    }
    pub fn for_type(&self, secret_type: &str, transaction: Option<&Transaction>) -> Result<Vec<SecretEntry>> {
        let Some(pg) = self.transaction(transaction)? else {
            return Ok(Vec::new());
        };
        let result = pg.text_query(&format!(
            "SELECT secret_name, serialized_secret FROM {} WHERE LOWER(secret_type) = {}",
            self.table,
            super::literal(&secret_type.to_ascii_lowercase())
        ))?;
        result
            .rows
            .iter()
            .filter_map(|r| self.decode(r.get(1).and_then(Option::as_deref)).transpose())
            .collect()
    }
    pub fn store(
        &self,
        secret: &KeyValueSecret,
        on_conflict: OnCreateConflict,
        transaction: &Transaction,
    ) -> Result<()> {
        let name = std::str::from_utf8(secret.name.as_bytes())
            .map_err(|_| Error::invalid_input("Invalid PostgreSQL secret name"))?;
        if self.get(name, Some(transaction))?.is_some() {
            match on_conflict {
                OnCreateConflict::IgnoreOnConflict => return Ok(()),
                OnCreateConflict::ReplaceOnConflict => {}
                _ => {
                    return Err(Error::invalid_input(format!(
                        "Persistent secret with name '{name}' already exists in secret storage '{}'!",
                        self.name
                    )));
                }
            }
        }
        let bytes = BinarySerializer::serialize(secret, SerializationOptions::default())?;
        let mut serialized = String::from("\\\\x");
        for byte in bytes {
            serialized.push(char::from(HEX[usize::from(byte >> 4)]));
            serialized.push(char::from(HEX[usize::from(byte & 15)]));
        }
        let pg = self.transaction(Some(transaction))?.ok_or_else(|| {
            Error::invalid_input(format!(
                "Cannot store secret: database '{}' is not attached",
                self.attached
            ))
        })?;
        let conflict = if on_conflict == OnCreateConflict::ErrorOnConflict {
            ""
        } else {
            " ON CONFLICT (secret_name) DO UPDATE SET secret_type = EXCLUDED.secret_type, serialized_secret = EXCLUDED.serialized_secret"
        };
        let query = format!(
            "INSERT INTO {} (secret_name, secret_type, serialized_secret) VALUES ({}, {}, E'{serialized}'){conflict}",
            self.table,
            super::literal(name),
            super::literal(&secret.secret_type)
        );
        pg.execute(&query, true).map_err(|error| {
            if error.kind() == ErrorKind::Interrupt {
                return error;
            }
            Error::new(
                ErrorKind::Io,
                error
                    .raw_message()
                    .replacen(&serialized, "<serialized_secret>", 1),
            )
        })
    }
    pub fn drop_secret(
        &self,
        name: &str,
        if_not_found: OnEntryNotFound,
        transaction: &Transaction,
    ) -> Result<()> {
        let Some(pg) = self.transaction(Some(transaction))? else {
            return if if_not_found == OnEntryNotFound::ThrowException {
                Err(Error::invalid_input(format!(
                    "Cannot drop secret: database '{}' is not attached",
                    self.attached
                )))
            } else {
                Ok(())
            };
        };
        if self.get(name, Some(transaction))?.is_none() && if_not_found == OnEntryNotFound::ThrowException {
            return Err(Error::invalid_input(format!(
                "Failed to remove non-existent persistent secret '{name}' in secret storage '{}'",
                self.name
            )));
        }
        pg.execute(
            &format!(
                "DELETE FROM {} WHERE secret_name = {}",
                self.table,
                super::literal(name)
            ),
            true,
        )
    }
}

/// Probe/create on an independent pool lease before publishing the attachment.
/// Empty explicit names disable even the probe, as in the pinned extension.
pub(crate) fn initialize(
    catalog: &super::catalog::PgCatalog,
    table: &str,
    explicit: bool,
    attached: &str,
) -> Result<bool> {
    if table.is_empty() && explicit {
        return Ok(false);
    }
    let lease = catalog.pool.acquire(None)?;
    let session = lease.session()?;
    let exists = if let Ok(result) =
        session.text_query(&format!("SELECT to_regclass({})", super::literal(table)), None)
    {
        result
            .rows
            .first()
            .and_then(|r| r.first())
            .is_some_and(Option::is_some)
    } else {
        let name = super::literal(table);
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema=CASE WHEN {name} LIKE '%.%' THEN split_part({name},'.',1) ELSE 'public' END AND table_name=CASE WHEN {name} LIKE '%.%' THEN split_part({name},'.',2) ELSE {name} END)"
        );
        session
            .text_query(&sql, None)
            .ok()
            .and_then(|r| r.rows.first().and_then(|r| r.first()).cloned().flatten())
            .is_some_and(|v| v == "t")
    };
    if exists {
        return Ok(true);
    }
    if !explicit {
        return Ok(false);
    }
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table} (secret_name VARCHAR PRIMARY KEY, secret_type VARCHAR NOT NULL, serialized_secret BYTEA NOT NULL)"
    );
    session.text_query(&sql,None).map_err(|error| Error::new(ErrorKind::Io,format!("Error initializing secrets storage table, name: \"{table}\" in attached database: \"{attached}\": {}",super::transaction::query_error(&sql,error).raw_message())))?;
    Ok(true)
}
