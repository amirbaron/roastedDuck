//! The SQL scanner: a hand-written port of `libpg_query`'s flex scanner (`scan.l`)
//! plus the one-token lookahead filter of `base_yylex` (`parser.cpp`).
//!
//! Flex picks the longest match among all rules and breaks ties by rule order; each
//! `scan_*` function below reproduces that choice for one family of rules. Token
//! locations are byte offsets. Errors reproduce `scanner_yyerror`: the message names
//! the text from the error location up to the end of the current flex match.

mod literal;

use rustdb_common::{Error, Result};

use crate::error::parser_error;
use crate::keywords;
use crate::semantic::SemValue;
use crate::tables::token;

/// Characters that may form multi-character operators (`op_chars`).
const fn is_op_char(b: u8) -> bool {
    matches!(
        b,
        b'~' | b'!'
            | b'@'
            | b'^'
            | b'&'
            | b'|'
            | b'`'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'<'
            | b'>'
            | b'='
    )
}

/// Characters returned as single-character tokens (`self`).
const fn is_self_char(b: u8) -> bool {
    matches!(
        b,
        b',' | b'('
            | b')'
            | b'['
            | b']'
            | b'.'
            | b';'
            | b':'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'<'
            | b'>'
            | b'='
    )
}

/// `{space}`: SQL whitespace.
pub(crate) const fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

const fn is_newline(b: u8) -> bool {
    matches!(b, b'\n' | b'\r')
}

/// `{ident_start}`.
const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

/// `{ident_cont}`.
const fn is_ident_cont(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit() || b == b'$'
}

/// A token produced by the scanner.
#[derive(Debug)]
pub(crate) struct Token<'a> {
    /// The grammar's token code; 0 at end of input.
    pub(crate) code: u16,
    /// The semantic value (`yylval`).
    pub(crate) value: SemValue<'a>,
    /// Byte offset of the token (`yylloc`).
    pub(crate) loc: usize,
    /// Byte offset just past the matched text (where flex keeps its NUL).
    pub(crate) end: usize,
}

/// Scanner configuration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanOptions {
    /// Keep identifier case instead of folding ASCII letters to lower case.
    pub(crate) preserve_identifier_case: bool,
}

/// The scanner state for one query string.
pub(crate) struct Scanner<'a> {
    text: &'a str,
    buf: &'a [u8],
    pos: usize,
    options: ScanOptions,
    /// Scratch buffer for literal text (`literalbuf`).
    lit: Vec<u8>,
    /// A token read ahead by the `base_yylex` filter.
    lookahead: Option<Token<'a>>,
    /// Location and match end of the most recently returned token, used for
    /// syntax errors reported by the grammar.
    current: (usize, usize),
}

impl<'a> Scanner<'a> {
    /// Prepares to scan `text`. Like the C scanner, input stops at the first NUL.
    pub(crate) fn new(text: &'a str, options: ScanOptions) -> Self {
        let text = text.find('\0').map_or(text, |nul| &text[..nul]);
        Self {
            text,
            buf: text.as_bytes(),
            pos: 0,
            options,
            lit: Vec::new(),
            lookahead: None,
            current: (0, 0),
        }
    }

    fn peek(&self, offset: usize) -> u8 {
        self.buf.get(self.pos + offset).copied().unwrap_or(0)
    }

    fn at(&self, index: usize) -> u8 {
        self.buf.get(index).copied().unwrap_or(0)
    }

    // ---------------------------------------------------------------------------------
    // Errors
    // ---------------------------------------------------------------------------------

    /// `scanner_yyerror`: `message` at the given location, naming the text between
    /// the location and the end of the current match.
    fn yyerror(&self, message: &str, loc: usize, end: usize) -> Error {
        // The C scanner prints the NUL-terminated string at `loc`: it ends where flex
        // parked its NUL (`end`) or, past that point, at the end of the buffer.
        let full = if loc == end || loc >= self.buf.len() {
            format!("{message} at end of input")
        } else {
            let end = if loc > end { self.buf.len() } else { end };
            format!("{message} at or near \"{}\"", self.lossy(loc, end))
        };
        parser_error(full, self.buf, Some(loc))
    }

    fn lossy(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.buf[start..end.min(self.buf.len())]).into_owned()
    }

    /// A syntax error reported by the grammar at the most recently returned token
    /// (`parser_yyerror`).
    pub(crate) fn syntax_error(&self, message: &str) -> Error {
        let (loc, end) = self.current;
        self.yyerror(message, loc, end)
    }

    /// `ereport(ERROR, errmsg(message), parser_errposition(location))`; a negative
    /// location means "unknown" and attaches no position.
    pub(crate) fn error_at(&self, message: String, location: i32) -> Error {
        let location = usize::try_from(location).ok();
        parser_error(message, self.buf, location)
    }

    // ---------------------------------------------------------------------------------
    // The `base_yylex` filter
    // ---------------------------------------------------------------------------------

    /// Returns the next token, replacing `NOT`, `NULLS` and `WITH` by their `_LA`
    /// variants when the following token requires it (`base_yylex`).
    pub(crate) fn next_token(&mut self) -> Result<Token<'a>> {
        let mut tok = match self.lookahead.take() {
            Some(tok) => tok,
            None => self.scan()?,
        };
        if matches!(tok.code, token::NOT | token::NULLS_P | token::WITH) {
            // Errors while reading ahead point at the lookahead token.
            let next = self.scan()?;
            tok.code = match (tok.code, next.code) {
                (token::NOT, token::BETWEEN | token::IN_P | token::LIKE | token::ILIKE | token::SIMILAR) => {
                    token::NOT_LA
                }
                (token::NULLS_P, token::FIRST_P | token::LAST_P) => token::NULLS_LA,
                (token::WITH, token::TIME | token::ORDINALITY) => token::WITH_LA,
                (code, _) => code,
            };
            self.lookahead = Some(next);
        }
        self.current = (tok.loc, tok.end);
        Ok(tok)
    }

    // ---------------------------------------------------------------------------------
    // The core scanner (core_yylex)
    // ---------------------------------------------------------------------------------

    fn token(&self, code: u16, value: SemValue<'a>, loc: usize) -> Token<'a> {
        Token {
            code,
            value,
            loc,
            end: self.pos,
        }
    }

    fn punct(&mut self, code: u16, loc: usize, len: usize) -> Token<'a> {
        self.pos = loc + len;
        self.token(code, SemValue::None, loc)
    }

    /// Scans one token (`core_yylex`).
    pub(crate) fn scan(&mut self) -> Result<Token<'a>> {
        loop {
            let start = self.pos;
            let c = self.peek(0);
            if start >= self.buf.len() {
                return Ok(Token {
                    code: 0,
                    value: SemValue::None,
                    loc: start,
                    end: start,
                });
            }
            match c {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0c => {
                    while is_space(self.peek(0)) {
                        self.pos += 1;
                    }
                }
                b'-' if self.peek(1) == b'-' => self.skip_line_comment(),
                b'/' if self.peek(1) == b'*' => self.skip_block_comment()?,
                b'\'' => return self.scan_quoted_string(start, start + 1),
                b'"' => return self.scan_quoted_identifier(start),
                b'$' => return self.scan_dollar(start),
                b'?' if self.peek(1).is_ascii_digit() => return Ok(self.scan_param(start)),
                b'0'..=b'9' => return Ok(self.scan_number(start)),
                b'.' => {
                    return Ok(if self.peek(1) == b'.' {
                        self.punct(token::DOT_DOT, start, 2)
                    } else if self.peek(1).is_ascii_digit() {
                        return Ok(self.scan_number(start));
                    } else {
                        self.punct(u16::from(b'.'), start, 1)
                    });
                }
                b':' => {
                    return Ok(match self.peek(1) {
                        b':' => self.punct(token::TYPECAST, start, 2),
                        b'=' => self.punct(token::COLON_EQUALS, start, 2),
                        _ => self.punct(token::SINGLE_COLON, start, 1),
                    });
                }
                c if is_op_char(c) => return self.scan_operator(start),
                c if is_self_char(c) => return Ok(self.punct(u16::from(c), start, 1)),
                c if is_ident_start(c) => return self.scan_word(start),
                c => return Ok(self.punct(u16::from(c), start, 1)),
            }
        }
    }

    /// `{comment}`: `--` up to (not including) the next newline.
    fn skip_line_comment(&mut self) {
        while self.pos < self.buf.len() && !is_newline(self.buf[self.pos]) {
            self.pos += 1;
        }
    }

    /// The `xc` state: nested `/* ... */` comments.
    fn skip_block_comment(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 0usize;
        loop {
            match (self.peek(0), self.peek(1)) {
                _ if self.pos >= self.buf.len() => {
                    return Err(self.yyerror("unterminated /* comment", start, self.buf.len()));
                }
                (b'/', b'*') => {
                    depth += 1;
                    self.pos += 2;
                }
                (b'*', _) => {
                    while self.peek(0) == b'*' {
                        self.pos += 1;
                    }
                    if self.peek(0) == b'/' {
                        self.pos += 1;
                        if depth == 0 {
                            return Ok(());
                        }
                        depth -= 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// `{operator}` and the operator-like tokens it competes with.
    fn scan_operator(&mut self, start: usize) -> Result<Token<'a>> {
        let mut end = start;
        while is_op_char(self.at(end)) {
            end += 1;
        }
        let run = &self.buf[start..end];
        // Fixed tokens listed before {operator} win ties on length.
        let fixed = match run {
            [c] if is_self_char(*c) => Some(u16::from(*c)),
            b"->" => Some(token::SINGLE_ARROW),
            b"->>" => Some(token::DOUBLE_ARROW),
            b"**" => Some(token::POWER_OF),
            b"//" => Some(token::INTEGER_DIVISION),
            b"=>" => Some(token::EQUALS_GREATER),
            b"<=" => Some(token::LESS_EQUALS),
            b">=" => Some(token::GREATER_EQUALS),
            b"<>" | b"!=" => Some(token::NOT_EQUALS),
            _ => None,
        };
        if let Some(code) = fixed {
            return Ok(self.punct(code, start, run.len()));
        }
        // Stop at an embedded comment start.
        let mut nchars = run.len();
        if let Some(i) = run.windows(2).position(|w| w == b"/*" || w == b"--") {
            nchars = i;
        }
        // '+' and '-' cannot end a multi-character operator unless it contains a
        // character that is not an SQL operator.
        while nchars > 1 && matches!(run[nchars - 1], b'+' | b'-') {
            if run[..nchars - 1].iter().any(|c| b"~!@^&|`?%".contains(c)) {
                break;
            }
            nchars -= 1;
        }
        self.pos = start + nchars;
        if nchars < run.len() {
            if nchars == 1 && b",()[].;:+-*/%^<>=?".contains(&run[0]) {
                return Ok(self.token(u16::from(run[0]), SemValue::None, start));
            }
            if nchars == 2 {
                let code = match &run[..2] {
                    b"=>" => Some(token::EQUALS_GREATER),
                    b">=" => Some(token::GREATER_EQUALS),
                    b"<=" => Some(token::LESS_EQUALS),
                    b"<>" | b"!=" => Some(token::NOT_EQUALS),
                    _ => None,
                };
                if let Some(code) = code {
                    return Ok(self.token(code, SemValue::None, start));
                }
            }
        }
        if nchars >= 64 {
            return Err(self.yyerror(
                "operator too long: operators longer than 64 bytes are not supported",
                start,
                self.pos,
            ));
        }
        let op = self.text[start..self.pos].to_owned();
        Ok(self.token(token::Op, SemValue::Str(Some(op)), start))
    }

    /// `{integer}`: digits with single underscores between them; returns the end.
    fn integer_end(&self, mut i: usize) -> usize {
        while self.at(i).is_ascii_digit() || (self.at(i) == b'_' && self.at(i + 1).is_ascii_digit()) {
            i += 1;
        }
        i
    }

    /// `{integer}`, `{decimal}`, `{real}` and their `fail` variants.
    fn scan_number(&mut self, start: usize) -> Token<'a> {
        let int_end = self.integer_end(start);
        let mut end = int_end;
        let mut is_float = false;
        if self.at(end) == b'.' {
            if self.at(end + 1) == b'.' && end > start {
                // {decimalfail}: "1..": the integer, then DOT_DOT.
                self.pos = end;
                return self.integer_token(start);
            }
            end += 1;
            if self.at(end).is_ascii_digit() {
                end = self.integer_end(end);
            }
            is_float = true;
        }
        if matches!(self.at(end), b'e' | b'E') {
            let mut exp = end + 1;
            if matches!(self.at(exp), b'+' | b'-') {
                exp += 1;
            }
            if self.at(exp).is_ascii_digit() {
                // {real}
                end = self.integer_end(exp);
                is_float = true;
            } else {
                // {realfail1}/{realfail2}: give back the exponent; the result is a
                // float even if no '.' was seen.
                is_float = true;
            }
        }
        self.pos = end;
        if is_float {
            let text = self.text[start..end].to_owned();
            return self.token(token::FCONST, SemValue::Str(Some(text)), start);
        }
        self.integer_token(start)
    }

    /// `process_integer_literal`: an `ICONST`, or an `FCONST` if it overflows `int`.
    fn integer_token(&self, start: usize) -> Token<'a> {
        let digits = &self.text[start..self.pos];
        let cleaned: std::borrow::Cow<'_, str> = if digits.contains('_') {
            digits.replace('_', "").into()
        } else {
            digits.into()
        };
        match cleaned.parse::<i32>() {
            Ok(value) => self.token(token::ICONST, SemValue::Int(value), start),
            Err(_) => self.token(token::FCONST, SemValue::Str(Some(cleaned.into_owned())), start),
        }
    }

    /// `{param}` (`$1`) and `{param2}` (`?1`): `atol` of the digits after the sigil.
    fn scan_param(&mut self, start: usize) -> Token<'a> {
        self.pos = self.integer_end(start + 1);
        let mut value: i64 = 0;
        for &b in self.buf[start + 1..self.pos]
            .iter()
            .take_while(|b| b.is_ascii_digit())
        {
            value = value.saturating_mul(10).saturating_add(i64::from(b - b'0'));
        }
        // C stores the `long` from atol in an `int`.
        self.token(token::PARAM, SemValue::Int(value as i32), start)
    }

    /// `$`: a parameter, a dollar-quoted string, or a lone `$`.
    fn scan_dollar(&mut self, start: usize) -> Result<Token<'a>> {
        let next = self.peek(1);
        if next.is_ascii_digit() {
            return Ok(self.scan_param(start));
        }
        if next == b'$' {
            return self.scan_dollar_quoted(start, start + 2);
        }
        if next.is_ascii_alphabetic() || next == b'_' || next >= 0x80 {
            let mut i = start + 2;
            while self.at(i).is_ascii_alphanumeric() || self.at(i) == b'_' || self.at(i) >= 0x80 {
                i += 1;
            }
            if self.at(i) == b'$' {
                return self.scan_dollar_quoted(start, i + 1);
            }
            // {dolqfailed}: return just the '$'.
        }
        Ok(self.punct(u16::from(b'$'), start, 1))
    }

    /// The `xdolq` state: `$tag$ ... $tag$` with no escape processing.
    fn scan_dollar_quoted(&mut self, start: usize, body: usize) -> Result<Token<'a>> {
        let delim = &self.buf[start..body];
        let mut i = body;
        while i < self.buf.len() {
            if self.buf[i] == b'$' && self.buf[i..].starts_with(delim) {
                self.pos = i + delim.len();
                let text = self.text[body..i].to_owned();
                return Ok(self.token(token::SCONST, SemValue::Str(Some(text)), start));
            }
            i += 1;
        }
        Err(self.yyerror("unterminated dollar-quoted string", start, self.buf.len()))
    }

    /// Identifiers, keywords, and the `b'`, `x'`, `n'`, `e'`, `u&'`, `u&"` prefixes.
    fn scan_word(&mut self, start: usize) -> Result<Token<'a>> {
        let c = self.buf[start].to_ascii_lowercase();
        let next = self.peek(1);
        match (c, next) {
            (b'b', b'\'') => return self.scan_bit_string(start, b'b'),
            (b'x', b'\'') => return self.scan_bit_string(start, b'x'),
            (b'e', b'\'') => return self.scan_extended_string(start),
            (b'n', b'\'') => return Ok(self.scan_national(start)),
            (b'u', b'&') => match self.peek(2) {
                b'\'' => return self.scan_unicode_string(start),
                b'"' => return self.scan_unicode_identifier(start),
                // {xufailed}: the identifier "u", then '&' is scanned again.
                _ => {
                    self.pos = start + 1;
                    let ident = self.fold_case(start, start + 1);
                    return Ok(self.token(token::IDENT, SemValue::Str(Some(ident)), start));
                }
            },
            _ => {}
        }
        let mut end = start + 1;
        while is_ident_cont(self.at(end)) {
            end += 1;
        }
        self.pos = end;
        let word = &self.text[start..end];
        if let Some(keyword) = keywords::lookup(word) {
            return Ok(self.token(keyword.token, SemValue::Keyword(word), start));
        }
        let ident = self.fold_case(start, end);
        Ok(self.token(token::IDENT, SemValue::Str(Some(ident)), start))
    }

    /// `downcase_identifier`: ASCII case folding unless case is preserved.
    fn fold_case(&self, start: usize, end: usize) -> String {
        let mut ident = self.text[start..end].to_owned();
        if !self.options.preserve_identifier_case {
            ident.make_ascii_lowercase();
        }
        ident
    }

    /// `{xnstart}`: `N'...'` is the `NCHAR` keyword followed by a string.
    fn scan_national(&mut self, start: usize) -> Token<'a> {
        self.pos = start + 1;
        match keywords::lookup("nchar") {
            Some(keyword) => self.token(keyword.token, SemValue::Keyword(keyword.name), start),
            None => self.token(token::IDENT, SemValue::Str(Some("n".to_owned())), start),
        }
    }
}

#[cfg(test)]
mod tests;
