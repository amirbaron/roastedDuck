//! The tokenizer the matcher runs on (DuckDB's `BaseTokenizer`, `tokenizer.cpp`): splits
//! SQL into words, literals and operators, without classifying keywords.

use crate::peg_parser::is_space;

/// One token (DuckDB's `MatcherToken`): its text and its byte offset in the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatcherToken {
    pub text: String,
    pub offset: usize,
}

impl MatcherToken {
    pub fn new(text: impl Into<String>, offset: usize) -> Self {
        Self {
            text: text.into(),
            offset,
        }
    }
}

/// The state of the tokenizer (DuckDB's `TokenizeState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenizeState {
    Standard,
    SingleLineComment,
    MultiLineComment,
    QuotedIdentifier,
    StringLiteral,
    Keyword,
    Numeric,
    Operator,
    DollarQuotedString,
}

/// What a tokenizer does at statement ends and with the trailing word (the virtual
/// methods of DuckDB's `BaseTokenizer`).
pub trait TokenizerCallbacks {
    /// `OnStatementEnd`: by default the tokens of the finished statement are dropped.
    fn on_statement_end(&mut self, tokens: &mut Vec<MatcherToken>, _pos: usize) {
        tokens.clear();
    }

    /// `OnLastToken`: the unfinished word at the end of the input and where it starts,
    /// with the state the tokenizer ended in.
    fn on_last_token(
        &mut self,
        tokens: &mut Vec<MatcherToken>,
        state: TokenizeState,
        last_word: String,
        last_pos: usize,
    );
}

/// `StringUtil::CharacterIsOperator`.
pub(crate) const fn is_string_util_operator(c: u8) -> bool {
    if c == b'_' {
        return false;
    }
    matches!(c, b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~')
}

/// `BaseTokenizer::IsSingleByteOperator`.
const fn is_single_byte_operator(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'{' | b'}' | b'[' | b']' | b',' | b'?' | b'$' | b'-' | b'#'
    )
}

/// `BaseTokenizer::CharacterIsInitialNumber`.
pub(crate) const fn is_initial_number(c: u8) -> bool {
    c.is_ascii_digit() || c == b'.'
}

/// `BaseTokenizer::CharacterIsSpecialStringCharacter`: the prefix of `N'..'`, `X'..'`,
/// `E'..'` and `B'..'` strings.
const fn is_special_string_character(c: u8) -> bool {
    matches!(c, b'N' | b'n' | b'X' | b'x' | b'E' | b'e' | b'B' | b'b')
}

/// `BaseTokenizer::CharacterIsNumber`.
pub(crate) const fn is_number(c: u8) -> bool {
    is_initial_number(c) || matches!(c, b'e' | b'E' | b'_')
}

/// `BaseTokenizer::CharacterIsScientific`.
pub(crate) const fn is_scientific(c: u8) -> bool {
    matches!(c, b'e' | b'E')
}

/// `BaseTokenizer::CharacterIsControlFlow`.
const fn is_control_flow(c: u8) -> bool {
    matches!(c, b'\'' | b'-' | b';' | b'"' | b'.')
}

/// `BaseTokenizer::CharacterIsKeyword`: bytes that continue a word (every non-ASCII
/// byte does).
pub(crate) const fn is_keyword_character(c: u8) -> bool {
    !is_single_byte_operator(c) && !is_string_util_operator(c) && !is_space(c) && !is_control_flow(c)
}

/// `BaseTokenizer::CharacterIsOperator`.
const fn is_operator_character(c: u8) -> bool {
    !is_single_byte_operator(c) && !is_control_flow(c) && is_string_util_operator(c)
}

/// `BaseTokenizer::IsValidDollarTagCharacter`.
const fn is_dollar_tag_character(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// The text of `sql[start..end]`. Token boundaries are ASCII bytes, so the slice is
/// valid UTF-8; DuckDB copies bytes either way.
fn substring(sql: &str, start: usize, end: usize) -> String {
    sql.get(start..end).map_or_else(
        || String::from_utf8_lossy(&sql.as_bytes()[start..end]).into_owned(),
        str::to_owned,
    )
}

/// `BaseTokenizer::IsSpecialOperator`: the length of the multi-byte operator at `pos`.
fn special_operator_length(sql: &[u8], pos: usize) -> Option<usize> {
    if pos + 2 < sql.len() && sql[pos..].starts_with(b"->>") {
        return Some(3);
    }
    if pos + 1 >= sql.len() {
        return None;
    }
    [b"::", b":=", b"->", b"**", b"//"]
        .iter()
        .any(|op| sql[pos..].starts_with(*op))
        .then_some(2)
}

/// `BaseTokenizer::PushToken`.
fn push_token(sql: &str, tokens: &mut Vec<MatcherToken>, start: usize, end: usize) {
    if start < end {
        tokens.push(MatcherToken::new(substring(sql, start, end), start));
    }
}

/// `BaseTokenizer::TokenizeInput`: tokenizes `sql` into `tokens`, calling `callbacks` at
/// every `;` and with the trailing word. Returns `false` if the input ends inside a
/// comment or a dollar-quoted string (which have no suggestions).
#[allow(
    clippy::too_many_lines,
    reason = "one state machine, like DuckDB's TokenizeInput"
)]
pub fn tokenize_input(
    sql: &str,
    tokens: &mut Vec<MatcherToken>,
    callbacks: &mut impl TokenizerCallbacks,
) -> bool {
    let bytes = sql.as_bytes();
    let size = bytes.len();
    let mut state = TokenizeState::Standard;
    let mut last_pos = 0;
    let mut dollar_quote_marker: &[u8] = &[];
    let mut i = 0;
    while i < size {
        let c = bytes[i];
        match state {
            TokenizeState::Standard => {
                if c == b'\'' {
                    state = TokenizeState::StringLiteral;
                    last_pos = i;
                } else if c == b'"' {
                    state = TokenizeState::QuotedIdentifier;
                    last_pos = i;
                } else if c == b';' {
                    // The end of a statement.
                    callbacks.on_statement_end(tokens, i);
                    last_pos = i + 1;
                } else if c == b'$' {
                    // A dollar-quoted string, a parameter ($1) or a column label ($label).
                    if i + 1 < size {
                        if bytes[i + 1].is_ascii_digit() {
                            // $[numeric] is a parameter, not a dollar-quoted string.
                            tokens.push(MatcherToken::new("$", i));
                        } else {
                            last_pos = i;
                            let next_dollar = bytes[i + 1..]
                                .iter()
                                .position(|&byte| byte == b'$' || !is_dollar_tag_character(byte))
                                .map(|offset| i + 1 + offset)
                                .filter(|&end| bytes[end] == b'$');
                            if let Some(next_dollar) = next_dollar {
                                state = TokenizeState::DollarQuotedString;
                                dollar_quote_marker = &bytes[last_pos + 1..next_dollar];
                                i = next_dollar;
                            } else {
                                // A column label parameter ($collabel).
                                tokens.push(MatcherToken::new("$", i));
                            }
                        }
                    }
                } else if c == b'-' && i + 1 < size && bytes[i + 1] == b'-' {
                    i += 1;
                    state = TokenizeState::SingleLineComment;
                } else if c == b'/' && i + 1 < size && bytes[i + 1] == b'*' {
                    i += 1;
                    state = TokenizeState::MultiLineComment;
                } else if is_space(c) {
                    last_pos = i + 1;
                } else if let Some(op_len) = special_operator_length(bytes, i) {
                    // DuckDB records the offset of the preceding word boundary here.
                    tokens.push(MatcherToken::new(substring(sql, i, i + op_len), last_pos));
                    i += op_len - 1;
                    last_pos = i + 1;
                } else if is_single_byte_operator(c) {
                    tokens.push(MatcherToken::new(char::from(c).to_string(), last_pos));
                    last_pos = i + 1;
                } else if is_initial_number(c) {
                    state = TokenizeState::Numeric;
                    last_pos = i;
                } else if is_special_string_character(c) && i + 1 < size && bytes[i + 1] == b'\'' {
                    // A prefixed string literal (N'..', X'..', E'..', B'..').
                    state = TokenizeState::StringLiteral;
                    last_pos = i;
                    i += 1;
                } else if is_string_util_operator(c) {
                    state = TokenizeState::Operator;
                    last_pos = i;
                } else {
                    state = TokenizeState::Keyword;
                    last_pos = i;
                }
            }
            TokenizeState::Numeric => {
                let continues = is_initial_number(c)
                    // An underscore only directly followed by a digit (no doubled underscores).
                    || (c == b'_' && i + 1 < size && is_initial_number(bytes[i + 1]))
                    // A scientific notation marker (not "1ee5").
                    || (is_scientific(c) && !is_scientific(bytes[i - 1]))
                    // A sign right after the marker ("1e-5", not "1+5").
                    || ((c == b'+' || c == b'-') && is_scientific(bytes[i - 1]));
                if !continues {
                    // The end of the number: back up to its last digit.
                    while !is_initial_number(bytes[i - 1]) {
                        i -= 1;
                    }
                    push_token(sql, tokens, last_pos, i);
                    state = TokenizeState::Standard;
                    last_pos = i;
                    continue;
                }
            }
            TokenizeState::Operator => {
                if !is_operator_character(c) {
                    // PostgreSQL's rule: an operator cannot end in '+' unless it contains
                    // one of ~ ! @ # % ^ & | ` ?
                    let mut end_pos = i;
                    let has_special = bytes[last_pos..end_pos].iter().any(|byte| {
                        matches!(
                            byte,
                            b'~' | b'!' | b'@' | b'#' | b'%' | b'^' | b'&' | b'|' | b'`' | b'?'
                        )
                    });
                    if !has_special {
                        while end_pos > last_pos && bytes[end_pos - 1] == b'+' {
                            end_pos -= 1;
                        }
                    }
                    push_token(sql, tokens, last_pos, end_pos);
                    // Every trimmed '+' is a token of its own.
                    for plus in end_pos..i {
                        tokens.push(MatcherToken::new("+", plus));
                    }
                    state = TokenizeState::Standard;
                    last_pos = i;
                    continue;
                }
            }
            TokenizeState::Keyword => {
                if !is_keyword_character(c) {
                    push_token(sql, tokens, last_pos, i);
                    state = TokenizeState::Standard;
                    last_pos = i;
                    continue;
                }
            }
            TokenizeState::StringLiteral | TokenizeState::QuotedIdentifier => {
                let quote = if state == TokenizeState::StringLiteral {
                    b'\''
                } else {
                    b'"'
                };
                if c == quote {
                    if i + 1 < size && bytes[i + 1] == quote {
                        // An escaped quote.
                        i += 1;
                    } else {
                        push_token(sql, tokens, last_pos, i + 1);
                        last_pos = i + 1;
                        state = TokenizeState::Standard;
                    }
                }
            }
            TokenizeState::SingleLineComment => {
                if c == b'\n' || c == b'\r' {
                    last_pos = i + 1;
                    state = TokenizeState::Standard;
                }
            }
            TokenizeState::MultiLineComment => {
                if c == b'*' && i + 1 < size && bytes[i + 1] == b'/' {
                    i += 1;
                    last_pos = i + 1;
                    state = TokenizeState::Standard;
                }
            }
            TokenizeState::DollarQuotedString => {
                // Only $[marker]$ ends a dollar-quoted string.
                if c == b'$' && i + 1 < size {
                    let start = i + 1;
                    let end = bytes[start..]
                        .iter()
                        .position(|&byte| byte == b'$')
                        .map(|offset| start + offset);
                    if let Some(end) = end
                        && &bytes[start..end] == dollar_quote_marker
                    {
                        // The string becomes a single-quoted literal.
                        let full_marker_len = dollar_quote_marker.len() + 2;
                        let content = substring(sql, last_pos + full_marker_len, start - 1);
                        let quoted = format!("'{}'", content.replace('\'', "''"));
                        // DuckDB records the marker length as the offset.
                        tokens.push(MatcherToken::new(quoted, full_marker_len));
                        dollar_quote_marker = &[];
                        state = TokenizeState::Standard;
                        i = end;
                        last_pos = i + 1;
                    }
                }
            }
        }
        i += 1;
    }
    // Check the final state.
    match state {
        TokenizeState::StringLiteral => last_pos += 1,
        // No suggestions in comments or dollar-quoted strings.
        TokenizeState::SingleLineComment
        | TokenizeState::MultiLineComment
        | TokenizeState::DollarQuotedString => {
            return false;
        }
        _ => {}
    }
    let last_word = substring(sql, last_pos.min(size), size);
    callbacks.on_last_token(tokens, state, last_word, last_pos);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps the trailing word as a token, like DuckDB's `ParserTokenizer`.
    struct KeepLast;

    impl TokenizerCallbacks for KeepLast {
        fn on_last_token(
            &mut self,
            tokens: &mut Vec<MatcherToken>,
            _: TokenizeState,
            word: String,
            pos: usize,
        ) {
            if !word.is_empty() {
                tokens.push(MatcherToken::new(word, pos));
            }
        }
    }

    fn texts(sql: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        assert!(tokenize_input(sql, &mut tokens, &mut KeepLast));
        tokens.into_iter().map(|token| token.text).collect()
    }

    #[test]
    fn words_numbers_and_operators() {
        assert_eq!(
            texts("SELECT a::INT, 1e-5+2, x->>'k' FROM t"),
            [
                "SELECT", "a", "::", "INT", ",", "1e-5", "+", "2", ",", "x", "->>", "'k'", "FROM", "t"
            ]
        );
        assert_eq!(texts("1e"), ["1e"]);
        assert_eq!(texts("1e "), ["1", "e"]);
        assert_eq!(texts("a <=+ b"), ["a", "<=", "+", "b"]);
    }

    #[test]
    fn quotes_comments_and_dollar_strings() {
        assert_eq!(
            texts("'it''s' \"a\"\"b\" -- c\nd"),
            ["'it''s'", "\"a\"\"b\"", "d"]
        );
        assert_eq!(texts("$tag$it's$tag$ $1 $x"), ["'it''s'", "$", "1", "$", "x"]);
        assert_eq!(texts("E'x' /* c */ y"), ["E'x'", "y"]);
        let mut tokens = Vec::new();
        assert!(!tokenize_input("SELECT /* open", &mut tokens, &mut KeepLast));
    }

    #[test]
    fn statements_reset_the_tokens() {
        assert_eq!(texts("SELECT 1; SELECT 2"), ["SELECT", "2"]);
    }
}
