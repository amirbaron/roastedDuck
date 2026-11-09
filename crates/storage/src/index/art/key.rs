//! ART keys: values encoded so that byte-wise comparison matches value order (DuckDB's
//! `ARTKey` and `Radix`).
//!
//! Integers are stored big-endian with the sign bit flipped, floats are mapped to
//! order-preserving unsigned integers, and strings are escaped (bytes 0 and 1 get a `\x01`
//! prefix) and terminated by `\x00`. Compound keys concatenate their column keys; a key
//! with a NULL column is empty and never inserted.

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_types::{DataChunk, LogicalType, PhysicalType, StringT, Value, Vector, VectorKind};

/// An encoded key.
pub type ArtKey = Vec<u8>;

/// Size of an encoded row id (`sizeof(row_t)`).
pub const ROW_ID_SIZE: usize = 8;

/// Whether `ty` can be indexed by an ART.
pub fn is_supported_key_type(ty: &LogicalType) -> bool {
    matches!(
        ty.physical_type(),
        PhysicalType::Bool
            | PhysicalType::Int8
            | PhysicalType::Int16
            | PhysicalType::Int32
            | PhysicalType::Int64
            | PhysicalType::Int128
            | PhysicalType::UInt8
            | PhysicalType::UInt16
            | PhysicalType::UInt32
            | PhysicalType::UInt64
            | PhysicalType::UInt128
            | PhysicalType::Float
            | PhysicalType::Double
            | PhysicalType::Varchar
    )
}

/// The error for an unsupported key type (`InvalidTypeException`).
pub fn invalid_key_type(ty: &LogicalType) -> Error {
    Error::new(
        ErrorKind::InvalidType,
        format!("Invalid Type [{ty}]: Invalid type for index key."),
    )
}

/// Encodes a row id (`ARTKey::CreateARTKey<row_t>`).
pub fn row_id_key(row_id: i64) -> [u8; ROW_ID_SIZE] {
    let mut bytes = row_id.to_be_bytes();
    bytes[0] ^= 0x80;
    bytes
}

/// Decodes a key made by [`row_id_key`] (`ARTKey::GetRowId`).
pub fn decode_row_id(key: &[u8]) -> i64 {
    let mut bytes = [0u8; ROW_ID_SIZE];
    bytes.copy_from_slice(&key[..ROW_ID_SIZE]);
    bytes[0] ^= 0x80;
    i64::from_be_bytes(bytes)
}

/// `Radix::EncodeFloat`.
pub fn encode_float(x: f32) -> u32 {
    if x == 0.0 {
        return 1 << 31;
    }
    if x.is_nan() {
        return u32::MAX;
    }
    if x > f32::MAX {
        return u32::MAX - 1;
    }
    if x < -f32::MAX {
        return 0;
    }
    let bits = x.to_bits();
    if bits & (1 << 31) == 0 {
        bits | (1 << 31)
    } else {
        !bits
    }
}

/// `Radix::EncodeDouble`.
pub fn encode_double(x: f64) -> u64 {
    if x == 0.0 {
        return 1 << 63;
    }
    if x.is_nan() {
        return u64::MAX;
    }
    if x > f64::MAX {
        return u64::MAX - 1;
    }
    if x < -f64::MAX {
        return 0;
    }
    let bits = x.to_bits();
    if bits < (1 << 63) { bits + (1 << 63) } else { !bits }
}

fn push_string(out: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        if byte <= 1 {
            out.push(1);
        }
        out.push(byte);
    }
    out.push(0);
}

fn push_i128(out: &mut Vec<u8>, value: i128) {
    // upper as a signed 64-bit integer, lower as an unsigned one
    let mut upper = ((value >> 64) as i64).to_be_bytes();
    upper[0] ^= 0x80;
    out.extend_from_slice(&upper);
    out.extend_from_slice(&(value as u64).to_be_bytes());
}

/// Appends the encoding of `value` (non-NULL) to `out` (`ARTKey::CreateKey`).
pub fn encode_value(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    let ty = value.logical_type();
    let missing = || Error::internal("Invalid type for the ART key.");
    match ty.physical_type() {
        PhysicalType::Bool => out.push(u8::from(value.as_bool().ok_or_else(missing)?)),
        PhysicalType::Int8 => out.push((value.as_i8().ok_or_else(missing)? as u8) ^ 0x80),
        PhysicalType::Int16 => {
            let mut b = value.as_i16().ok_or_else(missing)?.to_be_bytes();
            b[0] ^= 0x80;
            out.extend_from_slice(&b);
        }
        PhysicalType::Int32 => {
            let mut b = value.as_i32().ok_or_else(missing)?.to_be_bytes();
            b[0] ^= 0x80;
            out.extend_from_slice(&b);
        }
        PhysicalType::Int64 => out.extend_from_slice(&row_id_key(value.as_i64().ok_or_else(missing)?)),
        PhysicalType::Int128 => push_i128(out, value.as_i128().ok_or_else(missing)?),
        PhysicalType::UInt8 => out.push(value.as_u8().ok_or_else(missing)?),
        PhysicalType::UInt16 => out.extend_from_slice(&value.as_u16().ok_or_else(missing)?.to_be_bytes()),
        PhysicalType::UInt32 => out.extend_from_slice(&value.as_u32().ok_or_else(missing)?.to_be_bytes()),
        PhysicalType::UInt64 => out.extend_from_slice(&value.as_u64().ok_or_else(missing)?.to_be_bytes()),
        PhysicalType::UInt128 => out.extend_from_slice(&value.as_u128().ok_or_else(missing)?.to_be_bytes()),
        PhysicalType::Float => {
            out.extend_from_slice(&encode_float(value.as_f32().ok_or_else(missing)?).to_be_bytes());
        }
        PhysicalType::Double => {
            out.extend_from_slice(&encode_double(value.as_f64().ok_or_else(missing)?).to_be_bytes());
        }
        PhysicalType::Varchar => push_string(out, value.as_bytes().ok_or_else(missing)?),
        _ => return Err(missing()),
    }
    Ok(())
}

/// The key of a single non-NULL value.
pub fn create_key(value: &Value) -> Result<ArtKey> {
    let mut key = Vec::new();
    encode_value(value, &mut key)?;
    Ok(key)
}

/// Appends the encoding of every row of `vector` to the matching key; rows that are NULL
/// make their key `None`.
fn concat_vector(vector: &Vector, count: usize, keys: &mut [Option<ArtKey>]) -> Result<()> {
    let flat;
    let vector = if vector.kind() == VectorKind::Sequence {
        flat = vector.flattened(count);
        &flat
    } else {
        vector
    };
    macro_rules! fixed {
        ($ty:ty, $encode:expr) => {{
            let data = vector.unified::<$ty>();
            for (row, key) in keys.iter_mut().enumerate().take(count) {
                let Some(key_bytes) = key.as_mut() else { continue };
                match data.get(row) {
                    Some(value) => {
                        let encode: fn($ty, &mut Vec<u8>) = $encode;
                        encode(value, key_bytes);
                    }
                    None => *key = None,
                }
            }
        }};
    }
    match vector.logical_type().physical_type() {
        PhysicalType::Bool => fixed!(bool, |v, out| out.push(u8::from(v))),
        PhysicalType::Int8 => fixed!(i8, |v, out| out.push((v as u8) ^ 0x80)),
        PhysicalType::Int16 => fixed!(i16, |v, out| {
            let mut b = v.to_be_bytes();
            b[0] ^= 0x80;
            out.extend_from_slice(&b);
        }),
        PhysicalType::Int32 => fixed!(i32, |v, out| {
            let mut b = v.to_be_bytes();
            b[0] ^= 0x80;
            out.extend_from_slice(&b);
        }),
        PhysicalType::Int64 => fixed!(i64, |v, out| out.extend_from_slice(&row_id_key(v))),
        PhysicalType::Int128 => fixed!(i128, |v, out| push_i128(out, v)),
        PhysicalType::UInt8 => fixed!(u8, |v, out| out.push(v)),
        PhysicalType::UInt16 => fixed!(u16, |v, out| out.extend_from_slice(&v.to_be_bytes())),
        PhysicalType::UInt32 => fixed!(u32, |v, out| out.extend_from_slice(&v.to_be_bytes())),
        PhysicalType::UInt64 => fixed!(u64, |v, out| out.extend_from_slice(&v.to_be_bytes())),
        PhysicalType::UInt128 => fixed!(u128, |v, out| out.extend_from_slice(&v.to_be_bytes())),
        PhysicalType::Float => fixed!(f32, |v, out| out
            .extend_from_slice(&encode_float(v).to_be_bytes())),
        PhysicalType::Double => fixed!(f64, |v, out| out
            .extend_from_slice(&encode_double(v).to_be_bytes())),
        PhysicalType::Varchar => {
            let data = vector.unified::<StringT>();
            for (row, key) in keys.iter_mut().enumerate().take(count) {
                let Some(key_bytes) = key.as_mut() else { continue };
                match data.get(row) {
                    Some(value) => push_string(key_bytes, vector.string_value(&value)),
                    None => *key = None,
                }
            }
        }
        _ => return Err(Error::internal("Invalid type for index")),
    }
    Ok(())
}

/// The keys of the rows of `chunk`, one column per key part (`ART::GenerateKeys`); rows
/// with a NULL in any column get `None`.
pub fn generate_keys(chunk: &DataChunk) -> Result<Vec<Option<ArtKey>>> {
    let count = chunk.len();
    let mut keys: Vec<Option<ArtKey>> = vec![Some(Vec::new()); count];
    for column in chunk.columns() {
        concat_vector(column, count, &mut keys)?;
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_keys_sort_like_values() {
        let values = [i64::MIN, -5, -1, 0, 1, 42, i64::MAX];
        let keys: Vec<_> = values.iter().map(|&v| row_id_key(v)).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        for &v in &values {
            assert_eq!(decode_row_id(&row_id_key(v)), v);
        }
    }

    #[test]
    fn float_keys_sort_like_values() {
        let values = [
            f64::NEG_INFINITY,
            -1e300,
            -1.5,
            0.0,
            1e-300,
            2.5,
            f64::INFINITY,
            f64::NAN,
        ];
        let keys: Vec<_> = values.iter().map(|&v| encode_double(v)).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(encode_double(-0.0), encode_double(0.0));
    }

    #[test]
    fn strings_are_escaped_and_terminated() {
        let mut out = Vec::new();
        push_string(&mut out, b"a\x00b\x01");
        assert_eq!(out, b"a\x01\x00b\x01\x01\x00");
        let mut a = Vec::new();
        push_string(&mut a, b"ab");
        let mut b = Vec::new();
        push_string(&mut b, b"abc");
        assert!(a < b);
    }

    #[test]
    fn hugeint_keys_sort_like_values() {
        let values = [i128::MIN, -1, 0, 1, i128::MAX];
        let keys: Vec<_> = values
            .iter()
            .map(|&v| {
                let mut out = Vec::new();
                push_i128(&mut out, v);
                out
            })
            .collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }
}
