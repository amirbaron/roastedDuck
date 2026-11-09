//! The AES state (DuckDB's `EncryptionState` as `AESStateMBEDTLS` implements it), its
//! nonces and tags, and the encryption of temporary buffers
//! (`EncryptionEngine::EncryptTemporaryBuffer`).
//!
//! DuckDB drives a state as `InitializeEncryption`/`InitializeDecryption`, one `Process`
//! over the whole buffer and `Finalize`; [`EncryptionState::encrypt`] and
//! [`EncryptionState::decrypt`] are those calls at once. mbedtls' GCM derives the
//! pre-counter block of a 16-byte IV with GHASH and counts in the last 32 bits; its CTR
//! mode counts the whole 128-bit IV big-endian: `AesGcm` with a 12- or
//! 16-byte nonce and `Ctr128BE` do the same.

use aes::Aes256;
use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::{AeadInOut, AesGcm, KeyInit};
use ctr::cipher::{KeyIvInit, StreamCipher};
use roastedduck_common::{Error, ErrorKind, Result};

use crate::{
    AES_IV_LEN, AES_NONCE_LEN, AES_NONCE_LEN_DEPRECATED, AES_TAG_LEN, CipherType,
    DEFAULT_ENCRYPTION_KEY_LENGTH, ENCRYPTED_BUFFER_HEADER_SIZE, EncryptionKey, EncryptionVersion,
};

/// GCM with a 16-byte IV (encryption version 0).
type Aes256GcmDeprecated = AesGcm<Aes256, U16>;
type Aes256Gcm = AesGcm<Aes256, U12>;
type Aes256Ctr = ctr::Ctr128BE<Aes256>;

/// Fills `data` with random bytes from the operating system (`GenerateRandomData`).
pub fn generate_random_data(data: &mut [u8]) -> Result<()> {
    getrandom::fill(data).map_err(|error| Error::internal(format!("Failed to generate random data: {error}")))
}

/// A nonce or IV (`EncryptionNonce`): `size()` bytes are random and stored with the
/// ciphertext, `total_size()` bytes go to the cipher (a CTR IV of version 1 adds a zeroed
/// 4-byte block counter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptionNonce {
    bytes: [u8; AES_IV_LEN],
    total_size: usize,
    version: EncryptionVersion,
}

impl EncryptionNonce {
    /// A zeroed nonce for `cipher` in `version`.
    pub fn new(cipher: CipherType, version: EncryptionVersion) -> Self {
        let total_size = match version {
            EncryptionVersion::V0_0 => AES_NONCE_LEN_DEPRECATED,
            EncryptionVersion::V0_1 if cipher == CipherType::Ctr => AES_IV_LEN,
            // Version 1 GCM and unversioned (Parquet) nonces have 12 bytes.
            EncryptionVersion::V0_1 | EncryptionVersion::None => AES_NONCE_LEN,
        };
        Self {
            bytes: [0; AES_IV_LEN],
            total_size,
            version,
        }
    }

    /// The bytes stored with the ciphertext: 16 in version 0, 12 otherwise (`size`).
    pub fn size(&self) -> usize {
        if self.version == EncryptionVersion::V0_0 {
            AES_NONCE_LEN_DEPRECATED
        } else {
            AES_NONCE_LEN
        }
    }

    /// The length of the IV (`total_size`).
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// The stored bytes (`size()` of them).
    pub fn stored(&self) -> &[u8] {
        &self.bytes[..self.size()]
    }

    /// The stored bytes, to read or generate them.
    pub fn stored_mut(&mut self) -> &mut [u8] {
        let size = self.size();
        &mut self.bytes[..size]
    }

    /// The IV given to the cipher.
    pub fn iv(&self) -> &[u8] {
        &self.bytes[..self.total_size]
    }

    /// A nonce with random stored bytes (`GenerateRandomData(nonce.data(), nonce.size())`).
    pub fn random(cipher: CipherType, version: EncryptionVersion) -> Result<Self> {
        let mut nonce = Self::new(cipher, version);
        generate_random_data(nonce.stored_mut())?;
        Ok(nonce)
    }
}

/// A GCM tag (`EncryptionTag`); all zeros for CTR.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncryptionTag(pub [u8; AES_TAG_LEN]);

impl EncryptionTag {
    /// `IsAllZeros`.
    pub fn is_all_zeros(&self) -> bool {
        self.0.iter().fold(0, |acc, byte| acc | byte) == 0
    }
}

/// The error of a GCM tag that does not match (`FinalizeGCM` decrypting).
fn tag_mismatch() -> Error {
    Error::invalid_input("Computed AES tag differs from read AES tag, are you using the right key?")
}

/// An AES-256 state for one cipher (`EncryptionState` with its `EncryptionStateMetadata`).
#[derive(Clone, Copy, Debug)]
pub struct EncryptionState {
    cipher: CipherType,
    version: EncryptionVersion,
}

impl EncryptionState {
    /// A state for `cipher` with a `key_len`-byte key in `version`
    /// (`AESStateMBEDTLS::AESStateMBEDTLS`, `GetCipher`).
    pub fn new(cipher: CipherType, key_len: usize, version: EncryptionVersion) -> Result<Self> {
        match cipher {
            CipherType::Gcm | CipherType::Ctr => {}
            CipherType::Cbc => return Err(Error::invalid_input("CBC cipher is disabled")),
            CipherType::Invalid => {
                return Err(Error::internal(format!(
                    "Invalid Encryption/Decryption Cipher: {}",
                    cipher.name()
                )));
            }
        }
        if key_len != DEFAULT_ENCRYPTION_KEY_LENGTH {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("Invalid AES key length for {}", cipher.name()),
            ));
        }
        Ok(Self { cipher, version })
    }

    /// The cipher (`GetCipher`).
    pub fn cipher(&self) -> CipherType {
        self.cipher
    }

    /// The encryption version of the metadata.
    pub fn version(&self) -> EncryptionVersion {
        self.version
    }

    /// Encrypts `data` in place and returns the GCM tag (zeros for CTR).
    pub fn encrypt(
        &self,
        nonce: &EncryptionNonce,
        key: &EncryptionKey,
        data: &mut [u8],
    ) -> Result<EncryptionTag> {
        let key = key.as_bytes();
        if self.cipher == CipherType::Gcm {
            gcm_apply(key, nonce, data)
        } else {
            ctr_apply(key, nonce, data)?;
            Ok(EncryptionTag::default())
        }
    }

    /// Decrypts `data` in place. With GCM a `tag` is checked; without one the data is
    /// decrypted unauthenticated (the canary of encryption version 0).
    pub fn decrypt(
        &self,
        nonce: &EncryptionNonce,
        key: &EncryptionKey,
        data: &mut [u8],
        tag: Option<&EncryptionTag>,
    ) -> Result<()> {
        let key = key.as_bytes();
        match (self.cipher, tag) {
            (CipherType::Gcm, Some(tag)) => gcm_decrypt(key, nonce, data, tag),
            // GCM's keystream is that of CTR mode: applying it again decrypts.
            (CipherType::Gcm, None) => gcm_apply(key, nonce, data).map(|_| ()),
            _ => ctr_apply(key, nonce, data),
        }
    }
}

/// Encrypts with GCM (or decrypts without checking the tag), returning the tag.
fn gcm_apply(
    key: &[u8; DEFAULT_ENCRYPTION_KEY_LENGTH],
    nonce: &EncryptionNonce,
    data: &mut [u8],
) -> Result<EncryptionTag> {
    let failed = |_| Error::internal("Encryption or Decryption failed at Process");
    let tag = match nonce.total_size() {
        AES_NONCE_LEN => Aes256Gcm::new(key.into())
            .encrypt_inout_detached(nonce.iv().try_into().map_err(|_| invalid_iv())?, &[], data.into())
            .map_err(failed)?,
        AES_NONCE_LEN_DEPRECATED => Aes256GcmDeprecated::new(key.into())
            .encrypt_inout_detached(nonce.iv().try_into().map_err(|_| invalid_iv())?, &[], data.into())
            .map_err(failed)?,
        _ => return Err(invalid_iv()),
    };
    let mut result = EncryptionTag::default();
    result.0.copy_from_slice(&tag);
    Ok(result)
}

fn gcm_decrypt(
    key: &[u8; DEFAULT_ENCRYPTION_KEY_LENGTH],
    nonce: &EncryptionNonce,
    data: &mut [u8],
    tag: &EncryptionTag,
) -> Result<()> {
    let tag = (&tag.0).into();
    match nonce.total_size() {
        AES_NONCE_LEN => Aes256Gcm::new(key.into())
            .decrypt_inout_detached(
                nonce.iv().try_into().map_err(|_| invalid_iv())?,
                &[],
                data.into(),
                tag,
            )
            .map_err(|_| tag_mismatch()),
        AES_NONCE_LEN_DEPRECATED => Aes256GcmDeprecated::new(key.into())
            .decrypt_inout_detached(
                nonce.iv().try_into().map_err(|_| invalid_iv())?,
                &[],
                data.into(),
                tag,
            )
            .map_err(|_| tag_mismatch()),
        _ => Err(invalid_iv()),
    }
}

/// Applies the CTR keystream of the 16-byte IV.
fn ctr_apply(
    key: &[u8; DEFAULT_ENCRYPTION_KEY_LENGTH],
    nonce: &EncryptionNonce,
    data: &mut [u8],
) -> Result<()> {
    let iv: &[u8; AES_IV_LEN] = nonce.iv().try_into().map_err(|_| invalid_iv())?;
    let mut cipher = Aes256Ctr::new(key.into(), iv.into());
    cipher
        .try_apply_keystream(data)
        .map_err(|_| Error::internal("Encryption or Decryption failed at Process"))
}

fn invalid_iv() -> Error {
    Error::internal("Failed to set IV for encryption")
}

/// Encrypts a temporary buffer in place with AES-GCM under `key`; its nonce and tag go to
/// `metadata` (`EncryptionEngine::EncryptTemporaryBuffer`: GCM whatever the databases
/// use, encryption version 1).
pub fn encrypt_temporary_buffer(
    key: &EncryptionKey,
    buffer: &mut [u8],
    metadata: &mut [u8; ENCRYPTED_BUFFER_HEADER_SIZE],
) -> Result<()> {
    let state = EncryptionState::new(
        CipherType::Gcm,
        DEFAULT_ENCRYPTION_KEY_LENGTH,
        EncryptionVersion::V0_1,
    )?;
    *metadata = [0; ENCRYPTED_BUFFER_HEADER_SIZE];
    let nonce = EncryptionNonce::random(CipherType::Gcm, EncryptionVersion::V0_1)?;
    metadata[..nonce.size()].copy_from_slice(nonce.stored());
    let tag = state.encrypt(&nonce, key, buffer)?;
    metadata[nonce.size()..nonce.size() + AES_TAG_LEN].copy_from_slice(&tag.0);
    Ok(())
}

/// Decrypts a temporary buffer in place, checking its tag
/// (`EncryptionEngine::DecryptTemporaryBuffer`).
pub fn decrypt_temporary_buffer(
    key: &EncryptionKey,
    buffer: &mut [u8],
    metadata: &[u8; ENCRYPTED_BUFFER_HEADER_SIZE],
) -> Result<()> {
    let state = EncryptionState::new(
        CipherType::Gcm,
        DEFAULT_ENCRYPTION_KEY_LENGTH,
        EncryptionVersion::V0_1,
    )?;
    let mut nonce = EncryptionNonce::new(CipherType::Gcm, EncryptionVersion::V0_1);
    let size = nonce.size();
    nonce.stored_mut().copy_from_slice(&metadata[..size]);
    let mut tag = EncryptionTag::default();
    tag.0.copy_from_slice(&metadata[size..size + AES_TAG_LEN]);
    state.decrypt(&nonce, key, buffer, Some(&tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> EncryptionKey {
        EncryptionKey::new(&mut [byte; DEFAULT_ENCRYPTION_KEY_LENGTH])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut text, byte| {
            use std::fmt::Write;
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// NIST SP 800-38D test case 14 (AES-256, 96-bit IV, 16 zero bytes).
    #[test]
    fn gcm_matches_the_nist_vectors() {
        let state = EncryptionState::new(CipherType::Gcm, 32, EncryptionVersion::V0_1).unwrap();
        let nonce = EncryptionNonce::new(CipherType::Gcm, EncryptionVersion::V0_1);
        let mut data = [0u8; 16];
        let tag = state.encrypt(&nonce, &key(0), &mut data).unwrap();
        assert_eq!(hex(&data), "cea7403d4d606b6e074ec5d3baf39d18");
        assert_eq!(hex(&tag.0), "d0d1c8a799996bf0265b98b5d48ab919");
        state.decrypt(&nonce, &key(0), &mut data, Some(&tag)).unwrap();
        assert_eq!(data, [0; 16]);
    }

    #[test]
    fn a_wrong_tag_is_duckdbs_error() {
        let state = EncryptionState::new(CipherType::Gcm, 32, EncryptionVersion::V0_1).unwrap();
        let nonce = EncryptionNonce::random(CipherType::Gcm, EncryptionVersion::V0_1).unwrap();
        let mut data = *b"some block data!";
        let tag = state.encrypt(&nonce, &key(1), &mut data).unwrap();
        let error = state.decrypt(&nonce, &key(2), &mut data, Some(&tag)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid Input Error: Computed AES tag differs from read AES tag, are you using the right key?"
        );
    }

    /// GCM without a tag check applies the same keystream; CTR counts all 128 bits.
    #[test]
    fn keystreams_round_trip() {
        for (cipher, version) in [
            (CipherType::Gcm, EncryptionVersion::V0_0),
            (CipherType::Ctr, EncryptionVersion::V0_0),
            (CipherType::Ctr, EncryptionVersion::V0_1),
        ] {
            let state = EncryptionState::new(cipher, 32, version).unwrap();
            let nonce = EncryptionNonce::random(cipher, version).unwrap();
            let plain: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
            let mut data = plain.clone();
            state.encrypt(&nonce, &key(3), &mut data).unwrap();
            assert_ne!(data, plain);
            state.decrypt(&nonce, &key(3), &mut data, None).unwrap();
            assert_eq!(data, plain);
        }
        // A version 1 CTR IV is the 12 random bytes and a zero counter.
        let nonce = EncryptionNonce::random(CipherType::Ctr, EncryptionVersion::V0_1).unwrap();
        assert_eq!(nonce.size(), 12);
        assert_eq!(nonce.iv().len(), 16);
        assert_eq!(&nonce.iv()[12..], &[0; 4]);
        // The counter carries into the nonce bytes, as mbedtls' does.
        let state = EncryptionState::new(CipherType::Ctr, 32, EncryptionVersion::V0_0).unwrap();
        let mut nonce = EncryptionNonce::new(CipherType::Ctr, EncryptionVersion::V0_0);
        nonce.stored_mut()[8..].copy_from_slice(&[0xff; 8]);
        let mut two_blocks = [0u8; 32];
        state.encrypt(&nonce, &key(4), &mut two_blocks).unwrap();
        let mut next = EncryptionNonce::new(CipherType::Ctr, EncryptionVersion::V0_0);
        next.stored_mut()[7] = 1;
        let mut second = [0u8; 16];
        state.encrypt(&next, &key(4), &mut second).unwrap();
        assert_eq!(&two_blocks[16..], &second);
    }

    #[test]
    fn temporary_buffers_round_trip() {
        let key = key(5);
        let plain: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let mut buffer = plain.clone();
        let mut metadata = [0u8; ENCRYPTED_BUFFER_HEADER_SIZE];
        encrypt_temporary_buffer(&key, &mut buffer, &mut metadata).unwrap();
        assert_eq!(&metadata[28..], &[0; 4]);
        decrypt_temporary_buffer(&key, &mut buffer, &metadata).unwrap();
        assert_eq!(buffer, plain);
    }
}
