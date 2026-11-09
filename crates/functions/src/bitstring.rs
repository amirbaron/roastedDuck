//! Operations on stored BIT strings (DuckDB's `Bit::BitwiseAnd`, `Bit::LeftShift`, ...),
//! shared by the bitwise operators, `get_bit`/`set_bit` and the `bit_*` aggregates.
//!
//! See [`roastedduck_types::bit`] for the layout: a padding-count byte followed by the bits,
//! most significant first, with the padding bits set to 1.

use roastedduck_common::{Error, Result};
use roastedduck_types::bit::bit_length;

/// The padding count of a stored bit string.
fn padding(bits: &[u8]) -> usize {
    bits.first().map_or(0, |&p| usize::from(p.min(7)))
}

/// Sets the padding bits to 1 (DuckDB's `Bit::Finalize`).
pub(crate) fn finalize(bits: &mut [u8]) {
    let padding = padding(bits);
    if let Some(first) = bits.get_mut(1) {
        for i in 0..padding {
            *first |= 1 << (7 - i);
        }
    }
}

/// Bit `n` (0-based from the most significant data bit, padding excluded).
pub(crate) fn get_bit(bits: &[u8], n: usize) -> bool {
    let index = n + padding(bits);
    bits.get(1 + index / 8)
        .is_some_and(|byte| byte >> (7 - index % 8) & 1 == 1)
}

/// Sets bit `n` (padding excluded).
pub(crate) fn set_bit(bits: &mut [u8], n: usize, value: bool) {
    let index = n + padding(bits);
    if let Some(byte) = bits.get_mut(1 + index / 8) {
        let mask = 1u8 << (7 - index % 8);
        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
}

/// Which binary bitwise operation to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BitwiseOp {
    And,
    Or,
    Xor,
}

impl BitwiseOp {
    const fn verb(self) -> &'static str {
        match self {
            Self::And => "AND",
            Self::Or => "OR",
            Self::Xor => "XOR",
        }
    }
}

/// Applies `op` to two bit strings of equal length into `lhs` (DuckDB's
/// `Bit::BitwiseAnd(rhs, lhs, lhs)` and friends).
pub(crate) fn bitwise_in_place(op: BitwiseOp, lhs: &mut [u8], rhs: &[u8]) -> Result<()> {
    if bit_length(lhs) != bit_length(rhs) {
        return Err(Error::invalid_input(format!(
            "Cannot {} bit strings of different sizes",
            op.verb()
        )));
    }
    for (l, &r) in lhs.iter_mut().zip(rhs).skip(1) {
        *l = match op {
            BitwiseOp::And => *l & r,
            BitwiseOp::Or => *l | r,
            BitwiseOp::Xor => *l ^ r,
        };
    }
    finalize(lhs);
    Ok(())
}

/// `~bits` (DuckDB's `Bit::BitwiseNot`).
pub(crate) fn bitwise_not(bits: &[u8]) -> Vec<u8> {
    let mut result: Vec<u8> = bits
        .iter()
        .enumerate()
        .map(|(i, &b)| if i == 0 { b } else { !b })
        .collect();
    finalize(&mut result);
    result
}

/// `bits << shift` (DuckDB's `Bit::LeftShift`): the length is kept.
pub(crate) fn left_shift(bits: &[u8], shift: usize) -> Vec<u8> {
    let mut result = vec![0u8; bits.len()];
    if let Some(&first) = bits.first() {
        result[0] = first;
    }
    let len = bit_length(bits);
    for i in 0..len {
        let bit = i < len.saturating_sub(shift) && get_bit(bits, shift + i);
        set_bit(&mut result, i, bit);
    }
    finalize(&mut result);
    result
}

/// `bits >> shift` (DuckDB's `Bit::RightShift`): the length is kept.
pub(crate) fn right_shift(bits: &[u8], shift: usize) -> Vec<u8> {
    let mut result = vec![0u8; bits.len()];
    if let Some(&first) = bits.first() {
        result[0] = first;
    }
    for i in 0..bit_length(bits) {
        let bit = i >= shift && get_bit(bits, i - shift);
        set_bit(&mut result, i, bit);
    }
    finalize(&mut result);
    result
}

/// An all-zero bit string of `len` bits.
pub(crate) fn zeros(len: usize) -> Vec<u8> {
    let mut result = vec![0u8; roastedduck_types::bit::stored_len(len)];
    result[0] = ((8 - len % 8) % 8) as u8;
    finalize(&mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        roastedduck_types::bit::parse_text(text.as_bytes(), &mut out).unwrap();
        out
    }

    fn render(bits: &[u8]) -> String {
        let mut out = Vec::new();
        roastedduck_types::bit::write_text(bits, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn bitwise_operations_match_duckdb() {
        let mut a = parse("10101");
        bitwise_in_place(BitwiseOp::And, &mut a, &parse("10001")).unwrap();
        assert_eq!(render(&a), "10001");
        assert_eq!(render(&bitwise_not(&parse("10101"))), "01010");
        assert_eq!(render(&left_shift(&parse("1001011"), 3)), "1011000");
        assert_eq!(render(&right_shift(&parse("1001011"), 3)), "0001001");
        assert_eq!(render(&zeros(10)), "0000000000");
        let error = bitwise_in_place(BitwiseOp::Or, &mut parse("1"), &parse("10")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid Input Error: Cannot OR bit strings of different sizes"
        );
    }
}
