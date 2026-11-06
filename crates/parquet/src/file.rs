//! Reading a Parquet file through the external file cache (DuckDB's `ParquetReader`
//! reads through a `CachingFileHandle`): positional reads, prefetched ranges and the
//! footer (`LoadMetadata` and `ParseParquetFooter`).

use std::sync::{Arc, OnceLock};

use rustdb_common::{Error, Result};
use rustdb_function::caching_file_system::{CachedRead, CachingFileHandle};
use rustdb_function::external_file_cache::{CacheValidationMode, ExternalFileCache};

use crate::thrift::{FileMetaData, ThriftValue};

/// An open Parquet file. Reads are positional, so one handle serves every thread.
#[derive(Debug)]
pub struct ParquetFile {
    handle: CachingFileHandle,
    size: u64,
}

impl ParquetFile {
    /// Opens `path` for reading through `cache` (`CachingFileSystem::OpenFile`).
    pub fn open(
        path: &[u8],
        cache: Option<Arc<ExternalFileCache>>,
        validation: CacheValidationMode,
    ) -> Result<Self> {
        let handle = CachingFileHandle::open(cache, path, validation)?;
        let size = handle.file_size()?;
        Ok(Self { handle, size })
    }

    pub fn path(&self) -> &[u8] {
        self.handle.path()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The file's handle in the external file cache.
    pub fn handle(&self) -> &CachingFileHandle {
        &self.handle
    }

    /// Fills `buffer` from offset `offset`, bypassing the cache.
    pub fn read_exact_at(&self, buffer: &mut [u8], offset: u64) -> Result<()> {
        self.handle.read_direct(buffer, offset)
    }

    /// The bytes `offset..offset + len`, bypassing the cache.
    pub fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0; len];
        self.read_exact_at(&mut buffer, offset)?;
        Ok(buffer)
    }

    /// The `len` bytes at `offset`, through the external file cache (a prefetch of
    /// `ThriftFileTransport`).
    pub fn read_cached(&self, offset: u64, len: u64) -> Result<CachedRead> {
        self.handle.read(len, offset)
    }
}

/// A range of the file to prefetch, read through the cache on first use (DuckDB's
/// `ReadHead`).
#[derive(Debug)]
pub struct ReadHead {
    pub location: u64,
    pub size: u64,
    data: OnceLock<CachedRead>,
}

impl ReadHead {
    fn new(location: u64, size: u64) -> Self {
        Self {
            location,
            size,
            data: OnceLock::new(),
        }
    }

    /// `GetEnd`.
    pub fn end(&self) -> u64 {
        self.location + self.size
    }

    /// The bytes of the range, reading them through the cache if not done yet.
    pub fn bytes(&self, file: &ParquetFile) -> Result<&[u8]> {
        if self.data.get().is_none() {
            let read = file.read_cached(self.location, self.size)?;
            let _ = self.data.set(read);
        }
        self.data
            .get()
            .map(CachedRead::bytes)
            .ok_or_else(|| Error::internal("prefetched range without data"))
    }

    /// Reads the range now (`ReadAheadBuffer::Prefetch`).
    fn prefetch(&self, file: &ParquetFile) -> Result<()> {
        self.bytes(file).map(|_| ())
    }
}

/// The ranges registered for prefetching, consecutive ones merged (DuckDB's two-step
/// `ReadAheadBuffer`: register the ranges, then prefetch them).
#[derive(Debug, Default)]
pub struct ReadAheadBuffer {
    /// The read heads, the newest first (`read_heads`).
    heads: Vec<Arc<ReadHead>>,
    /// The ranges registered since the last `finalize_registration`, the newest first,
    /// and whether others may merge into them (`merge_set`).
    registered: Vec<(u64, u64, bool)>,
}

impl ReadAheadBuffer {
    /// Ranges within this many bytes of each other merge (`ReadHeadComparator::ALLOW_GAP`).
    const ALLOW_GAP: u64 = 1 << 14;

    /// Whether neither of two ranges lies wholly before the other, gap included (the
    /// equivalence of `ReadHeadComparator`).
    fn mergeable(a: (u64, u64), b: (u64, u64)) -> bool {
        let before = |a: (u64, u64), b: (u64, u64)| {
            let a_end = a.0 + a.1;
            let a_end = if a_end <= u64::MAX - Self::ALLOW_GAP {
                a_end + Self::ALLOW_GAP
            } else {
                a_end
            };
            a.0 < b.0 && a_end < b.0
        };
        !before(a, b) && !before(b, a)
    }

    /// Registers `len` bytes at `pos` (`AddReadHead`); with `merge`, into a registered
    /// range it touches or overlaps.
    pub fn add_read_head(&mut self, file: &ParquetFile, pos: u64, len: u64, merge: bool) -> Result<()> {
        if merge
            && let Some(existing) = self
                .registered
                .iter_mut()
                .find(|(start, size, mergeable)| *mergeable && Self::mergeable((*start, *size), (pos, len)))
        {
            let new_start = existing.0.min(pos);
            let new_end = (existing.0 + existing.1).max(pos + len);
            *existing = (new_start, new_end - new_start, true);
            return Ok(());
        }
        self.registered.insert(0, (pos, len, merge));
        if pos + len > file.size() {
            return Err(Error::invalid_input(rustdb_common::byte_format!(
                "Prefetch registered for bytes outside file: {}, attempted range: [{pos}, {}), file size: {}",
                file.path(),
                pos + len,
                file.size()
            )));
        }
        Ok(())
    }

    /// Ends the registration: no more merging (`FinalizeRegistration`).
    pub fn finalize_registration(&mut self) {
        let registered: Vec<Arc<ReadHead>> = self
            .registered
            .drain(..)
            .map(|(pos, len, _)| Arc::new(ReadHead::new(pos, len)))
            .collect();
        self.heads.splice(0..0, registered);
    }

    /// Registers, then reads `len` bytes at `pos` (`ThriftFileTransport::Prefetch`).
    pub fn prefetch_range(&mut self, file: &ParquetFile, pos: u64, len: u64) -> Result<()> {
        self.add_read_head(file, pos, len, false)?;
        self.finalize_registration();
        self.prefetch(file)
    }

    /// Reads every registered range (`Prefetch`).
    pub fn prefetch(&self, file: &ParquetFile) -> Result<()> {
        for head in &self.heads {
            if head.end() > file.size() {
                return Err(Error::invalid_input(
                    "Prefetch registered requested for bytes outside file",
                ));
            }
            head.prefetch(file)?;
        }
        Ok(())
    }

    /// The range holding `pos` (`GetReadHead`).
    pub fn read_head(&self, pos: u64) -> Option<&Arc<ReadHead>> {
        self.heads
            .iter()
            .find(|head| pos >= head.location && pos < head.end())
    }

    /// Drops the ranges (`ClearPrefetch`): they stay cached, but unpinned.
    pub fn clear(&mut self) {
        self.heads.clear();
        self.registered.clear();
    }

    /// `HasPrefetch`.
    pub fn has_prefetch(&self) -> bool {
        !self.heads.is_empty() || !self.registered.is_empty()
    }
}

/// The parsed footer of a file (DuckDB's `ParquetFileMetadataCache`).
#[derive(Debug)]
pub struct FileMetadata {
    pub metadata: FileMetaData,
    /// The length of the Thrift-encoded footer.
    pub footer_len: u32,
    /// How to decrypt the file's modules, if its footer is encrypted.
    pub decryption: Option<crate::crypto::FileDecryption>,
}

/// `LoadMetadata`: checks the magic bytes and parses the footer. The end of the file is
/// read through the cache: its last 8 bytes, or with `prefetch` (`ShouldAndCanPrefetch`)
/// about a thousandth of the file to save a second read; the footer then comes from that
/// range, or from a second one if it is larger.
pub fn load_metadata(
    file: &ParquetFile,
    key: Option<&crate::crypto::FooterKey>,
    prefetch: bool,
) -> Result<FileMetadata> {
    /// Estimate 1/1000th of the file to be footer (`ESTIMATED_FOOTER_RATIO`).
    const ESTIMATED_FOOTER_RATIO: u64 = 1000;
    const MIN_PREFETCH_SIZE: u64 = 16384;
    const MAX_PREFETCH_SIZE: u64 = 262_144;
    let path = file.path();
    let size = file.size();
    if size < 12 {
        return Err(Error::invalid_input(rustdb_common::byte_format!(
            "File '{path}' too small to be a Parquet file"
        )));
    }
    let mut prefetch_size = 8;
    if prefetch {
        prefetch_size = (size / ESTIMATED_FOOTER_RATIO).clamp(MIN_PREFETCH_SIZE, MAX_PREFETCH_SIZE);
        prefetch_size = prefetch_size.next_power_of_two().min(size);
    }
    let end = file.read_cached(size - prefetch_size, prefetch_size)?;
    let end_bytes = end.bytes();
    let mut tail = [0u8; 8];
    tail.copy_from_slice(&end_bytes[end_bytes.len() - 8..]);
    // `ParseParquetFooter`.
    let footer_encrypted = match &tail[4..] {
        b"PAR1" => {
            if key.is_some() {
                return Err(Error::invalid_input(rustdb_common::byte_format!(
                    "File '{path}' is not encrypted, but 'encryption_config' was set"
                )));
            }
            false
        }
        b"PARE" => {
            if key.is_none() {
                return Err(Error::invalid_input(rustdb_common::byte_format!(
                    "File '{path}' is encrypted, but 'encryption_config' was not set"
                )));
            }
            true
        }
        _ => {
            return Err(Error::invalid_input(rustdb_common::byte_format!(
                "No magic bytes found at end of file '{path}'"
            )));
        }
    };
    let footer_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if footer_len == 0 || size < 12 + u64::from(footer_len) {
        return Err(Error::invalid_input(rustdb_common::byte_format!(
            "Footer length error in file '{path}'"
        )));
    }
    let metadata_pos = size - 8 - u64::from(footer_len);
    let second_read;
    let footer: &[u8] = if u64::from(footer_len) > prefetch_size - 8 {
        second_read = file.read_cached(metadata_pos, u64::from(footer_len))?;
        second_read.bytes()
    } else {
        let start = (metadata_pos - (size - prefetch_size)) as usize;
        &end_bytes[start..start + footer_len as usize]
    };
    let mut reader = crate::thrift::CompactReader::new(footer);
    let mut decryption = None;
    let metadata = match key {
        Some(key) if footer_encrypted => {
            let crypto = crate::thrift::FileCryptoMetaData::read(&mut reader)?;
            if crypto.encryption_algorithm.aes_gcm_ctr_v1.is_some() {
                return Err(Error::invalid_input(rustdb_common::byte_format!(
                    "File '{path}' is encrypted with AES_GCM_CTR_V1, but only AES_GCM_V1 is supported"
                )));
            }
            let file_aad = crypto
                .encryption_algorithm
                .aes_gcm_v1
                .as_ref()
                .and_then(|aes| aes.aad_file_unique.as_ref())
                .map(|unique| unique.0.clone())
                .unwrap_or_default();
            let aad = crate::crypto::module_aad(&file_aad, crate::crypto::module::FOOTER, -1, -1, -1)?;
            let (plain, _) = crate::crypto::decrypt_module(&key.0, &aad, &footer[reader.position()..])?;
            decryption = Some(crate::crypto::FileDecryption {
                key: std::sync::Arc::clone(&key.0),
                file_aad,
            });
            FileMetaData::read(&mut crate::thrift::CompactReader::new(&plain))?
        }
        _ => FileMetaData::read(&mut reader)?,
    };
    if metadata.num_rows < 0 {
        return Err(Error::invalid_input(rustdb_common::byte_format!(
            "Failed to read Parquet file \"{path}\": has negative number of rows"
        )));
    }
    Ok(FileMetadata {
        metadata,
        footer_len,
        decryption,
    })
}
