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

fn io_error(message: String) -> Error {
    Error::new(ErrorKind::Io, message)
}

/// An open local file with what the cache validates it by.
#[derive(Debug)]
struct OpenFile {
    file: File,
    size: u64,
    /// Microseconds since the epoch, at a resolution of seconds (`Timestamp::FromEpochSeconds`
    /// of `st_mtime`).
    last_modified: i64,
    /// The device, inode, size and modification time (`GetPosixVersionTag`), hex-encoded.
    version_tag: String,
}

impl OpenFile {
    /// `LocalFileSystem::OpenFile` for reading, with its errors.
    fn open(path: &str) -> Result<Self> {
        crate::file_system::verify_can_access_file(path)?;
        let file = File::open(path).map_err(|error| {
            let reason = match error.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
                std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
                _ => error.to_string(),
            };
            io_error(format!("Cannot open file \"{path}\": {reason}"))
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error(format!("Could not get file size of \"{path}\": {error}")))?;
        if metadata.is_dir() {
            return Err(io_error(format!("Cannot open file \"{path}\": Is a directory")));
        }
        let (last_modified, version_tag) = file_version(&metadata);
        Ok(Self {
            file,
            size: metadata.len(),
            last_modified,
            version_tag,
        })
    }

    /// `LocalFileSystem::Read` of `buffer.len()` bytes at `location`.
    fn read_exact_at(&self, path: &str, buffer: &mut [u8], location: u64) -> Result<()> {
        let end = location.checked_add(buffer.len() as u64);
        if end.is_none_or(|end| end > self.size) {
            return Err(io_error(format!(
                "Could not read enough bytes from file \"{path}\": attempted to read {} bytes from location {location}",
                buffer.len()
            )));
        }
        read_exact_at(&self.file, buffer, location)
            .map_err(|error| io_error(format!("Could not read from file \"{path}\": {error}")))
    }
}

/// The modification time (microseconds, whole seconds) and version tag of a file.
#[cfg(unix)]
fn file_version(metadata: &std::fs::Metadata) -> (i64, String) {
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
fn file_version(metadata: &std::fs::Metadata) -> (i64, String) {
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
    path: String,
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
        path: &str,
        validation: CacheValidationMode,
    ) -> Result<Self> {
        let cached_file = cache.as_ref().map(|cache| cache.get_or_create_cached_file(path));
        let handle = Self {
            validate: validation.should_validate(path),
            path: path.to_owned(),
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

    pub fn path(&self) -> &str {
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
            state.on_disk_file = true;
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
