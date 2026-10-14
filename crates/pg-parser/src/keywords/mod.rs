//! SQL keywords (`kwlist.hpp`) and `ScanKeywordLookup`.

#[rustfmt::skip]
mod list;

use list::{HASH_MASK, HASH_SEED, KEYWORDS, MAX_KEYWORD_LEN, SLOTS};

/// How reserved a keyword is (DuckDB's `KeywordCategory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeywordCategory {
    /// Usable as any identifier.
    Unreserved,
    /// Usable as a column name but not as a function or type name.
    ColName,
    /// Usable as a function or type name but not as a column name.
    TypeFunc,
    /// Never usable as an identifier without quoting.
    Reserved,
}

/// One entry of the keyword list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keyword {
    /// The keyword in lower case.
    pub name: &'static str,
    /// The grammar's token code for the keyword.
    pub(crate) token: u16,
    /// How reserved the keyword is.
    pub category: KeywordCategory,
}

/// FNV-1a style hash used to build the lookup table (see `scripts/extract_tables.py`).
fn hash(word: &[u8]) -> usize {
    let mut h = HASH_SEED;
    for &b in word {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    ((h ^ (h >> 15)) as usize) & HASH_MASK
}

/// Looks up a keyword case-insensitively (ASCII only, like `ScanKeywordLookup`).
pub fn lookup(text: &str) -> Option<&'static Keyword> {
    lookup_bytes(text.as_bytes())
}

pub(crate) fn lookup_bytes(text: &[u8]) -> Option<&'static Keyword> {
    if text.is_empty() || text.len() > MAX_KEYWORD_LEN {
        return None;
    }
    let mut buf = [0u8; MAX_KEYWORD_LEN];
    let word = &mut buf[..text.len()];
    word.copy_from_slice(text);
    word.make_ascii_lowercase();
    let mut slot = hash(word);
    loop {
        let index = usize::from(SLOTS[slot]);
        if index == 0 {
            return None;
        }
        let keyword = &KEYWORDS[index - 1];
        if keyword.name.as_bytes() == word {
            return Some(keyword);
        }
        slot = (slot + 1) & HASH_MASK;
    }
}

/// Every keyword, in alphabetical order (DuckDB's `Parser::KeywordList`).
pub fn keyword_list() -> &'static [Keyword] {
    &KEYWORDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_keyword_is_found_in_any_case() {
        for keyword in keyword_list() {
            assert_eq!(lookup(keyword.name).map(|k| k.name), Some(keyword.name));
            let upper = keyword.name.to_ascii_uppercase();
            assert_eq!(lookup(&upper).map(|k| k.name), Some(keyword.name));
        }
    }

    #[test]
    fn non_keywords_are_rejected() {
        for word in [
            "",
            "selec",
            "selects",
            "x",
            "näme",
            "a_very_long_identifier_that_is_no_keyword",
        ] {
            assert!(lookup(word).is_none(), "{word}");
        }
        assert_eq!(
            lookup("SeLeCt").map(|k| k.category),
            Some(KeywordCategory::Reserved)
        );
    }
}
