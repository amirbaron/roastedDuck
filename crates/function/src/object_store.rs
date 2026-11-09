//! Database-local immutable files owned by external relation dependencies.
//! Names are weakly registered: closing a relation does not turn ephemeral input into a cache.
use crate::ObjectCache;
use roastedduck_common::ByteString;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
const KEY: &str = "roastedduck_object_store_files";
#[derive(Debug, Default)]
pub struct ObjectStore {
    files: Mutex<HashMap<ByteString, Weak<MemoryFile>>>,
}
#[derive(Debug)]
pub struct MemoryFile {
    path: ByteString,
    pub bytes: Arc<[u8]>,
    store: Weak<ObjectStore>,
}
impl Drop for MemoryFile {
    fn drop(&mut self) {
        if let Some(store) = self.store.upgrade() {
            store
                .files
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&self.path);
        }
    }
}
impl ObjectStore {
    pub fn initialize(cache: &ObjectCache) -> Arc<Self> {
        cache.get_or_insert_with(KEY, Self::default)
    }

    pub fn register(cache: &ObjectCache, path: ByteString, bytes: Arc<[u8]>) -> Arc<MemoryFile> {
        let store = Self::initialize(cache);
        let file = Arc::new(MemoryFile {
            path: path.clone(),
            bytes,
            store: Arc::downgrade(&store),
        });
        store
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path, Arc::downgrade(&file));
        file
    }
}
pub fn is_registered() -> bool {
    crate::file_system::opener()
        .and_then(|client| client.object_cache())
        .and_then(|cache| cache.get::<ObjectStore>(KEY))
        .is_some()
}
pub fn lookup(path: &[u8]) -> Option<Arc<MemoryFile>> {
    let cache = crate::file_system::opener()?.object_cache()?;
    let store = cache.get::<ObjectStore>(KEY)?;
    store
        .files
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)?
        .upgrade()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_ownership_survives_open_buffers_without_leaking_names() {
        let cache = Arc::new(ObjectCache::default());
        let client = Arc::new(crate::FunctionClient::default());
        client.set_object_cache(Arc::clone(&cache));
        let _scope = crate::file_system::activate_opener(client);
        assert!(!is_registered());
        let owner = ObjectStore::register(
            &cache,
            "DUCKDB_INTERNAL_OBJECTSTORE://test".into(),
            Arc::from(b"a,b\n1,2\n".as_slice()),
        );
        assert!(is_registered());
        let opened = lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://test").unwrap();
        let buffer = Arc::clone(&opened.bytes);
        drop(opened);
        assert!(lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://test").is_some());
        let other = Arc::new(crate::FunctionClient::default());
        other.set_object_cache(Arc::new(ObjectCache::default()));
        {
            let _other_scope = crate::file_system::activate_opener(other);
            assert!(lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://test").is_none());
        }
        assert!(lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://test").is_some());
        drop(owner);
        assert!(is_registered());
        assert!(lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://test").is_none());
        assert_eq!(buffer.as_ref(), b"a,b\n1,2\n");
    }
    #[test]
    fn range_reader_keeps_snapshot_bytes_after_name_release() {
        let cache = Arc::new(ObjectCache::default());
        let client = Arc::new(crate::FunctionClient::default());
        client.set_object_cache(Arc::clone(&cache));
        let _scope = crate::file_system::activate_opener(client);
        let owner = ObjectStore::register(
            &cache,
            "DUCKDB_INTERNAL_OBJECTSTORE://ranges".into(),
            Arc::from(b"0123456789".as_slice()),
        );
        let reader = crate::caching_file_system::CachingFileHandle::open(
            None,
            b"DUCKDB_INTERNAL_OBJECTSTORE://ranges",
            crate::external_file_cache::CacheValidationMode::ValidateAll,
        )
        .unwrap();
        assert_eq!(reader.file_size().unwrap(), 10);
        assert_eq!(reader.read(3, 4).unwrap().bytes(), b"456");
        drop(owner);
        assert!(lookup(b"DUCKDB_INTERNAL_OBJECTSTORE://ranges").is_none());
        assert_eq!(reader.read(2, 8).unwrap().bytes(), b"89");
        assert!(reader.read(1, 10).is_err());
    }
}
