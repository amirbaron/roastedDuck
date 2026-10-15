//! BIGNUM values (DuckDB's `Bignum` in `common/types/bignum.cpp` and
//! `BignumIntermediate` in `common/bignum.cpp`).
//!
//! A stored bignum is a 3-byte header followed by the big-endian magnitude. The header
//! holds the number of data bytes with bit 23 set; for negative numbers the header and
//! every data byte are bit-inverted, so that bignums compare like their bytes.
//!
//! Conversions reproduce DuckDB's quirks, e.g. the floating-point byte counting of
//! integer casts (`2^56::BIGNUM` is `0` in DuckDB).

use rustdb_common::{Error, Result};

/// Size of the header.
pub const HEADER_SIZE: usize = 3;
/// The largest number of data bytes (`Bignum::MAX_DATA_SIZE`).
pub const MAX_DATA_SIZE: usize = 8_388_607;

/// `Bignum::SetHeader`.
pub fn header(number_of_bytes: usize, negative: bool) -> [u8; 3] {
    let mut header = (number_of_bytes as u32) | 0x0080_0000;
    if negative {
        header = !header;
    }
    [(header >> 16) as u8, (header >> 8) as u8, header as u8]
}

/// Whether a stored bignum is negative.
pub fn is_negative(bignum: &[u8]) -> bool {
    bignum.first().is_some_and(|byte| byte & 0x80 == 0)
}

/// The bignum `0` (`Bignum::InitializeBignumZero`).
pub fn zero() -> Vec<u8> {
    let mut result = header(1, false).to_vec();
    result.push(0);
    result
}

/// A bignum from its magnitude bytes (big-endian, as many as given).
fn from_magnitude(magnitude: &[u8], negative: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(HEADER_SIZE + magnitude.len());
    result.extend_from_slice(&header(magnitude.len(), negative));
    result.extend(magnitude.iter().map(|&byte| if negative { !byte } else { byte }));
    result
}

/// `ceil(log2(value) / 8)` computed in floating point like DuckDB.
fn float_byte_count(value: u64) -> usize {
    ((value as f64).log2() / 8.0).ceil() as usize
}

/// DuckDB's `IntToBignum` for a value given as a sign and a 64-bit magnitude.
pub fn from_u64(negative: bool, magnitude: u64) -> Vec<u8> {
    let size = if magnitude == u64::MAX {
        float_byte_count(magnitude)
    } else if magnitude == 0 {
        1
    } else {
        float_byte_count(magnitude + 1)
    };
    let bytes: Vec<u8> = (0..size)
        .rev()
        .map(|i| if i < 8 { (magnitude >> (i * 8)) as u8 } else { 0 })
        .collect();
    from_magnitude(&bytes, negative)
}

/// DuckDB's `IntToBignum<T>` for signed integers up to 64 bits.
pub fn from_i64(value: i64) -> Vec<u8> {
    from_u64(value < 0, value.unsigned_abs())
}

/// The byte layout of DuckDB's `HugeintCastToBignum` for a 128-bit magnitude.
fn u128_magnitude(magnitude: u128) -> Vec<u8> {
    let upper = (magnitude >> 64) as u64;
    let lower = magnitude as u64;
    let upper_size = if upper == u64::MAX {
        float_byte_count(upper)
    } else if upper == 0 {
        0
    } else {
        float_byte_count(upper + 1)
    };
    let mut size = upper_size;
    if size > 0 {
        size += 8;
    } else if lower == u64::MAX {
        size += float_byte_count(lower);
    } else {
        size += float_byte_count(lower + 1);
    }
    let size = size.max(1);
    let mut bytes = Vec::with_capacity(size);
    for i in (0..upper_size).rev() {
        bytes.push(if i < 8 { (upper >> (i * 8)) as u8 } else { 0 });
    }
    for i in (0..size - upper_size).rev() {
        bytes.push(if i < 8 { (lower >> (i * 8)) as u8 } else { 0 });
    }
    bytes
}

/// `HugeintCastToBignum::Operation(uhugeint_t)`.
pub fn from_u128(value: u128) -> Vec<u8> {
    from_magnitude(&u128_magnitude(value), false)
}

/// `HugeintCastToBignum::Operation(hugeint_t)`.
pub fn from_i128(value: i128) -> Vec<u8> {
    from_magnitude(&u128_magnitude(value.unsigned_abs()), value < 0)
}

/// `DoubleToBignum`: `None` for infinities and NaN. The fraction is dropped.
pub fn from_f64(value: f64) -> Option<Vec<u8>> {
    if !value.is_finite() {
        return None;
    }
    let negative = value < 0.0;
    let mut magnitude = value.abs();
    if magnitude == 0.0 {
        return Some(zero());
    }
    let mut bytes = Vec::new();
    while magnitude > 0.0 {
        let truncated = (magnitude / 256.0).floor();
        bytes.push((magnitude - truncated * 256.0) as u8);
        magnitude = truncated;
    }
    bytes.reverse();
    Some(from_magnitude(&bytes, negative))
}

/// `Bignum::VarcharFormatting` followed by `Bignum::EncodeBignum`: `None` if the text is
/// not a number (a fraction is truncated).
pub fn from_text(text: &[u8]) -> Option<Vec<u8>> {
    let (start, end, negative, is_zero) = varchar_formatting(text)?;
    if is_zero {
        return Some(zero());
    }
    Some(encode(&text[start..end], negative))
}

/// `Bignum::VarcharFormatting`: the digit range, the sign and whether the value is zero.
fn varchar_formatting(text: &[u8]) -> Option<(usize, usize, bool, bool)> {
    let first = *text.first()?;
    let mut start = 0;
    let mut end = text.len();
    let negative = first == b'-';
    if negative || first == b'+' {
        start += 1;
    }
    let mut at_least_one_zero = false;
    while start < end && text[start] == b'0' {
        start += 1;
        at_least_one_zero = true;
    }
    if start == end {
        return at_least_one_zero.then_some((start, end, negative, true));
    }
    let mut position = start;
    while position < end && text[position].is_ascii_digit() {
        position += 1;
    }
    if position < end {
        let possible_end = position;
        if text[position] != b'.' {
            return None;
        }
        position += 1;
        let mut digit_after_point = false;
        while position < end {
            if !text[position].is_ascii_digit() {
                return None;
            }
            digit_after_point = true;
            position += 1;
        }
        if possible_end == start {
            if !at_least_one_zero && !digit_after_point {
                return None;
            }
            return Some((start, end, negative, true));
        }
        end = possible_end;
    }
    Some((start, end, negative, false))
}

/// `Bignum::EncodeBignum` of a digit string without leading zeros.
fn encode(digits_text: &[u8], negative: bool) -> Vec<u8> {
    const MAX_DIGITS: usize = 16;
    const CHUNK: u64 = 10_000_000_000_000_000;
    // Chunks of 16 decimal digits, least significant first.
    let mut digits: Vec<u64> = digits_text
        .rchunks(MAX_DIGITS)
        .map(|chunk| {
            chunk
                .iter()
                .fold(0u64, |value, &c| value * 10 + u64::from(c - b'0'))
        })
        .collect();
    let mut bytes = Vec::new();
    while !digits.is_empty() {
        let mut remainder: u64 = 0;
        let size = digits.len();
        for index in (0..size).rev() {
            digits[index] += remainder * CHUNK;
            remainder = digits[index] % 256;
            digits[index] /= 256;
            if digits[index] == 0 && index == digits.len() - 1 {
                digits.pop();
            }
        }
        bytes.push(remainder as u8);
    }
    bytes.reverse();
    from_magnitude(&bytes, negative)
}

/// `Bignum::BignumToVarchar`.
pub fn to_text(bignum: &[u8]) -> Result<String> {
    let mut out = Vec::new();
    rustdb_types::bignum::write_text(bignum, &mut out).map_err(Error::invalid_input)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `Bignum::BignumToDouble`.
pub fn to_f64(bignum: &[u8]) -> Result<f64> {
    if bignum.len() < 4 {
        return Err(Error::invalid_input("Invalid blob size."));
    }
    let negative = is_negative(bignum);
    let mut result = 0.0;
    for (position, &byte) in bignum[HEADER_SIZE..].iter().rev().enumerate() {
        let byte = if negative { !byte } else { byte };
        result += f64::from(byte) * 256f64.powf(position as f64);
    }
    if negative {
        result *= -1.0;
    }
    if !result.is_finite() {
        return Err(Error::conversion(format!(
            "Could not convert bignum '{}' to Double",
            to_text(bignum)?
        )));
    }
    Ok(result)
}

/// `Bignum::BignumToInt`: the sign and magnitude, checked against a target whose largest
/// value is `max` (negative values may reach `max + 1`). The magnitude accumulates in 128
/// bits like DuckDB's `uhugeint_t`.
pub fn to_integer(bignum: &[u8], max: u128) -> Result<(bool, u128)> {
    let negative = is_negative(bignum);
    let mut magnitude: u128 = 0;
    for &byte in bignum.get(HEADER_SIZE..).unwrap_or_default() {
        let byte = if negative { !byte } else { byte };
        magnitude = (magnitude << 8) | u128::from(byte);
    }
    if negative {
        // `max + 1` wraps to 0 for UHUGEINT, like DuckDB's `uhugeint_t`.
        if magnitude > max.wrapping_add(1) {
            return Err(Error::out_of_range("Negative bignum too small for type"));
        }
    } else if magnitude > max {
        return Err(Error::out_of_range("Positive bignum too large for type"));
    }
    Ok((negative, magnitude))
}

/// A bignum being computed on (DuckDB's `BignumIntermediate`): the sign and the stored
/// data bytes (bit-inverted when negative), possibly with leading padding bytes.
#[derive(Clone, Debug, Default)]
pub struct Intermediate {
    pub negative: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Greater,
    Smaller,
    Equal,
}

impl Intermediate {
    /// The intermediate form of a stored bignum.
    pub fn from_bignum(bignum: &[u8]) -> Self {
        Self {
            negative: is_negative(bignum),
            data: bignum.get(HEADER_SIZE..).unwrap_or_default().to_vec(),
        }
    }

    /// `BignumIntermediate::Initialize`: zero.
    pub fn zero() -> Self {
        Self {
            negative: false,
            data: vec![0],
        }
    }

    fn absolute_byte(&self, index: i64) -> u8 {
        match usize::try_from(index) {
            Ok(index) => {
                let byte = self.data.get(index).copied().unwrap_or(0);
                if self.negative { !byte } else { byte }
            }
            Err(_) => 0,
        }
    }

    /// `GetStartDataPos`: the number of leading padding bytes.
    fn start_data_pos(data: &[u8], negative: bool) -> usize {
        let pad = if negative { 0xFF } else { 0x00 };
        data.iter().take_while(|&&byte| byte == pad).count()
    }

    fn start(&self) -> usize {
        Self::start_data_pos(&self.data, self.negative)
    }

    fn is_msb_set(&self) -> bool {
        let first = self.data.first().copied().unwrap_or(0);
        if self.negative {
            first & 0x80 == 0
        } else {
            first & 0x80 != 0
        }
    }

    /// `IsAbsoluteBigger`.
    fn compare_absolute(&self, other: &Self) -> Comparison {
        let start = self.start();
        let other_start = other.start();
        let size = self.data.len() - start;
        let other_size = other.data.len() - other_start;
        if size != other_size {
            return if size > other_size {
                Comparison::Greater
            } else {
                Comparison::Smaller
            };
        }
        for offset in 0..size {
            let left = self.absolute_byte((start + offset) as i64);
            let right = other.absolute_byte((other_start + offset) as i64);
            if left != right {
                return if left > right {
                    Comparison::Greater
                } else {
                    Comparison::Smaller
                };
            }
        }
        Comparison::Equal
    }

    /// `OverOrUnderflow`: whether more than `MAX_DATA_SIZE` significant bytes are used.
    fn overflows(data: &[u8], negative: bool) -> bool {
        if data.len() <= MAX_DATA_SIZE {
            return false;
        }
        let pad = if negative { 0xFF } else { 0x00 };
        data[..data.len() - MAX_DATA_SIZE].iter().any(|&byte| byte != pad)
    }

    fn overflow_error(negative: bool, start: usize, size: usize) -> Error {
        Error::out_of_range(format!(
            "{} in Bignum Operation. A Bignum can hold max {MAX_DATA_SIZE} data bytes. Current bignum has {} bytes.",
            if negative { "Underflow" } else { "Overflow" },
            size - start
        ))
    }

    /// `Trim`: drops leading padding bytes, keeping at least one.
    fn trim(data: &mut Vec<u8>, negative: bool) {
        let start = Self::start_data_pos(data, negative);
        if start == 0 {
            return;
        }
        if start == data.len() {
            data.drain(..start - 1);
        } else {
            data.drain(..start);
        }
    }

    /// The stored bignum (`ToBignum`, which trims first).
    pub fn to_bignum(&mut self) -> Vec<u8> {
        Self::trim(&mut self.data, self.negative);
        let mut result = header(self.data.len(), self.negative).to_vec();
        result.extend_from_slice(&self.data);
        result
    }

    /// `Negate`: the stored negation.
    pub fn negate(&self) -> Vec<u8> {
        if !self.negative && self.data.len() == 1 && self.data[0] == 0 {
            let mut result = header(1, false).to_vec();
            result.push(0);
            return result;
        }
        let mut result = header(self.data.len(), !self.negative).to_vec();
        result.extend(self.data.iter().map(|&byte| !byte));
        result
    }

    /// `NegateInPlace`.
    pub fn negate_in_place(&mut self) {
        if !self.negative && self.data.len() == 1 && self.data[0] == 0 {
            return;
        }
        self.negative = !self.negative;
        for byte in &mut self.data {
            *byte = !*byte;
        }
    }

    /// `BignumAddition` into `result` (the result's stored data bytes).
    fn addition(result: &mut [u8], target_bigger: bool, lhs: &Self, rhs: &Self) {
        let result_negative = if target_bigger { lhs.negative } else { rhs.negative };
        let mut i_target = lhs.data.len() as i64 - 1;
        let mut i_source = rhs.data.len() as i64 - 1;
        let mut carry: u16 = 0;
        let mut borrow: u16 = 0;
        for slot in result.iter_mut().rev() {
            let target = u16::from(lhs.absolute_byte(i_target));
            let source = u16::from(rhs.absolute_byte(i_source));
            let sum = if lhs.negative == rhs.negative {
                let sum = target + source + carry;
                carry = (sum >> 8) & 0xFF;
                sum
            } else if target_bigger {
                let sum = target.wrapping_sub(source).wrapping_sub(borrow);
                borrow = u16::from(sum > target);
                sum
            } else {
                let sum = source.wrapping_sub(target).wrapping_sub(borrow);
                borrow = u16::from(sum > source);
                sum
            };
            let byte = (sum & 0xFF) as u8;
            *slot = if result_negative { !byte } else { byte };
            i_target -= 1;
            i_source -= 1;
        }
        if result_negative != lhs.negative {
            let extra = result.len() as i64 - rhs.data.len() as i64;
            for slot in result.iter_mut().take(usize::try_from(extra).unwrap_or(0)) {
                *slot = if result_negative { 0xFF } else { 0x00 };
            }
        }
    }

    /// `BignumIntermediate::Add`: the stored sum.
    pub fn add(lhs: &Self, rhs: &Self) -> Result<Vec<u8>> {
        let same_sign = lhs.negative == rhs.negative;
        let size = lhs.data.len() - lhs.start();
        let rhs_size = rhs.data.len() - rhs.start();
        let mut result_size = size;
        if size < rhs_size
            || (same_sign && (lhs.is_msb_set() || (rhs.is_msb_set() && lhs.data.len() == rhs.data.len())))
        {
            result_size = if size < rhs_size { rhs_size + 1 } else { size + 1 };
        }
        let mut target_bigger = true;
        result_size = result_size.max(1);
        if !same_sign {
            match lhs.compare_absolute(rhs) {
                Comparison::Equal => return Ok(zero()),
                Comparison::Smaller => target_bigger = false,
                Comparison::Greater => {}
            }
        }
        let mut data = vec![0u8; result_size];
        Self::addition(&mut data, target_bigger, lhs, rhs);
        let negative = if target_bigger { lhs.negative } else { rhs.negative };
        if Self::overflows(&data, negative) {
            let start = Self::start_data_pos(&data, negative);
            return Err(Self::overflow_error(negative, start, data.len()));
        }
        Self::trim(&mut data, negative);
        let mut result = header(data.len(), negative).to_vec();
        result.extend_from_slice(&data);
        Ok(result)
    }

    /// `Reallocate`: grows to more than `min_size` bytes, padding at the front.
    fn reallocate(&mut self, min_size: usize) {
        if min_size < self.data.len() {
            return;
        }
        let mut new_size = self.data.len().max(1);
        while new_size <= min_size {
            new_size *= 2;
        }
        let pad = if self.negative { 0xFF } else { 0x00 };
        let grow = new_size - self.data.len();
        self.data.splice(0..0, std::iter::repeat_n(pad, grow));
    }

    /// `AddInPlace` (the `sum(BIGNUM)` accumulation).
    pub fn add_in_place(&mut self, rhs: &Self) -> Result<()> {
        let same_sign = self.negative == rhs.negative;
        let size = self.data.len() - self.start();
        let rhs_size = rhs.data.len() - rhs.start();
        if size < rhs_size
            || (same_sign && (self.is_msb_set() || (rhs.is_msb_set() && self.data.len() == rhs.data.len())))
        {
            let min_size = if size < rhs_size {
                rhs_size + 1
            } else {
                self.data.len() + 1
            };
            self.reallocate(min_size);
        }
        let mut target_bigger = true;
        if !same_sign {
            match self.compare_absolute(rhs) {
                Comparison::Equal => {
                    *self = Self::zero();
                    return Ok(());
                }
                Comparison::Smaller => target_bigger = false,
                Comparison::Greater => {}
            }
        }
        let negative = if target_bigger {
            self.negative
        } else {
            rhs.negative
        };
        // DuckDB adds into the target's own bytes; each byte is read before it is written.
        let lhs = self.clone();
        Self::addition(&mut self.data, target_bigger, &lhs, rhs);
        self.negative = negative;
        if Self::overflows(&self.data, self.negative) {
            return Err(Self::overflow_error(negative, self.start(), self.data.len()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> String {
        to_text(&from_text(value.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn text_round_trips() {
        for value in [
            "0",
            "1",
            "-1",
            "255",
            "256",
            "-256",
            "123456789012345678901234567890",
            "-99999999999999999",
        ] {
            assert_eq!(text(value), value);
        }
        assert_eq!(text("+0012.75"), "12");
        assert_eq!(text("-0"), "0");
        assert!(from_text(b"1e5").is_none());
        assert!(from_text(b"-").is_none());
    }

    #[test]
    fn integer_casts_match_duckdb() {
        assert_eq!(to_text(&from_i64(-1)).unwrap(), "-1");
        assert_eq!(to_text(&from_i64(i64::MIN)).unwrap(), "-9223372036854775808");
        // DuckDB's floating-point byte count loses the top byte of 2^56.
        assert_eq!(to_text(&from_i64(1 << 56)).unwrap(), "0");
        assert_eq!(to_text(&from_i128(i128::MIN)).unwrap(), i128::MIN.to_string());
        assert_eq!(to_text(&from_u128(u128::MAX)).unwrap(), u128::MAX.to_string());
    }

    #[test]
    fn addition_matches_duckdb() {
        let add = |a: &str, b: &str| {
            let left = Intermediate::from_bignum(&from_text(a.as_bytes()).unwrap());
            let right = Intermediate::from_bignum(&from_text(b.as_bytes()).unwrap());
            to_text(&Intermediate::add(&left, &right).unwrap()).unwrap()
        };
        assert_eq!(add("255", "1"), "256");
        assert_eq!(add("-256", "1"), "-255");
        assert_eq!(add("5", "-5"), "0");
        assert_eq!(add("-1000000000000000000000", "-1"), "-1000000000000000000001");
    }
}
