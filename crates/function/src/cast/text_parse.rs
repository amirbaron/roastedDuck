//! Text parsing primitives for readers outside the cast system (DuckDB's
//! `TrySimpleIntegerCast`, `TryDoubleCast`, `TryDecimalStringCast` and `TryCastStringBool`
//! as the CSV reader calls them, with its decimal separator).

use super::parse::{ParseInteger, parse_bool as parse_bool_impl, parse_decimal as parse_decimal_impl};

/// Integer types the readers parse directly.
pub trait TextInteger: Sized {
    /// `TrySimpleIntegerCast` (non-strict).
    fn parse_integer(input: &[u8]) -> Option<Self> {
        Self::parse_integer_with(input, false)
    }

    /// `TrySimpleIntegerCast`; `strict` (the sniffer's) rejects leading zeros, `+`,
    /// underscores and decimals.
    fn parse_integer_with(input: &[u8], strict: bool) -> Option<Self>;
}

macro_rules! text_integer {
    ($($ty:ty),*) => {$(
        impl TextInteger for $ty {
            fn parse_integer_with(input: &[u8], strict: bool) -> Option<Self> {
                <$ty as ParseInteger>::parse_text(input, strict)
            }
        }
    )*};
}
text_integer!(i8, i16, i32, i64, u8, u16, u32, u64, i128, u128);

/// Float types the readers parse directly.
pub trait TextFloat: Sized {
    /// `TryDoubleCast` (non-strict) with `decimal_separator` as the decimal point.
    fn parse_float(input: &[u8], decimal_separator: u8) -> Option<Self> {
        Self::parse_float_with(input, decimal_separator, false)
    }

    /// `TryDoubleCast` with `decimal_separator` as the decimal point; `strict` as the
    /// sniffer parses.
    fn parse_float_with(input: &[u8], decimal_separator: u8, strict: bool) -> Option<Self>;
}

/// The input with `,` as the decimal point rewritten to `.`; `None` when the input uses
/// `.` (which is not a decimal point then).
fn with_decimal_point(input: &[u8], decimal_separator: u8) -> Option<std::borrow::Cow<'_, [u8]>> {
    if decimal_separator == b'.' {
        return Some(std::borrow::Cow::Borrowed(input));
    }
    if input.contains(&b'.') {
        return None;
    }
    Some(std::borrow::Cow::Owned(
        input
            .iter()
            .map(|&byte| if byte == decimal_separator { b'.' } else { byte })
            .collect(),
    ))
}

macro_rules! text_float {
    ($($ty:ty),*) => {$(
        impl TextFloat for $ty {
            fn parse_float_with(input: &[u8], decimal_separator: u8, strict: bool) -> Option<Self> {
                let input = with_decimal_point(input, decimal_separator)?;
                super::parse::parse_float::<$ty>(&input, strict)
            }
        }
    )*};
}
text_float!(f32, f64);

/// `TryDecimalStringCast` with `decimal_separator` as the decimal point: the unscaled
/// value of a DECIMAL(width, scale).
pub fn parse_decimal(input: &[u8], width: u8, scale: u8, decimal_separator: u8) -> Option<i128> {
    let input = with_decimal_point(input, decimal_separator)?;
    parse_decimal_impl(&input, width, scale)
}

/// `TryCastStringBool` (non-strict).
pub fn parse_bool(input: &[u8]) -> Option<bool> {
    parse_bool_impl(input, false)
}

/// `TryCastStringBool` as the sniffer calls it (strict: only `true`/`false`/`t`/`f`...).
pub fn parse_bool_strict(input: &[u8]) -> Option<bool> {
    parse_bool_impl(input, true)
}
