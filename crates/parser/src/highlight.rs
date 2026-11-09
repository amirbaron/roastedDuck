//! Token classes for syntax highlighting of queries and error messages (DuckDB's
//! `Parser::Tokenize` and `Parser::TokenizeError`, used by the shell).

use roastedduck_pg_parser::{SimplifiedTokenType, tokenize};

/// The highlighting class of a token (DuckDB's `SimplifiedTokenType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightTokenType {
    Identifier,
    NumericConstant,
    StringConstant,
    Operator,
    Keyword,
    Comment,
    /// Plain text of an error message.
    Error,
    /// The emphasized parts of an error message (the `... Error:` prefix, `LINE n:`,
    /// keywords of the quoted query and the token at the caret).
    ErrorEmphasis,
    /// A quoted suggestion in an error message.
    ErrorSuggestion,
}

/// A token's highlighting class and the byte offset where it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HighlightToken {
    pub kind: HighlightTokenType,
    pub start: usize,
}

/// `Parser::Tokenize`: the tokens of a query.
pub fn tokenize_query(query: &str) -> Vec<HighlightToken> {
    tokenize(query)
        .into_iter()
        .map(|token| HighlightToken {
            kind: match token.kind {
                SimplifiedTokenType::Identifier => HighlightTokenType::Identifier,
                SimplifiedTokenType::NumericConstant => HighlightTokenType::NumericConstant,
                SimplifiedTokenType::StringConstant => HighlightTokenType::StringConstant,
                SimplifiedTokenType::Operator => HighlightTokenType::Operator,
                SimplifiedTokenType::Keyword => HighlightTokenType::Keyword,
                SimplifiedTokenType::Comment => HighlightTokenType::Comment,
            },
            start: token.start,
        })
        .collect()
}

fn token(kind: HighlightTokenType, start: usize) -> HighlightToken {
    HighlightToken { kind, start }
}

/// `Parser::TokenizeError`: highlighting of an error message — the `... Error:` prefix,
/// quoted suggestions, and the `LINE n: query` context with the token at the caret.
pub fn tokenize_error(error_msg: &str) -> Vec<HighlightToken> {
    let msg = error_msg.as_bytes();
    let mut error_start = 0;
    let mut error_end = msg.len();
    let mut tokens = Vec::new();
    if let Some(error) = error_msg.find("Error:") {
        tokens.push(token(HighlightTokenType::ErrorEmphasis, 0));
        tokens.push(token(HighlightTokenType::Error, error + 6));
        error_start = error + 6;
    } else {
        tokens.push(token(HighlightTokenType::Error, 0));
    }
    let line_pos = error_msg.find("\nLINE ");
    if let Some(line_pos) = line_pos {
        error_end = line_pos;
    }
    let mut in_quotes = false;
    let mut quote_char = 0;
    for (i, &c) in msg.iter().enumerate().take(error_end).skip(error_start) {
        if in_quotes {
            if c == quote_char {
                tokens.push(token(HighlightTokenType::Error, i));
                in_quotes = false;
            }
            if c == b'\n' || c == b'\r' {
                tokens.pop();
                in_quotes = false;
            }
        } else if c == b'"' || c == b'\'' {
            tokens.push(token(HighlightTokenType::ErrorSuggestion, i + 1));
            quote_char = c;
            in_quotes = true;
        }
    }
    if in_quotes {
        tokens.pop();
    }
    let Some(line_pos) = line_pos else {
        return tokens;
    };
    tokens.push(token(HighlightTokenType::ErrorEmphasis, line_pos + 1));
    let mut query_start = line_pos + 6;
    while query_start < msg.len() && (msg[query_start] == b':' || msg[query_start].is_ascii_digit()) {
        query_start += 1;
    }
    if query_start >= msg.len() {
        return tokens;
    }
    tokens.push(token(HighlightTokenType::Error, query_start));
    let query_end = msg[query_start..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(msg.len(), |offset| query_start + offset);
    // After `LINE n: query` comes a line with a caret under the error position.
    let caret_start = query_end + 1;
    let mut caret_position = None;
    if caret_start < msg.len()
        && let Some(offset) = msg[caret_start..].iter().position(|&c| c == b'^')
    {
        caret_position = Some(offset.wrapping_sub((query_start - line_pos) - 1));
    }
    let query = String::from_utf8_lossy(&msg[query_start..query_end]);
    for mut query_token in tokenize_query(&query) {
        if let Some(position) = caret_position
            && query_token.start >= position
        {
            tokens.push(token(HighlightTokenType::ErrorEmphasis, query_start + position));
            caret_position = None;
            continue;
        }
        query_token.kind = if query_token.kind == HighlightTokenType::Keyword {
            HighlightTokenType::ErrorEmphasis
        } else {
            HighlightTokenType::Error
        };
        query_token.start += query_start;
        tokens.push(query_token);
    }
    if query_end < msg.len() {
        tokens.push(token(HighlightTokenType::Error, query_end));
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages() {
        let message =
            "Parser Error: syntax error at or near \"FRM\"\n\nLINE 1: SELECT 1 FRM t\n                 ^";
        let tokens = tokenize_error(message);
        assert_eq!(tokens[0], token(HighlightTokenType::ErrorEmphasis, 0));
        assert_eq!(tokens[1], token(HighlightTokenType::Error, 13));
        assert_eq!(tokens[2], token(HighlightTokenType::ErrorSuggestion, 39));
        assert!(tokens.contains(&token(HighlightTokenType::ErrorEmphasis, 45)));
        assert_eq!(tokenize_query("SELECT 1")[0].kind, HighlightTokenType::Keyword);
    }
}
