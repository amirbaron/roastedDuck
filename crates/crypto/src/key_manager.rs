//! Derived keys and the database's key cache (DuckDB's `EncryptionKey`,
//! `EncryptionKeyManager` and the key functions of `EncryptionEngine`).
//!
//! A key is held in its own page-locked allocation (`mlock`, so it is not swapped out) and
//! zeroed when it is dropped. Every buffer a key passes through on its way in (the user's
//! key, its base64 decoding, the derived key) is zeroed once it was used, as DuckDB's
//! `SecureClearData` does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{DEFAULT_ENCRYPTION_KEY_LENGTH, SALT_LEN, TEMP_KEY_NAME, generate_random_data};

/// A derived 32-byte key (`EncryptionKey`).
pub struct EncryptionKey {
    key: Box<[u8; DEFAULT_ENCRYPTION_KEY_LENGTH]>,
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionKey").finish_non_exhaustive()
    }
}

impl EncryptionKey {
    /// Moves `source` into a locked allocation and zeroes it.
    pub fn new(source: &mut [u8; DEFAULT_ENCRYPTION_KEY_LENGTH]) -> Self {
        let mut key = Box::new([0u8; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        key.copy_from_slice(source);
        source.zeroize();
        lock_memory(key.as_ptr(), DEFAULT_ENCRYPTION_KEY_LENGTH);
        Self { key }
    }

    /// The key's bytes (`GetPtr`).
    pub fn as_bytes(&self) -> &[u8; DEFAULT_ENCRYPTION_KEY_LENGTH] {
        &self.key
    }

    /// Zeroes the key (`ClearKey`).
    pub fn clear(&mut self) {
        self.key.zeroize();
    }
}

impl Drop for EncryptionKey {
    /// `UnlockEncryptionKey`: zeroes the key, then unlocks its memory.
    fn drop(&mut self) {
        self.clear();
        unlock_memory(self.key.as_ptr(), DEFAULT_ENCRYPTION_KEY_LENGTH);
    }
}

/// `mlock`: keeps the page of the key in memory. Failing (a low `RLIMIT_MEMLOCK`) is not
/// an error, as DuckDB ignores it too.
fn lock_memory(ptr: *const u8, len: usize) {
    #[cfg(unix)]
    // SAFETY: `ptr` points to `len` bytes of a live allocation; `mlock` does not access them.
    unsafe {
        libc::mlock(ptr.cast(), len);
    }
    #[cfg(not(unix))]
    let _ = (ptr, len);
}

fn unlock_memory(ptr: *const u8, len: usize) {
    #[cfg(unix)]
    // SAFETY: as in `lock_memory`.
    unsafe {
        libc::munlock(ptr.cast(), len);
    }
    #[cfg(not(unix))]
    let _ = (ptr, len);
}

/// The keys of a database instance by name (`EncryptionKeyManager`, which DuckDB keeps in
/// its object cache): the derived keys of the attached encrypted files, under random ids,
/// and the key of encrypted temporary files.
#[derive(Debug, Default)]
pub struct EncryptionKeyManager {
    keys: Mutex<HashMap<String, Arc<EncryptionKey>>>,
}

impl EncryptionKeyManager {
    /// `KEY_ID_BYTES`: the random bytes of a key id.
    pub const KEY_ID_BYTES: usize = 8;

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<EncryptionKey>>> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `key` under `name` unless a key of that name exists, and zeroes `key`
    /// (`AddKey`).
    pub fn add_key(&self, name: &str, key: &mut [u8; DEFAULT_ENCRYPTION_KEY_LENGTH]) {
        let mut keys = self.lock();
        if !keys.contains_key(name) {
            keys.insert(name.to_owned(), Arc::new(EncryptionKey::new(key)));
        }
        key.zeroize();
    }

    /// Adds `key` under `name` unless a key of that name exists; returns the key stored
    /// under `name`.
    pub fn insert_key(&self, name: &str, key: EncryptionKey) -> Arc<EncryptionKey> {
        Arc::clone(
            self.lock()
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(key)),
        )
    }

    /// `HasKey`.
    pub fn has_key(&self, name: &str) -> bool {
        self.lock().contains_key(name)
    }

    /// The key named `name` (`GetKey`).
    pub fn get_key(&self, name: &str) -> Option<Arc<EncryptionKey>> {
        self.lock().get(name).cloned()
    }

    /// Removes the key named `name` (`DeleteKey`); it is zeroed once no one uses it.
    pub fn delete_key(&self, name: &str) {
        self.lock().remove(name);
    }

    /// A random key id (`GenerateRandomKeyID`), as hex.
    pub fn generate_random_key_id() -> roastedduck_common::Result<String> {
        let mut id = [0u8; Self::KEY_ID_BYTES];
        generate_random_data(&mut id)?;
        Ok(id.iter().fold(String::new(), |mut text, byte| {
            use std::fmt::Write;
            let _ = write!(text, "{byte:02x}");
            text
        }))
    }

    /// Adds `key` under a new random id, which it returns; `key` is zeroed
    /// (`EncryptionEngine::AddKeyToCache(db, key)`).
    pub fn add_key_with_random_id(
        &self,
        key: &mut [u8; DEFAULT_ENCRYPTION_KEY_LENGTH],
    ) -> roastedduck_common::Result<String> {
        let id = match Self::generate_random_key_id() {
            Ok(id) => id,
            Err(error) => {
                key.zeroize();
                return Err(error);
            }
        };
        self.add_key(&id, key);
        Ok(id)
    }

    /// The key of encrypted temporary files, created at first use
    /// (`EncryptionEngine::AddTempKeyToCache`).
    pub fn temp_key(&self) -> roastedduck_common::Result<Arc<EncryptionKey>> {
        if let Some(key) = self.get_key(TEMP_KEY_NAME) {
            return Ok(key);
        }
        let mut key = [0u8; DEFAULT_ENCRYPTION_KEY_LENGTH];
        generate_random_data(&mut key)?;
        self.add_key(TEMP_KEY_NAME, &mut key);
        self.get_key(TEMP_KEY_NAME)
            .ok_or_else(|| roastedduck_common::Error::internal("the temporary key was not added"))
    }
}

/// `Blob::FromBase64`: the bytes of `text`, or `None` if it is not base64 (its length is
/// not a multiple of 4, it has a byte outside the alphabet, or padding before the last
/// two bytes).
pub fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
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
    if text.len() < 4 {
        return Some(Vec::new());
    }
    // `FromBase64Size`: padding is recognised by the last two bytes.
    let base_size = text.len() / 4 * 3;
    let size = if text[text.len() - 2] == b'=' {
        base_size - 2
    } else if text[text.len() - 1] == b'=' {
        base_size - 1
    } else {
        base_size
    };
    let mut result = Vec::with_capacity(size);
    let groups = text.len() / 4;
    for (index, group) in text.as_chunks::<4>().0.iter().enumerate() {
        let last = index + 1 == groups;
        let mut combined = 0u32;
        for (position, &byte) in group.iter().enumerate() {
            let decoded = if last && position >= 2 && byte == b'=' {
                0
            } else {
                value(byte)?
            };
            combined = (combined << 6) | decoded;
        }
        for shift in [16, 8, 0] {
            if result.len() < size {
                result.push((combined >> shift) as u8);
            }
        }
    }
    Some(result)
}

/// Derives the key of a file from the user's key and the file's salt, its database
/// identifier (`EncryptionKeyManager::DeriveKey`): SHA-256 of the salt followed by the
/// key, base64-decoded if it is base64. `user_key` and the decoded key are zeroed.
pub fn derive_key(user_key: &mut Vec<u8>, salt: &[u8; SALT_LEN]) -> [u8; DEFAULT_ENCRYPTION_KEY_LENGTH] {
    let mut decoded = base64_decode(user_key).unwrap_or_else(|| user_key.clone());
    // The hasher wipes its buffer when dropped (`sha2`'s `zeroize`).
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(&decoded);
    let mut digest = hasher.finalize();
    let mut derived = [0u8; DEFAULT_ENCRYPTION_KEY_LENGTH];
    derived.copy_from_slice(&digest);
    digest.as_mut_slice().zeroize();
    user_key.zeroize();
    decoded.zeroize();
    derived
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_follows_duckdb() {
        assert_eq!(base64_decode(b"asdf"), Some(vec![0x6a, 0xc7, 0x5f]));
        assert_eq!(base64_decode(b"YQ=="), Some(b"a".to_vec()));
        assert_eq!(base64_decode(b"YWI="), Some(b"ab".to_vec()));
        assert_eq!(base64_decode(b"abc"), None);
        assert_eq!(base64_decode(b"ab$d"), None);
        // Padding only in the last group, and not in its first two bytes.
        assert_eq!(base64_decode(b"YQ==YWJj"), None);
        assert_eq!(base64_decode(b"=bcd"), None);
        assert_eq!(base64_decode(b"gH8@#v$k1!!9=+=rYZ^32x==0plm*"), None);
    }

    /// The derived key of 'asdf' with an all-zero salt: SHA-256 of 16 zero bytes followed
    /// by the base64 decoding of the key.
    #[test]
    fn keys_are_derived_from_salt_and_decoded_key() {
        let mut key = b"asdf".to_vec();
        let derived = derive_key(&mut key, &[0; SALT_LEN]);
        let mut expected = Sha256::new();
        expected.update([0u8; SALT_LEN]);
        expected.update([0x6a, 0xc7, 0x5f]);
        assert_eq!(derived[..], expected.finalize()[..]);
        // A key that is not base64 is used as it is.
        let mut raw = b"gH8@#v$k1!!9=+=rYZ^32x==0plm*".to_vec();
        let derived = derive_key(&mut raw, &[1; SALT_LEN]);
        let mut expected = Sha256::new();
        expected.update([1u8; SALT_LEN]);
        expected.update(b"gH8@#v$k1!!9=+=rYZ^32x==0plm*");
        assert_eq!(derived[..], expected.finalize()[..]);
    }

    /// `test_encryption_key_wiping.cpp`: "Base64 input key derivation and wiping".
    #[test]
    fn derivation_wipes_the_user_key_and_adding_wipes_the_derived_key() {
        let manager = EncryptionKeyManager::default();
        let mut user_key = b"7nS+mQWj8vN2KzBfXzR4uL9kP1hT6mY3aI5oV8bC2uE=".to_vec();
        let mut salt = [0u8; SALT_LEN];
        generate_random_data(&mut salt).unwrap();
        let mut derived = derive_key(&mut user_key, &salt);
        manager.add_key("base64_key", &mut derived);
        assert_eq!(user_key, Vec::<u8>::new());
        assert_eq!(derived, [0; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        assert!(manager.has_key("base64_key"));
    }

    /// "Delete encryption key from cache" and "`EncryptionEngine` key management testing".
    #[test]
    fn the_cache_wipes_what_it_is_given() {
        let manager = EncryptionKeyManager::default();
        let mut derived = *b"a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6";
        manager.add_key("test_key", &mut derived);
        assert_eq!(derived, [0; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        assert_eq!(
            manager.get_key("test_key").unwrap().as_bytes(),
            b"a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"
        );
        manager.delete_key("test_key");
        assert!(!manager.has_key("test_key"));
        // A second key of the same name is wiped and not stored.
        let mut first = *b"a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6";
        manager.add_key("engine_key", &mut first);
        let mut second = *b"bbbbb3d4e5f6g7h8i9j0k1l2m3n4o5p6";
        manager.add_key("engine_key", &mut second);
        assert_eq!(second, [0; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        assert_eq!(
            manager.get_key("engine_key").unwrap().as_bytes(),
            b"a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"
        );
        // A cleared key is zero.
        let mut key = EncryptionKey::new(&mut [7; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        key.clear();
        assert_eq!(key.as_bytes(), &[0; DEFAULT_ENCRYPTION_KEY_LENGTH]);
        // Random ids differ; the temporary key is created once.
        let mut a = [1; DEFAULT_ENCRYPTION_KEY_LENGTH];
        let mut b = [2; DEFAULT_ENCRYPTION_KEY_LENGTH];
        let id_a = manager.add_key_with_random_id(&mut a).unwrap();
        let id_b = manager.add_key_with_random_id(&mut b).unwrap();
        assert_ne!(id_a, id_b);
        assert_eq!(id_a.len(), 2 * EncryptionKeyManager::KEY_ID_BYTES);
        let temp = manager.temp_key().unwrap();
        assert!(Arc::ptr_eq(&temp, &manager.temp_key().unwrap()));
    }
}
