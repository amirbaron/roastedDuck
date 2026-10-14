//! Error construction matching `libpg_query`'s `ereport` plumbing.
//!
//! `libpg_query` formats messages with `vsnprintf` into a `BUFSIZ` buffer and reports
//! positions as 1-based character offsets, which DuckDB turns into a 0-based
//! `position`. Messages are therefore truncated to `BUFSIZ - 1` bytes, and the
//! position is the number of characters before the offending byte offset.

use rustdb_common::Error;

/// `BUFSIZ` of the reference build (macOS libc), which bounds `errmsg` output.
const BUFSIZ: usize = 1024;

/// A parser error with an optional byte offset into the scanned text.
pub(crate) fn parser_error(message: String, text: &[u8], location: Option<usize>) -> Error {
    let error = Error::parser(truncate_message(message));
    match location {
        Some(loc) => error.with_query_location(char_position(text, loc)),
        None => error,
    }
}

/// The error thrown by `libpg_query`'s stubbed C helpers (`errhint`, `exprLocation`,
/// `pg_verifymbstr`, ...): `"<name> NOT IMPLEMENTED"`, without a position.
pub(crate) fn unimplemented(function: &str) -> Error {
    Error::parser(format!("{function} NOT IMPLEMENTED"))
}

/// An inconsistency in the parser itself (never expected for any input).
pub(crate) fn internal(message: impl Into<String>) -> Error {
    Error::internal(message.into())
}

/// `vsnprintf(buf, BUFSIZ, ...)`: keeps at most `BUFSIZ - 1` bytes (on a character
/// boundary, since Rust strings must stay valid UTF-8).
fn truncate_message(mut message: String) -> String {
    if message.len() >= BUFSIZ {
        let mut end = BUFSIZ - 1;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

/// `pg_mbstrlen_with_len(scanbuf, location)`: the number of UTF-8 characters that
/// start before byte offset `location`, counted the way `libpg_query` does (by lead
/// byte, stopping at a NUL).
fn char_position(text: &[u8], location: usize) -> usize {
    let mut remaining = location as isize;
    let mut pos = 0;
    let mut chars = 0;
    while remaining > 0 && pos < text.len() && text[pos] != 0 {
        let len = utf8_lead_len(text[pos]);
        remaining -= len as isize;
        pos += len;
        chars += 1;
    }
    chars
}

/// `pg_utf_mblen`: the sequence length announced by a UTF-8 lead byte.
const fn utf8_lead_len(byte: u8) -> usize {
    if byte & 0x80 == 0 {
        1
    } else if byte & 0xe0 == 0xc0 {
        2
    } else if byte & 0xf0 == 0xe0 {
        3
    } else if byte & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_characters() {
        let text = "SELECT 'é' FRM t".as_bytes();
        assert_eq!(char_position(text, 16), 15);
        assert_eq!(char_position(b"abc", 0), 0);
    }

    #[test]
    fn messages_are_truncated_like_vsnprintf() {
        let long = "x".repeat(2000);
        assert_eq!(truncate_message(long).len(), BUFSIZ - 1);
        let accents = "é".repeat(600);
        let cut = truncate_message(accents);
        assert!(cut.len() < BUFSIZ && cut.is_char_boundary(cut.len()));
    }
}
