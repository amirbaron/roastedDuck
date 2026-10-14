//! LEB128 variable-length integers (DuckDB's `EncodingUtil`).

/// Appends the unsigned LEB128 encoding of `value`.
pub(crate) fn encode_unsigned(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Appends the signed LEB128 encoding of `value`.
pub(crate) fn encode_signed(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Decodes an unsigned value of `bits` width from a complete varint (continuation bits
/// already checked by the caller); `None` signals corrupt data.
pub(crate) fn decode_unsigned(bytes: &[u8], bits: u32) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    for &byte in bytes {
        if shift >= bits {
            return None;
        }
        result |= u64::from(byte & 0x7F) << shift;
        shift += 7;
    }
    Some(result)
}

/// Decodes a signed value of `bits` width, sign-extending like `DecodeSignedLEB128`.
pub(crate) fn decode_signed(bytes: &[u8], bits: u32) -> Option<i64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    let mut last = 0u8;
    for &byte in bytes {
        if shift >= bits {
            return None;
        }
        result |= u64::from(byte & 0x7F) << shift;
        shift += 7;
        last = byte;
    }
    if shift < bits && last & 0x40 != 0 {
        result |= u64::MAX << shift;
    }
    Some(result as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_encodings() {
        let mut out = Vec::new();
        encode_unsigned(&mut out, 624_485);
        assert_eq!(out, [0xE5, 0x8E, 0x26]);
        out.clear();
        encode_signed(&mut out, -123_456);
        assert_eq!(out, [0xC0, 0xBB, 0x78]);
        out.clear();
        encode_signed(&mut out, 64);
        assert_eq!(out, [0xC0, 0x00]);
        assert_eq!(decode_signed(&[0xC0, 0xBB, 0x78], 64), Some(-123_456));
        assert_eq!(decode_unsigned(&[0xE5, 0x8E, 0x26], 64), Some(624_485));
        assert_eq!(decode_signed(&[0x7F], 8), Some(-1));
        assert_eq!(decode_unsigned(&[0x80, 0x80, 0x01], 8), None);
    }
}
