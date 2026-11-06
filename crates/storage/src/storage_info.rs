//! The database file format constants and headers (DuckDB's `storage_info.hpp/.cpp`).
//!
//! A database file starts with three 4 KiB header blocks: the main header (magic bytes,
//! version, flags) and two database headers, of which the one with the higher iteration
//! is active. Every header and every block starts with an 8-byte checksum. Blocks start
//! at [`BLOCK_START`].

use rustdb_common::{Error, ErrorKind, Result};

/// Size of each of the three header blocks (`Storage::FILE_HEADER_SIZE`).
pub const FILE_HEADER_SIZE: usize = 4096;
/// File offset of block 0 (`Storage::BLOCK_START`).
pub const BLOCK_START: u64 = 3 * FILE_HEADER_SIZE as u64;
/// The checksum in front of every block (`Storage::DEFAULT_BLOCK_HEADER_SIZE`).
pub const DEFAULT_BLOCK_HEADER_SIZE: usize = 8;
/// `Storage::DEFAULT_BLOCK_ALLOC_SIZE`.
pub const DEFAULT_BLOCK_ALLOC_SIZE: usize = 262_144;
/// `Storage::MIN_BLOCK_ALLOC_SIZE` / `MAX_BLOCK_ALLOC_SIZE`.
pub const MIN_BLOCK_ALLOC_SIZE: usize = 16_384;
pub const MAX_BLOCK_ALLOC_SIZE: usize = 262_144;
/// The vector size recorded in database headers.
pub const STANDARD_VECTOR_SIZE: u64 = rustdb_common::STANDARD_VECTOR_SIZE as u64;
/// "No block" in headers and pointers (`INVALID_BLOCK` as an `idx_t`).
pub const INVALID_BLOCK_INDEX: u64 = u64::MAX;

/// The version number new files are written with (`VERSION_NUMBER`).
pub const VERSION_NUMBER: u64 = 64;
/// The range of version numbers that can be read.
pub const VERSION_NUMBER_LOWER: u64 = 64;
pub const VERSION_NUMBER_UPPER: u64 = 68;
/// `MainHeader::DEPRECATED_VERSION_NUMBER`.
pub const DEPRECATED_VERSION_NUMBER: u64 = 999;

/// The magic bytes at offset 8 (`MainHeader::MAGIC_BYTES`).
pub const MAGIC_BYTES: &[u8; 4] = b"DUCK";
pub const MAGIC_BYTE_OFFSET: usize = DEFAULT_BLOCK_HEADER_SIZE;
pub const MAX_VERSION_SIZE: usize = 32;
pub const FLAG_COUNT: usize = 4;
pub const ENCRYPTION_METADATA_LEN: usize = 8;
pub const DB_IDENTIFIER_LEN: usize = 16;
pub const CANARY_BYTE_SIZE: usize = 8;
pub const AES_NONCE_LEN: usize = 12;
pub const AES_TAG_LEN: usize = 16;
/// Flag bit 0: the file is encrypted (`MainHeader::ENCRYPTED_DATABASE_FLAG`).
pub const ENCRYPTED_DATABASE_FLAG: u64 = 1;
/// The known plaintext whose encryption the main header of an encrypted file holds, to
/// recognise a wrong key (`MainHeader::CANARY`, "DUCKKEY" with its NUL).
pub const CANARY: &[u8; CANARY_BYTE_SIZE] = b"DUCKKEY\0";

/// The library version and source id written into new files.
pub const LIBRARY_VERSION: &str = "v1.5.6";
pub const SOURCE_ID: &str = "982c372";

/// The serialization version new files use (`SerializationCompatibility::Default`).
pub const DEFAULT_SERIALIZATION_VERSION: u64 = 1;
/// The newest serialization version this build reads and writes.
pub const LATEST_SERIALIZATION_VERSION: u64 = 7;

/// DuckDB releases and the storage version numbers they write (generated in DuckDB by
/// `scripts/generate_storage_info.py`).
const STORAGE_VERSION_INFO: &[(&str, u64)] = &[
    ("v0.0.4", 1),
    ("v0.1.0", 1),
    ("v0.1.1", 1),
    ("v0.1.2", 1),
    ("v0.1.3", 1),
    ("v0.1.4", 1),
    ("v0.1.5", 1),
    ("v0.1.6", 1),
    ("v0.1.7", 1),
    ("v0.1.8", 1),
    ("v0.1.9", 1),
    ("v0.2.0", 1),
    ("v0.2.1", 1),
    ("v0.2.2", 4),
    ("v0.2.3", 6),
    ("v0.2.4", 11),
    ("v0.2.5", 13),
    ("v0.2.6", 15),
    ("v0.2.7", 17),
    ("v0.2.8", 18),
    ("v0.2.9", 21),
    ("v0.3.0", 25),
    ("v0.3.1", 27),
    ("v0.3.2", 31),
    ("v0.3.3", 33),
    ("v0.3.4", 33),
    ("v0.3.5", 33),
    ("v0.4.0", 33),
    ("v0.5.0", 38),
    ("v0.5.1", 38),
    ("v0.6.0", 39),
    ("v0.6.1", 39),
    ("v0.7.0", 43),
    ("v0.7.1", 43),
    ("v0.8.0", 51),
    ("v0.8.1", 51),
    ("v0.9.0", 64),
    ("v0.9.1", 64),
    ("v0.9.2", 64),
    ("v0.10.0", 64),
    ("v0.10.1", 64),
    ("v0.10.2", 64),
    ("v0.10.3", 64),
    ("v1.0.0", 64),
    ("v1.1.0", 64),
    ("v1.1.1", 64),
    ("v1.1.2", 64),
    ("v1.1.3", 64),
    ("v1.2.0", 65),
    ("v1.2.1", 65),
    ("v1.2.2", 65),
    ("v1.3.0", 66),
    ("v1.3.1", 66),
    ("v1.3.2", 66),
    ("v1.4.0", 67),
    ("v1.4.1", 67),
    ("v1.4.2", 67),
    ("v1.4.3", 67),
    ("v1.4.4", 67),
    ("v1.4.5", 67),
    ("v1.5.0", 68),
    ("v1.5.1", 68),
    ("v1.5.2", 68),
    ("v1.5.3", 68),
    ("v1.5.4", 68),
    ("v1.5.5", 68),
    ("v1.5.6", 68),
];

/// DuckDB releases and their serialization versions.
const SERIALIZATION_VERSION_INFO: &[(&str, u64)] = &[
    ("v0.10.0", 1),
    ("v0.10.1", 1),
    ("v0.10.2", 1),
    ("v0.10.3", 2),
    ("v1.0.0", 2),
    ("v1.1.0", 3),
    ("v1.1.1", 3),
    ("v1.1.2", 3),
    ("v1.1.3", 3),
    ("v1.2.0", 4),
    ("v1.2.1", 4),
    ("v1.2.2", 4),
    ("v1.3.0", 5),
    ("v1.3.1", 5),
    ("v1.3.2", 5),
    ("v1.4.0", 6),
    ("v1.4.1", 6),
    ("v1.4.2", 6),
    ("v1.4.3", 6),
    ("v1.4.4", 6),
    ("v1.4.5", 6),
    ("v1.5.0", 7),
    ("v1.5.1", 7),
    ("v1.5.2", 7),
    ("v1.5.3", 7),
    ("v1.5.4", 7),
    ("v1.5.5", 7),
    ("v1.5.6", 7),
    ("latest", 7),
];

/// The releases that wrote storage version `version_number`, as "a, b or c"
/// (`GetDuckDBVersions`).
pub fn duckdb_versions(version_number: u64) -> String {
    let versions: Vec<&str> = STORAGE_VERSION_INFO
        .iter()
        .filter(|(_, version)| *version == version_number)
        .map(|(name, _)| *name)
        .collect();
    let mut result = String::new();
    for (i, version) in versions.iter().enumerate() {
        if i > 0 {
            result.push_str(if i + 1 == versions.len() { " or " } else { ", " });
        }
        result.push_str(version);
    }
    result
}

/// The storage version number of a release name (`GetStorageVersion`).
pub fn storage_version(version_name: &str) -> Option<u64> {
    STORAGE_VERSION_INFO
        .iter()
        .find(|(name, _)| *name == version_name)
        .map(|&(_, version)| version)
}

/// The serialization version of a release name (`GetSerializationVersion`).
pub fn serialization_version(version_name: &str) -> Option<u64> {
    SERIALIZATION_VERSION_INFO
        .iter()
        .find(|(name, _)| *name == version_name)
        .map(|&(_, version)| version)
}

/// The oldest release using `serialization_version`, with a `+` suffix if asked
/// (`GetStorageVersionName`).
pub fn storage_version_name(serialization_version: u64, add_suffix: bool) -> String {
    if serialization_version < 4 {
        return "v1.0.0+".to_owned();
    }
    let Some((name, _)) = SERIALIZATION_VERSION_INFO
        .iter()
        .find(|(name, version)| *name != "latest" && *version == serialization_version)
    else {
        return "--UNKNOWN--".to_owned();
    };
    if add_suffix {
        format!("{name}+")
    } else {
        (*name).to_owned()
    }
}

/// The error for a file whose version cannot be read (`ShowUnsupportedStorageVersionError`).
pub fn unsupported_version_error(version_number: u64) -> Error {
    let versions = duckdb_versions(version_number);
    let version_text = if !versions.is_empty() {
        format!("DuckDB version {versions}")
    } else if version_number > VERSION_NUMBER_UPPER {
        "a newer version of DuckDB".to_owned()
    } else {
        "an older development version of DuckDB".to_owned()
    };
    Error::new(
        ErrorKind::Io,
        format!(
            "Trying to read a database file with version number {version_number}, but we can only read versions \
             between {VERSION_NUMBER_LOWER} and {VERSION_NUMBER_UPPER}.\nThe database file was created with \
             {version_text}.\n\nNewer DuckDB version might introduce backward incompatible changes (possibly guarded \
             by compatibility settings).\nSee the storage page for migration strategy and more information: \
             https://duckdb.org/internals/storage"
        ),
    )
}

/// A little-endian reader over a header block.
struct HeaderReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> HeaderReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn read_bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.position + N;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or_else(|| Error::new(ErrorKind::Io, "database header is truncated"))?;
        self.position = end;
        let mut result = [0; N];
        result.copy_from_slice(bytes);
        Ok(result)
    }

    fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.read_bytes::<8>()?))
    }
}

/// The main header (DuckDB's `MainHeader`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainHeader {
    pub version_number: u64,
    pub flags: [u64; FLAG_COUNT],
    pub library_git_desc: [u8; MAX_VERSION_SIZE],
    pub library_git_hash: [u8; MAX_VERSION_SIZE],
    pub encryption_metadata: [u8; ENCRYPTION_METADATA_LEN],
    pub db_identifier: [u8; DB_IDENTIFIER_LEN],
    pub encrypted_canary: [u8; CANARY_BYTE_SIZE],
    pub canary_iv: [u8; AES_NONCE_LEN],
    pub canary_tag: [u8; AES_TAG_LEN],
}

/// A fixed-size, zero-padded version string.
fn version_bytes(version: &str) -> [u8; MAX_VERSION_SIZE] {
    let mut result = [0; MAX_VERSION_SIZE];
    let len = version.len().min(MAX_VERSION_SIZE);
    result[..len].copy_from_slice(&version.as_bytes()[..len]);
    result
}

impl MainHeader {
    /// The header of a new file (`ConstructMainHeader`).
    pub fn new(version_number: u64) -> Self {
        Self {
            version_number,
            flags: [0; FLAG_COUNT],
            library_git_desc: version_bytes(LIBRARY_VERSION),
            library_git_hash: version_bytes(SOURCE_ID),
            encryption_metadata: [0; ENCRYPTION_METADATA_LEN],
            db_identifier: [0; DB_IDENTIFIER_LEN],
            encrypted_canary: [0; CANARY_BYTE_SIZE],
            canary_iv: [0; AES_NONCE_LEN],
            canary_tag: [0; AES_TAG_LEN],
        }
    }

    /// Whether flag bit 0 (`MainHeader::ENCRYPTED_DATABASE_FLAG`) is set.
    pub fn is_encrypted(&self) -> bool {
        self.flags[0] & ENCRYPTED_DATABASE_FLAG != 0
    }

    /// `SetEncrypted`.
    pub fn set_encrypted(&mut self) {
        self.flags[0] |= ENCRYPTED_DATABASE_FLAG;
    }

    /// The cipher byte of the encryption metadata (`GetEncryptionCipher`).
    pub fn encryption_cipher(&self) -> u8 {
        self.encryption_metadata[2]
    }

    /// The encryption version byte of the encryption metadata (`GetEncryptionVersion`).
    pub fn encryption_version(&self) -> u8 {
        self.encryption_metadata[3]
    }

    /// `MainHeader::Write`: the bytes after the block header.
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC_BYTES);
        out.extend_from_slice(&self.version_number.to_le_bytes());
        for flag in self.flags {
            out.extend_from_slice(&flag.to_le_bytes());
        }
        out.extend_from_slice(&self.library_git_desc);
        out.extend_from_slice(&self.library_git_hash);
        out.extend_from_slice(&self.encryption_metadata);
        out.extend_from_slice(&self.db_identifier);
        out.extend_from_slice(&self.encrypted_canary);
        out.extend_from_slice(&self.canary_iv);
        out.extend_from_slice(&self.canary_tag);
    }

    /// `MainHeader::Read`, checking the magic bytes and the version.
    pub fn read(data: &[u8]) -> Result<Self> {
        let mut reader = HeaderReader::new(data);
        if &reader.read_bytes::<4>()? != MAGIC_BYTES {
            return Err(Error::new(
                ErrorKind::Io,
                "The file is not a valid DuckDB database file!",
            ));
        }
        let version_number = reader.read_u64()?;
        if version_number != DEPRECATED_VERSION_NUMBER
            && !(VERSION_NUMBER_LOWER..=VERSION_NUMBER_UPPER).contains(&version_number)
        {
            return Err(unsupported_version_error(version_number));
        }
        let mut flags = [0; FLAG_COUNT];
        for flag in &mut flags {
            *flag = reader.read_u64()?;
        }
        Ok(Self {
            version_number,
            flags,
            library_git_desc: reader.read_bytes()?,
            library_git_hash: reader.read_bytes()?,
            encryption_metadata: reader.read_bytes()?,
            db_identifier: reader.read_bytes()?,
            encrypted_canary: reader.read_bytes()?,
            canary_iv: reader.read_bytes()?,
            canary_tag: reader.read_bytes()?,
        })
    }
}

/// One of the two database headers (DuckDB's `DatabaseHeader`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseHeader {
    /// Incremented by every checkpoint; the higher one is active.
    pub iteration: u64,
    /// The metadata pointer of the catalog, or [`INVALID_BLOCK_INDEX`].
    pub meta_block: u64,
    /// The metadata pointer of the free list, or [`INVALID_BLOCK_INDEX`].
    pub free_list: u64,
    /// The number of blocks in the file.
    pub block_count: u64,
    pub block_alloc_size: u64,
    pub vector_size: u64,
    pub serialization_compatibility: u64,
}

impl DatabaseHeader {
    /// `DatabaseHeader::Write`.
    pub fn write(&self, out: &mut Vec<u8>) {
        for value in [
            self.iteration,
            self.meta_block,
            self.free_list,
            self.block_count,
            self.block_alloc_size,
            self.vector_size,
            self.serialization_compatibility,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// `DatabaseHeader::Read`.
    pub fn read(main_header: &MainHeader, data: &[u8]) -> Result<Self> {
        let mut reader = HeaderReader::new(data);
        let iteration = reader.read_u64()?;
        let meta_block = reader.read_u64()?;
        let free_list = reader.read_u64()?;
        let block_count = reader.read_u64()?;
        let mut block_alloc_size = reader.read_u64()?;
        if block_alloc_size == 0 {
            block_alloc_size = DEFAULT_BLOCK_ALLOC_SIZE as u64;
        }
        let mut vector_size = reader.read_u64()?;
        if vector_size == 0 {
            vector_size = STANDARD_VECTOR_SIZE;
        }
        if vector_size != STANDARD_VECTOR_SIZE {
            return Err(Error::new(
                ErrorKind::Io,
                format!(
                    "Cannot read database file: DuckDB's compiled vector size is {STANDARD_VECTOR_SIZE} bytes, but \
                     the file has a vector size of {vector_size} bytes."
                ),
            ));
        }
        let serialization_compatibility = if main_header.version_number == 64 {
            1
        } else {
            reader.read_u64()?
        };
        Ok(Self {
            iteration,
            meta_block,
            free_list,
            block_count,
            block_alloc_size,
            vector_size,
            serialization_compatibility,
        })
    }
}

/// DuckDB's block checksum (`Checksum(uint8_t *, size_t)`): 64-bit words are mixed with a
/// multiplication, the remainder with a murmur-style hash.
pub fn checksum(buffer: &[u8]) -> u64 {
    let mut result: u64 = 5381;
    let words = buffer.len() / 8;
    for i in 0..words {
        let mut word = [0u8; 8];
        word.copy_from_slice(&buffer[i * 8..i * 8 + 8]);
        result ^= u64::from_le_bytes(word).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    }
    if buffer.len() > words * 8 {
        result ^= checksum_remainder(&buffer[words * 8..]);
    }
    result
}

/// `ChecksumRemainder` (robin-hood-hashing's `hash_bytes`).
fn checksum_remainder(data: &[u8]) -> u64 {
    const M: u64 = 0xc6a4_a793_5bd1_e995;
    const SEED: u64 = 0xe17a_1465;
    const R: u32 = 47;
    let mut h = SEED ^ (data.len() as u64).wrapping_mul(M);
    let blocks = data.len() / 8;
    for i in 0..blocks {
        let mut word = [0u8; 8];
        word.copy_from_slice(&data[i * 8..i * 8 + 8]);
        let mut k = u64::from_le_bytes(word);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h ^= k;
        h = h.wrapping_mul(M);
    }
    let tail = &data[blocks * 8..];
    if !tail.is_empty() {
        for (i, &byte) in tail.iter().enumerate().rev() {
            h ^= u64::from(byte) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> R;
    h = h.wrapping_mul(M);
    h ^= h >> R;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_error_names_the_releases() {
        assert_eq!(duckdb_versions(27), "v0.3.1");
        assert_eq!(duckdb_versions(33), "v0.3.3, v0.3.4, v0.3.5 or v0.4.0");
        let message = unsupported_version_error(27).to_string();
        assert!(message.starts_with(
            "IO Error: Trying to read a database file with version number 27, but we can only read versions between 64 and 68.\nThe database file was created with DuckDB version v0.3.1."
        ));
        assert_eq!(storage_version_name(7, true), "v1.5.0+");
        assert_eq!(storage_version_name(2, false), "v1.0.0+");
    }

    /// The checksums of the headers of an empty database written by DuckDB 1.5.6.
    #[test]
    fn checksums_match_duckdb() {
        let header = DatabaseHeader {
            iteration: 0,
            meta_block: INVALID_BLOCK_INDEX,
            free_list: INVALID_BLOCK_INDEX,
            block_count: 0,
            block_alloc_size: DEFAULT_BLOCK_ALLOC_SIZE as u64,
            vector_size: STANDARD_VECTOR_SIZE,
            serialization_compatibility: 1,
        };
        let mut main = MainHeader::new(64);
        main.library_git_desc = version_bytes("v0.0.1");
        let mut main_bytes = Vec::new();
        main.write(&mut main_bytes);
        main_bytes.resize(FILE_HEADER_SIZE - DEFAULT_BLOCK_HEADER_SIZE, 0);
        assert_eq!(checksum(&main_bytes), 0x9254_a4fa_3371_55c4);
        // A new file's database headers are written over the main header's buffer.
        let mut header_bytes = Vec::new();
        header.write(&mut header_bytes);
        let mut bytes = main_bytes.clone();
        bytes[..header_bytes.len()].copy_from_slice(&header_bytes);
        assert_eq!(checksum(&bytes), 0x83b0_928e_603b_ba67);
        assert_eq!(checksum_remainder(&[1, 2, 3]), checksum(&[1, 2, 3]) ^ 0x1505);
    }

    #[test]
    fn database_headers_round_trip() {
        let main = MainHeader::new(VERSION_NUMBER);
        let mut bytes = Vec::new();
        main.write(&mut bytes);
        assert_eq!(MainHeader::read(&bytes).unwrap(), main);
        let header = DatabaseHeader {
            iteration: 3,
            meta_block: 7,
            free_list: INVALID_BLOCK_INDEX,
            block_count: 9,
            block_alloc_size: DEFAULT_BLOCK_ALLOC_SIZE as u64,
            vector_size: STANDARD_VECTOR_SIZE,
            serialization_compatibility: 1,
        };
        let mut bytes = Vec::new();
        header.write(&mut bytes);
        assert_eq!(DatabaseHeader::read(&main, &bytes).unwrap(), header);
    }
}
