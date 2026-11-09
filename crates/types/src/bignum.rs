//! Text rendering of stored BIGNUM values (DuckDB's `Bignum::BignumToVarchar`); the
//! arithmetic and conversions live in `roastedduck_function::bignum`.
//!
//! A stored bignum is a 3-byte header followed by the big-endian magnitude; for negative
//! numbers the header and every data byte are bit-inverted.

/// Size of the header.
pub const HEADER_SIZE: usize = 3;

/// Appends the decimal text of a stored bignum (the `CPython` base conversion, Knuth's
/// method 1b, as in DuckDB). `Err` for a value shorter than the header plus one byte.
pub fn write_text(bignum: &[u8], out: &mut Vec<u8>) -> Result<(), &'static str> {
    const DECIMAL_BASE: u64 = 1_000_000_000;
    if bignum.len() < HEADER_SIZE + 1 {
        return Err("Invalid blob size.");
    }
    let negative = bignum[0] & 0x80 == 0;
    let mut bytes: Vec<u8> = bignum[HEADER_SIZE..]
        .iter()
        .map(|&byte| if negative { !byte } else { byte })
        .collect();
    let padding = bytes.len().wrapping_neg() & 3;
    bytes.splice(0..0, std::iter::repeat_n(0, padding));
    let mut digits: Vec<u32> = Vec::new();
    for word in bytes.as_chunks::<4>().0 {
        let mut hi = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        for digit in &mut digits {
            let tmp = (u64::from(*digit) << 32) | u64::from(hi);
            hi = (tmp / DECIMAL_BASE) as u32;
            *digit = (tmp - DECIMAL_BASE * u64::from(hi)) as u32;
        }
        while hi != 0 {
            digits.push(hi % DECIMAL_BASE as u32);
            hi /= DECIMAL_BASE as u32;
        }
    }
    if digits.is_empty() {
        digits.push(0);
    }
    if negative {
        out.push(b'-');
    }
    if let Some((last, rest)) = digits.split_last() {
        out.extend_from_slice(last.to_string().as_bytes());
        for digit in rest.iter().rev() {
            out.extend_from_slice(format!("{digit:09}").as_bytes());
        }
    }
    Ok(())
}
