//! BIT strings (DuckDB's `Bit`).
//!
//! A bit string is stored like DuckDB's `bitstring_t`: the first byte holds the number of
//! padding bits (0..=7), followed by the bits packed most-significant first. The padding
//! bits are the leading (high) bits of the first data byte and are always set to 1. For
//! example `'0101'::BIT` is stored as `[4, 0b1111_0101]`.
//!
//! Functions taking a stored bit string never panic, even on malformed input (empty, or a
//! padding byte above 7); the result is unspecified for such input. Error messages are
//! DuckDB's, byte for byte, including invalid UTF-8 in conversion diagnostics.

use crate::PhysicalType;
use rustdb_common::ByteString;

/// Number of padding bits a bit string of `bit_len` bits needs.
const fn padding_for(bit_len: usize) -> u8 {
    ((8 - bit_len % 8) % 8) as u8
}

/// The padding count of a stored bit string, clamped to a valid value.
fn padding(bits: &[u8]) -> usize {
    bits.first().map_or(0, |&p| usize::from(p.min(7)))
}

/// The data bytes of a stored bit string (everything after the padding byte).
fn data(bits: &[u8]) -> &[u8] {
    bits.get(1..).unwrap_or_default()
}

/// The data bytes with the padding bits of the first byte cleared (DuckDB's
/// `Bit::GetFirstByte` applied to the first byte).
fn masked_data(bits: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mask = 0xFFu8 >> padding(bits);
    data(bits)
        .iter()
        .enumerate()
        .map(move |(i, &b)| if i == 0 { b & mask } else { b })
}

/// Size in bytes of a stored bit string holding `bit_len` bits, including the padding
/// byte (DuckDB's `Bit::ComputeBitstringLen`).
pub const fn stored_len(bit_len: usize) -> usize {
    bit_len.div_ceil(8) + 1
}

/// Number of bits in a stored bit string (`bit_length`, DuckDB's `Bit::BitLength`).
pub fn bit_length(bits: &[u8]) -> usize {
    (data(bits).len() * 8).saturating_sub(padding(bits))
}

/// Number of data bytes in a stored bit string (`octet_length`, DuckDB's
/// `Bit::OctetLength`).
pub fn octet_length(bits: &[u8]) -> usize {
    data(bits).len()
}

/// Validates the text form of a bit string and returns its stored size (DuckDB's
/// `Bit::TryGetBitStringSize`).
pub fn text_stored_len(input: &[u8]) -> Result<usize, ByteString> {
    if let Some(&bad) = input.iter().find(|&&c| c != b'0' && c != b'1') {
        return Err(rustdb_common::byte_format!(
            "Invalid character encountered in string -> bit conversion: '{}'",
            &[bad][..]
        ));
    }
    if input.is_empty() {
        return Err("Cannot cast empty string to BIT".into());
    }
    Ok(stored_len(input.len()))
}

/// Appends the stored form of the bit string written as `input`, a non-empty sequence of
/// `0`/`1` characters (`CAST(text AS BIT)`, DuckDB's `TryCastToBit`).
///
/// On failure `out` is left unchanged and the error is DuckDB's conversion message, e.g.
/// `Invalid character encountered in string -> bit conversion: 'a'`.
pub fn parse_text(input: &[u8], out: &mut Vec<u8>) -> Result<(), ByteString> {
    out.reserve(text_stored_len(input)?);
    let padding = padding_for(input.len());
    out.push(padding);
    // Left-pad the first chunk with `1`s so every chunk is a full byte.
    let (head, tail) = input.split_at(input.len() % 8);
    let pack = |init: u8, chunk: &[u8]| chunk.iter().fold(init, |acc, &c| acc << 1 | u8::from(c == b'1'));
    if !head.is_empty() {
        out.push(pack(0xFF, head));
    }
    out.extend(tail.chunks(8).map(|chunk| pack(0, chunk)));
    Ok(())
}

/// Appends the `0`/`1` text form of a stored bit string (`CAST(bit AS VARCHAR)`,
/// DuckDB's `Bit::ToString`).
pub fn write_text(bits: &[u8], out: &mut Vec<u8>) {
    let skip = padding(bits);
    out.reserve(bit_length(bits));
    let all_bits = data(bits)
        .iter()
        .flat_map(|&byte| (0..8).rev().map(move |i| b'0' + ((byte >> i) & 1)));
    out.extend(all_bits.skip(skip));
}

/// Appends the bit string holding the bytes of a BLOB (`CAST(blob AS BIT)`, DuckDB's
/// `Bit::BlobToBit`). Fails with DuckDB's message for an empty BLOB.
pub fn from_blob(blob: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    if blob.is_empty() {
        return Err("Cannot cast empty BLOB to BIT".to_owned());
    }
    out.reserve(blob.len() + 1);
    out.push(0);
    out.extend_from_slice(blob);
    Ok(())
}

/// Appends the bytes of a stored bit string as a BLOB, with the padding bits cleared
/// (`CAST(bit AS BLOB)`, DuckDB's `Bit::BitToBlob`).
pub fn to_blob(bits: &[u8], out: &mut Vec<u8>) {
    out.extend(masked_data(bits));
}

/// A numeric type that converts to and from BIT by its big-endian bytes (DuckDB's
/// `Bit::NumericToBit` / `Bit::BitToNumeric`).
pub trait BitNumeric: Copy + sealed::Sealed {
    /// The type named in "Bitstring doesn't fit inside of ..." errors.
    const PHYSICAL: PhysicalType;
    /// Size of the value in bytes.
    const SIZE: usize;

    /// Appends the big-endian bytes of `self`.
    fn write_be(self, out: &mut Vec<u8>);

    /// Builds a value from at most [`BitNumeric::SIZE`] big-endian bytes; missing leading
    /// bytes are zero.
    fn from_be(bytes: impl Iterator<Item = u8>) -> Self;
}

mod sealed {
    pub trait Sealed {}
}

/// Packs big-endian bytes into the low end of a `u128` (callers pass at most 16 bytes).
fn pack_be(bytes: impl Iterator<Item = u8>) -> u128 {
    bytes.fold(0, |acc, b| acc << 8 | u128::from(b))
}

macro_rules! bit_numeric {
    ($($ty:ty => $phys:ident, |$v:ident| $from_bits:expr;)*) => {$(
        impl sealed::Sealed for $ty {}

        impl BitNumeric for $ty {
            const PHYSICAL: PhysicalType = PhysicalType::$phys;
            const SIZE: usize = size_of::<$ty>();

            fn write_be(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_be_bytes());
            }

            fn from_be(bytes: impl Iterator<Item = u8>) -> Self {
                let $v = pack_be(bytes);
                $from_bits
            }
        }
    )*};
}

// Truncating casts reinterpret the low `SIZE` bytes, which hold all the packed bits.
bit_numeric! {
    i8 => Int8, |v| v as i8;
    i16 => Int16, |v| v as i16;
    i32 => Int32, |v| v as i32;
    i64 => Int64, |v| v as i64;
    i128 => Int128, |v| v as i128;
    u8 => UInt8, |v| v as u8;
    u16 => UInt16, |v| v as u16;
    u32 => UInt32, |v| v as u32;
    u64 => UInt64, |v| v as u64;
    u128 => UInt128, |v| v;
    f32 => Float, |v| f32::from_bits(v as u32);
    f64 => Double, |v| f64::from_bits(v as u64);
}

impl sealed::Sealed for bool {}

/// BOOLEAN converts through `UINT8`: `true` is `00000001`, and a bit string is `true` when
/// any bit is set.
impl BitNumeric for bool {
    const PHYSICAL: PhysicalType = PhysicalType::UInt8;
    const SIZE: usize = 1;

    fn write_be(self, out: &mut Vec<u8>) {
        out.push(u8::from(self));
    }

    fn from_be(bytes: impl Iterator<Item = u8>) -> Self {
        pack_be(bytes) as u8 > 0
    }
}

/// Appends the bit string holding the big-endian bytes of `value` (`CAST(number AS BIT)`,
/// DuckDB's `Bit::NumericToBit`); the result has `8 * size_of::<T>()` bits.
pub fn from_numeric<T: BitNumeric>(value: T, out: &mut Vec<u8>) {
    out.reserve(T::SIZE + 1);
    out.push(0);
    value.write_be(out);
}

/// Reads a stored bit string as a number (`CAST(bit AS number)`, DuckDB's
/// `CastFromBitToNumeric`): the bits are right-aligned and zero-extended.
///
/// Fails with DuckDB's `Bitstring doesn't fit inside of INT8` (etc.) when the bit string
/// has more data bytes than the target type. DuckDB raises this as a hard conversion error
/// that `TRY_CAST` does not suppress.
pub fn to_numeric<T: BitNumeric>(bits: &[u8]) -> Result<T, String> {
    if octet_length(bits) > T::SIZE {
        return Err(format!("Bitstring doesn't fit inside of {}", T::PHYSICAL.name()));
    }
    Ok(T::from_be(masked_data(bits)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Vec<u8>, ByteString> {
        let mut out = Vec::new();
        parse_text(text.as_bytes(), &mut out).map(|()| out)
    }

    fn bit(text: &str) -> Vec<u8> {
        parse(text).unwrap()
    }

    fn text(bits: &[u8]) -> String {
        let mut out = Vec::new();
        write_text(bits, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn parse_stores_padding_like_duckdb() {
        assert_eq!(bit("0101"), [4, 0b1111_0101]);
        assert_eq!(bit("1"), [7, 0xFF]);
        assert_eq!(bit("00000000"), [0, 0]);
        assert_eq!(bit("101010101"), [7, 0xFF, 0b0101_0101]);
        assert_eq!(bit("0000000011"), [6, 0b1111_1100, 0b0000_0011]);
        assert_eq!(text_stored_len(b"101010101"), Ok(3));
    }

    #[test]
    fn text_round_trip() {
        for s in [
            "0101",
            "1",
            "0",
            "00000000",
            "101010101",
            "01",
            "1000000000000000",
        ] {
            let bits = bit(s);
            assert_eq!(text(&bits), s);
            assert_eq!(bit_length(&bits), s.len());
        }
        assert_eq!(octet_length(&bit("101010101")), 2);
        assert_eq!(octet_length(&bit("01")), 1);
    }

    #[test]
    fn parse_errors_match_duckdb() {
        let invalid = "Invalid character encountered in string -> bit conversion: ";
        assert_eq!(parse("01a1").unwrap_err(), format!("{invalid}'a'"));
        assert_eq!(parse(" 1").unwrap_err(), format!("{invalid}' '"));
        assert_eq!(parse("x1").unwrap_err(), format!("{invalid}'x'"));
        assert_eq!(parse("2").unwrap_err(), format!("{invalid}'2'"));
        assert_eq!(parse("").unwrap_err(), "Cannot cast empty string to BIT");
        // DuckDB embeds the lone byte 0xC3, rather than a Unicode replacement.
        assert_eq!(
            parse("é").unwrap_err().as_bytes(),
            b"Invalid character encountered in string -> bit conversion: '\xc3'"
        );
    }

    fn numeric_bits<T: BitNumeric>(value: T) -> String {
        let mut out = Vec::new();
        from_numeric(value, &mut out);
        text(&out)
    }

    #[test]
    fn numeric_to_bit() {
        assert_eq!(numeric_bits(5i8), "00000101");
        assert_eq!(numeric_bits(-1i16), "1".repeat(16));
        assert_eq!(numeric_bits(1i32), format!("{}1", "0".repeat(31)));
        assert_eq!(numeric_bits(1i64), format!("{}1", "0".repeat(63)));
        assert_eq!(numeric_bits(1i128), format!("{}1", "0".repeat(127)));
        assert_eq!(numeric_bits(1u128), format!("{}1", "0".repeat(127)));
        assert_eq!(numeric_bits(-1i128), "1".repeat(128));
        assert_eq!(numeric_bits(255u8), "11111111");
        assert_eq!(numeric_bits(1.5f32), "00111111110000000000000000000000");
        assert_eq!(numeric_bits(1.5f64), format!("0011111111111{}", "0".repeat(51)));
        assert_eq!(numeric_bits(true), "00000001");
    }

    #[test]
    fn bit_to_numeric() {
        assert_eq!(to_numeric::<i8>(&bit("0101")), Ok(5));
        assert_eq!(to_numeric::<i8>(&bit("1111")), Ok(15));
        assert_eq!(to_numeric::<i8>(&bit("11111111")), Ok(-1));
        assert_eq!(to_numeric::<i16>(&bit("1000000000000000")), Ok(-32768));
        assert_eq!(to_numeric::<u16>(&bit("1111")), Ok(15));
        assert_eq!(to_numeric::<i128>(&bit(&"1".repeat(128))), Ok(-1));
        assert_eq!(to_numeric::<u128>(&bit("1")), Ok(1));
        assert_eq!(to_numeric::<u64>(&bit("1")), Ok(1));
        assert_eq!(to_numeric::<f32>(&bit("1")), Ok(1e-45));
        assert_eq!(to_numeric::<f64>(&bit("1")), Ok(5e-324));
        let mut half = Vec::new();
        from_numeric(1.5f64, &mut half);
        assert_eq!(to_numeric::<f64>(&half), Ok(1.5));
        assert_eq!(to_numeric::<bool>(&bit("0")), Ok(false));
        assert_eq!(to_numeric::<bool>(&bit("1")), Ok(true));
        assert_eq!(to_numeric::<bool>(&bit("00000010")), Ok(true));
    }

    #[test]
    fn bit_to_numeric_overflow_messages() {
        fn err<T: BitNumeric + std::fmt::Debug>(len: usize) -> String {
            to_numeric::<T>(&bit(&"1".repeat(len))).unwrap_err()
        }
        let msg = |name: &str| format!("Bitstring doesn't fit inside of {name}");
        assert_eq!(err::<i8>(9), msg("INT8"));
        assert_eq!(err::<bool>(9), msg("UINT8"));
        assert_eq!(err::<u8>(9), msg("UINT8"));
        assert_eq!(err::<i16>(17), msg("INT16"));
        assert_eq!(err::<u16>(17), msg("UINT16"));
        assert_eq!(err::<i32>(33), msg("INT32"));
        assert_eq!(err::<u32>(33), msg("UINT32"));
        assert_eq!(err::<i64>(65), msg("INT64"));
        assert_eq!(err::<u64>(65), msg("UINT64"));
        assert_eq!(err::<i128>(129), msg("INT128"));
        assert_eq!(err::<u128>(129), msg("UINT128"));
        assert_eq!(err::<f32>(33), msg("FLOAT"));
        assert_eq!(err::<f64>(65), msg("DOUBLE"));
    }

    #[test]
    fn blob_conversions() {
        let mut out = Vec::new();
        from_blob(b"\x01\xFF", &mut out).unwrap();
        assert_eq!(text(&out), "0000000111111111");
        assert_eq!(
            from_blob(b"", &mut out).unwrap_err(),
            "Cannot cast empty BLOB to BIT"
        );
        let blob = |s: &str| {
            let mut out = Vec::new();
            to_blob(&bit(s), &mut out);
            out
        };
        assert_eq!(blob("0101"), b"\x05");
        assert_eq!(blob("101010101"), b"\x01U");
        assert_eq!(blob("1"), b"\x01");
        assert_eq!(blob("1000"), b"\x08");
    }

    #[test]
    fn malformed_storage_does_not_panic() {
        for bits in [&[][..], &[9], &[200, 0xAB], &[0]] {
            let mut out = Vec::new();
            write_text(bits, &mut out);
            to_blob(bits, &mut out);
            let _ = to_numeric::<i64>(bits);
            let _ = bit_length(bits);
        }
    }
}
