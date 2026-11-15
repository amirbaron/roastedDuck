//! DuckDB `HTTPMetadataCache`: statement-local by default, database-local on opt-in.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug, Default)]
pub(super) struct Entry {
    pub size: u64,
    pub modified: i64,
    pub etag: String,
    pub version: String,
    pub region: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Cache(Mutex<HashMap<String, Entry>>);

impl Cache {
    /// Metadata carried by S3 listing results lasts only for this statement.
    pub fn listing() -> Option<Arc<Self>> {
        let client = roastedduck_function::file_system::opener()?;
        Some(
            client
                .query_cache()
                .get_or_insert_with("httpfs.listing_metadata", Self::default),
        )
    }

    pub fn current() -> Option<Arc<Self>> {
        let client = roastedduck_function::file_system::opener()?;
        let shared = client
            .settings()
            .and_then(|s| s.current_setting("enable_http_metadata_cache").ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let objects = if shared {
            client.object_cache()?
        } else {
            client.query_cache()
        };
        Some(objects.get_or_insert_with("httpfs.metadata_cache", Self::default))
    }

    pub fn get(&self, path: &str) -> Option<Entry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .cloned()
    }

    pub fn put(&self, path: String, entry: Entry) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path, entry);
    }

    pub fn erase(&self, path: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
    }
}
