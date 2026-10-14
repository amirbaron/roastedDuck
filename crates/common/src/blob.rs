//! DuckDB's textual BLOB encoding (`Blob::ToString` / `Blob::ToBlob`).
//!
//! Printable ASCII is kept as is; every other byte (and `\`, `'`, `"`) becomes `\xHH`.

use crate::{Error, Result};

const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";

const fn is_regular_character(byte: u8) -> bool {
    byte >= 32 && byte <= 126 && byte != b'\\' && byte != b'\'' && byte != b'"'
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Renders blob bytes as text (`Blob::ToString`).
pub fn to_string(blob: &[u8]) -> String {
    let mut out = String::with_capacity(blob.len());
    for &byte in blob {
        if is_regular_character(byte) {
            out.push(char::from(byte));
        } else {
            out.push('\\');
            out.push('x');
            out.push(char::from(HEX_TABLE[usize::from(byte >> 4)]));
            out.push(char::from(HEX_TABLE[usize::from(byte & 0x0F)]));
        }
    }
    out
}

/// Parses the textual form back into bytes (`Blob::ToBlob` with its validation).
pub fn from_string(text: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let byte = text[i];
        if byte == b'\\' {
            let lossy = String::from_utf8_lossy(text);
            if i + 3 >= text.len() {
                return Err(Error::conversion(format!(
                    "Invalid hex escape code encountered in string -> blob conversion of string \"{lossy}\": \
                     unterminated escape code at end of blob"
                )));
            }
            match (text[i + 1], hex_value(text[i + 2]), hex_value(text[i + 3])) {
                (b'x', Some(high), Some(low)) => out.push((high << 4) | low),
                _ => {
                    return Err(Error::conversion(format!(
                        "Invalid hex escape code encountered in string -> blob conversion of string \"{lossy}\": {}",
                        String::from_utf8_lossy(&text[i..i + 4])
                    )));
                }
            }
            i += 4;
        } else if byte <= 127 {
            out.push(byte);
            i += 1;
        } else {
            return Err(Error::conversion(format!(
                "Invalid byte encountered in STRING -> BLOB conversion of string \"{}\". All non-ascii characters \
                 must be escaped with hex codes (e.g. \\xAA)",
                String::from_utf8_lossy(text)
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = [0u8, b'a', b'\\', 0xFF, b'"', b' '];
        let text = to_string(&bytes);
        assert_eq!(text, "\\x00a\\x5C\\xFF\\x22 ");
        assert_eq!(from_string(text.as_bytes()).unwrap(), bytes);
        assert!(from_string(b"\\x0").is_err());
        assert!(from_string("é".as_bytes()).is_err());
    }
}
