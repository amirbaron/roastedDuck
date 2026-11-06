//! UTF-8 validation with DuckDB's classification of invalid input (`Utf8Proc::Analyze` in
//! `third_party/utf8proc/utf8proc_wrapper.cpp`) and the error it reports
//! (`ErrorManager::InvalidUnicodeError`).

use crate::Error;

/// What a byte string holds (DuckDB's `UnicodeType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnicodeType {
    Ascii,
    Utf8,
    Invalid(UnicodeInvalidReason),
}

/// Why a byte string is not UTF-8 (DuckDB's `UnicodeInvalidReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnicodeInvalidReason {
    /// A bad start byte, a truncated sequence or a continuation byte not of the form
    /// `10xxxxxx`.
    ByteMismatch,
    /// A well-formed sequence encoding an overlong form, a surrogate or a code point above
    /// U+10FFFF.
    InvalidUnicode,
}

/// Classifies `bytes` (`Utf8Proc::Analyze`).
pub fn analyze(bytes: &[u8]) -> UnicodeType {
    let mut kind = UnicodeType::Ascii;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c & 0x80 == 0 {
            i += 1;
            continue;
        }
        let (extra_bytes, mask, first) = if c & 0xE0 == 0xC0 {
            (1, 0x00_0780, c & 0x1F)
        } else if c & 0xF0 == 0xE0 {
            (2, 0x00_F800, c & 0x0F)
        } else if c & 0xF8 == 0xF0 {
            (3, 0x1F_0000, c & 0x07)
        } else {
            return UnicodeType::Invalid(UnicodeInvalidReason::ByteMismatch);
        };
        match extra_byte_loop(&bytes[i..], extra_bytes, mask, u32::from(first)) {
            Ok(()) => kind = UnicodeType::Utf8,
            Err(reason) => return UnicodeType::Invalid(reason),
        }
        i += extra_bytes + 1;
    }
    kind
}

/// Validates the sequence starting at `sequence[0]` with `extra_bytes` continuation bytes
/// (`UTF8ExtraByteLoop`); `mask` selects the bits an overlong encoding leaves zero.
fn extra_byte_loop(
    sequence: &[u8],
    extra_bytes: usize,
    mask: u32,
    mut code_point: u32,
) -> Result<(), UnicodeInvalidReason> {
    let Some(continuation) = sequence.get(1..=extra_bytes) else {
        return Err(UnicodeInvalidReason::ByteMismatch);
    };
    for &c in continuation {
        if c & 0xC0 != 0x80 {
            return Err(UnicodeInvalidReason::ByteMismatch);
        }
        code_point = (code_point << 6) | u32::from(c & 0x3F);
    }
    if code_point & mask == 0 || code_point > 0x10_FFFF || code_point & 0x1FF_F800 == 0xD800 {
        return Err(UnicodeInvalidReason::InvalidUnicode);
    }
    Ok(())
}

/// The error for invalid UTF-8 `bytes` met in `context`
/// (`ErrorManager::InvalidUnicodeError`).
pub fn invalid_unicode_error(bytes: &[u8], context: &str) -> Error {
    let base_message = match analyze(bytes) {
        UnicodeType::Invalid(UnicodeInvalidReason::ByteMismatch) => {
            "Invalid unicode (byte sequence mismatch)"
        }
        UnicodeType::Invalid(UnicodeInvalidReason::InvalidUnicode) => "Invalid unicode",
        UnicodeType::Ascii | UnicodeType::Utf8 => {
            return Error::invalid_input(format!(
                "Invalid unicode error thrown but no invalid unicode detected in {context}"
            ));
        }
    };
    Error::invalid_input(format!("{base_message} detected in {context}"))
}

/// Native utf8proc traversal, including its malformed-sequence behavior and errors.
pub fn native_grapheme_cluster(bytes: &[u8], start: usize) -> crate::Result<(usize, usize)> {
    crate::tree_renderer::unicode::cluster(bytes, start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_utf8proc() {
        assert_eq!(analyze(b"plain"), UnicodeType::Ascii);
        assert_eq!(analyze("caf\u{e9} \u{1F600}".as_bytes()), UnicodeType::Utf8);
        let mismatch = UnicodeType::Invalid(UnicodeInvalidReason::ByteMismatch);
        let invalid = UnicodeType::Invalid(UnicodeInvalidReason::InvalidUnicode);
        assert_eq!(analyze(b"a\xff\xfeb"), mismatch);
        assert_eq!(analyze(b"\xc3\x28"), mismatch);
        assert_eq!(analyze(b"abc\xe2\x82"), mismatch);
        assert_eq!(analyze(b"\xc0\x80"), invalid);
        assert_eq!(analyze(b"\xed\xa0\x80"), invalid);
        assert_eq!(analyze(b"\xf4\x90\x80\x80"), invalid);
    }
}
