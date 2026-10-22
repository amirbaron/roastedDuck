//! Database encryption (DuckDB's `common/encryption_types`, `encryption_state`,
//! `encryption_key_manager` and `encryption_functions`, with the AES and SHA-256 parts of
//! its mbedtls wrapper), on the pure-Rust `RustCrypto` crates `aes`, `aes-gcm`, `ctr` and `sha2`.
//!
//! DuckDB encrypts database blocks, its WAL entries and temporary files with AES-256 in
//! GCM or CTR mode under a key derived from the user's key and the file's salt. The
//! bytes it writes are reproduced exactly: the same IV layouts (12-byte GCM nonces,
//! 16-byte ones for encryption version 0, CTR IVs whose last four bytes count blocks),
//! the same tags and the same key derivation. Nonces and keys of temporary files come from
//! the operating system's CSPRNG, which is what DuckDB uses with httpfs's OpenSSL module
//! loaded; its mbedtls fallback, which it only writes with `force_mbedtls_unsafe`, is not
//! needed.

mod key_manager;
mod state;

pub use key_manager::{EncryptionKey, EncryptionKeyManager, base64_decode, derive_key};
pub use state::{
    EncryptionNonce, EncryptionState, EncryptionTag, decrypt_temporary_buffer, encrypt_temporary_buffer,
    generate_random_data,
};

pub use zeroize::Zeroize;

use rustdb_common::{Error, Result};

/// The key given to `ATTACH ... (ENCRYPTION_KEY ...)`, as the bytes of its BLOB cast
/// (`StorageOptions::user_key`). It is zeroed once a key is derived from it, and when it
/// is dropped (`ClearUserKey`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct UserKey(Vec<u8>);

impl std::fmt::Debug for UserKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserKey").finish_non_exhaustive()
    }
}

impl UserKey {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Derives the key of a file with salt `salt` and wipes this key
    /// (`EncryptionKeyManager::DeriveKey`).
    pub fn derive(&mut self, salt: &[u8; SALT_LEN]) -> [u8; DEFAULT_ENCRYPTION_KEY_LENGTH] {
        derive_key(&mut self.0, salt)
    }

    /// Whether the key was used up (or never given).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for UserKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The length of a derived key (`MainHeader::DEFAULT_ENCRYPTION_KEY_LENGTH`).
pub const DEFAULT_ENCRYPTION_KEY_LENGTH: usize = 32;
/// The length of the salt a key is derived with: the database identifier
/// (`MainHeader::DB_IDENTIFIER_LEN`).
pub const SALT_LEN: usize = 16;
/// A GCM nonce (`MainHeader::AES_NONCE_LEN`).
pub const AES_NONCE_LEN: usize = 12;
/// The nonce of encryption version 0 (`MainHeader::AES_NONCE_LEN_DEPRECATED`).
pub const AES_NONCE_LEN_DEPRECATED: usize = 16;
/// A CTR IV: the nonce followed by the block counter (`MainHeader::AES_IV_LEN`).
pub const AES_IV_LEN: usize = 16;
/// A GCM tag (`MainHeader::AES_TAG_LEN`).
pub const AES_TAG_LEN: usize = 16;
/// The nonce and tag in front of an encrypted temporary buffer
/// (`DEFAULT_ENCRYPTED_BUFFER_HEADER_SIZE`).
pub const ENCRYPTED_BUFFER_HEADER_SIZE: usize = 32;
/// The block header of an encrypted database file: nonce, tag and checksum
/// (`DEFAULT_ENCRYPTION_BLOCK_HEADER_SIZE`).
pub const ENCRYPTION_BLOCK_HEADER_SIZE: usize = 40;
/// The name of the key temporary files are encrypted with (`AddTempKeyToCache`).
pub const TEMP_KEY_NAME: &str = "temp_key";

/// A cipher (`EncryptionTypes::CipherType`), stored as its value in a file's main header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CipherType {
    #[default]
    Invalid = 0,
    Gcm = 1,
    Ctr = 2,
    Cbc = 3,
}

impl CipherType {
    /// The cipher stored as `value`.
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Gcm,
            2 => Self::Ctr,
            3 => Self::Cbc,
            _ => Self::Invalid,
        }
    }

    /// `EncryptionTypes::CipherToString`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gcm => "GCM",
            Self::Ctr => "CTR",
            Self::Cbc => "CBC",
            Self::Invalid => "INVALID",
        }
    }

    /// `EncryptionTypes::StringToCipher` (case-insensitive): CBC is disabled, anything
    /// else unknown is `Invalid`.
    pub fn from_name(name: &str) -> Result<Self> {
        Ok(match name.to_ascii_uppercase().as_str() {
            "GCM" => Self::Gcm,
            "CTR" => Self::Ctr,
            "CBC" => return Err(Error::not_implemented("CBC encryption is disabled")),
            _ => Self::Invalid,
        })
    }
}

/// The version of DuckDB's encryption format (`EncryptionTypes::EncryptionVersion`): 0
/// encrypts with 16-byte nonces and leaves the canary unauthenticated, 1 uses 12-byte
/// nonces and a GCM-tagged canary. `None` is "not set".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EncryptionVersion {
    V0_0 = 0,
    V0_1 = 1,
    #[default]
    None = 127,
}

impl EncryptionVersion {
    /// The newest version (`MAX_VERSION`).
    pub const MAX: Self = Self::V0_1;

    /// The version stored as `value`, if it is one.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::V0_0),
            1 => Some(Self::V0_1),
            127 => Some(Self::None),
            _ => None,
        }
    }

    /// `EncryptionTypes::StringToVersion` (`debug_encryption_version`).
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "v0" => Ok(Self::V0_0),
            "v1" => Ok(Self::V0_1),
            _ => Err(Error::not_implemented(format!(
                "No encryption version higher then v{} is supported yet in this DuckDB version",
                Self::MAX as u8
            ))),
        }
    }
}

/// How a key is derived (`EncryptionTypes::KeyDerivationFunction`), stored in a file's
/// main header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum KeyDerivationFunction {
    #[default]
    Default = 0,
    Sha256 = 1,
    Pbkdf2 = 2,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_duckdb() {
        assert_eq!(CipherType::from_name("gcm").unwrap(), CipherType::Gcm);
        assert_eq!(CipherType::from_name("Ctr").unwrap(), CipherType::Ctr);
        assert_eq!(CipherType::from_name("random").unwrap(), CipherType::Invalid);
        assert_eq!(
            CipherType::from_name("cbc").unwrap_err().to_string(),
            "Not implemented Error: CBC encryption is disabled"
        );
        assert_eq!(
            EncryptionVersion::from_name("v1").unwrap(),
            EncryptionVersion::V0_1
        );
        assert_eq!(
            EncryptionVersion::from_name("v9").unwrap_err().to_string(),
            "Not implemented Error: No encryption version higher then v1 is supported yet in this DuckDB version"
        );
    }
}
