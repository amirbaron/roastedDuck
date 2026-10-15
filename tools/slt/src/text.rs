//! Byte-exact ports of the `duckdb::StringUtil` helpers the runner relies on.
//!
//! These differ from the obvious `std` equivalents in small ways (e.g. `split` drops empty
//! pieces, `trim` also strips trailing non-ASCII bytes) that are observable in test files.

/// `StringUtil::CharacterIsSpace`.
pub fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `StringUtil::Split(input, split)`: splits on every occurrence of `separator` and drops empty
/// pieces; if nothing remains the whole input is returned as the only element.
pub fn split(input: &str, separator: &str) -> Vec<String> {
    let mut pieces: Vec<String> = input
        .split(separator)
        .filter(|piece| !piece.is_empty())
        .map(str::to_owned)
        .collect();
    if pieces.is_empty() {
        pieces.push(input.to_owned());
    }
    pieces
}

/// `StringUtil::Trim`: strips leading whitespace, then everything after the last byte that is
/// ASCII and not whitespace (so trailing multi-byte UTF-8 characters are removed, like DuckDB).
pub fn trim(input: &str) -> String {
    let bytes = input.as_bytes();
    let start = bytes.iter().position(|&b| !is_space(b)).unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b.is_ascii() && b != 0 && !is_space(b))
        .map_or(0, |pos| pos + 1);
    if start >= end {
        return String::new();
    }
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

/// `std::stoi`/`std::stoll`-style parsing: optional leading whitespace and sign, then at least one
/// digit; trailing characters are ignored.
pub fn parse_leading_int(input: &str) -> Option<i64> {
    let trimmed = input.trim_start_matches(|c: char| c.is_ascii() && is_space(c as u8));
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let end = digits
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let magnitude: i64 = digits[..end].parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_drops_empty_pieces() {
        assert_eq!(split("a\t\tb", "\t"), ["a", "b"]);
        assert_eq!(split("\t", "\t"), ["\t"]);
        assert_eq!(split("", ","), [""]);
        assert_eq!(split("i=1&&j>2", "&&"), ["i=1", "j>2"]);
    }

    #[test]
    fn trim_matches_duckdb() {
        assert_eq!(trim("  HTTP Error \n"), "HTTP Error");
        assert_eq!(trim("abcé"), "abc");
        assert_eq!(trim("   "), "");
    }

    #[test]
    fn leading_int() {
        assert_eq!(parse_leading_int(" 42abc"), Some(42));
        assert_eq!(parse_leading_int("-7"), Some(-7));
        assert_eq!(parse_leading_int("x"), None);
    }
}
