//! The keyword categories of the PEG grammar (DuckDB's `PEGKeywordHelper`,
//! `keyword_helper.cpp`). DuckDB generates the sets (`keyword_map.cpp`) from the keyword
//! lists that also form the grammar's `ReservedKeyword`, `UnreservedKeyword`,
//! `ColumnNameKeyword`, `FuncNameKeyword` and `TypeNameKeyword` rules; here they are read
//! from those rules.

use std::collections::HashMap;

use rustdb_common::{Error, Result};

use crate::peg_parser::{PegParser, PegTokenType};

/// A keyword category (DuckDB's `PEGKeywordCategory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PegKeywordCategory {
    Unreserved,
    Reserved,
    TypeFunc,
    ColName,
}

impl PegKeywordCategory {
    /// The bit of the category in [`KeywordCategories`].
    const fn bit(self) -> u8 {
        match self {
            Self::Unreserved => 1,
            Self::Reserved => 2,
            Self::TypeFunc => 4,
            Self::ColName => 8,
        }
    }
}

/// The categories one word belongs to (a keyword can be in several, e.g. `ASOF`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeywordCategories(u8);

impl KeywordCategories {
    /// Whether the word is in `category` (`PEGKeywordHelper::KeywordCategoryType`).
    pub const fn contains(self, category: PegKeywordCategory) -> bool {
        self.0 & category.bit() != 0
    }
}

/// The rules listing each category's keywords (`FILENAME_TO_CATEGORY` of
/// `inline_grammar.py`): the function and type name lists form one category.
const CATEGORY_RULES: [(&str, PegKeywordCategory); 5] = [
    ("ReservedKeyword", PegKeywordCategory::Reserved),
    ("UnreservedKeyword", PegKeywordCategory::Unreserved),
    ("ColumnNameKeyword", PegKeywordCategory::ColName),
    ("FuncNameKeyword", PegKeywordCategory::TypeFunc),
    ("TypeNameKeyword", PegKeywordCategory::TypeFunc),
];

/// The keyword categories, by lower-case keyword (DuckDB's `PEGKeywordHelper`).
#[derive(Debug)]
pub(crate) struct PegKeywordHelper {
    keywords: HashMap<String, KeywordCategories>,
    /// The length of the longest keyword: longer words are no keywords.
    max_length: usize,
}

impl PegKeywordHelper {
    /// Collects the keywords of the grammar's keyword list rules
    /// (`PEGKeywordHelper::InitializeKeywordMaps`).
    pub fn from_grammar(parser: &PegParser<'_>) -> Result<Self> {
        let mut keywords: HashMap<String, KeywordCategories> = HashMap::new();
        for (rule_name, category) in CATEGORY_RULES {
            let rule = parser
                .rule(rule_name)
                .ok_or_else(|| Error::internal(format!("the grammar has no rule {rule_name}")))?;
            for token in rule
                .tokens
                .iter()
                .filter(|token| token.ty == PegTokenType::Literal)
            {
                keywords.entry(token.text.to_ascii_lowercase()).or_default().0 |= category.bit();
            }
        }
        let max_length = keywords.keys().map(String::len).max().unwrap_or(0);
        Ok(Self { keywords, max_length })
    }

    /// The categories of `text`, compared case-insensitively.
    pub fn categories(&self, text: &str) -> KeywordCategories {
        if text.len() > self.max_length {
            return KeywordCategories::default();
        }
        let found = if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            self.keywords.get(&text.to_ascii_lowercase())
        } else {
            self.keywords.get(text)
        };
        found.copied().unwrap_or_default()
    }
}
