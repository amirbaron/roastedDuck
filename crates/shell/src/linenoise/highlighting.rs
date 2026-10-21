//! Syntax highlighting of the line being edited (DuckDB's
//! `tools/shell/linenoise/highlighting.cpp`): the tokens of SQL or a dot command, extra
//! highlighting (search matches, completion characters), and the colored text.

use rustdb_parser::highlight::{HighlightTokenType, tokenize_query};

use crate::highlight::{self, HighlightElementType, PrintIntensity, RESET_TERMINAL_CODE};

use super::Linenoise;

/// `tokenType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenType {
    Identifier,
    NumericConstant,
    StringConstant,
    Operator,
    Keyword,
    Comment,
    Continuation,
    ContinuationSelected,
    Bracket,
    Error,
}

/// `ExtraHighlightingType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtraHighlightingType {
    None,
    Underline,
    Bold,
}

/// `highlightToken`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightToken {
    pub token_type: TokenType,
    pub start: usize,
    pub extra_highlighting: ExtraHighlightingType,
}

impl HighlightToken {
    pub fn new(token_type: TokenType, start: usize) -> Self {
        Self {
            token_type,
            start,
            extra_highlighting: ExtraHighlightingType::None,
        }
    }
}

/// `ExtraHighlighting`.
#[derive(Clone, Copy, Debug)]
pub struct ExtraHighlighting {
    pub start: usize,
    pub end: usize,
    pub kind: ExtraHighlightingType,
}

impl Default for ExtraHighlighting {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            kind: ExtraHighlightingType::None,
        }
    }
}

/// `Highlighting::IsEnabled`.
pub fn is_enabled() -> bool {
    highlight::is_enabled()
}

/// `GetParseTokens`: the tokens of SQL, starting at its first byte.
fn parse_tokens(buf: &[u8]) -> Vec<HighlightToken> {
    let sql = String::from_utf8_lossy(buf);
    let mut tokens: Vec<HighlightToken> = tokenize_query(&sql)
        .into_iter()
        .map(|token| {
            let token_type = match token.kind {
                HighlightTokenType::NumericConstant => TokenType::NumericConstant,
                HighlightTokenType::StringConstant => TokenType::StringConstant,
                HighlightTokenType::Operator => TokenType::Operator,
                HighlightTokenType::Keyword => TokenType::Keyword,
                HighlightTokenType::Comment => TokenType::Comment,
                _ => TokenType::Identifier,
            };
            HighlightToken::new(token_type, token.start)
        })
        .collect();
    if tokens.first().is_some_and(|token| token.start > 0) {
        tokens.insert(0, HighlightToken::new(TokenType::Identifier, 0));
    }
    if tokens.is_empty() && !buf.is_empty() {
        tokens.push(HighlightToken::new(TokenType::Identifier, 0));
    }
    tokens
}

/// `GetDotCommandTokens`: the command as a keyword, every argument as a string.
fn dot_command_tokens(buf: &[u8]) -> Vec<HighlightToken> {
    let mut tokens = vec![HighlightToken::new(TokenType::Keyword, 0)];
    for i in 0..buf.len().saturating_sub(1) {
        if Linenoise::is_space(buf[i]) {
            tokens.push(HighlightToken::new(TokenType::StringConstant, i + 1));
        }
    }
    tokens
}

/// `Highlighting::Tokenize`.
pub fn tokenize(buf: &[u8], is_dot_command: bool) -> Vec<HighlightToken> {
    if is_dot_command {
        dot_command_tokens(buf)
    } else {
        parse_tokens(buf)
    }
}

/// `Highlighting::AddExtraHighlighting`: marks the tokens from `start` to `end`.
pub fn add_extra_highlighting(len: usize, tokens: &mut Vec<HighlightToken>, extra: ExtraHighlighting) {
    if tokens.is_empty() || extra.kind == ExtraHighlightingType::None {
        return;
    }
    for i in 0..tokens.len() - 1 {
        if tokens[i].start <= extra.start && tokens[i + 1].start >= extra.start {
            let mut token_position = i + 1;
            let mut end_type = tokens[i].token_type;
            if tokens[i].start == extra.start {
                tokens[i].extra_highlighting = extra.kind;
            } else {
                let mut search_token = HighlightToken::new(tokens[i].token_type, extra.start);
                search_token.extra_highlighting = extra.kind;
                tokens.insert(token_position, search_token);
                token_position += 1;
            }
            while token_position < tokens.len() && tokens[token_position].start < extra.end {
                end_type = tokens[token_position].token_type;
                tokens[token_position].extra_highlighting = extra.kind;
                token_position += 1;
            }
            if token_position >= tokens.len() || tokens[token_position].start > extra.end {
                tokens.insert(token_position, HighlightToken::new(end_type, extra.end));
            }
            return;
        }
    }
    // The highlighting is at the end of the buffer.
    let Some(last) = tokens.last().copied() else {
        return;
    };
    if extra.start >= last.start {
        let mut start_token = HighlightToken::new(last.token_type, extra.start);
        start_token.extra_highlighting = extra.kind;
        tokens.push(start_token);
        if extra.end < len {
            tokens.push(HighlightToken::new(start_token.token_type, extra.end));
        }
    }
}

/// `Highlighting::HighlightText`: the bytes from `start_pos` to `end_pos` with the terminal
/// codes of their tokens.
pub fn highlight_text(buf: &[u8], start_pos: usize, end_pos: usize, tokens: &[HighlightToken]) -> Vec<u8> {
    let mut result = Vec::with_capacity(buf.len());
    let mut prev_pos = 0;
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1).map_or(buf.len(), |next| next.start);
        if next < start_pos {
            continue;
        }
        let start = token.start.max(start_pos);
        let end = next.min(end_pos);
        if end <= start {
            continue;
        }
        if prev_pos > start {
            // Rendering behind what was rendered: skipped.
            continue;
        }
        prev_pos = start;
        let text = &buf[start..end.min(buf.len())];
        let element_type = match token.token_type {
            TokenType::Keyword => HighlightElementType::Keyword,
            TokenType::NumericConstant => HighlightElementType::NumericConstant,
            TokenType::StringConstant => HighlightElementType::StringConstant,
            TokenType::Continuation => HighlightElementType::Continuation,
            TokenType::ContinuationSelected => HighlightElementType::ContinuationSelected,
            TokenType::Bracket => HighlightElementType::Bracket,
            TokenType::Error => HighlightElementType::ErrorToken,
            TokenType::Comment => HighlightElementType::Comment,
            TokenType::Identifier | TokenType::Operator => HighlightElementType::None,
        };
        let element = highlight::highlight_element(element_type);
        let mut intensity = element.intensity;
        match token.extra_highlighting {
            ExtraHighlightingType::Underline => {
                intensity = if intensity == PrintIntensity::Bold {
                    PrintIntensity::BoldUnderline
                } else {
                    PrintIntensity::Underline
                };
            }
            ExtraHighlightingType::Bold => {
                intensity = match intensity {
                    PrintIntensity::Underline => PrintIntensity::BoldUnderline,
                    // Bold text flips to standard.
                    PrintIntensity::Bold => PrintIntensity::Standard,
                    _ => PrintIntensity::Bold,
                };
            }
            ExtraHighlightingType::None => {}
        }
        let terminal_code = highlight::terminal_code(element.color, intensity);
        if terminal_code.is_empty() {
            result.extend_from_slice(text);
        } else {
            result.extend_from_slice(terminal_code.as_bytes());
            result.extend_from_slice(text);
            result.extend_from_slice(RESET_TERMINAL_CODE.as_bytes());
        }
    }
    result
}
