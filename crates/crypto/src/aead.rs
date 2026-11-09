//! AES-GCM modules with the raw key sizes used by Parquet modular encryption.
use aes::{Aes128, Aes192, Aes256};
use aes_gcm::aead::consts::U12;
use aes_gcm::{AeadInOut, AesGcm, KeyInit};
use roastedduck_common::{Error, Result};

/// Encrypt in place, authenticating `aad`, and return the detached 16-byte tag.
pub fn encrypt_gcm(key: &[u8], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> Result<[u8; 16]> {
    macro_rules! encrypt {
        ($cipher:ty) => {{
            AesGcm::<$cipher, U12>::new_from_slice(key)
                .map_err(|_| invalid_key())?
                .encrypt_inout_detached(nonce.into(), aad, data.into())
                .map_err(|_| Error::internal("AES-GCM encryption failed"))?
        }};
    }
    let tag = match key.len() {
        16 => encrypt!(Aes128),
        24 => encrypt!(Aes192),
        32 => encrypt!(Aes256),
        _ => return Err(invalid_key()),
    };
    Ok(tag.into())
}

/// Authenticate and decrypt in place. Callers must discard the buffer on error.
pub fn decrypt_gcm(key: &[u8], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> Result<()> {
    macro_rules! decrypt {
        ($cipher:ty) => {{
            AesGcm::<$cipher, U12>::new_from_slice(key)
                .map_err(|_| invalid_key())?
                .decrypt_inout_detached(nonce.into(), aad, data.into(), tag.into())
                .map_err(|_| {
                    Error::invalid_input(
                        "Computed AES tag differs from read AES tag, are you using the right key?",
                    )
                })
        }};
    }
    match key.len() {
        16 => decrypt!(Aes128),
        24 => decrypt!(Aes192),
        32 => decrypt!(Aes256),
        _ => Err(invalid_key()),
    }
}

fn invalid_key() -> Error {
    Error::invalid_input("Invalid AES key. Must have a length of 128, 192, or 256 bits (16, 24, or 32 bytes)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_empty_gcm_tag_and_supported_key_sizes_authenticate() {
        // NIST SP800-38D: all-zero AES-128 key/nonce and empty plaintext/AAD.
        assert_eq!(
            encrypt_gcm(&[0; 16], &[0; 12], &[], &mut []).unwrap(),
            [
                0x58, 0xe2, 0xfc, 0xce, 0xfa, 0x7e, 0x30, 0x61, 0x36, 0x7f, 0x1d, 0x57, 0xa4, 0xe7, 0x45,
                0x5a
            ]
        );
        for size in [16, 24, 32] {
            let key = vec![7; size];
            let mut data = b"module data across more than one AES block".to_vec();
            let plain = data.clone();
            let tag = encrypt_gcm(&key, &[9; 12], b"module aad", &mut data).unwrap();
            let cipher = data.clone();
            decrypt_gcm(&key, &[9; 12], b"module aad", &mut data, &tag).unwrap();
            assert_eq!(data, plain);
            assert!(decrypt_gcm(&key, &[9; 12], b"wrong aad", &mut cipher.clone(), &tag).is_err());
            let mut corrupt = tag;
            corrupt[0] ^= 1;
            assert!(decrypt_gcm(&key, &[9; 12], b"module aad", &mut cipher.clone(), &corrupt).is_err());
        }
    }
}
