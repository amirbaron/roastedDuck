//! `Parser::StripUnicodeSpaces`: DuckDB replaces Unicode space characters outside
//! quotes and comments with ASCII spaces before parsing.

/// Unicode spaces DuckDB replaces (see <https://jkorpela.fi/chars/spaces.html>):
/// returns the byte length of the space starting at `bytes[0]`, if any.
fn unicode_space_len(bytes: &[u8]) -> Option<usize> {
    match *bytes {
        // U+00A0
        [0xC2, 0xA0, ..] => Some(2),
        // U+2000..U+200B, U+202F, U+205F, U+2060, U+3000, U+FEFF
        [0xE2, 0x80, 0x80..=0x8B | 0xAF, ..]
        | [0xE2, 0x81, 0x9F | 0xA0, ..]
        | [0xE3, 0x80, 0x80, ..]
        | [0xEF, 0xBB, 0xBF, ..] => Some(3),
        _ => None,
    }
}

const fn is_dollar_tag_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

const fn is_dollar_tag_char(c: u8) -> bool {
    is_dollar_tag_start(c) || c.is_ascii_digit()
}

/// Returns `query` with Unicode spaces replaced by `' '`, or `None` if it has none.
///
/// Quoted strings and identifiers, dollar-quoted strings and `--` comments are left
/// alone. This reproduces DuckDB's scan exactly, including its bounds (it stops
/// looking two bytes before the end of the input).
pub fn strip_unicode_spaces(query: &str) -> Option<String> {
    if query.is_ascii() {
        // All the spaces in question are multi-byte UTF-8 sequences.
        return None;
    }
    let q = query.as_bytes();
    let n = q.len();
    let mut spaces: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0;
    // The C++ implementation is a goto state machine; each state is a loop here.
    'regular: loop {
        while pos + 2 < n {
            let c = q[pos];
            if let Some(len) = unicode_space_len(&q[pos..]) {
                spaces.push((pos, len));
            } else if c == b'"' || c == b'\'' {
                // in_quotes
                let quote = c;
                pos += 1;
                while pos + 1 < n {
                    if q[pos] == quote {
                        if q[pos + 1] == quote {
                            pos += 2;
                            continue;
                        }
                        pos += 1;
                        continue 'regular;
                    }
                    pos += 1;
                }
                break 'regular;
            } else if c == b'$' && (q[pos + 1] == b'$' || is_dollar_tag_start(q[pos + 1])) {
                // a (possibly tagged) dollar-quoted string
                pos += 1;
                let tag_start = pos;
                while pos + 2 < n {
                    if q[pos] == b'$' {
                        let tag = &q[tag_start..pos];
                        // in_dollar_quotes
                        while pos + 2 < n {
                            if q[pos] == b'$'
                                && n - (pos + 1) > tag.len()
                                && q[pos + tag.len() + 1] == b'$'
                                && &q[pos + 1..pos + 1 + tag.len()] == tag
                            {
                                pos += tag.len() + 1;
                                continue 'regular;
                            }
                            pos += 1;
                        }
                        break 'regular;
                    }
                    if !is_dollar_tag_char(q[pos]) {
                        continue 'regular;
                    }
                    pos += 1;
                }
                break 'regular;
            } else if c == b'-' && q[pos + 1] == b'-' {
                // in_comment
                while pos < n {
                    if q[pos] == b'\n' || q[pos] == b'\r' {
                        continue 'regular;
                    }
                    pos += 1;
                }
                break 'regular;
            }
            pos += 1;
        }
        break;
    }
    if spaces.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(n);
    let mut prev = 0;
    for (start, len) in spaces {
        out.push_str(&query[prev..start]);
        out.push(' ');
        prev = start + len;
    }
    out.push_str(&query[prev..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_spaces_outside_quotes() {
        assert_eq!(strip_unicode_spaces("SELECT\u{a0}1"), Some("SELECT 1".to_owned()));
        assert_eq!(
            strip_unicode_spaces("SELECT\u{3000}1,\u{feff}2 "),
            Some("SELECT 1, 2 ".to_owned())
        );
        assert_eq!(strip_unicode_spaces("SELECT 1"), None);
        assert_eq!(strip_unicode_spaces("SELECT '\u{a0}' "), None);
        assert_eq!(strip_unicode_spaces("SELECT $$\u{a0}$$ "), None);
        assert_eq!(strip_unicode_spaces("SELECT 1 -- \u{a0}\n"), None);
        assert_eq!(
            strip_unicode_spaces("SELECT 1 -- x\n\u{a0}2"),
            Some("SELECT 1 -- x\n 2".to_owned())
        );
    }
}
