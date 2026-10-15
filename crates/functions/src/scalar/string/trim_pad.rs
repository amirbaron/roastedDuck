//! `left`, `right`, `left_grapheme`, `right_grapheme` (DuckDB's `left_right.cpp`),
//! `lpad`, `rpad` (`pad.cpp`), `trim`, `ltrim`, `rtrim` (`trim.cpp`), `instr`
//! (`instr.cpp`) and `translate` (`translate.cpp`).

use std::collections::{HashMap, HashSet};

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, binary, ternary, unary};
use crate::scalar::string::contains::find;
use crate::scalar::string::substring::{substring_grapheme, substring_unicode};
use crate::scalar::string::utf8::{char_count, decode_first, grapheme_count, push_char};

type Substring = for<'a> fn(&'a [u8], i64, i64) -> Result<&'a [u8]>;

/// How `left`/`right` count characters: code points or grapheme clusters.
#[derive(Clone, Copy)]
struct Counting {
    length: fn(&[u8]) -> usize,
    substring: Substring,
}

const UNICODE: Counting = Counting {
    length: char_count,
    substring: substring_unicode,
};

const GRAPHEME: Counting = Counting {
    length: grapheme_count,
    substring: substring_grapheme,
};

/// DuckDB's `LeftScalarFunction`.
fn left(input: &[u8], position: i64, counting: Counting) -> Result<&[u8]> {
    if position >= 0 {
        return (counting.substring)(input, 1, position);
    }
    let characters = (counting.length)(input) as i64;
    (counting.substring)(input, 1, (characters + position).max(0))
}

/// DuckDB's `RightScalarFunction`.
fn right(input: &[u8], position: i64, counting: Counting) -> Result<&[u8]> {
    let characters = (counting.length)(input) as i64;
    let length = if position >= 0 {
        characters.min(position)
    } else if position == i64::MIN {
        0
    } else {
        characters - characters.min(-position)
    };
    (counting.substring)(input, characters - length + 1, length)
}

fn left_right_set(
    name: &str,
    op: fn(&[u8], i64, Counting) -> Result<&[u8]>,
    counting: Counting,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::BIGINT],
        LogicalType::VARCHAR,
        move |args, _, result| {
            binary::<Str, i64, StrOut>(args, result, |input, position, out| {
                out.extend_from_slice(op(input, position, counting)?);
                Ok(true)
            })
        },
    ));
    set
}

/// `left`, `right`, `left_grapheme` and `right_grapheme`.
pub fn left_right_functions() -> Vec<ScalarFunctionSet> {
    vec![
        left_right_set("left", left, UNICODE),
        left_right_set("left_grapheme", left, GRAPHEME),
        left_right_set("right", right, UNICODE),
        left_right_set("right_grapheme", right, GRAPHEME),
    ]
}

/// The code points of `input` (invalid bytes are taken one at a time).
fn code_points(input: &[u8]) -> impl Iterator<Item = (i64, &[u8])> {
    let mut rest = input;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (code, length) =
            decode_first(rest).map_or((-1, 1), |(c, length)| (i64::from(u32::from(c)), length));
        let (bytes, tail) = rest.split_at(length);
        rest = tail;
        Some((code, bytes))
    })
}

/// DuckDB's `PadCountChars`: the bytes and characters of the first `len` characters.
fn pad_count_chars(len: usize, input: &[u8]) -> (usize, usize) {
    let mut bytes = 0;
    let mut chars = 0;
    for (_, character) in code_points(input).take(len) {
        bytes += character.len();
        chars += 1;
    }
    (bytes, chars)
}

/// DuckDB's `InsertPadding`: `len` characters of `pad`, repeated.
fn insert_padding(len: usize, pad: &[u8], out: &mut Vec<u8>) -> bool {
    if len > 0 && pad.is_empty() {
        return false;
    }
    let characters: Vec<&[u8]> = code_points(pad).map(|(_, bytes)| bytes).collect();
    for index in 0..len {
        out.extend_from_slice(characters[index % characters.len()]);
    }
    true
}

fn pad(input: &[u8], len: i32, padding: &[u8], left: bool, out: &mut Vec<u8>) -> Result<()> {
    let len = usize::try_from(len.max(0)).unwrap_or(0);
    let (bytes, chars) = pad_count_chars(len, input);
    if !left {
        out.extend_from_slice(&input[..bytes]);
    }
    if !insert_padding(len - chars, padding, out) {
        return Err(Error::invalid_input(if left {
            "Insufficient padding in LPAD."
        } else {
            "Insufficient padding in RPAD."
        }));
    }
    if left {
        out.extend_from_slice(&input[..bytes]);
    }
    Ok(())
}

fn pad_set(name: &str, left: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::INTEGER, LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| {
            ternary::<Str, i32, Str, StrOut>(args, result, |input, len, padding, out| {
                pad(input, len, padding, left, out)?;
                Ok(true)
            })
        },
    ));
    set
}

/// `lpad` and `rpad`.
pub fn pad_functions() -> Vec<ScalarFunctionSet> {
    vec![pad_set("lpad", true), pad_set("rpad", false)]
}

/// Whether a code point is in Unicode category Zs (space separator).
fn is_space_separator(code: i64) -> bool {
    matches!(
        code,
        0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000
    )
}

/// Trims leading and/or trailing code points for which `trimmed` holds.
fn trim<'a>(input: &'a [u8], left: bool, right: bool, trimmed: impl Fn(i64) -> bool) -> &'a [u8] {
    let mut begin = 0;
    if left {
        for (code, bytes) in code_points(input) {
            if !trimmed(code) {
                break;
            }
            begin += bytes.len();
        }
    }
    let end = if right {
        let mut end = begin;
        let mut next = begin;
        for (code, bytes) in code_points(&input[begin..]) {
            next += bytes.len();
            if !trimmed(code) {
                end = next;
            }
        }
        end
    } else {
        input.len()
    };
    &input[begin..end]
}

fn trim_set(name: &str, left: bool, right: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                out.extend_from_slice(trim(input, left, right, is_space_separator));
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| {
            binary::<Str, Str, StrOut>(args, result, |input, characters, out| {
                let ignored: HashSet<i64> = code_points(characters).map(|(code, _)| code).collect();
                out.extend_from_slice(trim(input, left, right, |code| ignored.contains(&code)));
                Ok(true)
            })
        },
    ));
    set
}

/// `trim`, `ltrim` and `rtrim`.
pub fn trim_functions() -> Vec<ScalarFunctionSet> {
    vec![
        trim_set("trim", true, true),
        trim_set("ltrim", true, false),
        trim_set("rtrim", false, true),
    ]
}

/// `instr(haystack, needle)`: the 1-based character position, 0 if absent.
pub fn instr_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("instr");
    set.add(ScalarFunction::new(
        "instr",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BIGINT,
        |args, _, result| {
            binary::<Str, Str, i64>(args, result, |haystack, needle, out| {
                *out =
                    find(haystack, needle).map_or(0, |location| char_count(&haystack[..location]) as i64 + 1);
                Ok(true)
            })
        },
    ));
    set
}

/// DuckDB's `TranslateScalarFunction`.
fn translate(input: &[u8], from: &[u8], to: &[u8], out: &mut Vec<u8>) {
    let from: Vec<i64> = code_points(from).map(|(code, _)| code).collect();
    let to: Vec<i64> = code_points(to).map(|(code, _)| code).collect();
    let mut replace: HashMap<i64, i64> = HashMap::new();
    for (&source, &target) in from.iter().zip(&to) {
        replace.entry(source).or_insert(target);
    }
    let delete: HashSet<i64> = from[to.len().min(from.len())..]
        .iter()
        .copied()
        .filter(|code| !replace.contains_key(code))
        .collect();
    for (code, bytes) in code_points(input) {
        if let Some(&target) = replace.get(&code) {
            if let Some(c) = u32::try_from(target).ok().and_then(char::from_u32) {
                push_char(out, c);
            }
        } else if !delete.contains(&code) {
            out.extend_from_slice(bytes);
        }
    }
}

/// `translate(string, from, to)`.
pub fn translate_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("translate");
    set.add(ScalarFunction::new(
        "translate",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        |args, _, result| {
            ternary::<Str, Str, Str, StrOut>(args, result, |input, from, to, out| {
                translate(input, from, to, out);
                Ok(true)
            })
        },
    ));
    set
}
