//! Regular expression support shared by the `regexp_*` functions (DuckDB's
//! `regexp_util.cpp`), emulating RE2 on top of the `regex` crate.
//!
//! RE2 and `regex` share their syntax and matching semantics (leftmost-first, linear
//! time, no backreferences or lookaround). The differences that matter are bridged here:
//! RE2's `\d`, `\w`, `\s` and `\b` are ASCII-only (they are rewritten to ASCII classes),
//! RE2 reads `[` inside a class and `&&`/`--`/`~~` literally, a `{` that does not start a
//! repetition is a literal, and `\Q...\E` quotes. Parse errors are reported with RE2's
//! messages for the common cases.

use std::borrow::Cow;
use std::fmt::Write as _;

use regex::bytes::{CaptureLocations, Regex, RegexBuilder};
use regex_syntax::ast::ErrorKind as AstErrorKind;
use roastedduck_common::{Error, Result};
use roastedduck_types::Value;

/// The RE2 options DuckDB exposes (`RE2::Options`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RegexOptions {
    pub(crate) case_sensitive: bool,
    pub(crate) dot_nl: bool,
    pub(crate) literal: bool,
}

impl Default for RegexOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            dot_nl: false,
            literal: false,
        }
    }
}

/// DuckDB's `ParseRegexOptions`; `global` receives the `g` flag where it is allowed.
pub(crate) fn parse_options(
    options: &[u8],
    target: &mut RegexOptions,
    mut global: Option<&mut bool>,
) -> Result<()> {
    for &option in options {
        match option {
            b'c' => target.case_sensitive = true,
            b'i' => target.case_sensitive = false,
            b'l' => target.literal = true,
            b'm' | b'n' | b'p' => target.dot_nl = false,
            b's' => target.dot_nl = true,
            b'g' => match global.as_deref_mut() {
                Some(global) => *global = true,
                None => {
                    return Err(Error::invalid_input(
                        "Option 'g' (global replace) is only valid for regexp_replace",
                    ));
                }
            },
            b' ' | b'\t' | b'\n' => {}
            other => {
                return Err(Error::invalid_input(format!(
                    "Unrecognized Regex option {}",
                    char::from(other)
                )));
            }
        }
    }
    Ok(())
}

/// The options argument of a call (DuckDB's `ParseRegexOptions(context, expr, ...)`).
pub(crate) fn parse_options_argument(
    argument_type: &roastedduck_types::LogicalType,
    constant: Option<&Option<Value>>,
    target: &mut RegexOptions,
    global: Option<&mut bool>,
) -> Result<()> {
    if argument_type.id() == roastedduck_types::LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(Some(value)) = constant else {
        return Err(Error::invalid_input("Regex options field must be a constant"));
    };
    if value.is_null() {
        return Err(Error::invalid_input("Regex options field must not be NULL"));
    }
    let Some(options) = value.as_str() else {
        return Err(Error::invalid_input("Regex options field must be a string"));
    };
    parse_options(options.as_bytes(), target, global)
}

/// A constant VARCHAR pattern argument (DuckDB's `TryParseConstantPattern`).
pub(crate) fn constant_pattern(constant: Option<&Option<Value>>) -> Option<String> {
    let Some(Some(value)) = constant else {
        return None;
    };
    if value.is_null() || value.logical_type().id() != roastedduck_types::LogicalTypeId::Varchar {
        return None;
    }
    value.as_str().map(str::to_owned)
}

/// The RE2 error message for a pattern `regex-syntax` rejects (best effort): `pattern`
/// is the user's pattern, `translated` its `regex` form that is parsed.
fn re2_error(pattern: &str, translated: &str) -> Option<String> {
    let Err(error) = regex_syntax::ast::parse::Parser::new().parse(translated) else {
        return None;
    };
    let span = error.span();
    let snippet = translated
        .get(span.start.offset..span.end.offset)
        .unwrap_or_default();
    let message = match error.kind() {
        AstErrorKind::GroupUnclosed => format!("missing ): {pattern}"),
        AstErrorKind::GroupUnopened => format!("unexpected ): {pattern}"),
        AstErrorKind::ClassUnclosed => {
            format!(
                "missing ]: {}",
                translated.get(span.start.offset..).unwrap_or_default()
            )
        }
        AstErrorKind::EscapeUnexpectedEof => "trailing \\".to_owned(),
        AstErrorKind::EscapeHexInvalid => {
            // RE2 stops at the digit that takes the code point past U+10FFFF.
            let mut code = 0u32;
            let mut digits = String::new();
            for c in snippet.chars() {
                digits.push(c);
                code = code
                    .saturating_mul(16)
                    .saturating_add(c.to_digit(16).unwrap_or_default());
                if code > 0x0010_FFFF {
                    break;
                }
            }
            format!("invalid escape sequence: \\x{{{digits}")
        }
        AstErrorKind::EscapeUnrecognized
        | AstErrorKind::UnsupportedBackreference
        | AstErrorKind::EscapeHexInvalidDigit
        | AstErrorKind::EscapeHexEmpty => {
            format!("invalid escape sequence: {snippet}")
        }
        AstErrorKind::RepetitionMissing => {
            // RE2 names the operator; the span can be empty, so take it from the text.
            let operator = if snippet.is_empty() {
                translated
                    .get(span.start.offset..)
                    .and_then(|rest| rest.chars().next())
                    .map(String::from)
                    .unwrap_or_default()
            } else {
                snippet.to_owned()
            };
            format!("no argument for repetition operator: {operator}")
        }
        AstErrorKind::RepetitionCountInvalid
        | AstErrorKind::RepetitionCountUnclosed
        | AstErrorKind::RepetitionCountDecimalEmpty => format!("invalid repetition size: {snippet}"),
        AstErrorKind::ClassRangeInvalid => format!("invalid character class range: {snippet}"),
        AstErrorKind::GroupNameInvalid
        | AstErrorKind::GroupNameEmpty
        | AstErrorKind::GroupNameUnexpectedEof
        | AstErrorKind::GroupNameDuplicate { .. } => format!("invalid named capture group: {snippet}"),
        AstErrorKind::UnsupportedLookAround
        | AstErrorKind::FlagUnrecognized
        | AstErrorKind::FlagUnexpectedEof => {
            format!("invalid perl operator: {snippet}")
        }
        _ => return Some(error.to_string()),
    };
    Some(message)
}

/// Rewrites RE2-only syntax into the `regex` crate's equivalent (see the module docs).
fn translate(pattern: &str) -> String {
    const DIGIT: &str = "0-9";
    const WORD: &str = "0-9A-Za-z_";
    const SPACE: &str = "\\t\\n\\x0C\\r ";
    let mut out = String::with_capacity(pattern.len() + 16);
    let chars: Vec<char> = pattern.chars().collect();
    let mut in_class = false;
    let mut class_start = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let Some(&next) = chars.get(i) else {
                    out.push('\\');
                    continue;
                };
                i += 1;
                let class = |set: &str, negated: bool| -> String {
                    match (in_class, negated) {
                        (true, false) => set.to_owned(),
                        (_, true) => format!("[^{set}]"),
                        (false, false) => format!("[{set}]"),
                    }
                };
                match next {
                    'd' => out.push_str(&class(DIGIT, false)),
                    'D' => out.push_str(&class(DIGIT, true)),
                    'w' => out.push_str(&class(WORD, false)),
                    'W' => out.push_str(&class(WORD, true)),
                    's' => out.push_str(&class(SPACE, false)),
                    'S' => out.push_str(&class(SPACE, true)),
                    'b' if !in_class => out.push_str("(?-u:\\b)"),
                    'B' if !in_class => out.push_str("(?-u:\\B)"),
                    'Q' => {
                        let mut literal = String::new();
                        while i < chars.len() {
                            if chars[i] == '\\' && chars.get(i + 1) == Some(&'E') {
                                i += 2;
                                break;
                            }
                            literal.push(chars[i]);
                            i += 1;
                        }
                        out.push_str(&regex::escape(&literal));
                    }
                    // `\x{...}` is one escape: its braces are not a repetition.
                    'x' if chars.get(i) == Some(&'{') => {
                        out.push_str("\\x");
                        while i < chars.len() {
                            out.push(chars[i]);
                            i += 1;
                            if chars[i - 1] == '}' {
                                break;
                            }
                        }
                    }
                    // RE2's octal escapes: `\0` plus up to two octal digits, or `\1`-`\7`
                    // followed by an octal digit (a lone `\1` is a backreference).
                    '0'..='7' if next == '0' || chars.get(i).is_some_and(|c| ('0'..='7').contains(c)) => {
                        let mut code = next.to_digit(8).unwrap_or_default();
                        for _ in 0..2 {
                            match chars.get(i).and_then(|c| c.to_digit(8)) {
                                Some(digit) => {
                                    code = code * 8 + digit;
                                    i += 1;
                                }
                                None => break,
                            }
                        }
                        let _ = write!(out, "\\x{{{code:X}}}");
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                // RE2 reads a `-` after a class escape (`[\w-.]`) as a literal, which
                // the expanded class would turn into a range.
                if in_class
                    && matches!(next, 'd' | 'D' | 'w' | 'W' | 's' | 'S')
                    && chars.get(i) == Some(&'-')
                    && chars.get(i + 1).is_some_and(|&c| c != ']')
                {
                    out.push_str("\\-");
                    i += 1;
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                class_start = i;
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            '[' if chars.get(i) == Some(&':') => {
                // A POSIX class such as `[:alpha:]`.
                out.push('[');
                while i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                    if chars[i - 1] == ']' {
                        break;
                    }
                }
            }
            '[' => out.push_str("\\["),
            ']' if in_class && i > class_start => {
                in_class = false;
                out.push(']');
            }
            '&' | '~' | '-' if in_class && chars.get(i) == Some(&c) => {
                out.push('\\');
                out.push(c);
            }
            '{' if !in_class && !starts_repetition(&chars[i..]) => out.push_str("\\{"),
            _ => out.push(c),
        }
    }
    out
}

/// RE2's error for stacked repetition operators (`a**`, `a++`, `a{2}*`): Perl syntax
/// does not allow them.
fn stacked_repetition(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut last_repeat: Option<usize> = None;
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut repeat = None;
        match c {
            '\\' => {
                if chars.get(i + 1) == Some(&'Q') {
                    i += 2;
                    while i < chars.len() && !(chars[i] == '\\' && chars.get(i + 1) == Some(&'E')) {
                        i += 1;
                    }
                }
                i += 2;
                last_repeat = None;
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                last_repeat = None;
                continue;
            }
            ']' if in_class => in_class = false,
            _ if in_class => {}
            '*' | '+' | '?' => repeat = Some(i + 1),
            '{' if starts_repetition(&chars[i + 1..]) => {
                let close = chars[i..]
                    .iter()
                    .position(|&c| c == '}')
                    .map_or(chars.len(), |p| i + p + 1);
                repeat = Some(close);
            }
            _ => {}
        }
        let Some(mut end) = repeat else {
            last_repeat = None;
            i += 1;
            continue;
        };
        if chars.get(end) == Some(&'?') {
            end += 1;
        }
        if let Some(start) = last_repeat {
            let text: String = chars[start..end].iter().collect();
            return Some(format!("bad repetition operator: {text}"));
        }
        last_repeat = Some(i);
        i = end;
    }
    None
}

/// Whether `rest` (after a `{`) is a counted repetition `n}`, `n,}` or `n,m}`.
fn starts_repetition(rest: &[char]) -> bool {
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    match rest.get(digits) {
        Some('}') => true,
        Some(',') => {
            let more = rest[digits + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            rest.get(digits + 1 + more) == Some(&'}')
        }
        _ => false,
    }
}

/// Compiles `pattern` like `RE2(pattern, options)`; `full` anchors it at both ends (for
/// `FullMatch`).
pub(crate) fn compile(pattern: &[u8], options: RegexOptions, full: bool) -> Result<Regex> {
    let text = String::from_utf8_lossy(pattern);
    let source: Cow<'_, str> = if options.literal {
        Cow::Owned(regex::escape(&text))
    } else {
        if let Some(message) = stacked_repetition(&text) {
            return Err(Error::invalid_input(message));
        }
        let translated = translate(&text);
        if let Some(message) = re2_error(&text, &translated) {
            return Err(Error::invalid_input(message));
        }
        Cow::Owned(translated)
    };
    let source = if full {
        format!("^(?:{source})$")
    } else {
        source.into_owned()
    };
    RegexBuilder::new(&source)
        .case_insensitive(!options.case_sensitive)
        .dot_matches_new_line(options.dot_nl)
        .build()
        .map_err(|error| Error::invalid_input(error.to_string()))
}

/// The number of capturing groups (`RE2::NumberOfCapturingGroups`).
pub(crate) fn group_count(regex: &Regex) -> usize {
    regex.captures_len() - 1
}

/// The highest `\N` in a rewrite string (`RE2::MaxSubmatch`).
pub(crate) fn max_submatch(rewrite: &[u8]) -> usize {
    let mut max = 0;
    let mut i = 0;
    while i < rewrite.len() {
        if rewrite[i] == b'\\' {
            i += 1;
            if let Some(digit) = rewrite.get(i).filter(|c| c.is_ascii_digit()) {
                max = max.max(usize::from(digit - b'0'));
            }
        }
        i += 1;
    }
    max
}

/// Appends `rewrite` with `\N` replaced by group N (`RE2::Rewrite`); `false` on an invalid
/// rewrite (whatever was appended stays).
pub(crate) fn rewrite(
    out: &mut Vec<u8>,
    rewrite: &[u8],
    input: &[u8],
    captures: &CaptureLocations,
    groups: usize,
) -> bool {
    let mut i = 0;
    while i < rewrite.len() {
        let c = rewrite[i];
        i += 1;
        if c != b'\\' {
            out.push(c);
            continue;
        }
        match rewrite.get(i) {
            Some(digit) if digit.is_ascii_digit() => {
                let n = usize::from(digit - b'0');
                if n >= groups {
                    return false;
                }
                if let Some((start, end)) = captures.get(n) {
                    out.extend_from_slice(&input[start..end]);
                }
            }
            Some(b'\\') => out.push(b'\\'),
            _ => return false,
        }
        i += 1;
    }
    true
}

/// The byte length of the UTF-8 character starting at `base` (DuckDB's
/// `AdvanceOneUTF8Basic`).
pub(crate) fn advance_one_utf8(input: &[u8], base: usize) -> Result<usize> {
    let Some(&first) = input.get(base) else {
        return Ok(1);
    };
    let length = if first & 0x80 == 0 {
        1
    } else if first & 0xE0 == 0xC0 {
        2
    } else if first & 0xF0 == 0xE0 {
        3
    } else if first & 0xF8 == 0xF0 {
        4
    } else {
        return Err(Error::internal("Invalid UTF-8 lead byte in regexp_extract_all"));
    };
    if base + length > input.len() {
        return Err(Error::internal("Invalid UTF-8 sequence in regexp_extract_all"));
    }
    Ok(length)
}

/// `RE2::QuoteMeta`: escapes everything but ASCII letters, digits, `_` and non-ASCII
/// bytes; NUL becomes `\x00`.
pub(crate) fn quote_meta(input: &[u8], out: &mut Vec<u8>) {
    for &byte in input {
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80 {
            out.push(byte);
        } else if byte == 0 {
            out.extend_from_slice(b"\\x00");
        } else {
            out.push(b'\\');
            out.push(byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_matches_re2_syntax() {
        assert_eq!(translate(r"\d+\b"), r"[0-9]+(?-u:\b)");
        assert_eq!(translate(r"[\d\s]"), "[0-9\\t\\n\\x0C\\r ]");
        assert_eq!(translate("a{,"), "a\\{,");
        assert_eq!(translate("a{2,3}"), "a{2,3}");
        assert_eq!(translate("[]a[]"), "[\\]a\\[]");
        assert_eq!(translate(r"\Qa.b\E"), r"a\.b");
        assert_eq!(re2_error("(abc", "(abc").as_deref(), Some("missing ): (abc"));
        assert_eq!(re2_error("[abc", "[abc").as_deref(), Some("missing ]: [abc"));
    }
}
