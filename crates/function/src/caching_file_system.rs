//! Reading files through the external file cache (DuckDB's `CachingFileSystem` and
//! `CachingFileHandle`, `caching_file_system.cpp`): a read of a range returns the bytes
//! from the cache when a cached range holds them, else reads them (copying the parts
//! cached ranges hold) into a new buffer-managed range of the cache.
//!
//! The handle opens the file lazily: with validation off and the file cached, it serves
//! reads, size and modification time from the cache without touching the file.

use std::fs::File;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BufferHandle, MemoryTag};
use rustdb_common::{Error, ErrorKind, Result};

use crate::external_file_cache::{
    CacheValidationMode, CachedFile, CachedFileRange, ExternalFileCache, Overlap,
};

fn io_error(message: impl Into<rustdb_common::ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Io, message.into())
}

#[cfg(unix)]
fn native_io_reason(error: &std::io::Error) -> rustdb_common::ByteString {
    unsafe extern "C" {
        #[cfg_attr(target_env = "gnu", link_name = "__xpg_strerror_r")]
        fn strerror_r(
            error: std::ffi::c_int,
            buffer: *mut std::ffi::c_char,
            length: usize,
        ) -> std::ffi::c_int;
    }
    if let Some(errno) = error.raw_os_error() {
        let mut buffer = [0u8; 1024];
        // SAFETY: the POSIX function writes at most buffer.len() bytes to this
        // caller-owned allocation, including its terminating NUL.
        if unsafe { strerror_r(errno, buffer.as_mut_ptr().cast(), buffer.len()) } == 0 {
            let length = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
            return buffer[..length].into();
        }
    }
    error.to_string().into()
}
#[cfg(not(unix))]
fn native_io_reason(error: &std::io::Error) -> rustdb_common::ByteString {
    error.to_string().into()
}

/// An open local file with what the cache validates it by.
#[derive(Debug)]
struct OpenFile {
    file: ReadFile,
    size: u64,
    /// Microseconds since the epoch, at a resolution of seconds (`Timestamp::FromEpochSeconds`
    /// of `st_mtime`).
    last_modified: i64,
    /// The device, inode, size and modification time (`GetPosixVersionTag`), hex-encoded.
    version_tag: String,
}

#[derive(Debug)]
enum ReadFile {
    Local(File),
    Memory(Arc<[u8]>),
    Registered(Arc<dyn crate::registered_files::FileHandle>),
}

impl OpenFile {
    /// `LocalFileSystem::OpenFile` for reading, with its errors.
    fn open(path: &[u8]) -> Result<Self> {
        crate::file_system::verify_can_access_file(path)?;
        if let Some(fs) = crate::registered_files::lookup(path) {
            let handle = fs.open(path, false)?;
            let size = handle.size()?;
            let last_modified = handle.modified()?;
            let version_tag = handle.version_tag()?;
            return Ok(Self {
                file: ReadFile::Registered(handle),
                size,
                last_modified,
                version_tag,
            });
        }
        if let Some(file) = crate::object_store::lookup(path) {
            return Ok(Self {
                size: file.bytes.len() as u64,
                file: ReadFile::Memory(Arc::clone(&file.bytes)),
                last_modified: 0,
                version_tag: String::new(),
            });
        }
        // POSIX open receives path.c_str(); retain the full native path in diagnostics.
        let local = path.split(|byte| *byte == 0).next().unwrap_or_default();
        let file = File::open(crate::file_system::native_path(local)?).map_err(|error| {
            let reason = native_io_reason(&error);
            let result = io_error(rustdb_common::byte_format!(
                "Cannot open file \"{path}\": {reason}"
            ));
            match error.raw_os_error() {
                Some(errno) => result.with_extra_info("errno", errno.to_string()),
                None => result,
            }
        })?;
        let metadata = file.metadata().map_err(|error| {
            io_error(rustdb_common::byte_format!(
                "Could not get file size of \"{path}\": {error}"
            ))
        })?;
        let (last_modified, version_tag) = file_version(&metadata);
        Ok(Self {
            file: ReadFile::Local(file),
            size: metadata.len(),
            last_modified,
            version_tag,
        })
    }

    /// `LocalFileSystem::Read` of `buffer.len()` bytes at `location`.
    fn read_exact_at(&self, path: &[u8], buffer: &mut [u8], location: u64) -> Result<()> {
        let end = location.checked_add(buffer.len() as u64);
        if end.is_none_or(|end| end > self.size) {
            return Err(io_error(rustdb_common::byte_format!(
                "Could not read enough bytes from file \"{path}\": attempted to read {} bytes from location {location}",
                buffer.len()
            )));
        }
        if let ReadFile::Registered(handle) = &self.file {
            let n = handle.read_at(location, buffer)?;
            if n != buffer.len() {
                return Err(io_error("Registered filesystem returned a short read"));
            }
            return Ok(());
        }
        if let ReadFile::Memory(bytes) = &self.file {
            let start =
                usize::try_from(location).map_err(|_| io_error("File offset exceeds addressable memory"))?;
            buffer.copy_from_slice(&bytes[start..start + buffer.len()]);
            return Ok(());
        }
        let ReadFile::Local(file) = &self.file else {
            return Err(Error::internal("Missing local file handle"));
        };
        read_exact_at(file, buffer, location).map_err(|error| {
            io_error(rustdb_common::byte_format!(
                "Could not read from file \"{path}\": {error}"
            ))
        })
    }
}

/// The modification time (microseconds, whole seconds) and version tag of a file.
#[cfg(unix)]
pub fn file_version(metadata: &std::fs::Metadata) -> (i64, String) {
    use std::fmt::Write as _;
    use std::os::unix::fs::MetadataExt;
    let last_modified = metadata.mtime().saturating_mul(1_000_000);
    let mut version_tag = String::with_capacity(64);
    for part in [
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        last_modified as u64,
    ] {
        for byte in part.to_le_bytes() {
            let _ = write!(version_tag, "{byte:02x}");
        }
    }
    (last_modified, version_tag)
}

/// The modification time of a file; Windows has no version tag (`GetVersionTag`).
#[cfg(not(unix))]
pub fn file_version(metadata: &std::fs::Metadata) -> (i64, String) {
    let last_modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX / 1_000_000) * 1_000_000
        });
    (last_modified, String::new())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let read = std::os::windows::fs::FileExt::seek_read(file, buffer, offset)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

/// Bytes a [`CachingFileHandle`] read: a pinned range of the cache (kept in memory while
/// the read lives, the `BufferHandle` DuckDB's `Read` returns), or bytes of its own when
/// the cache is off.
#[derive(Debug)]
pub struct CachedRead {
    data: ReadData,
    offset: usize,
    len: usize,
}

#[derive(Debug)]
enum ReadData {
    Pinned(BufferHandle),
    Owned(Vec<u8>),
}

impl CachedRead {
    fn pinned(handle: BufferHandle, offset: usize, len: usize) -> Self {
        Self {
            data: ReadData::Pinned(handle),
            offset,
            len,
        }
    }

    /// The bytes read.
    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            // SAFETY: a cached range is written only before it enters the cache, by the
            // thread that allocated it; afterwards every pin only reads it.
            ReadData::Pinned(handle) => unsafe { &handle.as_slice()[self.offset..self.offset + self.len] },
            ReadData::Owned(bytes) => &bytes[self.offset..self.offset + self.len],
        }
    }
}

/// A file read through the external file cache (DuckDB's `CachingFileHandle`). Every
/// method takes `&self`: threads share a handle and read positionally.
#[derive(Debug)]
pub struct CachingFileHandle {
    /// The database's cache; `None` outside a database, where reads go to the file.
    cache: Option<Arc<ExternalFileCache>>,
    path: rustdb_common::ByteString,
    /// Whether cached data is checked against the file (`Validate()`).
    validate: bool,
    /// The file's entry in the cache (`cached_file`).
    cached_file: Mutex<Option<Arc<CachedFile>>>,
    /// The file, once opened (`file_handle`).
    file: Mutex<Option<Arc<OpenFile>>>,
}

/// The block header DuckDB's temporary buffers carry (`Storage::DEFAULT_BLOCK_HEADER_SIZE`,
/// in `AllocateTemporaryMemory`); rustdb's have none, so a cached range reserves its
/// bytes to take the same memory (its size plus the header, rounded to sectors).
const DEFAULT_BLOCK_HEADER_SIZE: usize = 8;

/// Grow a read to the next cached range when the gap is at most this many bytes (and at
/// most the read's own size) (`ShouldExpandToFillGap`).
const MAX_BOUND_TO_BE_ADDED_LENGTH: u64 = 1_048_576;

fn should_expand_to_fill_gap(current_length: u64, added_length: u64) -> bool {
    added_length <= MAX_BOUND_TO_BE_ADDED_LENGTH && added_length <= current_length
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CachingFileHandle {
    /// Opens `path` for reading through `cache` (`CachingFileSystem::OpenFile`); the
    /// file itself is opened now unless validation is off and the cache holds some of it.
    pub fn open(
        cache: Option<Arc<ExternalFileCache>>,
        path: impl AsRef<[u8]>,
        validation: CacheValidationMode,
    ) -> Result<Self> {
        let path = path.as_ref();
        let cached_file = cache.as_ref().map(|cache| cache.get_or_create_cached_file(path));
        let handle = Self {
            validate: validation.should_validate(path),
            path: path.into(),
            cached_file: Mutex::new(cached_file.clone()),
            file: Mutex::new(None),
            cache,
        };
        let cached_ranges = cached_file.is_some_and(|file| !file.read().ranges.is_empty());
        if !handle.cache_enabled() || handle.validate || !cached_ranges {
            handle.file_handle()?;
        }
        Ok(handle)
    }

    pub fn path(&self) -> &[u8] {
        &self.path
    }

    fn cache_enabled(&self) -> bool {
        self.cache.as_ref().is_some_and(|cache| cache.is_enabled())
    }

    /// Whether cached data is checked against the file (`Validate`).
    pub fn validate(&self) -> bool {
        self.validate
    }

    fn current_cached_file(&self) -> Option<Arc<CachedFile>> {
        lock(&self.cached_file).clone()
    }

    /// The open file, opening it (and checking the cached data against it) if needed
    /// (`GetFileHandle`).
    fn file_handle(&self) -> Result<Arc<OpenFile>> {
        let cached_file = self.current_cached_file();
        let mut file = lock(&self.file);
        if let Some(open) = &*file {
            return Ok(Arc::clone(open));
        }
        let open = Arc::new(OpenFile::open(&self.path)?);
        if let Some(cached_file) = cached_file {
            let mut state = cached_file.write();
            if !CachedFile::is_valid(&state, self.validate, &open.version_tag, open.last_modified) {
                state.ranges.clear();
            }
            state.file_size = open.size;
            state.last_modified = open.last_modified;
            state.version_tag.clone_from(&open.version_tag);
            state.can_seek = true;
            state.on_disk_file = matches!(open.file, ReadFile::Local(_));
        }
        *file = Some(Arc::clone(&open));
        Ok(open)
    }

    /// The handle's cache entry, replaced (and the file reopened) if the cache was
    /// disabled or enabled since (`EnsureCachedFileCurrent`).
    fn ensure_cached_file_current(&self) -> Result<Option<Arc<CachedFile>>> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };
        if let Some(current) = self.current_cached_file()
            && current.generation == cache.generation()
        {
            return Ok(Some(current));
        }
        let needs_reopen = lock(&self.file).take().is_some();
        let current = cache.get_or_create_cached_file(&self.path);
        *lock(&self.cached_file) = Some(Arc::clone(&current));
        if needs_reopen {
            self.file_handle()?;
        }
        Ok(Some(current))
    }

    /// A property of the file: from the cache when not validating, else from the file.
    fn property<T>(
        &self,
        cached: impl Fn(&crate::external_file_cache::CachedFileState) -> T,
        file: impl Fn(&OpenFile) -> T,
    ) -> Result<T> {
        if !self.validate
            && let Some(cached_file) = self.ensure_cached_file_current()?
        {
            return Ok(cached(&cached_file.read()));
        }
        Ok(file(&*self.file_handle()?))
    }

    /// `GetFileSize`.
    pub fn file_size(&self) -> Result<u64> {
        self.property(|state| state.file_size, |file| file.size)
    }

    /// `GetLastModifiedTime`, in microseconds since the epoch.
    pub fn last_modified(&self) -> Result<i64> {
        self.property(|state| state.last_modified, |file| file.last_modified)
    }

    /// `GetVersionTag`.
    pub fn version_tag(&self) -> Result<String> {
        self.property(|state| state.version_tag.clone(), |file| file.version_tag.clone())
    }

    /// Reads `buffer.len()` bytes at `location` from the file, bypassing the cache
    /// (`GetFileHandle().Read`).
    pub fn read_direct(&self, buffer: &mut [u8], location: u64) -> Result<()> {
        self.file_handle()?.read_exact_at(&self.path, buffer, location)
    }

    /// The `nr_bytes` bytes at `location`, from the cache or the file (`Read`).
    pub fn read(&self, nr_bytes: u64, location: u64) -> Result<CachedRead> {
        self.read_internal(nr_bytes, location, None)
    }

    /// [`Self::read`] by a caller that has the file's bytes at `location` already
    /// (`source`, at least `nr_bytes` long): a range the cache lacks is filled from
    /// them instead of the file.
    pub fn read_from(&self, nr_bytes: u64, location: u64, source: &[u8]) -> Result<CachedRead> {
        self.read_internal(nr_bytes, location, Some(source))
    }

    /// The version tag of the open file (empty if not opened), and whether the file
    /// gives nothing to validate cached data by (`no_validation_metadata`).
    fn validation_metadata(&self) -> (String, bool) {
        match &*lock(&self.file) {
            Some(open) => (
                open.version_tag.clone(),
                self.validate && open.version_tag.is_empty() && open.last_modified == 0,
            ),
            None => (String::new(), self.validate),
        }
    }

    /// Whether reads go through the cache: it is on and cached data of the file can be
    /// validated (the check at the start of `Read`).
    pub fn caches_reads(&self) -> bool {
        self.cache_enabled() && !self.validation_metadata().1
    }

    fn read_internal(&self, nr_bytes: u64, location: u64, source: Option<&[u8]>) -> Result<CachedRead> {
        let len = usize::try_from(nr_bytes).map_err(|_| Error::internal("read larger than memory"))?;
        let (version_tag, no_validation_metadata) = self.validation_metadata();
        let cache = match &self.cache {
            Some(cache) if cache.is_enabled() && !no_validation_metadata => cache,
            // Only cache when the cache is on and the file can be validated.
            _ => {
                let mut bytes = vec![0; len];
                match source {
                    Some(source) if source.len() >= len => bytes.copy_from_slice(&source[..len]),
                    _ => self.read_direct(&mut bytes, location)?,
                }
                return Ok(CachedRead {
                    data: ReadData::Owned(bytes),
                    offset: 0,
                    len,
                });
            }
        };
        let Some(cached_file) = self.ensure_cached_file_current()? else {
            return Err(Error::internal("caching file handle without a cache entry"));
        };
        let mut overlapping = Vec::new();
        let mut next_range = None;
        if let Some(read) = self.try_read_from_cache(
            &cached_file,
            nr_bytes,
            location,
            &mut overlapping,
            &mut next_range,
        )? {
            return Ok(read);
        }
        let mut new_nr_bytes = nr_bytes;
        if let Some(next) = next_range {
            let added = next - location - nr_bytes;
            if should_expand_to_fill_gap(nr_bytes, added) {
                new_nr_bytes = nr_bytes + added;
            }
        }
        let new_len =
            usize::try_from(new_nr_bytes).map_err(|_| Error::internal("read larger than memory"))?;
        let buffer_manager = cache.buffer_manager();
        let handle = buffer_manager.allocate(
            MemoryTag::ExternalFileCache,
            new_len + DEFAULT_BLOCK_HEADER_SIZE,
            true,
        )?;
        {
            // SAFETY: the buffer was just allocated: only this pin exists, and the range
            // enters the cache only after it is filled.
            let buffer = unsafe { &mut handle.as_mut_slice()[..new_len] };
            match source {
                Some(source) if source.len() >= new_len => buffer.copy_from_slice(&source[..new_len]),
                _ => self.read_and_copy_interleaved(&overlapping, buffer, location)?,
            }
        }
        let range = Arc::new(CachedFileRange {
            block: Arc::clone(handle.block()),
            nr_bytes: new_nr_bytes,
            location,
            version_tag,
        });
        self.try_insert_file_range(&cached_file, handle, len, location, range)
    }

    /// The bytes from a cached range that holds them all, collecting the ranges that
    /// hold some in `overlapping` and the start of the first range after the read in
    /// `next_range` (`TryReadFromCache`).
    fn try_read_from_cache(
        &self,
        cached_file: &CachedFile,
        nr_bytes: u64,
        location: u64,
        overlapping: &mut Vec<Arc<CachedFileRange>>,
        next_range: &mut Option<u64>,
    ) -> Result<Option<CachedRead>> {
        let state = cached_file.read();
        if let Some(range) = state.ranges.get(&location)
            && range.overlap(nr_bytes, location) == Overlap::Full
            && let Some(read) = self.try_read_from_file_range(range, nr_bytes, location)?
        {
            return Ok(Some(read));
        }
        let this_end = location + nr_bytes;
        // From the last range before `location` on: ranges never contain one another.
        let start = state
            .ranges
            .range(..location)
            .next_back()
            .map_or(location, |(&start, _)| start);
        for range in state.ranges.range(start..).map(|(_, range)| range) {
            if range.location >= this_end {
                *next_range = Some(range.location);
                break;
            }
            match range.overlap(nr_bytes, location) {
                Overlap::None => {}
                Overlap::Partial => overlapping.push(Arc::clone(range)),
                Overlap::Full => {
                    if let Some(read) = self.try_read_from_file_range(range, nr_bytes, location)? {
                        return Ok(Some(read));
                    }
                }
            }
        }
        Ok(None)
    }

    /// The bytes of a range that holds them all, unless it was evicted
    /// (`TryReadFromFileRange`).
    fn try_read_from_file_range(
        &self,
        range: &CachedFileRange,
        nr_bytes: u64,
        location: u64,
    ) -> Result<Option<CachedRead>> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };
        Ok(cache.buffer_manager().try_pin(&range.block)?.map(|handle| {
            CachedRead::pinned(handle, (location - range.location) as usize, nr_bytes as usize)
        }))
    }

    /// Adds `range` (pinned by `pin`) to the cache, dropping the ranges it contains;
    /// returns the first `len` bytes, or those of a range another thread added meanwhile
    /// that holds them all (`TryInsertFileRange`).
    fn try_insert_file_range(
        &self,
        cached_file: &CachedFile,
        pin: BufferHandle,
        len: usize,
        location: u64,
        range: Arc<CachedFileRange>,
    ) -> Result<CachedRead> {
        let mut state = cached_file.write();
        let this_end = location + range.nr_bytes;
        let start = state
            .ranges
            .range(..location)
            .next_back()
            .map_or(location, |(&start, _)| start);
        let candidates: Vec<u64> = state
            .ranges
            .range(start..)
            .take_while(|(_, existing)| existing.location < this_end)
            .map(|(&key, _)| key)
            .collect();
        for key in candidates {
            let Some(existing) = state.ranges.get(&key).map(Arc::clone) else {
                continue;
            };
            if existing.overlap(range.nr_bytes, range.location) == Overlap::Full {
                // Another thread read a range holding this one in the meantime.
                if let Some(read) = self.try_read_from_file_range(&existing, len as u64, location)? {
                    return Ok(read);
                }
                state.ranges.remove(&key);
                continue;
            }
            if range.overlap(existing.nr_bytes, existing.location) == Overlap::Full {
                // The new range makes this one obsolete.
                state.ranges.remove(&key);
            }
        }
        state.ranges.insert(location, range);
        Ok(CachedRead::pinned(pin, 0, len))
    }

    /// Fills `buffer` with the file's bytes at `location`: the parts `overlapping` hold
    /// are copied from them, the rest is read from the file (`ReadAndCopyInterleaved`
    /// with `actually_read`, as a local file always prefers).
    fn read_and_copy_interleaved(
        &self,
        overlapping: &[Arc<CachedFileRange>],
        buffer: &mut [u8],
        location: u64,
    ) -> Result<()> {
        let nr_bytes = buffer.len() as u64;
        let mut current_location = location;
        let mut remaining_bytes = nr_bytes;
        for range in overlapping {
            if remaining_bytes == 0 {
                break;
            }
            if range.location > current_location {
                // Read from the file up to the range.
                let offset = (nr_bytes - remaining_bytes) as usize;
                let bytes_to_read = (range.location - current_location).min(remaining_bytes);
                if bytes_to_read > 0 {
                    self.read_direct(
                        &mut buffer[offset..offset + bytes_to_read as usize],
                        current_location,
                    )?;
                }
                current_location += bytes_to_read;
                remaining_bytes -= bytes_to_read;
            }
            if range.overlap(remaining_bytes, current_location) == Overlap::None {
                continue;
            }
            let Some(pin) = self.try_read_from_file_range(range, range.nr_bytes, range.location)? else {
                continue;
            };
            let range_offset = current_location - range.location;
            if range_offset >= range.nr_bytes {
                continue;
            }
            let bytes_to_copy = (range.nr_bytes - range_offset).min(remaining_bytes);
            let offset = (nr_bytes - remaining_bytes) as usize;
            let source = &pin.bytes()[range_offset as usize..(range_offset + bytes_to_copy) as usize];
            buffer[offset..offset + bytes_to_copy as usize].copy_from_slice(source);
            current_location += bytes_to_copy;
            remaining_bytes -= bytes_to_copy;
        }
        if remaining_bytes != 0 {
            let offset = (nr_bytes - remaining_bytes) as usize;
            self.read_direct(&mut buffer[offset..], current_location)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_buffer::{BufferManager, BufferManagerConfig};

    /// A file of `len` bytes whose byte `i` is `i % 251`, unique to the test.
    fn test_file(name: &str, len: usize) -> String {
        let path = std::env::temp_dir().join(format!("rustdb-efc-{name}-{}", std::process::id()));
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn cache() -> Arc<ExternalFileCache> {
        let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
        Arc::new(ExternalFileCache::new(buffer_manager, true))
    }

    fn open(
        cache: &Arc<ExternalFileCache>,
        path: impl AsRef<[u8]>,
        mode: CacheValidationMode,
    ) -> CachingFileHandle {
        CachingFileHandle::open(Some(Arc::clone(cache)), path, mode).unwrap()
    }

    fn ranges(cache: &ExternalFileCache) -> Vec<(u64, u64)> {
        let mut ranges: Vec<(u64, u64)> = cache
            .cached_file_information()
            .into_iter()
            .map(|range| (range.location, range.nr_bytes))
            .collect();
        ranges.sort_unstable();
        ranges
    }

    fn expected(location: u64, len: u64) -> Vec<u8> {
        (location..location + len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn reads_are_cached_and_merged() {
        let path = test_file("merge", 100_000);
        let cache = cache();
        let handle = open(&cache, &path, CacheValidationMode::ValidateAll);
        assert_eq!(handle.read(1000, 0).unwrap().bytes(), expected(0, 1000));
        assert_eq!(handle.read(1000, 50_000).unwrap().bytes(), expected(50_000, 1000));
        assert_eq!(ranges(&cache), vec![(0, 1000), (50_000, 1000)]);
        // A read inside a cached range is served from it.
        assert_eq!(handle.read(100, 200).unwrap().bytes(), expected(200, 100));
        assert_eq!(ranges(&cache), vec![(0, 1000), (50_000, 1000)]);
        // A read covering both replaces them, copying the cached parts.
        assert_eq!(handle.read(60_000, 0).unwrap().bytes(), expected(0, 60_000));
        assert_eq!(ranges(&cache), vec![(0, 60_000)]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn small_gaps_to_the_next_range_are_filled() {
        let path = test_file("gap", 10_000);
        let cache = cache();
        let handle = open(&cache, &path, CacheValidationMode::ValidateAll);
        handle.read(100, 1000).unwrap();
        // 500 bytes up to the cached range, no more than the read itself: filled.
        assert_eq!(handle.read(500, 0).unwrap().bytes(), expected(0, 500));
        assert_eq!(ranges(&cache), vec![(0, 1000), (1000, 100)]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn changed_files_are_read_again_unless_validation_is_off() {
        let path = test_file("validate", 4096);
        let cache = cache();
        let first = open(&cache, &path, CacheValidationMode::ValidateAll);
        assert_eq!(first.read(16, 0).unwrap().bytes(), expected(0, 16));
        // Rewritten with other bytes and another size: a new version.
        std::fs::write(&path, vec![7u8; 2048]).unwrap();
        let stale = open(&cache, &path, CacheValidationMode::NoValidation);
        assert_eq!(stale.file_size().unwrap(), 4096);
        assert_eq!(stale.read(16, 0).unwrap().bytes(), expected(0, 16));
        let fresh = open(&cache, &path, CacheValidationMode::ValidateAll);
        assert_eq!(fresh.file_size().unwrap(), 2048);
        assert_eq!(fresh.read(16, 0).unwrap().bytes(), vec![7u8; 16]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_disabled_cache_keeps_nothing() {
        let path = test_file("disabled", 4096);
        let cache = cache();
        let handle = open(&cache, &path, CacheValidationMode::ValidateAll);
        handle.read(100, 0).unwrap();
        assert_eq!(cache.cached_file_count(), 1);
        cache.set_enabled(false);
        assert_eq!(cache.cached_file_count(), 0);
        assert!(!handle.caches_reads());
        assert_eq!(handle.read(100, 100).unwrap().bytes(), expected(100, 100));
        assert_eq!(ranges(&cache), Vec::new());
        cache.set_enabled(true);
        assert_eq!(handle.read(100, 100).unwrap().bytes(), expected(100, 100));
        assert_eq!(ranges(&cache), vec![(100, 100)]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn version_validation_rules() {
        // Version tags decide when either side has one.
        assert!(ExternalFileCache::is_valid(true, "a", 0, "a", 1));
        assert!(!ExternalFileCache::is_valid(true, "a", 0, "b", 0));
        // Without, the modification time must match and be more than 10 seconds old.
        assert!(!ExternalFileCache::is_valid(true, "", 1, "", 2));
        assert!(ExternalFileCache::is_valid(true, "", 1, "", 1));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
            });
        assert!(!ExternalFileCache::is_valid(true, "", now, "", now));
        assert!(ExternalFileCache::is_valid(false, "a", 0, "b", 1));
    }
}
