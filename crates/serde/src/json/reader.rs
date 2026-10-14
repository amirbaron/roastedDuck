//! A JSON parser with yyjson's value model and DuckDB's read flags
//! (`ALLOW_INF_AND_NAN`, `ALLOW_TRAILING_COMMAS`, `BIGNUM_AS_RAW`).

use rustdb_common::{Error, Result};

use super::JsonValue;

/// Maximum nesting depth accepted, to keep recursion bounded on hostile input.
const MAX_DEPTH: usize = 10_000;

/// Parses a complete JSON document.
pub fn parse_json(text: &str) -> Result<JsonValue> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("unexpected content after document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> Error {
        Error::invalid_input(format!("Malformed JSON at byte {}: {what}", self.pos))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat_literal(&mut self, literal: &str) -> bool {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue> {
        match self.peek() {
            Some(b'{') => self.nested(Self::parse_object),
            Some(b'[') => self.nested(Self::parse_array),
            Some(b'"') => self.parse_string().map(JsonValue::Str),
            Some(b't') if self.eat_literal("true") => Ok(JsonValue::Bool(true)),
            Some(b'f') if self.eat_literal("false") => Ok(JsonValue::Bool(false)),
            Some(b'n') if self.eat_literal("null") => Ok(JsonValue::Null),
            Some(b'N') if self.eat_literal("NaN") => Ok(JsonValue::Real(f64::NAN)),
            Some(b'I') if self.eat_literal("Infinity") => Ok(JsonValue::Real(f64::INFINITY)),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(self.error("unexpected character")),
        }
    }

    fn nested(&mut self, parse: fn(&mut Self) -> Result<JsonValue>) -> Result<JsonValue> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("document nested too deeply"));
        }
        let value = parse(self);
        self.depth -= 1;
        value
    }

    fn parse_array(&mut self) -> Result<JsonValue> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(JsonValue::Array(items));
            }
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {}
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue> {
        self.pos += 1;
        let mut entries = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(entries));
                }
                Some(b'"') => {}
                _ => return Err(self.error("expected object key")),
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            self.skip_whitespace();
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {}
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
            if self.eat_literal("Infinity") {
                return Ok(JsonValue::Real(f64::NEG_INFINITY));
            }
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(self.error("invalid number"));
        }
        let mut is_real = false;
        if self.peek() == Some(b'.') {
            is_real = true;
            self.pos += 1;
            let fraction_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == fraction_start {
                return Err(self.error("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_real = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == exponent_start {
                return Err(self.error("invalid number"));
            }
        }
        let literal = &self.text[start..self.pos];
        if is_real {
            return literal
                .parse::<f64>()
                .map(JsonValue::Real)
                .map_err(|_| self.error("invalid number"));
        }
        let parsed = if negative {
            literal.parse::<i64>().ok().map(JsonValue::SInt)
        } else {
            literal.parse::<u64>().ok().map(JsonValue::UInt)
        };
        Ok(parsed.unwrap_or_else(|| JsonValue::Raw(literal.to_owned())))
    }

    fn parse_string(&mut self) -> Result<String> {
        self.pos += 1;
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            match byte {
                b'"' => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    self.parse_escape(&mut out)?;
                    run_start = self.pos;
                }
                0..0x20 => return Err(self.error("unescaped control character in string")),
                _ => self.pos += 1,
            }
        }
    }

    fn parse_escape(&mut self, out: &mut String) -> Result<()> {
        let Some(byte) = self.peek() else {
            return Err(self.error("unterminated escape"));
        };
        self.pos += 1;
        let decoded = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let high = self.parse_hex4()?;
                let code = if (0xD800..0xDC00).contains(&high) {
                    if !self.eat_literal("\\u") {
                        return Err(self.error("unpaired surrogate"));
                    }
                    let low = self.parse_hex4()?;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(self.error("invalid surrogate pair"));
                    }
                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                } else {
                    high
                };
                char::from_u32(code).ok_or_else(|| self.error("invalid unicode escape"))?
            }
            _ => return Err(self.error("invalid escape")),
        };
        out.push(decoded);
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u32> {
        let hex = self
            .text
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("truncated unicode escape"))?;
        let code = u32::from_str_radix(hex, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.pos += 4;
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yyjson_number_classes() {
        let value = parse_json(
            r#"{"a":[1,-2,1.5,1e300,18446744073709551615,-9223372036854775809,NaN,-Infinity,],"b":"é\n"}"#,
        )
        .unwrap();
        let JsonValue::Object(entries) = value else {
            panic!("not an object")
        };
        let JsonValue::Array(items) = &entries[0].1 else {
            panic!("not an array")
        };
        assert_eq!(items[0], JsonValue::UInt(1));
        assert_eq!(items[1], JsonValue::SInt(-2));
        assert_eq!(items[2], JsonValue::Real(1.5));
        assert_eq!(items[4], JsonValue::UInt(u64::MAX));
        assert_eq!(items[5], JsonValue::Raw("-9223372036854775809".into()));
        assert!(matches!(items[6], JsonValue::Real(v) if v.is_nan()));
        assert_eq!(entries[1].1, JsonValue::Str("é\n".into()));
        assert!(parse_json("[1,").is_err());
        assert!(parse_json("{} x").is_err());
    }
}
