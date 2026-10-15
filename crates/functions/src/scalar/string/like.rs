//! `LIKE` (`~~`), `NOT LIKE` (`!~~`), `ILIKE` (`~~*`), `NOT ILIKE` (`!~~*`), `GLOB`
//! (`~~~`) and the `*_escape` variants (DuckDB's `like.cpp`).

use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::scalar::executor::{Str, binary, ternary, unary};
use crate::scalar::string::caseconvert::case_convert;
use crate::scalar::string::contains::find;

/// Whether `byte` starts a UTF-8 character.
#[inline]
fn is_character(byte: u8) -> bool {
    byte & 0xc0 != 0x80
}

/// DuckDB's `Glob`: `*`, `?`, `[...]` (with `!` negation and ranges) and `\` escapes.
pub(crate) fn glob(string: &[u8], pattern: &[u8]) -> bool {
    let (slen, plen) = (string.len(), pattern.len());
    let (mut sidx, mut pidx) = (0, 0);
    'main: loop {
        while sidx < slen && pidx < plen {
            let s = string[sidx];
            match pattern[pidx] {
                b'*' => {
                    pidx += 1;
                    while pidx < plen && pattern[pidx] == b'*' {
                        pidx += 1;
                    }
                    if pidx == plen {
                        return true;
                    }
                    return (sidx..slen).any(|start| glob(&string[start..], &pattern[pidx..]));
                }
                b'?' => {}
                b'[' => {
                    pidx += 1;
                    // Parse the bracket expression.
                    if pidx == plen {
                        return false;
                    }
                    let invert = pattern[pidx] == b'!';
                    if invert {
                        pidx += 1;
                    }
                    let mut found_match = invert;
                    let start_pos = pidx;
                    let mut found_closing_bracket = false;
                    while pidx < plen {
                        let p = pattern[pidx];
                        if p == b']' && pidx > start_pos {
                            found_closing_bracket = true;
                            pidx += 1;
                            break;
                        }
                        if pidx + 1 == plen {
                            break;
                        }
                        let matches = if pattern[pidx + 1] == b'-' {
                            if pidx + 2 == plen {
                                break;
                            }
                            let next = pattern[pidx + 2];
                            pidx += 3;
                            // DuckDB compares `char`s, which are signed.
                            (s as i8) >= (p as i8) && (s as i8) <= (next as i8)
                        } else {
                            pidx += 1;
                            p == s
                        };
                        if found_match == invert && matches {
                            found_match = !invert;
                        }
                    }
                    if !found_closing_bracket || !found_match {
                        return false;
                    }
                    sidx += 1;
                    continue 'main;
                }
                b'\\' => {
                    pidx += 1;
                    if pidx == plen || s != pattern[pidx] {
                        return false;
                    }
                }
                p => {
                    if s != p {
                        return false;
                    }
                }
            }
            sidx += 1;
            pidx += 1;
        }
        while pidx < plen && pattern[pidx] == b'*' {
            pidx += 1;
        }
        return pidx == plen && sidx == slen;
    }
}

fn syntax_error(message: &str) -> Error {
    Error::new(ErrorKind::Syntax, message.to_owned())
}

/// DuckDB's `TemplatedLikeOperator` with the standard character reader: `%` matches any
/// sequence, `_` one character, and `escape` (if any) quotes the next pattern byte.
pub(crate) fn like(string: &[u8], pattern: &[u8], escape: Option<u8>) -> Result<bool> {
    let (slen, plen) = (string.len(), pattern.len());
    let (mut sidx, mut pidx) = (0, 0);
    while pidx < plen && sidx < slen {
        let p = pattern[pidx];
        let s = string[sidx];
        if Some(p) == escape {
            pidx += 1;
            if pidx == plen {
                return Err(syntax_error("Like pattern must not end with escape character!"));
            }
            if pattern[pidx] != s {
                return Ok(false);
            }
            sidx += 1;
        } else if p == b'_' {
            sidx += 1;
            while sidx < slen && !is_character(string[sidx]) {
                sidx += 1;
            }
        } else if p == b'%' {
            pidx += 1;
            while pidx < plen && pattern[pidx] == b'%' {
                pidx += 1;
            }
            if pidx == plen {
                return Ok(true);
            }
            for start in sidx..slen {
                if like(&string[start..], &pattern[pidx..], escape)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        } else if p == s {
            sidx += 1;
        } else {
            return Ok(false);
        }
        pidx += 1;
    }
    while pidx < plen && pattern[pidx] == b'%' {
        pidx += 1;
    }
    Ok(pidx == plen && sidx == slen)
}

/// `ILIKE`: `LIKE` on the lowercased string and pattern.
fn ilike(string: &[u8], pattern: &[u8], escape: Option<u8>) -> Result<bool> {
    let mut lower_string = Vec::with_capacity(string.len());
    case_convert(string, false, &mut lower_string);
    let mut lower_pattern = Vec::with_capacity(pattern.len());
    case_convert(pattern, false, &mut lower_pattern);
    like(&lower_string, &lower_pattern, escape)
}

/// A constant pattern made only of literal segments and `%` (DuckDB's `LikeMatcher`).
#[derive(Debug)]
struct LikeMatcher {
    segments: Vec<Vec<u8>>,
    has_start_percentage: bool,
    has_end_percentage: bool,
}

impl FunctionData for LikeMatcher {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl LikeMatcher {
    /// DuckDB's `CreateLikeMatcher`: `None` if the pattern has `_` or NUL (the default
    /// escape) or no literal segment.
    fn new(pattern: &[u8]) -> Option<Self> {
        let mut segments = Vec::new();
        let mut last_non_pattern = 0;
        let mut has_start_percentage = false;
        let mut has_end_percentage = false;
        for (i, &c) in pattern.iter().enumerate() {
            if c == 0 || c == b'%' || c == b'_' {
                if i > last_non_pattern {
                    segments.push(pattern[last_non_pattern..i].to_vec());
                }
                last_non_pattern = i + 1;
                if c != b'%' {
                    return None;
                }
                if i == 0 {
                    has_start_percentage = true;
                }
                if i + 1 == pattern.len() {
                    has_end_percentage = true;
                }
            }
        }
        if last_non_pattern < pattern.len() {
            segments.push(pattern[last_non_pattern..].to_vec());
        }
        (!segments.is_empty()).then_some(Self {
            segments,
            has_start_percentage,
            has_end_percentage,
        })
    }

    fn matches(&self, string: &[u8]) -> bool {
        let mut rest = string;
        let mut index = 0;
        let last = self.segments.len() - 1;
        if !self.has_start_percentage {
            let first = &self.segments[0];
            if !rest.starts_with(first) {
                return false;
            }
            rest = &rest[first.len()..];
            index = 1;
            if self.segments.len() == 1 {
                return self.has_end_percentage || rest.is_empty();
            }
        }
        while index < last {
            let segment = &self.segments[index];
            let Some(offset) = find(rest, segment) else {
                return false;
            };
            rest = &rest[offset + segment.len()..];
            index += 1;
        }
        let segment = &self.segments[last];
        if self.has_end_percentage {
            find(rest, segment).is_some()
        } else {
            rest.ends_with(segment)
        }
    }
}

/// DuckDB's `LikeBindFunction`: a constant pattern becomes a matcher.
fn bind_like(
    invert: bool,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    move |function, input| {
        let Some(Some(pattern)) = input.constants.get(1) else {
            return Ok(None);
        };
        if pattern.is_null() {
            return Ok(None);
        }
        let Some(matcher) = LikeMatcher::new(crate::values::as_text(pattern)?.as_bytes()) else {
            return Ok(None);
        };
        let matcher = Arc::new(matcher);
        let data: Arc<dyn FunctionData> = Arc::clone(&matcher) as Arc<dyn FunctionData>;
        function.kernel = Arc::new(move |args, _, result| {
            let strings = rustdb_types::DataChunk::new(vec![args.column(0).clone()], args.len());
            unary::<Str, bool>(&strings, result, |string, out| {
                *out = matcher.matches(string) != invert;
                Ok(true)
            })
        });
        Ok(Some(data))
    }
}

type Matcher = fn(&[u8], &[u8], Option<u8>) -> Result<bool>;

fn binary_like(name: &str, op: Matcher, invert: bool) -> ScalarFunction {
    ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        move |args, _, result| {
            binary::<Str, Str, bool>(args, result, |string, pattern, out| {
                *out = op(string, pattern, None)? != invert;
                Ok(true)
            })
        },
    )
}

/// DuckDB's `GetEscapeChar`.
fn escape_char(escape: &[u8]) -> Result<u8> {
    match escape {
        [] => Ok(0),
        [c] => Ok(*c),
        _ => Err(syntax_error(
            "Invalid escape string. Escape string must be empty or one character.",
        )),
    }
}

fn escape_like(name: &str, op: Matcher, invert: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        move |args, _, result| {
            ternary::<Str, Str, Str, bool>(args, result, |string, pattern, escape, out| {
                *out = op(string, pattern, Some(escape_char(escape)?))? != invert;
                Ok(true)
            })
        },
    ));
    set
}

fn single(function: ScalarFunction) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(Arc::clone(&function.name));
    set.add(function);
    set
}

/// Every LIKE/GLOB function set.
pub fn like_functions() -> Vec<ScalarFunctionSet> {
    vec![
        single(binary_like("~~", like, false).with_bind(bind_like(false))),
        single(binary_like("!~~", like, true).with_bind(bind_like(true))),
        single(binary_like(
            "~~~",
            |string, pattern, _| Ok(glob(string, pattern)),
            false,
        )),
        single(binary_like("~~*", ilike, false)),
        single(binary_like("!~~*", ilike, true)),
        escape_like("like_escape", like, false),
        escape_like("not_like_escape", like, true),
        escape_like("ilike_escape", ilike, false),
        escape_like("not_ilike_escape", ilike, true),
    ]
}
