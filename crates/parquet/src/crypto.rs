//! Parquet modular encryption, reading side (DuckDB's `ParquetCrypto` with the mbedtls
//! AES-GCM state): AES-GCM decryption of modules, their additional authenticated data
//! (AAD) and the keys `PRAGMA add_parquet_key` registers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};

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
    pub fn get(cache: &rustdb_function::ObjectCache) -> Arc<Self> {
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
pub fn add_key(cache: &rustdb_function::ObjectCache, name: &str, key: &str) -> Result<()> {
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

/// The AES S-box.
const SBOX: [u8; 256] = {
    let mut sbox = [0u8; 256];
    // Generated from the multiplicative inverse in GF(2^8) and the affine transform.
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p * 3
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1B } else { 0 };
        // q / 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
};

/// `xtime`: multiplication by x in GF(2^8).
const fn xtime(byte: u8) -> u8 {
    (byte << 1) ^ if byte & 0x80 != 0 { 0x1B } else { 0 }
}

/// An expanded AES key (encryption only: GCM decrypts with the forward cipher).
struct Aes {
    round_keys: Vec<[u8; 16]>,
}

impl Aes {
    fn new(key: &[u8]) -> Result<Self> {
        let nk = key.len() / 4;
        if !valid_key(key) {
            return Err(Error::invalid_input(
                "Invalid AES key. Must have a length of 128, 192, or 256 bits (16, 24, or 32 bytes)",
            ));
        }
        let rounds = nk + 6;
        let mut words: Vec<[u8; 4]> = key
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| [word[0], word[1], word[2], word[3]])
            .collect();
        let mut rcon: u8 = 1;
        for index in nk..4 * (rounds + 1) {
            let mut word = words[index - 1];
            if index % nk == 0 {
                word = [
                    SBOX[word[1] as usize] ^ rcon,
                    SBOX[word[2] as usize],
                    SBOX[word[3] as usize],
                    SBOX[word[0] as usize],
                ];
                rcon = xtime(rcon);
            } else if nk > 6 && index % nk == 4 {
                word = word.map(|byte| SBOX[byte as usize]);
            }
            let previous = words[index - nk];
            words.push([
                word[0] ^ previous[0],
                word[1] ^ previous[1],
                word[2] ^ previous[2],
                word[3] ^ previous[3],
            ]);
        }
        let round_keys = words
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| {
                let mut round_key = [0u8; 16];
                for (column, word) in chunk.iter().enumerate() {
                    round_key[4 * column..4 * column + 4].copy_from_slice(word);
                }
                round_key
            })
            .collect();
        Ok(Self { round_keys })
    }

    /// Encrypts one block.
    fn encrypt(&self, block: &mut [u8; 16]) {
        let rounds = self.round_keys.len() - 1;
        xor_into(block, &self.round_keys[0]);
        for (round, round_key) in self.round_keys.iter().enumerate().skip(1) {
            for byte in block.iter_mut() {
                *byte = SBOX[*byte as usize];
            }
            // ShiftRows (column-major state).
            let state = *block;
            for column in 0..4 {
                for row in 0..4 {
                    block[4 * column + row] = state[4 * ((column + row) % 4) + row];
                }
            }
            if round != rounds {
                // MixColumns.
                for column in block.as_chunks_mut::<4>().0 {
                    let [a0, a1, a2, a3] = [column[0], column[1], column[2], column[3]];
                    let all = a0 ^ a1 ^ a2 ^ a3;
                    column[0] ^= all ^ xtime(a0 ^ a1);
                    column[1] ^= all ^ xtime(a1 ^ a2);
                    column[2] ^= all ^ xtime(a2 ^ a3);
                    column[3] ^= all ^ xtime(a3 ^ a0);
                }
            }
            xor_into(block, round_key);
        }
    }
}

fn xor_into(block: &mut [u8; 16], other: &[u8; 16]) {
    for (byte, other) in block.iter_mut().zip(other) {
        *byte ^= other;
    }
}

/// Multiplication in GF(2^128) with GCM's bit order.
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xE1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for bit in 0..128 {
        if (x >> (127 - bit)) & 1 == 1 {
            z ^= v;
        }
        v = if v & 1 == 1 { (v >> 1) ^ R } else { v >> 1 };
    }
    z
}

/// GHASH of `aad` and `ciphertext` under `h`.
fn ghash(h: u128, aad: &[u8], ciphertext: &[u8]) -> u128 {
    let mut y = 0u128;
    for data in [aad, ciphertext] {
        for chunk in data.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            y = gf_mul(y ^ u128::from_be_bytes(block), h);
        }
    }
    let lengths = (u128::from(aad.len() as u64 * 8) << 64) | u128::from(ciphertext.len() as u64 * 8);
    gf_mul(y ^ lengths, h)
}

/// AES-GCM decryption of `ciphertext` with a 12-byte `nonce`, checking `tag`
/// (`AESStateMBEDTLS` with `FinalizeGCM`).
fn gcm_decrypt(key: &[u8], nonce: &[u8], aad: &[u8], ciphertext: &[u8], tag: &[u8]) -> Result<Vec<u8>> {
    let aes = Aes::new(key)?;
    let mut h = [0u8; 16];
    aes.encrypt(&mut h);
    let h = u128::from_be_bytes(h);
    let mut counter = [0u8; 16];
    counter[..NONCE_BYTES].copy_from_slice(nonce);
    counter[15] = 1;
    let mut tag_mask = counter;
    aes.encrypt(&mut tag_mask);
    let mut plaintext = Vec::with_capacity(ciphertext.len());
    let mut block_counter = 1u32;
    for chunk in ciphertext.chunks(16) {
        block_counter = block_counter.wrapping_add(1);
        counter[12..].copy_from_slice(&block_counter.to_be_bytes());
        let mut keystream = counter;
        aes.encrypt(&mut keystream);
        plaintext.extend(chunk.iter().zip(keystream).map(|(byte, key)| byte ^ key));
    }
    let computed = (ghash(h, aad, ciphertext) ^ u128::from_be_bytes(tag_mask)).to_be_bytes();
    if computed[..] != *tag {
        return Err(Error::invalid_input(
            "Computed AES tag differs from read AES tag, are you using the right key?",
        ));
    }
    Ok(plaintext)
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

impl std::fmt::Debug for FooterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FooterKey").finish_non_exhaustive()
    }
}

/// `ParquetEncryptionConfig::ParquetEncryptionConfig(context, arg)`: the footer key of an
/// `encryption_config` STRUCT, by key name (`footer_key`) or value (`footer_key_value`).
pub fn parse_encryption_config(
    value: &rustdb_types::Value,
    cache: Option<&rustdb_function::ObjectCache>,
) -> Result<FooterKey> {
    let names = value
        .logical_type()
        .struct_children()
        .filter(|_| value.logical_type().id() == rustdb_types::LogicalTypeId::Struct)
        .ok_or_else(|| Error::binder("Parquet encryption_config must be of type STRUCT"))?;
    let children = value.children().unwrap_or_default();
    let mut footer_key = Vec::new();
    for ((name, _), child) in names.iter().zip(children) {
        match name.to_lowercase().as_str() {
            "footer_key" => {
                let key_name = child.to_varchar()?;
                let key = cache
                    .and_then(|cache| ParquetKeys::get(cache).key(&key_name))
                    .ok_or_else(|| {
                        Error::binder(format!(
                            "No key with name \"{key_name}\" exists. Add it with PRAGMA add_parquet_key('<key_name>','<key>');"
                        ))
                    })?;
                footer_key = key;
            }
            "footer_key_value" => {
                footer_key = match child.logical_type().id() {
                    rustdb_types::LogicalTypeId::Blob => child.as_bytes().unwrap_or_default().to_vec(),
                    _ => rustdb_common::blob::from_string(child.to_varchar()?.as_bytes())?,
                };
            }
            "column_keys" => {
                return Err(Error::not_implemented(
                    "Parquet encryption_config column_keys not yet implemented",
                ));
            }
            _ => {
                return Err(Error::binder(format!(
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
    fn aes_known_answers() {
        // FIPS-197 appendix C.
        let plain: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        for (key, expected) in [
            (
                "000102030405060708090a0b0c0d0e0f",
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ] {
            let mut block = plain;
            Aes::new(&hex(key)).unwrap().encrypt(&mut block);
            assert_eq!(block.to_vec(), hex(expected));
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
