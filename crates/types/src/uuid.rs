//! UUID values (DuckDB's `UUID`, `BaseUUID`).

use std::fmt;

use crate::{NativeType, PhysicalType};

/// A UUID stored like DuckDB: as a signed 128-bit integer whose top bit is flipped, so
/// that signed comparison orders UUIDs like their text form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Uuid(pub i128);

// SAFETY: a transparent `i128`; every bit pattern is a valid value.
unsafe impl NativeType for Uuid {
    const PHYSICAL: PhysicalType = PhysicalType::Int128;
}

/// The bit flipped between the stored value and the UUID's 128 bits.
const SIGN_FLIP: u128 = 1 << 127;

impl Uuid {
    /// Length of the canonical text form (`8-4-4-4-12` hex digits).
    pub const STRING_SIZE: usize = 36;

    /// Builds a UUID from its 128 bits in text order (the first hex digit is the top nibble).
    pub const fn from_u128(bits: u128) -> Self {
        Self((bits ^ SIGN_FLIP) as i128)
    }

    /// The UUID's 128 bits in text order; the inverse of [`Uuid::from_u128`]. This is
    /// DuckDB's `BaseUUID::ToUHugeint`, used by UUID <-> UHUGEINT casts.
    pub const fn to_u128(self) -> u128 {
        (self.0 as u128) ^ SIGN_FLIP
    }

    /// Parses the text form like DuckDB's `BaseUUID::FromString`.
    ///
    /// Accepts 32 hex digits (either case), optionally wrapped in `{...}`, with `-` allowed
    /// anywhere between them. In `strict` mode the input must additionally be exactly 36
    /// bytes with hyphens at offsets 8, 13, 18 and 23. Leading/trailing whitespace is not
    /// accepted. Returns `None` for anything else.
    pub fn parse(input: &[u8], strict: bool) -> Option<Self> {
        let digits = match input {
            [b'{', inner @ .., b'}'] => inner,
            [] | [b'{', ..] => return None,
            _ => input,
        };
        if strict
            && (input.len() != Self::STRING_SIZE
                || [8, 13, 18, 23].iter().any(|&i| input.get(i) != Some(&b'-')))
        {
            return None;
        }
        let mut bits = 0u128;
        let mut count = 0;
        for &c in digits.iter().filter(|&&c| c != b'-') {
            if count == 32 {
                return None;
            }
            bits = (bits << 4) | u128::from(hex_value(c)?);
            count += 1;
        }
        (count == 32).then_some(Self::from_u128(bits))
    }

    /// Appends the canonical lowercase 36-byte text form (DuckDB's `BaseUUID::ToString`).
    pub fn write_to(self, out: &mut Vec<u8>) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bits = self.to_u128();
        out.reserve(Self::STRING_SIZE);
        for nibble in 0..32 {
            if matches!(nibble, 8 | 12 | 16 | 20) {
                out.push(b'-');
            }
            let digit = (bits >> (124 - 4 * nibble)) & 0xF;
            out.push(HEX[digit as usize]);
        }
    }

    /// The 16 bytes of the UUID in text order, as produced by `CAST(uuid AS BLOB)`
    /// (DuckDB's `BaseUUID::ToBlob`).
    pub const fn to_blob(self) -> [u8; 16] {
        self.to_u128().to_be_bytes()
    }

    /// The inverse of [`Uuid::to_blob`] (DuckDB's `BaseUUID::FromBlob`).
    pub const fn from_blob(bytes: &[u8; 16]) -> Self {
        Self::from_u128(u128::from_be_bytes(*bytes))
    }

    /// `CAST(blob AS UUID)`: `None` unless the BLOB is exactly 16 bytes.
    pub fn try_from_blob(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self::from_blob)
    }
}

/// Value of an ASCII hex digit (either case).
fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = Vec::with_capacity(Self::STRING_SIZE);
        self.write_to(&mut text);
        // The text form is pure ASCII.
        f.write_str(std::str::from_utf8(&text).map_err(|_| fmt::Error)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11";

    fn parse(s: &str) -> Option<String> {
        Uuid::parse(s.as_bytes(), false).map(|u| u.to_string())
    }

    #[test]
    fn parse_accepts_duckdb_forms() {
        for input in [
            SAMPLE,
            "{A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11}",
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
            "{-a0eebc999c0b4ef8bb6d6bb9bd380a11-}",
        ] {
            assert_eq!(parse(input).as_deref(), Some(SAMPLE), "{input}");
        }
    }

    #[test]
    fn parse_rejects_malformed() {
        for input in [
            "",
            "{",
            "}",
            "{}",
            "{a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "a0eebc999c0b4ef8bb6d6bb9bd380a1",
            "a0eebc999c0b4ef8bb6d6bb9bd380a111",
            " a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "g0eebc999c0b4ef8bb6d6bb9bd380a11",
            "xyz",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
    }

    #[test]
    fn strict_parse_requires_canonical_layout() {
        assert!(Uuid::parse(SAMPLE.as_bytes(), true).is_some());
        assert!(Uuid::parse(b"A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11", true).is_some());
        assert!(Uuid::parse(b"a0eebc999c0b4ef8bb6d6bb9bd380a11", true).is_none());
        assert!(Uuid::parse(b"{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}", true).is_none());
        assert!(Uuid::parse(b"a0eebc99-9c0b4-ef8-bb6d-6bb9bd380a11", true).is_none());
    }

    #[test]
    fn storage_flips_the_sign_bit() {
        let zero = Uuid::parse(b"00000000-0000-0000-0000-000000000000", true).unwrap();
        assert_eq!(zero.0, i128::MIN);
        let max = Uuid::parse(b"ffffffff-ffff-ffff-ffff-ffffffffffff", true).unwrap();
        assert_eq!(max.0, i128::MAX);
        assert!(zero < max);
        assert_eq!(max.to_u128(), u128::MAX);
    }

    #[test]
    fn blob_round_trip() {
        let bytes: [u8; 16] = std::array::from_fn(|i| i as u8);
        let uuid = Uuid::from_blob(&bytes);
        assert_eq!(uuid.to_string(), "00010203-0405-0607-0809-0a0b0c0d0e0f");
        assert_eq!(uuid.to_blob(), bytes);
        assert_eq!(Uuid::try_from_blob(&bytes), Some(uuid));
        assert_eq!(Uuid::try_from_blob(&[0]), None);
        let zero = Uuid::parse(b"00000000-0000-0000-0000-000000000000", true).unwrap();
        assert_eq!(zero.to_blob(), [0; 16]);
    }

    #[test]
    fn write_to_appends() {
        let mut out = b"x".to_vec();
        Uuid::parse(SAMPLE.as_bytes(), true).unwrap().write_to(&mut out);
        assert_eq!(out, format!("x{SAMPLE}").as_bytes());
    }
}
