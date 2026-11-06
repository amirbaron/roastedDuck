//! The encryption of a database file (DuckDB's `EncryptionOptions`, the encryption parts
//! of `SingleFileBlockManager` and `EncryptionEngine::EncryptBlock` / `DecryptBlock`).
//!
//! # Format
//! The main header stays plaintext: its first flag marks the file as encrypted, its
//! encryption metadata names the key derivation (SHA-256), the cipher and the encryption
//! version, its database identifier is the salt of the key, and it holds the canary, a
//! known plaintext encrypted with the key (with its IV and GCM tag from version 1 on).
//! Every other block, the database headers included, has a 40-byte block header: the
//! nonce (12 bytes, 16 in version 0) and the tag (zeros for CTR) of the encryption of the
//! checksum and the data that follow.

use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_crypto::{
    AES_TAG_LEN, CipherType, DEFAULT_ENCRYPTION_KEY_LENGTH, EncryptionKey, EncryptionNonce, EncryptionState,
    EncryptionTag, EncryptionVersion, KeyDerivationFunction, UserKey,
};

use crate::storage_info::{
    CANARY, CANARY_BYTE_SIZE, DEFAULT_BLOCK_HEADER_SIZE, ENCRYPTION_METADATA_LEN, MainHeader,
};

/// The block header of an encrypted file (`DEFAULT_ENCRYPTION_BLOCK_HEADER_SIZE`).
pub const ENCRYPTION_BLOCK_HEADER_SIZE: usize = rustdb_crypto::ENCRYPTION_BLOCK_HEADER_SIZE;

/// How a database file is encrypted, as `ATTACH` gives it (the encryption fields of
/// DuckDB's `StorageOptions`): the key, the cipher (`Invalid` when not given) and the
/// encryption version (resolved by `StorageOptions::set_encryption_version`).
#[derive(Clone, Debug)]
pub struct EncryptionOptions {
    pub user_key: UserKey,
    pub cipher: CipherType,
    pub version: EncryptionVersion,
}

/// The encryption of an open file: its cipher and encryption version (the storage
/// manager's `GetCipher` and `GetEncryptionVersion`) and its derived key, cached under
/// `key_id` in the instance's key manager (`derived_key_id`).
#[derive(Clone, Debug)]
pub struct FileEncryption {
    pub cipher: CipherType,
    pub version: EncryptionVersion,
    pub key_id: String,
    pub key: Arc<EncryptionKey>,
}

impl FileEncryption {
    fn state(&self) -> Result<EncryptionState> {
        EncryptionState::new(self.cipher, DEFAULT_ENCRYPTION_KEY_LENGTH, self.version)
    }

    /// Encrypts `block[delta..]` (the checksum and the data) in place and stores the nonce
    /// and the tag in front of it (`EncryptionEngine::EncryptBlock`).
    pub fn encrypt_block(&self, block: &mut [u8], delta: usize) -> Result<()> {
        let state = self.state()?;
        let nonce = EncryptionNonce::random(self.cipher, self.version)?;
        let tag = state.encrypt(&nonce, &self.key, &mut block[delta..])?;
        let header = &mut block[..delta];
        header.fill(0);
        header[..nonce.size()].copy_from_slice(nonce.stored());
        header[nonce.size()..nonce.size() + AES_TAG_LEN].copy_from_slice(&tag.0);
        Ok(())
    }

    /// Decrypts `block[delta..]` in place with the nonce and tag in front of it, checking
    /// the tag of GCM (`EncryptionEngine::DecryptBlock`).
    pub fn decrypt_block(&self, block: &mut [u8], delta: usize) -> Result<()> {
        let state = self.state()?;
        let mut nonce = EncryptionNonce::new(self.cipher, self.version);
        let size = nonce.size();
        nonce.stored_mut().copy_from_slice(&block[..size]);
        let mut tag = EncryptionTag::default();
        tag.0.copy_from_slice(&block[size..size + AES_TAG_LEN]);
        let tag = (self.cipher == CipherType::Gcm).then_some(&tag);
        state.decrypt(&nonce, &self.key, &mut block[delta..], tag)
    }

    /// The bytes between the start of a block and its checksum (`delta`).
    pub fn delta() -> usize {
        ENCRYPTION_BLOCK_HEADER_SIZE - DEFAULT_BLOCK_HEADER_SIZE
    }
}

/// The encryption metadata of the main header (`StoreEncryptionMetadata`): the key
/// derivation function, whether AAD is used, the cipher, the encryption version and the
/// key length.
pub(crate) fn encryption_metadata(
    cipher: CipherType,
    version: EncryptionVersion,
) -> [u8; ENCRYPTION_METADATA_LEN] {
    let mut metadata = [0u8; ENCRYPTION_METADATA_LEN];
    metadata[0] = KeyDerivationFunction::Sha256 as u8;
    metadata[1] = 0;
    metadata[2] = cipher as u8;
    metadata[3] = version as u8;
    metadata[4..].copy_from_slice(&(DEFAULT_ENCRYPTION_KEY_LENGTH as u32).to_le_bytes());
    metadata
}

/// The state the canary of `header` is encrypted with: GCM from encryption version 1
/// on, the file's cipher before (`StoreEncryptedCanary` / `CheckAndAddEncryptionKey`).
fn canary_state(header: &MainHeader) -> Result<EncryptionState> {
    let Some(version) = EncryptionVersion::from_u8(header.encryption_version()) else {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Encryption version not recognized!",
        ));
    };
    if version == EncryptionVersion::None {
        return Err(Error::invalid_input("No valid encryption version found!"));
    }
    let cipher = if version == EncryptionVersion::V0_0 {
        CipherType::from_u8(header.encryption_cipher())
    } else {
        CipherType::Gcm
    };
    EncryptionState::new(cipher, DEFAULT_ENCRYPTION_KEY_LENGTH, version)
}

/// Encrypts the canary into `header` (`EncryptCanary`): version 0 with a zero nonce and
/// no tag, version 1 with a random IV and its tag.
pub(crate) fn encrypt_canary(header: &mut MainHeader, key: &EncryptionKey) -> Result<()> {
    let state = canary_state(header)?;
    let mut canary = *CANARY;
    if state.version() == EncryptionVersion::V0_0 {
        let nonce = EncryptionNonce::new(state.cipher(), EncryptionVersion::V0_0);
        state.encrypt(&nonce, key, &mut canary)?;
    } else {
        let nonce = EncryptionNonce::random(CipherType::Gcm, state.version())?;
        header.canary_iv.copy_from_slice(nonce.stored());
        let tag = state.encrypt(&nonce, key, &mut canary)?;
        header.canary_tag = tag.0;
    }
    header.encrypted_canary = canary;
    Ok(())
}

/// Whether `key` decrypts the canary of `header` (`DecryptCanary`); a wrong tag is the
/// error of a wrong key.
pub(crate) fn decrypt_canary(header: &MainHeader, key: &EncryptionKey) -> Result<bool> {
    let state = canary_state(header)?;
    let mut canary: [u8; CANARY_BYTE_SIZE] = header.encrypted_canary;
    if state.version() == EncryptionVersion::V0_0 {
        let nonce = EncryptionNonce::new(state.cipher(), EncryptionVersion::V0_0);
        state.decrypt(&nonce, key, &mut canary, None)?;
    } else {
        let mut nonce = EncryptionNonce::new(CipherType::Gcm, state.version());
        nonce.stored_mut().copy_from_slice(&header.canary_iv);
        let tag = EncryptionTag(header.canary_tag);
        if state.decrypt(&nonce, key, &mut canary, Some(&tag)).is_err() {
            return Err(wrong_key());
        }
    }
    Ok(&canary == CANARY)
}

/// `InvalidInputException("Wrong encryption key used to open the database file")`.
pub(crate) fn wrong_key() -> Error {
    Error::invalid_input("Wrong encryption key used to open the database file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn derived_key(header: &MainHeader, key: &str) -> EncryptionKey {
        EncryptionKey::new(&mut UserKey::new(key.as_bytes().to_vec()).derive(&header.db_identifier))
    }

    /// Main headers DuckDB 1.5.6 wrote for files encrypted with the key 'asdf': GCM with
    /// storage v1.4.0 (encryption version 1), GCM with `debug_encryption_version 'v0'`
    /// (whose IV and tag fields hold uninitialized bytes) and CTR.
    const DUCKDB_HEADERS: [&str; 3] = [
        "4455434b4300000000000000010000000000000000000000000000000000000000000000000000000000000076302e30\
         2e3100000000000000000000000000000000000000000000000000003938326333373200000000000000000000000000\
         00000000000000000000000001000101200000000b0b0099bc72be31523d6f3584abf643e16466f393edaa339fb01a93\
         db4fb282a321998616bb05b057185b2f10e925997cb66bca",
        "4455434b4400000000000000010000000000000000000000000000000000000000000000000000000000000076302e30\
         2e3100000000000000000000000000000000000000000000000000003938326333373200000000000000000000000000\
         0000000000000000000000000100010020000000789a97e116925972d2413cb91ce7c638585b89420fee385400000000\
         0000000010000000000000d80f00000000000068c3df0501",
        "4455434b4400000000000000010000000000000000000000000000000000000000000000000000000000000076302e30\
         2e3100000000000000000000000000000000000000000000000000003938326333373200000000000000000000000000\
         000000000000000000000000010002012000000038aa04c2bd1376163a17b6aeb3b10b4135e93d99baefb477f1c9f944\
         921e5496c6962014584ecc33bea99817bf96d24e0514cc22",
    ];

    #[test]
    fn duckdbs_canaries_check_the_key() {
        for (hex, cipher) in DUCKDB_HEADERS
            .iter()
            .zip([CipherType::Gcm, CipherType::Gcm, CipherType::Ctr])
        {
            let header = MainHeader::read(&bytes(hex)).unwrap();
            assert!(header.is_encrypted());
            assert_eq!(CipherType::from_u8(header.encryption_cipher()), cipher);
            assert!(decrypt_canary(&header, &derived_key(&header, "asdf")).unwrap());
            let wrong = decrypt_canary(&header, &derived_key(&header, "asdfx"));
            assert!(matches!(wrong, Ok(false) | Err(_)));
        }
    }

    #[test]
    fn canaries_round_trip() {
        for (cipher, version) in [
            (CipherType::Gcm, EncryptionVersion::V0_1),
            (CipherType::Ctr, EncryptionVersion::V0_1),
            (CipherType::Gcm, EncryptionVersion::V0_0),
            (CipherType::Ctr, EncryptionVersion::V0_0),
        ] {
            let mut header = MainHeader::new(68);
            header.set_encrypted();
            header.db_identifier = [7; 16];
            header.encryption_metadata = encryption_metadata(cipher, version);
            let key = derived_key(&header, "secret");
            encrypt_canary(&mut header, &key).unwrap();
            assert!(decrypt_canary(&header, &derived_key(&header, "secret")).unwrap());
            match decrypt_canary(&header, &derived_key(&header, "other")) {
                Ok(valid) => assert!(!valid),
                Err(error) => assert_eq!(error.to_string(), wrong_key().to_string()),
            }
        }
    }
}
