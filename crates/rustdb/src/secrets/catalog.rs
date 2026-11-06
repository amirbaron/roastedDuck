//! Versioned secret entries with the transaction manager's catalog undo lifecycle.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{ByteString, Error, ErrorKind, Result, byte_format};
use rustdb_storage::CatalogUndo;
use rustdb_storage::transaction::{TRANSACTION_ID_START, Transaction};

use super::KeyValueSecret;

#[derive(Debug)]
struct Version {
    timestamp: AtomicU64,
    secret: Option<KeyValueSecret>,
}

type Chains = BTreeMap<ByteString, Vec<Arc<Version>>>;

#[derive(Debug, Default)]
pub(super) struct SecretCatalog {
    chains: Mutex<Chains>,
}

fn visible(version: &Version, transaction: Option<&Transaction>) -> bool {
    let timestamp = version.timestamp.load(Ordering::Acquire);
    transaction.map_or(timestamp < TRANSACTION_ID_START, |transaction| {
        timestamp == transaction.transaction_id || timestamp < transaction.start_time
    })
}

impl SecretCatalog {
    fn lock(&self) -> MutexGuard<'_, Chains> {
        self.chains.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn has_chain(&self, name: &[u8]) -> bool {
        self.lock().contains_key(name)
    }

    /// Persistent files discovered at initialization are default catalog entries,
    /// visible even to transactions that began before the lazy file was read.
    pub(super) fn seed(&self, secret: KeyValueSecret) {
        self.lock()
            .entry(secret.name.to_ascii_lowercase())
            .or_insert_with(|| {
                vec![Arc::new(Version {
                    timestamp: AtomicU64::new(0),
                    secret: Some(secret),
                })]
            });
    }

    pub(super) fn get(&self, name: &[u8], transaction: Option<&Transaction>) -> Option<KeyValueSecret> {
        self.lock()
            .get(name)?
            .iter()
            .find(|version| visible(version, transaction))?
            .secret
            .clone()
    }

    pub(super) fn all(&self, transaction: Option<&Transaction>) -> Vec<KeyValueSecret> {
        self.lock()
            .values()
            .filter_map(|chain| chain.iter().find(|version| visible(version, transaction)))
            .filter_map(|version| version.secret.clone())
            .collect()
    }

    pub(super) fn change(
        self: &Arc<Self>,
        name: ByteString,
        secret: Option<KeyValueSecret>,
        transaction: &Transaction,
        operation: &str,
    ) -> Result<()> {
        let mut chains = self.lock();
        if let Some(head) = chains.get(&name).and_then(|chain| chain.first()) {
            let timestamp = head.timestamp.load(Ordering::Acquire);
            if timestamp >= TRANSACTION_ID_START && timestamp != transaction.transaction_id
                || timestamp < TRANSACTION_ID_START && timestamp > transaction.start_time
            {
                return Err(Error::new_bytes(
                    ErrorKind::Transaction,
                    byte_format!("Catalog write-write conflict on {operation} with \"{name}\""),
                ));
            }
        }
        let version = Arc::new(Version {
            timestamp: AtomicU64::new(transaction.transaction_id),
            secret,
        });
        let chain = chains.entry(name.clone()).or_insert_with(|| {
            vec![Arc::new(Version {
                timestamp: AtomicU64::new(0),
                secret: None,
            })]
        });
        chain.insert(0, Arc::clone(&version));
        transaction.push_catalog_undo(Box::new(SecretChange {
            catalog: Arc::clone(self),
            name,
            version,
        }));
        Ok(())
    }
}

#[derive(Debug)]
struct SecretChange {
    catalog: Arc<SecretCatalog>,
    name: ByteString,
    version: Arc<Version>,
}

impl CatalogUndo for SecretChange {
    fn commit(&self, commit_id: u64) {
        self.version.timestamp.store(commit_id, Ordering::Release);
    }

    fn revert_commit(&self, transaction_id: u64) {
        self.version.timestamp.store(transaction_id, Ordering::Release);
    }

    fn rollback(&self) {
        let mut chains = self.catalog.lock();
        if let Some(chain) = chains.get_mut(&self.name) {
            chain.retain(|version| !Arc::ptr_eq(version, &self.version));
            if chain
                .iter()
                .all(|version| version.secret.is_none() && version.timestamp.load(Ordering::Acquire) == 0)
            {
                chains.remove(&self.name);
            }
        }
    }

    fn cleanup(&self) {
        let mut chains = self.catalog.lock();
        if let Some(chain) = chains.get_mut(&self.name)
            && let Some(position) = chain
                .iter()
                .position(|version| Arc::ptr_eq(version, &self.version))
        {
            chain.truncate(position + 1);
        }
    }
}
