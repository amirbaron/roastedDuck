//! The GLOB matching of the shell (DuckDB's `tools/shell/shell_helpers.cpp`, SQLite's
//! `patternCompare`), used to filter `.help`. (Its LIKE variant, `StringLike`, has no
//! caller in DuckDB's shell.)

/// How a pattern is matched (`compareInfo`).
struct CompareInfo {
    /// `*` or `%`.
    match_all: u32,
    /// `?` or `_`.
    match_one: u32,
    /// `[`, or 0 without sets.
    match_set: u32,
    /// Whether ASCII letters match regardless of case.
    no_case: bool,
}

/// `globInfo`.
const GLOB_INFO: CompareInfo = CompareInfo {
    match_all: b'*' as u32,
    match_one: b'?' as u32,
    match_set: b'[' as u32,
    no_case: false,
};

/// The outcome of [`pattern_compare`] (`SQLITE_MATCH`, `SQLITE_NOMATCH`,
/// `SQLITE_NOWILDCARDMATCH`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchResult {
    Match,
    NoMatch,
    NoWildcardMatch,
}

/// `sqlite3Utf8Trans1`: the value bits of the first byte of a multi-byte character.
const UTF8_TRANS1: [u8; 64] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x00, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x00, 0x01, 0x02,
    0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01, 0x02, 0x03, 0x00, 0x01, 0x00, 0x00,
];

/// The byte at `pos`, 0 past the end (the strings are NUL-terminated in DuckDB).
fn byte_at(text: &[u8], pos: usize) -> u8 {
    text.get(pos).copied().unwrap_or(0)
}

/// `Utf8Read` / `ReadCodepoint`: the code point at `*pos`, advancing past it; invalid
/// sequences read as U+FFFD.
fn utf8_read(text: &[u8], pos: &mut usize) -> u32 {
    let first = byte_at(text, *pos);
    if *pos < text.len() {
        *pos += 1;
    }
    let mut c = u32::from(first);
    if c >= 0xc0 {
        c = u32::from(UTF8_TRANS1[(c - 0xc0) as usize]);
        while byte_at(text, *pos) & 0xc0 == 0x80 {
            c = (c << 6) + (0x3f & u32::from(byte_at(text, *pos)));
            *pos += 1;
        }
        if c < 0x80 || (c & 0xFFFF_F800) == 0xD800 || (c & 0xFFFF_FFFE) == 0xFFFE {
            c = 0xFFFD;
        }
    }
    c
}

/// `SQLITE_SKIP_UTF8`.
fn skip_utf8(text: &[u8], pos: &mut usize) {
    let first = byte_at(text, *pos);
    *pos += 1;
    if first >= 0xc0 {
        while byte_at(text, *pos) & 0xc0 == 0x80 {
            *pos += 1;
        }
    }
}

fn to_lower(c: u32) -> u32 {
    u8::try_from(c).map_or(c, |c| u32::from(c.to_ascii_lowercase()))
}

fn to_upper(c: u32) -> u32 {
    u8::try_from(c).map_or(c, |c| u32::from(c.to_ascii_uppercase()))
}

/// `patternCompare`: whether `string` matches the GLOB or LIKE `pattern` (from
/// `pattern_pos` and `string_pos`); `match_other` is the escape character of LIKE or `[`
/// of GLOB.
fn pattern_compare(
    pattern: &[u8],
    mut pattern_pos: usize,
    string: &[u8],
    mut string_pos: usize,
    info: &CompareInfo,
    match_other: u32,
) -> MatchResult {
    // One past the last escaped input character.
    let mut escaped: Option<usize> = None;
    loop {
        let mut c = utf8_read(pattern, &mut pattern_pos);
        if c == 0 {
            break;
        }
        if c == info.match_all {
            // Skip further `*`s, and `?`s, each consuming a character of the input.
            loop {
                c = utf8_read(pattern, &mut pattern_pos);
                if c != info.match_all && c != info.match_one {
                    break;
                }
                if c == info.match_one && utf8_read(string, &mut string_pos) == 0 {
                    return MatchResult::NoWildcardMatch;
                }
            }
            if c == 0 {
                // A `*` at the end of the pattern matches.
                return MatchResult::Match;
            } else if c == match_other {
                if info.match_set == 0 {
                    c = utf8_read(pattern, &mut pattern_pos);
                    if c == 0 {
                        return MatchResult::NoWildcardMatch;
                    }
                } else {
                    // A set right after the `*`: a slow recursive search.
                    while byte_at(string, string_pos) != 0 {
                        let result =
                            pattern_compare(pattern, pattern_pos - 1, string, string_pos, info, match_other);
                        if result != MatchResult::NoMatch {
                            return result;
                        }
                        skip_utf8(string, &mut string_pos);
                    }
                    return MatchResult::NoWildcardMatch;
                }
            }
            // `c` is the first character after the `*`: find it in the input and
            // continue matching from there.
            if c <= 0x80 {
                let stops = if info.no_case {
                    [to_upper(c), to_lower(c)]
                } else {
                    [c, c]
                };
                loop {
                    while byte_at(string, string_pos) != 0
                        && !stops.contains(&u32::from(byte_at(string, string_pos)))
                    {
                        string_pos += 1;
                    }
                    if byte_at(string, string_pos) == 0 {
                        break;
                    }
                    string_pos += 1;
                    let result = pattern_compare(pattern, pattern_pos, string, string_pos, info, match_other);
                    if result != MatchResult::NoMatch {
                        return result;
                    }
                }
            } else {
                loop {
                    let c2 = utf8_read(string, &mut string_pos);
                    if c2 == 0 {
                        break;
                    }
                    if c2 != c {
                        continue;
                    }
                    let result = pattern_compare(pattern, pattern_pos, string, string_pos, info, match_other);
                    if result != MatchResult::NoMatch {
                        return result;
                    }
                }
            }
            return MatchResult::NoWildcardMatch;
        }
        if c == match_other {
            if info.match_set == 0 {
                c = utf8_read(pattern, &mut pattern_pos);
                if c == 0 {
                    return MatchResult::NoMatch;
                }
                escaped = Some(pattern_pos);
            } else {
                let mut prior_c = 0;
                let mut seen = false;
                let mut invert = false;
                c = utf8_read(string, &mut string_pos);
                if c == 0 {
                    return MatchResult::NoMatch;
                }
                let mut c2 = utf8_read(pattern, &mut pattern_pos);
                if c2 == u32::from(b'^') {
                    invert = true;
                    c2 = utf8_read(pattern, &mut pattern_pos);
                }
                if c2 == u32::from(b']') {
                    if c == u32::from(b']') {
                        seen = true;
                    }
                    c2 = utf8_read(pattern, &mut pattern_pos);
                }
                while c2 != 0 && c2 != u32::from(b']') {
                    let next = byte_at(pattern, pattern_pos);
                    if c2 == u32::from(b'-') && next != b']' && next != 0 && prior_c > 0 {
                        c2 = utf8_read(pattern, &mut pattern_pos);
                        if c >= prior_c && c <= c2 {
                            seen = true;
                        }
                        prior_c = 0;
                    } else {
                        if c == c2 {
                            seen = true;
                        }
                        prior_c = c2;
                    }
                    c2 = utf8_read(pattern, &mut pattern_pos);
                }
                if c2 == 0 || seen == invert {
                    return MatchResult::NoMatch;
                }
                continue;
            }
        }
        let c2 = utf8_read(string, &mut string_pos);
        if c == c2 {
            continue;
        }
        if info.no_case && to_lower(c) == to_lower(c2) && c < 0x80 && c2 < 0x80 {
            continue;
        }
        if c == info.match_one && escaped != Some(pattern_pos) && c2 != 0 {
            continue;
        }
        return MatchResult::NoMatch;
    }
    if byte_at(string, string_pos) == 0 {
        MatchResult::Match
    } else {
        MatchResult::NoMatch
    }
}

/// `ShellState::StringGlob`: whether `string` matches the GLOB `pattern`.
pub fn string_glob(pattern: &str, string: &str) -> bool {
    pattern_compare(
        pattern.as_bytes(),
        0,
        string.as_bytes(),
        0,
        &GLOB_INFO,
        u32::from(b'['),
    ) == MatchResult::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(string_glob("mo*", "mode"));
        assert!(!string_glob("mo*", "open"));
        assert!(string_glob("*", ""));
        assert!(string_glob("m?de", "mode"));
        assert!(string_glob("[a-m]ode", "mode"));
        assert!(!string_glob("[^a-m]ode", "mode"));
    }
}
