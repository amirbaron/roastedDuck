//! Quoted literals: the `xq`, `xe`, `xb`, `xh`, `xd`, `xus`/`xusend` and
//! `xui`/`xuiend` states of `scan.l`.
//!
//! DuckDB runs the scanner with `standard_conforming_strings` on, so `'...'` takes no
//! backslash escapes. Several escape paths end in libpg_query helpers that DuckDB
//! stubs out (`unicode_to_utf8`, `pg_verifymbstr`); those raise the same
//! `"... NOT IMPLEMENTED"` errors here.

use rustdb_common::Result;

use super::{Scanner, Token, is_newline, is_space};
use crate::error::unimplemented;
use crate::semantic::SemValue;
use crate::tables::token;

impl<'a> Scanner<'a> {
    /// After a closing quote at `quote`, `{quotecontinue}`: whitespace containing a
    /// newline (comments allowed) and another quote. Returns the position after
    /// that quote if the literal continues.
    fn quote_continues(&self, quote: usize) -> Option<usize> {
        let mut i = quote + 1;
        // {horiz_whitespace}*
        loop {
            match self.at(i) {
                b' ' | b'\t' | 0x0c => i += 1,
                b'-' if self.at(i + 1) == b'-' => {
                    while i < self.buf.len() && !is_newline(self.buf[i]) {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        // {newline}
        if !is_newline(self.at(i)) {
            return None;
        }
        i += 1;
        // {special_whitespace}*: spaces, or comments followed by a newline
        loop {
            match self.at(i) {
                c if is_space(c) => i += 1,
                b'-' if self.at(i + 1) == b'-' => {
                    let mut j = i;
                    while j < self.buf.len() && !is_newline(self.buf[j]) {
                        j += 1;
                    }
                    if !is_newline(self.at(j)) {
                        return None;
                    }
                    i = j + 1;
                }
                _ => break,
            }
        }
        (self.at(i) == b'\'').then_some(i + 1)
    }

    /// Takes the literal buffer as a string.
    fn take_literal(&mut self) -> Result<String> {
        String::from_utf8(std::mem::take(&mut self.lit))
            .map_err(|_| crate::error::internal("string literal is not valid UTF-8"))
    }

    /// `{xqstart}`: a standard string `'...'`; `body` is just past the opening quote.
    pub(super) fn scan_quoted_string(&mut self, start: usize, body: usize) -> Result<Token<'a>> {
        self.lit.clear();
        let mut i = body;
        loop {
            let chunk_start = i;
            while i < self.buf.len() && self.buf[i] != b'\'' {
                i += 1;
            }
            self.lit.extend_from_slice(&self.buf[chunk_start..i]);
            if i >= self.buf.len() {
                return Err(self.yyerror("unterminated quoted string", start, self.buf.len()));
            }
            if self.at(i + 1) == b'\'' {
                self.lit.push(b'\'');
                i += 2;
            } else if let Some(next) = self.quote_continues(i) {
                i = next;
            } else {
                self.pos = i + 1;
                let text = self.take_literal()?;
                return Ok(self.token(token::SCONST, SemValue::Str(Some(text)), start));
            }
        }
    }

    /// `{xbstart}` / `{xhstart}`: `B'...'` and `X'...'`. The value keeps a leading
    /// `b`/`x` marker; quotes cannot be doubled inside these literals.
    pub(super) fn scan_bit_string(&mut self, start: usize, marker: u8) -> Result<Token<'a>> {
        self.lit.clear();
        self.lit.push(marker);
        let mut i = start + 2;
        loop {
            let chunk_start = i;
            while i < self.buf.len() && self.buf[i] != b'\'' {
                i += 1;
            }
            self.lit.extend_from_slice(&self.buf[chunk_start..i]);
            if i >= self.buf.len() {
                let message = if marker == b'b' {
                    "unterminated bit string literal"
                } else {
                    "unterminated hexadecimal string literal"
                };
                return Err(self.yyerror(message, start, self.buf.len()));
            }
            match self.quote_continues(i) {
                Some(next) => i = next,
                None => {
                    self.pos = i + 1;
                    let text = self.take_literal()?;
                    let code = if marker == b'b' {
                        token::BCONST
                    } else {
                        token::XCONST
                    };
                    return Ok(self.token(code, SemValue::Str(Some(text)), start));
                }
            }
        }
    }

    /// `{xestart}`: `E'...'` with backslash escapes (the `xe` and `xeu` states).
    pub(super) fn scan_extended_string(&mut self, start: usize) -> Result<Token<'a>> {
        self.lit.clear();
        let mut saw_non_ascii = false;
        let mut i = start + 2;
        loop {
            if i >= self.buf.len() {
                return Err(self.yyerror("unterminated quoted string", start, self.buf.len()));
            }
            match self.buf[i] {
                b'\'' => {
                    if self.at(i + 1) == b'\'' {
                        self.lit.push(b'\'');
                        i += 2;
                    } else if let Some(next) = self.quote_continues(i) {
                        i = next;
                    } else {
                        self.pos = i + 1;
                        if saw_non_ascii {
                            return Err(unimplemented("pg_verifymbstr"));
                        }
                        let text = self.take_literal()?;
                        return Ok(self.token(token::SCONST, SemValue::Str(Some(text)), start));
                    }
                }
                b'\\' if i + 1 >= self.buf.len() => {
                    // `<xe>.`: a backslash right before the end of input
                    self.lit.push(b'\\');
                    i += 1;
                }
                b'\\' => {
                    let escape = self.buf[i + 1];
                    match escape {
                        b'u' | b'U' => {
                            let want = if escape == b'u' { 4 } else { 8 };
                            let digits = self.buf[i + 2..]
                                .iter()
                                .take(want)
                                .take_while(|b| b.is_ascii_hexdigit())
                                .count();
                            if digits < want {
                                // {xeunicodefail}: errhint() is unimplemented in libpg_query
                                return Err(unimplemented("errhint"));
                            }
                            return Err(self.unicode_escape(start, i, want));
                        }
                        b'0'..=b'7' => {
                            let mut value: u32 = 0;
                            let mut j = i + 1;
                            while j < i + 4 && matches!(self.at(j), b'0'..=b'7') {
                                value = value * 8 + u32::from(self.buf[j] - b'0');
                                j += 1;
                            }
                            let byte = value as u8;
                            self.lit.push(byte);
                            saw_non_ascii |= byte == 0 || byte >= 0x80;
                            i = j;
                        }
                        b'x' if self.at(i + 2).is_ascii_hexdigit() => {
                            let mut value: u32 = 0;
                            let mut j = i + 2;
                            while j < i + 4 && self.at(j).is_ascii_hexdigit() {
                                value = value * 16 + hex_value(self.buf[j]);
                                j += 1;
                            }
                            let byte = value as u8;
                            self.lit.push(byte);
                            saw_non_ascii |= byte == 0 || byte >= 0x80;
                            i = j;
                        }
                        _ => {
                            // {xeescape}: the escaped byte (a newline, a quote, a
                            // backslash, or a letter with a C meaning).
                            let byte = match escape {
                                b'b' => 0x08,
                                b'f' => 0x0c,
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                other => other,
                            };
                            saw_non_ascii |= byte >= 0x80;
                            self.lit.push(byte);
                            i += 2;
                        }
                    }
                }
                _ => {
                    let chunk_start = i;
                    while i < self.buf.len() && !matches!(self.buf[i], b'\'' | b'\\') {
                        i += 1;
                    }
                    self.lit.extend_from_slice(&self.buf[chunk_start..i]);
                }
            }
        }
    }

    /// A well-formed `\uXXXX` / `\UXXXXXXXX` escape at `escape` inside an `E'...'`
    /// string that starts at `start`. Every outcome is an error in DuckDB: either an
    /// invalid code point, or the unimplemented `unicode_to_utf8`.
    fn unicode_escape(&mut self, start: usize, escape: usize, digits: usize) -> rustdb_common::Error {
        let end = escape + 2 + digits;
        let code = parse_hex(&self.buf[escape + 2..end]);
        if is_utf16_surrogate_second(code) {
            return self.yyerror("invalid Unicode surrogate pair", start, end);
        }
        if is_utf16_surrogate_first(code) {
            // The `xeu` state: a low surrogate escape must follow immediately.
            let next = end;
            let wanted = match (self.at(next), self.at(next + 1)) {
                (b'\\', b'u') => Some(4),
                (b'\\', b'U') => Some(8),
                _ => None,
            };
            let Some(want) = wanted else {
                // `<xeu>.`, `<xeu>\n` and `<xeu><<EOF>>`: anything else after a high
                // surrogate. Flex's `.` matches a single byte.
                let err_end = (next + 1).min(self.buf.len());
                return self.yyerror("invalid Unicode surrogate pair", start, err_end);
            };
            let count = self.buf[next + 2..]
                .iter()
                .take(want)
                .take_while(|b| b.is_ascii_hexdigit())
                .count();
            if count < want {
                return unimplemented("errhint");
            }
            let second_end = next + 2 + want;
            let second = parse_hex(&self.buf[next + 2..second_end]);
            if !is_utf16_surrogate_second(second) {
                return self.yyerror("invalid Unicode surrogate pair", start, second_end);
            }
            return unimplemented("unicode_to_utf8");
        }
        if code == 0 || code > 0x0010_FFFF {
            return self.yyerror("invalid Unicode escape value", start, end);
        }
        unimplemented("unicode_to_utf8")
    }

    /// `{xdstart}`: a quoted identifier `"..."`; case is always kept.
    pub(super) fn scan_quoted_identifier(&mut self, start: usize) -> Result<Token<'a>> {
        self.lit.clear();
        let mut i = start + 1;
        loop {
            let chunk_start = i;
            while i < self.buf.len() && self.buf[i] != b'"' {
                i += 1;
            }
            self.lit.extend_from_slice(&self.buf[chunk_start..i]);
            if i >= self.buf.len() {
                return Err(self.yyerror("unterminated quoted identifier", start, self.buf.len()));
            }
            if self.at(i + 1) == b'"' {
                self.lit.push(b'"');
                i += 2;
                continue;
            }
            self.pos = i + 1;
            if self.lit.is_empty() {
                return Err(self.yyerror("zero-length delimited identifier", start, self.pos));
            }
            let text = self.take_literal()?;
            return Ok(self.token(token::IDENT, SemValue::Str(Some(text)), start));
        }
    }

    /// `{xusstart}`: `U&'...'`. The body is collected like a standard string; the
    /// `xusend` state then looks for `UESCAPE 'c'` before decoding escapes.
    pub(super) fn scan_unicode_string(&mut self, start: usize) -> Result<Token<'a>> {
        self.lit.clear();
        let mut i = start + 3;
        loop {
            let chunk_start = i;
            while i < self.buf.len() && self.buf[i] != b'\'' {
                i += 1;
            }
            self.lit.extend_from_slice(&self.buf[chunk_start..i]);
            if i >= self.buf.len() {
                return Err(self.yyerror("unterminated quoted string", start, self.buf.len()));
            }
            if self.at(i + 1) == b'\'' {
                self.lit.push(b'\'');
                i += 2;
            } else if let Some(next) = self.quote_continues(i) {
                i = next;
            } else {
                return Err(self.unicode_literal_end(start, i + 1, false));
            }
        }
    }

    /// `{xuistart}`: `U&"..."`.
    pub(super) fn scan_unicode_identifier(&mut self, start: usize) -> Result<Token<'a>> {
        self.lit.clear();
        let mut i = start + 3;
        loop {
            let chunk_start = i;
            while i < self.buf.len() && self.buf[i] != b'"' {
                i += 1;
            }
            self.lit.extend_from_slice(&self.buf[chunk_start..i]);
            if i >= self.buf.len() {
                return Err(self.yyerror("unterminated quoted identifier", start, self.buf.len()));
            }
            if self.at(i + 1) == b'"' {
                self.lit.push(b'"');
                i += 2;
            } else {
                return Err(self.unicode_literal_end(start, i + 1, true));
            }
        }
    }

    /// The `xusend`/`xuiend` states after the closing quote at `after - 1`, then
    /// `litbuf_udeescape`. DuckDB's stubbed `pg_verifymbstr` (called on every
    /// result) and `unicode_to_utf8` make every outcome an error.
    fn unicode_literal_end(&mut self, start: usize, after: usize, identifier: bool) -> rustdb_common::Error {
        // skip {whitespace}* (spaces and -- comments)
        let mut i = after;
        loop {
            match self.at(i) {
                c if is_space(c) => i += 1,
                b'-' if self.at(i + 1) == b'-' => {
                    while i < self.buf.len() && !is_newline(self.buf[i]) {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        if identifier && self.lit.is_empty() {
            // the zero-length check runs before UESCAPE is examined
            let end = self.uescape_end(i).unwrap_or(i);
            return self.yyerror("zero-length delimited identifier", start, end);
        }
        let mut escape = b'\\';
        let mut flex_end = i;
        if let Some(end) = self.uescape_end(i) {
            escape = self.buf[end - 2];
            flex_end = end;
            if !is_valid_uescape(escape) {
                return self.yyerror("invalid Unicode escape character", end - 2, end);
            }
        }
        self.decode_unicode_escapes(start, escape, flex_end)
    }

    /// `{xustop2}`: `UESCAPE {whitespace}* 'c'` at `i`; returns the end of the match.
    fn uescape_end(&self, i: usize) -> Option<usize> {
        if !self.buf.get(i..i + 7)?.eq_ignore_ascii_case(b"uescape") {
            return None;
        }
        let mut j = i + 7;
        loop {
            match self.at(j) {
                c if is_space(c) => j += 1,
                b'-' if self.at(j + 1) == b'-' => {
                    while j < self.buf.len() && !is_newline(self.buf[j]) {
                        j += 1;
                    }
                }
                _ => break,
            }
        }
        (self.at(j) == b'\'' && j + 1 < self.buf.len() && self.at(j + 1) != b'\'' && self.at(j + 2) == b'\'')
            .then_some(j + 3)
    }

    /// `litbuf_udeescape`: validates the escapes; reports the first problem.
    fn decode_unicode_escapes(&self, start: usize, escape: u8, flex_end: usize) -> rustdb_common::Error {
        let lit = &self.lit;
        let mut pair_first = false;
        let mut i = 0;
        // errors are reported at the escape's offset in the literal plus 3 (for U&")
        let at = |offset: usize| start + offset + 3;
        while i < lit.len() {
            if lit[i] == escape {
                if lit.get(i + 1) == Some(&escape) {
                    if pair_first {
                        return self.yyerror("invalid Unicode surrogate pair", at(i), flex_end);
                    }
                    i += 2;
                    continue;
                }
                let hex = |from: usize, n: usize| {
                    lit.get(from..from + n)
                        .filter(|s| s.iter().all(u8::is_ascii_hexdigit))
                        .map(parse_hex)
                };
                let (code, len) = if let Some(code) = hex(i + 1, 4) {
                    (code, 5)
                } else if lit.get(i + 1) == Some(&b'+') {
                    match hex(i + 2, 6) {
                        Some(code) => (code, 8),
                        None => return self.yyerror("invalid Unicode escape value", at(i), flex_end),
                    }
                } else {
                    return self.yyerror("invalid Unicode escape value", at(i), flex_end);
                };
                if pair_first {
                    if !is_utf16_surrogate_second(code) {
                        return self.yyerror("invalid Unicode surrogate pair", at(i), flex_end);
                    }
                    pair_first = false;
                } else if is_utf16_surrogate_second(code) {
                    return self.yyerror("invalid Unicode surrogate pair", start, flex_end);
                } else if is_utf16_surrogate_first(code) {
                    pair_first = true;
                    i += len;
                    continue;
                }
                // the decoded character is written with unicode_to_utf8
                return unimplemented("unicode_to_utf8");
            }
            if pair_first {
                return self.yyerror("invalid Unicode surrogate pair", at(i), flex_end);
            }
            i += 1;
        }
        if pair_first {
            return self.yyerror("invalid Unicode surrogate pair", at(i), flex_end);
        }
        unimplemented("pg_verifymbstr")
    }
}

fn hex_value(b: u8) -> u32 {
    char::from(b).to_digit(16).unwrap_or(0)
}

fn parse_hex(digits: &[u8]) -> u32 {
    digits
        .iter()
        .fold(0u32, |acc, &b| acc.wrapping_mul(16).wrapping_add(hex_value(b)))
}

const fn is_utf16_surrogate_first(c: u32) -> bool {
    c >= 0xD800 && c <= 0xDBFF
}

const fn is_utf16_surrogate_second(c: u32) -> bool {
    c >= 0xDC00 && c <= 0xDFFF
}

/// `check_uescapechar`.
fn is_valid_uescape(c: u8) -> bool {
    !(c.is_ascii_hexdigit() || c == b'+' || c == b'\'' || c == b'"' || is_space(c))
}
