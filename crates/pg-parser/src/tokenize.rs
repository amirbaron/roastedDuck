//! Coarse token classification for syntax highlighting and statement splitting
//! (DuckDB's `Parser::Tokenize` / `libpg_query`'s `tokenize`).

use crate::scanner::{ScanOptions, Scanner};
use crate::tables::token;

/// The coarse class of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimplifiedTokenType {
    /// An identifier (quoted or not).
    Identifier,
    /// An integer or decimal literal.
    NumericConstant,
    /// A string, bit-string or hex-string literal.
    StringConstant,
    /// An operator, punctuation, or a parameter.
    Operator,
    /// A keyword.
    Keyword,
    /// A comment. Never produced: like DuckDB, the tokenizer skips comments.
    Comment,
}

/// A token's class and byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SimplifiedToken {
    /// The token's class.
    pub kind: SimplifiedTokenType,
    /// Byte offset of the token in the query.
    pub start: usize,
}

/// Splits `sql` into classified tokens. Scanning stops silently at the first
/// lexical error (an unterminated string, for example), like DuckDB's tokenizer.
pub fn tokenize(sql: &str) -> Vec<SimplifiedToken> {
    tokenize_bytes(sql.as_bytes())
}

/// Native byte-tokenization API, stopping at the same lexical errors as C++.
pub fn tokenize_bytes(sql: &[u8]) -> Vec<SimplifiedToken> {
    let mut scanner = Scanner::new_bytes(
        sql,
        ScanOptions {
            preserve_identifier_case: true,
        },
    );
    let mut tokens = Vec::new();
    while let Ok(tok) = scanner.next_token() {
        if tok.code == 0 {
            break;
        }
        let kind = match tok.code {
            token::IDENT => SimplifiedTokenType::Identifier,
            token::ICONST | token::FCONST => SimplifiedTokenType::NumericConstant,
            token::SCONST | token::BCONST | token::XCONST => SimplifiedTokenType::StringConstant,
            token::Op
            | token::PARAM
            | token::COLON_EQUALS
            | token::EQUALS_GREATER
            | token::LESS_EQUALS
            | token::GREATER_EQUALS
            | token::NOT_EQUALS => SimplifiedTokenType::Operator,
            // every other token above the single-byte range (including `::` and
            // `->`) is classified as a keyword, exactly as DuckDB does
            code if code >= 255 => SimplifiedTokenType::Keyword,
            _ => SimplifiedTokenType::Operator,
        };
        tokens.push(SimplifiedToken { kind, start: tok.loc });
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::SimplifiedTokenType::{Identifier, Keyword, NumericConstant, Operator, StringConstant};
    use super::*;

    fn kinds(sql: &str) -> Vec<(SimplifiedTokenType, usize)> {
        tokenize(sql).into_iter().map(|t| (t.kind, t.start)).collect()
    }

    #[test]
    fn classifies_tokens() {
        assert_eq!(
            kinds("SELECT a, 1.5, 'x' FROM t WHERE b >= $1 -- c"),
            vec![
                (Keyword, 0),
                (Identifier, 7),
                (Operator, 8),
                (NumericConstant, 10),
                (Operator, 13),
                (StringConstant, 15),
                (Keyword, 19),
                (Identifier, 24),
                (Keyword, 26),
                (Identifier, 32),
                (Operator, 34),
                (Operator, 37),
            ]
        );
    }

    #[test]
    fn typecast_counts_as_keyword_and_errors_stop() {
        assert_eq!(kinds("a::int"), vec![(Identifier, 0), (Keyword, 1), (Keyword, 3)]);
        assert_eq!(kinds("a 'unterminated"), vec![(Identifier, 0)]);
        assert_eq!(kinds("NOT IN"), vec![(Keyword, 0), (Keyword, 4)]);
    }
}
