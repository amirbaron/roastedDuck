//! Parquet modular encryption, reading side (DuckDB's `ParquetCrypto` with the mbedtls
//! AES-GCM state): AES-GCM decryption of modules, their additional authenticated data
//! (AAD) and the keys `PRAGMA add_parquet_key` registers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::{Error, Result};

/// The encoded length in front of a module (`LENGTH_BYTES`).
pub const LENGTH_BYTES: usize = 4;
/// The nonce in front of the ciphertext (`NONCE_BYTES`).
pub const NONCE_BYTES: usize = 12;
/// The GCM tag after the ciphertext (`TAG_BYTES`).
pub const TAG_BYTES: usize = 16;

/// Module types of the AAD suffix (`ParquetCrypto::FOOTER`, ...).
pub mod module {
    pub const FOOTER: i8 = 0;
    pub const DATA_PAGE: i8 = 2;
    pub const DICTIONARY_PAGE: i8 = 3;
    pub const DATA_PAGE_HEADER: i8 = 4;
    pub const DICTIONARY_PAGE_HEADER: i8 = 5;
    pub const COLUMN_INDEX: i8 = 6;
    pub const OFFSET_INDEX: i8 = 7;
    pub const BLOOM_FILTER_HEADER: i8 = 8;
    pub const BLOOM_FILTER_BITSET: i8 = 9;
}

/// The keys added with `PRAGMA add_parquet_key`, by name (`ParquetKeys`), kept in the
/// database's object cache.
#[derive(Debug, Default)]
pub struct ParquetKeys {
    keys: Mutex<HashMap<String, Vec<u8>>>,
}

impl ParquetKeys {
    /// The object cache key (`ParquetKeys::ObjectType`).
    const OBJECT_TYPE: &'static str = "parquet_keys";

    /// The keys of the database of `cache` (`ParquetKeys::Get`).
    pub fn get(cache: &roastedduck_function::ObjectCache) -> Arc<Self> {
        if let Some(keys) = cache.get::<Self>(Self::OBJECT_TYPE) {
            return keys;
        }
        let keys = Arc::new(Self::default());
        cache.put(Self::OBJECT_TYPE.to_owned(), Arc::clone(&keys));
        keys
    }

    pub fn add(&self, name: &str, key: Vec<u8>) {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), key);
    }

    pub fn key(&self, name: &str) -> Option<Vec<u8>> {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }
}

/// `ParquetCrypto::ValidKey`: an AES-128, -192 or -256 key.
fn valid_key(key: &[u8]) -> bool {
    matches!(key.len(), 16 | 24 | 32)
}

/// `Blob::FromBase64`: `None` for invalid base64.
fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut result = Vec::with_capacity(text.len() / 4 * 3);
    for (index, group) in text.chunks(4).enumerate() {
        let last = index + 1 == text.len() / 4;
        let padding = group.iter().rev().take_while(|&&byte| byte == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut bits = 0u32;
        for &byte in &group[..4 - padding] {
            bits = (bits << 6) | value(byte)?;
        }
        bits <<= 6 * padding as u32;
        let bytes = bits.to_be_bytes();
        result.extend_from_slice(&bytes[1..4 - padding]);
    }
    Some(result)
}

/// `PRAGMA add_parquet_key(name, key)` (`ParquetCrypto::AddKey`): a plain AES key or a
/// base64 encoded one.
pub fn add_key(cache: &roastedduck_function::ObjectCache, name: &str, key: &str) -> Result<()> {
    let keys = ParquetKeys::get(cache);
    if valid_key(key.as_bytes()) {
        keys.add(name, key.as_bytes().to_vec());
        return Ok(());
    }
    let Some(decoded) = base64_decode(key.as_bytes()) else {
        return Err(Error::invalid_input(
            "Invalid AES key. Not a plain AES key NOR a base64 encoded string",
        ));
    };
    if !valid_key(&decoded) {
        return Err(Error::invalid_input(
            "Invalid AES key. Must have a length of 128, 192, or 256 bits (16, 24, or 32 bytes)",
        ));
    }
    keys.add(name, decoded);
    Ok(())
}

/// Authenticated AES-GCM decryption through the shared hardware-backed primitive.
fn gcm_decrypt(key: &[u8], nonce: &[u8], aad: &[u8], ciphertext: &[u8], tag: &[u8]) -> Result<Vec<u8>> {
    let nonce: &[u8; NONCE_BYTES] = nonce
        .try_into()
        .map_err(|_| Error::invalid_input("Invalid AES-GCM nonce length"))?;
    let tag: &[u8; TAG_BYTES] = tag
        .try_into()
        .map_err(|_| Error::invalid_input("Invalid AES-GCM tag length"))?;
    let mut data = ciphertext.to_vec();
    roastedduck_crypto::aead::decrypt_gcm(key, nonce, aad, &mut data, tag)?;
    Ok(data)
}

/// Encrypt one Parquet module: little-endian length, random nonce, ciphertext and tag.
pub fn encrypt_module(key: &[u8], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0; NONCE_BYTES];
    roastedduck_crypto::generate_random_data(&mut nonce)?;
    let mut data = plaintext.to_vec();
    let tag = roastedduck_crypto::aead::encrypt_gcm(key, &nonce, aad, &mut data)?;
    let length = u32::try_from(NONCE_BYTES + data.len() + TAG_BYTES)
        .map_err(|_| Error::invalid_input("Parquet encrypted module is too large"))?;
    let mut encoded = Vec::with_capacity(LENGTH_BYTES + length as usize);
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(&nonce);
    encoded.extend_from_slice(&data);
    encoded.extend_from_slice(&tag);
    Ok(encoded)
}

/// The AAD of a module (`ParquetAdditionalAuthenticatedData::WriteParquetAAD`): the file
/// identifier, the module type and the ordinals that are set (non-negative); empty
/// without a file identifier (DuckDB's own encryption without AAD).
pub fn module_aad(
    file_aad: &[u8],
    module_type: i8,
    row_group: i16,
    column: i16,
    page: i16,
) -> Result<Vec<u8>> {
    if file_aad.is_empty() {
        return Ok(Vec::new());
    }
    let mut aad = file_aad.to_vec();
    aad.push(module_type as u8);
    if row_group < 0 {
        if module_type != module::FOOTER {
            return Err(Error::invalid_input(
                "Parquet Encryption: Row group not initialized",
            ));
        }
        return Ok(aad);
    }
    aad.extend_from_slice(&row_group.to_le_bytes());
    if column < 0 {
        return Ok(aad);
    }
    aad.extend_from_slice(&column.to_le_bytes());
    if page < 0 {
        return Ok(aad);
    }
    aad.extend_from_slice(&page.to_le_bytes());
    Ok(aad)
}

/// The total length (length field included) of the module at the start of `bytes`.
pub fn module_len(bytes: &[u8]) -> Result<usize> {
    let length = bytes
        .get(..LENGTH_BYTES)
        .ok_or_else(|| Error::invalid_input("Too many bytes requested from crypto buffer"))?;
    let length = u32::from_le_bytes([length[0], length[1], length[2], length[3]]) as usize;
    Ok(LENGTH_BYTES + length)
}

/// Decrypts the module at the start of `bytes` (`DecryptionTransport::ReadAll`): its
/// plaintext and its total length.
pub fn decrypt_module(key: &[u8], aad: &[u8], bytes: &[u8]) -> Result<(Vec<u8>, usize)> {
    let total = module_len(bytes)?;
    let length = total - LENGTH_BYTES;
    if length < NONCE_BYTES + TAG_BYTES || bytes.len() < total {
        return Err(Error::invalid_input(
            "Too many bytes requested from crypto buffer",
        ));
    }
    let nonce = &bytes[LENGTH_BYTES..LENGTH_BYTES + NONCE_BYTES];
    let ciphertext = &bytes[LENGTH_BYTES + NONCE_BYTES..total - TAG_BYTES];
    let tag = &bytes[total - TAG_BYTES..total];
    Ok((gcm_decrypt(key, nonce, aad, ciphertext, tag)?, total))
}

/// Decrypts the module at the start of `bytes` whose plaintext must be `expected` bytes
/// (`ParquetCrypto::ReadData`).
pub fn decrypt_data(key: &[u8], aad: &[u8], bytes: &[u8], expected: usize) -> Result<(Vec<u8>, usize)> {
    let total = module_len(bytes)?;
    let length = total - LENGTH_BYTES;
    if length < NONCE_BYTES + TAG_BYTES || expected > length - NONCE_BYTES - TAG_BYTES {
        return Err(Error::invalid_input(
            "Too many bytes requested from crypto buffer",
        ));
    }
    if expected != length - NONCE_BYTES - TAG_BYTES {
        return Err(Error::invalid_input(
            "Encoded ciphertext length differs from actual ciphertext length",
        ));
    }
    decrypt_module(key, aad, bytes)
}

/// The key of `encryption_config` (`ParquetEncryptionConfig::footer_key`).
#[derive(Clone)]
pub struct FooterKey(pub Arc<[u8]>);

/// The frozen `ParquetEncryptionConfig` object stores the resolved raw footer key.
/// Its column-key map is empty (unsupported by both implementations) and omitted.
pub fn serialize_encryption_config(
    serializer: &mut dyn roastedduck_serde::Serializer,
    key: Option<&FooterKey>,
) -> Result<()> {
    let Some(key) = key else {
        serializer.write_null();
        return Ok(());
    };
    serializer.on_object_begin();
    serializer.write_property_with_default(
        100,
        "footer_key",
        &roastedduck_common::ByteString::from(key.0.as_ref()),
    )?;
    serializer.on_object_end();
    Ok(())
}

impl std::fmt::Debug for FooterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FooterKey").finish_non_exhaustive()
    }
}

/// `ParquetEncryptionConfig::ParquetEncryptionConfig(context, arg)`: the footer key of an
/// `encryption_config` STRUCT, by key name (`footer_key`) or value (`footer_key_value`).
pub fn parse_encryption_config(
    value: &roastedduck_types::Value,
    cache: Option<&roastedduck_function::ObjectCache>,
) -> Result<FooterKey> {
    let names = value
        .logical_type()
        .struct_children()
        .filter(|_| value.logical_type().id() == roastedduck_types::LogicalTypeId::Struct)
        .ok_or_else(|| Error::binder("Parquet encryption_config must be of type STRUCT"))?;
    let children = value.children().unwrap_or_default();
    let mut footer_key = Vec::new();
    for ((name, _), child) in names.iter().zip(children) {
        match name.to_ascii_lowercase().as_bytes() {
            b"footer_key" => {
                let key_name = child.to_varchar()?;
                let key = cache
                    .and_then(|cache| ParquetKeys::get(cache).key(&key_name))
                    .ok_or_else(|| {
                        Error::binder(roastedduck_common::byte_format!(
                            "No key with name \"{key_name}\" exists. Add it with PRAGMA add_parquet_key('<key_name>','<key>');"
                        ))
                    })?;
                footer_key = key;
            }
            b"footer_key_value" => {
                footer_key = match child.logical_type().id() {
                    roastedduck_types::LogicalTypeId::Blob => child.as_bytes().unwrap_or_default().to_vec(),
                    _ => roastedduck_common::blob::from_string(child.to_varchar()?.as_bytes())?,
                };
            }
            b"column_keys" => {
                return Err(Error::not_implemented(
                    "Parquet encryption_config column_keys not yet implemented",
                ));
            }
            _ => {
                return Err(Error::binder(roastedduck_common::byte_format!(
                    "Unknown key in encryption_config \"{name}\""
                )));
            }
        }
    }
    Ok(FooterKey(Arc::from(footer_key)))
}

/// The decryption settings of an encrypted file.
#[derive(Clone)]
pub struct FileDecryption {
    pub key: Arc<[u8]>,
    /// `aad_file_unique` (empty for DuckDB's own files without AAD).
    pub file_aad: Vec<u8>,
}

impl std::fmt::Debug for FileDecryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDecryption").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn modules_roundtrip_all_key_sizes_and_authenticate_aad() {
        for size in [16, 24, 32] {
            let key = vec![7; size];
            let first = encrypt_module(&key, b"aad", b"page payload").unwrap();
            let second = encrypt_module(&key, b"aad", b"page payload").unwrap();
            assert_ne!(first, second);
            assert_eq!(
                decrypt_module(&key, b"aad", &first).unwrap(),
                (b"page payload".to_vec(), first.len())
            );
            assert!(decrypt_module(&key, b"wrong", &first).is_err());
            assert!(decrypt_module(&key, b"aad", &first[..first.len() - 1]).is_err());
        }
    }

    #[test]
    fn gcm_known_answer() {
        // The GCM specification's test case 4 (AES-128, 60-byte plaintext, AAD).
        let key = hex("feffe9928665731c6d6a8f9467308308");
        let nonce = hex("cafebabefacedbaddecaf888");
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let ciphertext = hex(concat!(
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e",
            "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091"
        ));
        let tag = hex("5bc94fbc3221a5db94fae95ae7121a47");
        let plain = gcm_decrypt(&key, &nonce, &aad, &ciphertext, &tag).unwrap();
        assert_eq!(
            plain,
            hex(concat!(
                "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
                "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39"
            ))
        );
        let mut bad = tag.clone();
        bad[0] ^= 1;
        assert!(gcm_decrypt(&key, &nonce, &aad, &ciphertext, &bad).is_err());
    }

    #[test]
    fn base64_keys() {
        assert_eq!(
            base64_decode(b"AAECAwQFBgcICQoLDA0ODw==").unwrap(),
            (0..16).collect::<Vec<u8>>()
        );
        assert!(base64_decode(b"abc").is_none());
    }
}
