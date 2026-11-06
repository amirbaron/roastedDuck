//! The external file cache (DuckDB's `ExternalFileCache`, `external_file_cache.cpp` and
//! `external_file_cache_util.cpp`): ranges of files read through a
//! [`CachingFileHandle`](crate::caching_file_system::CachingFileHandle), kept in
//! buffer-managed memory (`EXTERNAL_FILE_CACHE`) that counts against the memory limit and
//! is evicted under pressure, like table blocks.
//!
//! DuckDB keeps a `CachedFile` per path in the database's object cache; here the cache
//! owns its map of files, which is what the object cache entries amount to.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_buffer::{BlockHandle, BlockState, BufferManager};

/// Which cached files are checked against the file before use (DuckDB's
/// `CacheValidationMode`, the `validate_external_file_cache` setting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheValidationMode {
    /// Every file is checked (`VALIDATE_ALL`).
    #[default]
    ValidateAll,
    /// Only remote files are checked (`VALIDATE_REMOTE`).
    ValidateRemote,
    /// Cached data is used as is (`NO_VALIDATION`).
    NoValidation,
}

impl CacheValidationMode {
    /// The mode named `text` (case-insensitive; the setting rejects other names).
    pub fn parse(text: &str) -> Self {
        if text.eq_ignore_ascii_case("VALIDATE_REMOTE") {
            Self::ValidateRemote
        } else if text.eq_ignore_ascii_case("NO_VALIDATION") {
            Self::NoValidation
        } else {
            Self::ValidateAll
        }
    }

    /// The `validate_external_file_cache` setting of the connection `client`
    /// (`ExternalFileCacheUtil::GetCacheValidationMode`).
    pub fn of_client(client: Option<&crate::FunctionClient>) -> Self {
        client
            .and_then(crate::FunctionClient::settings)
            .and_then(|settings| settings.current_setting("validate_external_file_cache").ok())
            .and_then(|value| value.to_varchar().ok())
            .map_or(Self::ValidateAll, |text| Self::parse(&text))
    }

    /// Whether cached data of `path` is checked against the file (`ShouldValidate`).
    pub fn should_validate(self, path: impl AsRef<[u8]>) -> bool {
        let path = path.as_ref();
        match self {
            Self::ValidateAll => true,
            Self::ValidateRemote => crate::file_system::is_remote_file(path),
            Self::NoValidation => false,
        }
    }
}

/// How a cached range covers another (`CachedFileRangeOverlap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overlap {
    None,
    Partial,
    Full,
}

/// Bytes of a file held in a buffer-managed block (`CachedFileRange`); immutable once in
/// the cache.
#[derive(Debug)]
pub struct CachedFileRange {
    pub(crate) block: Arc<BlockHandle>,
    pub(crate) nr_bytes: u64,
    pub(crate) location: u64,
    /// The file's version when the bytes were read.
    #[allow(dead_code, reason = "DuckDB keeps it with every range")]
    pub(crate) version_tag: String,
}

impl CachedFileRange {
    /// How this range covers `other_nr_bytes` bytes at `other_location` (`GetOverlap`).
    pub(crate) fn overlap(&self, other_nr_bytes: u64, other_location: u64) -> Overlap {
        let this_end = self.location + self.nr_bytes;
        let other_end = other_nr_bytes + other_location;
        if self.location <= other_location && this_end >= other_end {
            Overlap::Full
        } else if self.location < other_end && other_location < this_end {
            Overlap::Partial
        } else {
            Overlap::None
        }
    }
}

/// What the cache knows about a file (the members of `CachedFile` its lock guards).
#[derive(Debug, Default)]
pub(crate) struct CachedFileState {
    /// The cached ranges by location; none fully contains another.
    pub(crate) ranges: BTreeMap<u64, Arc<CachedFileRange>>,
    pub(crate) file_size: u64,
    /// The modification time in microseconds since the epoch.
    pub(crate) last_modified: i64,
    pub(crate) version_tag: String,
    pub(crate) can_seek: bool,
    pub(crate) on_disk_file: bool,
}

/// The cached ranges of one file (`CachedFile`).
#[derive(Debug)]
pub struct CachedFile {
    pub path: rustdb_common::ByteString,
    /// The cache's generation when the file was created: a file of an older generation
    /// is replaced (the cache was disabled and enabled since).
    pub generation: u64,
    state: RwLock<CachedFileState>,
}

impl CachedFile {
    fn new(path: &[u8], generation: u64) -> Self {
        Self {
            path: path.into(),
            generation,
            state: RwLock::new(CachedFileState::default()),
        }
    }

    /// The state for reading (`lock.GetSharedLock()`).
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, CachedFileState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The state for writing (`lock.GetExclusiveLock()`).
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, CachedFileState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the cached data is that of the file whose version tag and modification
    /// time are given (`CachedFile::IsValid`).
    pub(crate) fn is_valid(
        state: &CachedFileState,
        validate: bool,
        current_version_tag: &str,
        current_last_modified: i64,
    ) -> bool {
        ExternalFileCache::is_valid(
            validate,
            &state.version_tag,
            state.last_modified,
            current_version_tag,
            current_last_modified,
        )
    }
}

/// One cached range as `duckdb_external_file_cache()` lists it (`CachedFileInformation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedFileInformation {
    pub path: rustdb_common::ByteString,
    pub nr_bytes: u64,
    pub location: u64,
    /// Whether the range is in memory (not evicted).
    pub loaded: bool,
}

/// The database's external file cache (`ExternalFileCache`).
#[derive(Debug)]
pub struct ExternalFileCache {
    buffer_manager: BufferManager,
    /// `enable_external_file_cache`.
    enable: AtomicBool,
    /// Incremented whenever the cache is enabled or disabled.
    generation: AtomicU64,
    /// The cached files by path (the object cache entries of DuckDB's cache).
    files: Mutex<HashMap<rustdb_common::ByteString, Arc<CachedFile>>>,
}

impl ExternalFileCache {
    /// A cache allocating from `buffer_manager`, enabled or not.
    pub fn new(buffer_manager: BufferManager, enable: bool) -> Self {
        Self {
            buffer_manager,
            enable: AtomicBool::new(enable),
            generation: AtomicU64::new(0),
            files: Mutex::default(),
        }
    }

    fn files(&self) -> MutexGuard<'_, HashMap<rustdb_common::ByteString, Arc<CachedFile>>> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `IsEnabled`.
    pub fn is_enabled(&self) -> bool {
        self.enable.load(Ordering::Acquire)
    }

    /// `SetEnabled`: disabling drops every cached file.
    pub fn set_enabled(&self, enable: bool) {
        let dropped = {
            let mut files = self.files();
            if self.enable.load(Ordering::Acquire) == enable {
                return;
            }
            self.enable.store(enable, Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
            if enable {
                HashMap::new()
            } else {
                std::mem::take(&mut *files)
            }
        };
        drop(dropped);
    }

    /// `GetGeneration`.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The buffer manager the cached ranges live in.
    pub fn buffer_manager(&self) -> &BufferManager {
        &self.buffer_manager
    }

    /// `GetCachedFileInformation`: every cached range, file by file, by location.
    pub fn cached_file_information(&self) -> Vec<CachedFileInformation> {
        let files: Vec<Arc<CachedFile>> = self.files().values().cloned().collect();
        let mut result = Vec::new();
        for file in files {
            let state = file.read();
            for range in state.ranges.values() {
                result.push(CachedFileInformation {
                    path: file.path.clone(),
                    nr_bytes: range.nr_bytes,
                    location: range.location,
                    loaded: range.block.state() == BlockState::Loaded,
                });
            }
        }
        result
    }

    /// `GetCachedFileCount`.
    pub fn cached_file_count(&self) -> usize {
        self.files().len()
    }

    /// The shared cached file of `path`, created if needed; a file of its own that no
    /// one else sees while the cache is disabled (`GetOrCreateCachedFile`).
    pub fn get_or_create_cached_file(&self, path: impl AsRef<[u8]>) -> Arc<CachedFile> {
        let path = path.as_ref();
        let mut files = self.files();
        let generation = self.generation();
        if !self.is_enabled() {
            return Arc::new(CachedFile::new(path, generation));
        }
        if let Some(file) = files.get(path)
            && file.generation == generation
        {
            return Arc::clone(file);
        }
        let file = Arc::new(CachedFile::new(path, generation));
        files.insert(path.into(), Arc::clone(&file));
        file
    }

    /// Whether data cached with `cached_version_tag` / `cached_last_modified` is that of
    /// the file as it is now (`ExternalFileCache::IsValid`): by version tag where the
    /// file system has one, else by modification time, which must also be more than 10
    /// seconds in the past (file systems with a coarse clock may not show a quick change).
    pub fn is_valid(
        validate: bool,
        cached_version_tag: &str,
        cached_last_modified: i64,
        current_version_tag: &str,
        current_last_modified: i64,
    ) -> bool {
        const LAST_MODIFIED_THRESHOLD: i64 = 10 * 1000 * 1000;
        if !validate {
            return true;
        }
        if !current_version_tag.is_empty() || !cached_version_tag.is_empty() {
            return cached_version_tag == current_version_tag;
        }
        if cached_last_modified != current_last_modified {
            return false;
        }
        let access_time = now_micros();
        if access_time < current_last_modified {
            return false;
        }
        access_time - current_last_modified > LAST_MODIFIED_THRESHOLD
    }
}

/// `Timestamp::GetCurrentTimestamp` in microseconds since the epoch.
fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}
