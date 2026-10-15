//! UTF-8 helpers shared by the string functions (DuckDB's `Utf8Proc` wrappers).

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

/// The text of a VARCHAR value. VARCHARs are valid UTF-8; anything else is decoded
/// lossily instead of failing.
#[inline]
pub(crate) fn text(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Whether every byte is ASCII (DuckDB's `IsAscii`).
#[inline]
pub(crate) fn is_ascii(bytes: &[u8]) -> bool {
    bytes.is_ascii()
}

/// The number of code points (DuckDB's `Length`).
#[inline]
pub(crate) fn char_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| byte & 0xc0 != 0x80).count()
}

/// The number of grapheme clusters (DuckDB's `GraphemeCount`).
pub(crate) fn grapheme_count(bytes: &[u8]) -> usize {
    if is_ascii(bytes) {
        // DuckDB counts ASCII text by bytes (so "\r\n" counts twice).
        return bytes.len();
    }
    text(bytes).graphemes(true).count()
}

/// The byte ranges of the grapheme clusters (DuckDB's `Utf8Proc::GraphemeClusters`).
pub(crate) fn grapheme_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let text = text(bytes);
    text.grapheme_indices(true)
        .map(|(start, cluster)| (start, start + cluster.len()))
        .collect()
}

/// utf8proc's simple uppercase mapping (`utf8proc_toupper`): like Rust's full mapping,
/// except that characters whose full mapping has several characters map to their simple
/// mapping (or to themselves).
pub(crate) fn simple_upper(c: char) -> char {
    if c == '\u{DF}' {
        return '\u{1E9E}';
    }
    let code = u32::from(c);
    // Greek letters with ypogegrammeni map to their titlecase form.
    let titlecase = match code {
        0x1F80..=0x1F87 | 0x1F90..=0x1F97 | 0x1FA0..=0x1FA7 => Some(code + 8),
        0x1FB3 => Some(0x1FBC),
        0x1FC3 => Some(0x1FCC),
        0x1FF3 => Some(0x1FFC),
        _ => None,
    };
    if let Some(mapped) = titlecase.and_then(char::from_u32) {
        return mapped;
    }
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// utf8proc's simple lowercase mapping (`utf8proc_tolower`).
pub(crate) fn simple_lower(c: char) -> char {
    if c == '\u{130}' {
        return 'i';
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// Appends `c` encoded as UTF-8.
#[inline]
pub(crate) fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buffer = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
}

/// Decodes the code point starting at `bytes[0]` and its length (DuckDB's
/// `Utf8Proc::UTF8ToCodepoint`); `None` for invalid or empty input.
pub(crate) fn decode_first(bytes: &[u8]) -> Option<(char, usize)> {
    let length = match bytes.first()? {
        byte if byte & 0x80 == 0 => 1,
        byte if byte & 0xE0 == 0xC0 => 2,
        byte if byte & 0xF0 == 0xE0 => 3,
        byte if byte & 0xF8 == 0xF0 => 4,
        _ => return None,
    };
    let text = std::str::from_utf8(bytes.get(..length)?).ok()?;
    text.chars().next().map(|c| (c, length))
}
