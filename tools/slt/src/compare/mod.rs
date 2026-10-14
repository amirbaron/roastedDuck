//! Pure result-comparison helpers from `test/sqlite/result_helper.cpp`: sorting, hashing, regex
//! matching and the boolean comparison. The numeric comparison of the Rust backend lives in
//! [`numeric`].

pub mod numeric;

use std::fmt::Write as _;

use md5::{Digest, Md5};
use regex::RegexBuilder;

use crate::parser::SortStyle;

/// `TestResultHelper::SortQueryResult`. Returns an error if a row sort is requested for values
/// that do not form whole rows.
pub fn sort_values(style: SortStyle, values: &mut [String], columns: usize) -> Result<(), String> {
    match style {
        SortStyle::NoSort => Ok(()),
        SortStyle::ValueSort => {
            values.sort();
            Ok(())
        }
        SortStyle::RowSort => {
            if columns == 0 {
                return Ok(());
            }
            if !values.len().is_multiple_of(columns) {
                return Err(format!(
                    "Failed to sort query result - result is not aligned. Found {} rows with {columns} columns",
                    values.len()
                ));
            }
            let mut rows: Vec<Vec<String>> = values.chunks(columns).map(<[String]>::to_vec).collect();
            rows.sort();
            for (slot, value) in values.iter_mut().zip(rows.into_iter().flatten()) {
                *slot = value;
            }
            Ok(())
        }
    }
}

/// `TestResultHelper::ResultIsHash`: `<count> values hashing to <32 hex digits>`.
pub fn result_is_hash(line: &str) -> bool {
    const MIDDLE: &str = " values hashing to ";
    const HASH_LEN: usize = 32;
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits + MIDDLE.len() + HASH_LEN != line.len() {
        return false;
    }
    let Some(hash) = line[digits..].strip_prefix(MIDDLE) else {
        return false;
    };
    hash.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

/// The `N values hashing to <md5>` line for `values` (each value followed by `\n`).
pub fn hash_values(values: &[String]) -> String {
    let mut hasher = Md5::new();
    for value in values {
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    let mut hex = String::with_capacity(32);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{} values hashing to {hex}", values.len())
}

/// Whether `text` starts with a `<REGEX>:` or `<!REGEX>:` marker.
pub fn is_regex(text: &str) -> bool {
    text.starts_with("<REGEX>:") || text.starts_with("<!REGEX>:")
}

/// `TestResultHelper::MatchesRegex`: full match with `.` matching newlines; `<!REGEX>:` inverts.
/// An invalid pattern never matches.
pub fn matches_regex(actual: &str, pattern: &str) -> bool {
    let want_match = pattern.starts_with("<REGEX>:");
    let body = pattern.replace("<REGEX>:", "").replace("<!REGEX>:", "");
    let regex = RegexBuilder::new(&format!(r"\A(?:{body})\z"))
        .dot_matches_new_line(true)
        .size_limit(1 << 26)
        .build();
    match regex {
        Ok(regex) => regex.is_match(actual) == want_match,
        Err(error) => {
            eprintln!("Failed to parse regex: {error}");
            false
        }
    }
}

/// The `BOOLEAN` branch of `TestResultHelper::CompareValues`: `true`/`1` and `false`/`0` are
/// equal; values that are neither compare as NULL, and NULL equals NULL.
pub fn boolean_values_equal(actual: &str, expected: &str) -> bool {
    fn parse(text: &str) -> Option<bool> {
        let lower = text.to_ascii_lowercase();
        if lower == "true" || text == "1" {
            Some(true)
        } else if lower == "false" || text == "0" {
            Some(false)
        } else {
            None
        }
    }
    parse(actual) == parse(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn row_sort_keeps_rows_together() {
        let mut values = strings(&["b", "1", "a", "2", "a", "1"]);
        sort_values(SortStyle::RowSort, &mut values, 2).unwrap();
        assert_eq!(values, strings(&["a", "1", "a", "2", "b", "1"]));
        assert!(sort_values(SortStyle::RowSort, &mut strings(&["a", "b", "c"]), 2).is_err());
    }

    #[test]
    fn value_sort_is_bytewise() {
        let mut values = strings(&["b", "10", "9", "B"]);
        sort_values(SortStyle::ValueSort, &mut values, 2).unwrap();
        assert_eq!(values, strings(&["10", "9", "B", "b"]));
    }

    #[test]
    fn hash_format() {
        assert_eq!(
            hash_values(&strings(&["1"])),
            "1 values hashing to b026324c6904b2a9cb4b88d6d61c81d1"
        );
        assert!(result_is_hash(
            "3 values hashing to b026324c6904b2a9cb4b88d6d61c81d1"
        ));
        assert!(!result_is_hash(
            "values hashing to b026324c6904b2a9cb4b88d6d61c81d1"
        ));
        assert!(!result_is_hash(
            "3 values hashing to b026324c6904b2a9cb4b88d6d61c81d"
        ));
        assert!(!result_is_hash(
            "3 values hashing to B026324C6904B2A9CB4B88D6D61C81D1"
        ));
    }

    #[test]
    fn regex_is_full_match_with_dotall() {
        assert!(matches_regex("Binder Error: x\n", "<REGEX>:Binder Error.*"));
        assert!(!matches_regex("Binder Error: x\n", "<REGEX>:Binder Error: x"));
        assert!(matches_regex("abc", "<!REGEX>:xyz"));
        assert!(!matches_regex("abc", "<!REGEX>:a.c"));
        assert!(!matches_regex("abc", "<REGEX>:(unclosed"));
    }

    #[test]
    fn booleans() {
        assert!(boolean_values_equal("1", "true"));
        assert!(boolean_values_equal("0", "FALSE"));
        assert!(!boolean_values_equal("1", "0"));
        assert!(boolean_values_equal("NULL", "garbage"));
        assert!(!boolean_values_equal("NULL", "false"));
    }
}
