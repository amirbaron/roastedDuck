//! `substring`/`substr` and `substring_grapheme` (DuckDB's `substring.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;
use unicode_segmentation::UnicodeSegmentation;

use crate::scalar::executor::{Str, StrOut, binary, ternary};

const SUPPORTED_UPPER_BOUND: i64 = u32::MAX as i64;
const SUPPORTED_LOWER_BOUND: i64 = -SUPPORTED_UPPER_BOUND - 1;

/// Whether `byte` starts a UTF-8 character (DuckDB's `IsCharacter`).
#[inline]
pub(crate) fn is_character(byte: u8) -> bool {
    byte & 0xc0 != 0x80
}

/// DuckDB's `AssertInSupportedRange`.
fn check_range(input_size: usize, offset: i64, length: i64) -> Result<()> {
    if input_size as u64 > SUPPORTED_UPPER_BOUND as u64 {
        return Err(Error::out_of_range(format!(
            "Substring input size is too large (> {SUPPORTED_UPPER_BOUND})"
        )));
    }
    if offset < SUPPORTED_LOWER_BOUND {
        return Err(Error::out_of_range(format!(
            "Substring offset outside of supported range (< {SUPPORTED_LOWER_BOUND})"
        )));
    }
    if offset > SUPPORTED_UPPER_BOUND {
        return Err(Error::out_of_range(format!(
            "Substring offset outside of supported range (> {SUPPORTED_UPPER_BOUND})"
        )));
    }
    if length < SUPPORTED_LOWER_BOUND {
        return Err(Error::out_of_range(format!(
            "Substring length outside of supported range (< {SUPPORTED_LOWER_BOUND})"
        )));
    }
    if length > SUPPORTED_UPPER_BOUND {
        return Err(Error::out_of_range(format!(
            "Substring length outside of supported range (> {SUPPORTED_UPPER_BOUND})"
        )));
    }
    Ok(())
}

/// The byte (or character) range of a substring (DuckDB's `SubstringStartEnd`), or `None`
/// for an empty result.
fn start_end(input_size: i64, offset: i64, mut length: i64) -> Option<(i64, i64)> {
    if length == 0 {
        return None;
    }
    let mut start = if offset > 0 {
        input_size.min(offset - 1)
    } else if offset < 0 {
        (input_size + offset).max(0)
    } else {
        length -= 1;
        if length <= 0 {
            return None;
        }
        0
    };
    let end;
    if length > 0 {
        end = input_size.min(start + length);
    } else {
        end = start;
        start = (start + length).max(0);
    }
    (start != end).then_some((start, end))
}

/// The substring of `input` counted in bytes (DuckDB's `SubstringASCII`).
pub(crate) fn substring_ascii<'a>(input: &'a [u8], offset: i64, length: i64) -> Result<&'a [u8]> {
    check_range(input.len(), offset, length)?;
    Ok(match start_end(input.len() as i64, offset, length) {
        Some((start, end)) => &input[start as usize..end as usize],
        None => &[],
    })
}

/// The substring of `input` counted in code points (DuckDB's `SubstringUnicode`).
pub(crate) fn substring_unicode<'a>(input: &'a [u8], offset: i64, length: i64) -> Result<&'a [u8]> {
    let input_size = input.len();
    check_range(input_size, offset, length)?;
    if length == 0 {
        return Ok(&[]);
    }
    let negative = offset < 0;
    let offset = offset - 1;
    let (start_pos, end_pos);
    if negative {
        let (start, end) = if length < 0 {
            (-offset - length, -offset)
        } else {
            (-offset, -offset - length)
        };
        let mut start_index = 0;
        let mut end_index = if end <= 0 { Some(input_size) } else { None };
        let mut current = 0i64;
        for i in (1..=input_size).rev() {
            if is_character(input[i - 1]) {
                current += 1;
                if current == start {
                    start_index = i;
                    break;
                } else if current == end {
                    end_index = Some(i);
                }
            }
        }
        while start_index < input_size && !is_character(input[start_index]) {
            start_index += 1;
        }
        let Some(mut end_index) = end_index else {
            return Ok(&[]);
        };
        while end_index < input_size && !is_character(input[end_index]) {
            end_index += 1;
        }
        start_pos = start_index;
        end_pos = end_index;
    } else {
        let (start, end) = if length < 0 {
            ((offset + length).max(0), offset)
        } else {
            (offset.max(0), offset + length)
        };
        let mut start_index = None;
        let mut end_index = input_size;
        let mut current = 0i64;
        for (i, &byte) in input.iter().enumerate() {
            if is_character(byte) {
                if current == start {
                    start_index = Some(i);
                } else if current == end {
                    end_index = i;
                    break;
                }
                current += 1;
            }
        }
        let Some(start_index) = start_index else {
            return Ok(&[]);
        };
        if end == 0 || end <= start {
            return Ok(&[]);
        }
        start_pos = start_index;
        end_pos = end_index;
    }
    Ok(input.get(start_pos..end_pos.max(start_pos)).unwrap_or_default())
}

/// The substring of `input` counted in grapheme clusters (DuckDB's `SubstringGrapheme`).
pub(crate) fn substring_grapheme(input: &[u8], offset: i64, length: i64) -> Result<&[u8]> {
    check_range(input.len(), offset, length)?;
    let Some((mut start, mut end)) = start_end(input.len() as i64, offset, length) else {
        return Ok(&[]);
    };
    let ascii_end = ((end + 1) as usize).min(input.len());
    if input[..ascii_end].is_ascii() {
        return Ok(&input[start as usize..end as usize]);
    }
    let text = String::from_utf8_lossy(input);
    let clusters: Vec<usize> = text.grapheme_indices(true).map(|(index, _)| index).collect();
    if offset < 0 {
        // DuckDB ignores the result of this second `SubstringStartEnd` call: an
        // empty range (`start == end`) still scans from `start` to the end.
        let count = clusters.len() as i64;
        start = (count + offset).max(0);
        if length > 0 {
            end = count.min(start + length);
        } else {
            end = start;
            start = (start + length).max(0);
        }
    }
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return Ok(&[]);
    };
    let Some(&start_pos) = clusters.get(start) else {
        return Ok(&[]);
    };
    let end_pos = if end > start {
        clusters.get(end).copied().unwrap_or(input.len())
    } else {
        input.len()
    };
    Ok(input.get(start_pos..end_pos).unwrap_or_default())
}

type Substring = for<'a> fn(&'a [u8], i64, i64) -> Result<&'a [u8]>;

fn substring_set(name: &str, op: Substring) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::BIGINT, LogicalType::BIGINT],
        LogicalType::VARCHAR,
        move |args, _, result| {
            ternary::<Str, i64, i64, StrOut>(args, result, |input, offset, length, out| {
                out.extend_from_slice(op(input, offset, length)?);
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::BIGINT],
        LogicalType::VARCHAR,
        move |args, _, result| {
            binary::<Str, i64, StrOut>(args, result, |input, offset, out| {
                out.extend_from_slice(op(input, offset, i64::from(u32::MAX))?);
                Ok(true)
            })
        },
    ));
    set
}

/// The `substring` set (alias `substr`).
pub fn substring_functions() -> ScalarFunctionSet {
    substring_set("substring", substring_unicode)
}

/// The `substring_grapheme` set.
pub fn substring_grapheme_functions() -> ScalarFunctionSet {
    substring_set("substring_grapheme", substring_grapheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_substrings_match_duckdb() {
        assert_eq!(substring_unicode(b"hello", 2, 3).unwrap(), b"ell");
        assert_eq!(substring_unicode(b"hello", -3, 2).unwrap(), b"ll");
        assert_eq!(substring_unicode(b"hello", 0, 2).unwrap(), b"h");
        assert_eq!(substring_unicode(b"hello", 3, -2).unwrap(), b"he");
        assert_eq!(
            substring_unicode("héllo".as_bytes(), 2, 2).unwrap(),
            "él".as_bytes()
        );
        assert_eq!(substring_unicode(b"hello", 10, 2).unwrap(), b"");
    }
}
