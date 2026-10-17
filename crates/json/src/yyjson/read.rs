//! The yyjson reader (`yyjson_read_opts`, `read_root_single`, `read_root_minify`,
//! `read_number`, `read_string`, `is_truncated_end`) with DuckDB's flags:
//! `YYJSON_READ_ALLOW_INF_AND_NAN | YYJSON_READ_ALLOW_TRAILING_COMMAS |
//! YYJSON_READ_BIGNUM_AS_RAW`, plus `YYJSON_READ_STOP_WHEN_DONE` on request.
//!
//! The input is parsed in place: strings are unescaped into the input bytes, which must be
//! followed by [`PADDING_SIZE`] zero bytes (yyjson relies on them to stop scans).

use super::{
    PADDING_SIZE, SUBTYPE_NOESC, SUBTYPE_REAL, SUBTYPE_SINT, SUBTYPE_TRUE, SUBTYPE_UINT, TAG_BIT, TYPE_ARR,
    TYPE_BOOL, TYPE_NULL, TYPE_NUM, TYPE_OBJ, TYPE_RAW, TYPE_STR, Val, ValId,
};

/// `yyjson_read_code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadErrorCode {
    InvalidParameter,
    MemoryAllocation,
    EmptyContent,
    UnexpectedContent,
    UnexpectedEnd,
    UnexpectedCharacter,
    JsonStructure,
    InvalidComment,
    InvalidNumber,
    InvalidString,
    Literal,
}

/// `yyjson_read_err`: what went wrong and the byte offset (from the start of the input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadError {
    pub code: ReadErrorCode,
    pub msg: &'static str,
    pub pos: usize,
}

/// A parsed document: the root slot and how many input bytes were consumed
/// (`yyjson_doc_get_read_size`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadResult {
    pub root: ValId,
    pub read_size: usize,
}

/// `char_table` classes.
const SPACE: u8 = 1 << 0;
const NUMBER: u8 = 1 << 1;
const ESC_ASCII: u8 = 1 << 2;
const NON_ASCII: u8 = 1 << 3;
const CONTAINER: u8 = 1 << 4;
const HEX: u8 = 1 << 7;

const fn char_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let mut class = 0;
        if c == b' ' as usize || c == b'\t' as usize || c == b'\n' as usize || c == b'\r' as usize {
            class |= SPACE;
        }
        if c == b'-' as usize || (c >= b'0' as usize && c <= b'9' as usize) {
            class |= NUMBER;
        }
        if c < 0x20 || c == b'"' as usize || c == b'\\' as usize {
            class |= ESC_ASCII;
        }
        if c >= 0x80 {
            class |= NON_ASCII;
        }
        if c == b'{' as usize || c == b'[' as usize {
            class |= CONTAINER;
        }
        if (c >= b'0' as usize && c <= b'9' as usize)
            || (c >= b'a' as usize && c <= b'f' as usize)
            || (c >= b'A' as usize && c <= b'F' as usize)
        {
            class |= HEX;
        }
        table[c] = class;
        c += 1;
    }
    table
}

static CHAR_TABLE: [u8; 256] = char_table();

#[inline]
fn char_is(c: u8, class: u8) -> bool {
    CHAR_TABLE[c as usize] & class != 0
}

#[inline]
fn is_space(c: u8) -> bool {
    char_is(c, SPACE)
}

#[inline]
fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

#[inline]
fn is_exp(c: u8) -> bool {
    matches!(c, b'e' | b'E')
}

#[inline]
fn is_sign(c: u8) -> bool {
    matches!(c, b'+' | b'-')
}

#[inline]
fn hex_value(c: u8) -> Option<u16> {
    match c {
        b'0'..=b'9' => Some(u16::from(c - b'0')),
        b'a'..=b'f' => Some(u16::from(c - b'a' + 10)),
        b'A'..=b'F' => Some(u16::from(c - b'A' + 10)),
        _ => None,
    }
}

/// The parse state over `buf`, whose document spans `hdr..end` (followed by padding).
struct Reader<'b> {
    buf: &'b mut [u8],
    hdr: usize,
    end: usize,
    stop_when_done: bool,
}

/// A failed value read: the message and the position (absolute) it refers to.
type ValueError = (&'static str, usize);

impl Reader<'_> {
    #[inline]
    fn at(&self, pos: usize) -> u8 {
        self.buf[pos]
    }

    /// `byte_match_4` / literal checks against the input (padding reads as `\0`).
    #[inline]
    fn matches(&self, pos: usize, literal: &[u8]) -> bool {
        self.buf[pos..pos + literal.len()] == *literal
    }

    /// Case-insensitive `inf` / `infinity` / `nan` at `pos`; returns the end (`read_inf`,
    /// `read_nan`).
    fn inf_or_nan_end(&self, pos: usize) -> Option<usize> {
        let lower = |i: usize| self.at(pos + i).to_ascii_lowercase();
        if lower(0) == b'i' && lower(1) == b'n' && lower(2) == b'f' {
            if lower(3) == b'i'
                && lower(4) == b'n'
                && lower(5) == b'i'
                && lower(6) == b't'
                && lower(7) == b'y'
            {
                return Some(pos + 8);
            }
            return Some(pos + 3);
        }
        if lower(0) == b'n' && lower(1) == b'a' && lower(2) == b'n' {
            return Some(pos + 3);
        }
        None
    }

    /// `read_nan` only (after a failed `null`).
    fn nan_end(&self, pos: usize) -> Option<usize> {
        let lower = |i: usize| self.at(pos + i).to_ascii_lowercase();
        (lower(0) == b'n' && lower(1) == b'a' && lower(2) == b'n').then_some(pos + 3)
    }

    /// A raw value covering `start..end` (bignums and inf/nan literals).
    fn raw(start: usize, end: usize) -> Val {
        Val {
            tag: (((end - start) as u64) << TAG_BIT) | TYPE_RAW,
            uni: start as u64,
        }
    }

    /// `read_number` with `YYJSON_READ_BIGNUM_AS_RAW` and `ALLOW_INF_AND_NAN`: numbers that
    /// do not fit a 64-bit integer or a finite double become raw values.
    fn read_number(&self, cur: &mut usize) -> Result<Val, ValueError> {
        let hdr = *cur;
        let sign = self.at(hdr) == b'-';
        let mut p = hdr + usize::from(sign);
        if !is_digit(self.at(p)) {
            if let Some(end) = self.inf_or_nan_end(p) {
                *cur = end;
                return Ok(Self::raw(hdr, end));
            }
            return Err(("no digit after minus sign", p));
        }
        let mut is_real = false;
        if self.at(p) == b'0' {
            p += 1;
            let c = self.at(p);
            if is_digit(c) {
                return Err(("number with leading zero is not allowed", p - 1));
            }
        } else {
            while is_digit(self.at(p)) {
                p += 1;
            }
        }
        if self.at(p) == b'.' {
            is_real = true;
            p += 1;
            if !is_digit(self.at(p)) {
                return Err(("no digit after decimal point", p));
            }
            while is_digit(self.at(p)) {
                p += 1;
            }
        }
        if is_exp(self.at(p)) {
            is_real = true;
            p += 1;
            if is_sign(self.at(p)) {
                p += 1;
            }
            if !is_digit(self.at(p)) {
                return Err(("no digit after exponent sign", p));
            }
            while is_digit(self.at(p)) {
                p += 1;
            }
        }
        *cur = p;
        let digits = &self.buf[hdr + usize::from(sign)..p];
        if !is_real {
            // At most 20 digits can fit a u64; leading zeros are not allowed.
            let magnitude = if digits.len() <= 20 {
                digits.iter().try_fold(0u64, |acc, &d| {
                    acc.checked_mul(10)?.checked_add(u64::from(d - b'0'))
                })
            } else {
                None
            };
            return Ok(match magnitude {
                Some(value) if !sign => Val {
                    tag: TYPE_NUM | SUBTYPE_UINT,
                    uni: value,
                },
                Some(value) if value <= 1 << 63 => Val {
                    tag: TYPE_NUM | SUBTYPE_SINT,
                    uni: value.wrapping_neg(),
                },
                _ => Self::raw(hdr, p),
            });
        }
        let text = std::str::from_utf8(&self.buf[hdr..p]).unwrap_or("0");
        let value: f64 = text.parse().unwrap_or(0.0);
        if value.is_infinite() {
            return Ok(Self::raw(hdr, p));
        }
        Ok(Val {
            tag: TYPE_NUM | SUBTYPE_REAL,
            uni: value.to_bits(),
        })
    }

    /// The length of the valid UTF-8 sequence starting at `pos` (yyjson's
    /// `is_valid_seq_*` patterns), or 0.
    #[inline]
    fn utf8_seq_len(&self, pos: usize) -> usize {
        let b0 = self.at(pos);
        if b0 < 0x80 {
            return 1;
        }
        let b1 = self.at(pos + 1);
        if b0 & 0xE0 == 0xC0 {
            return if b1 & 0xC0 == 0x80 && b0 & 0x1E != 0 { 2 } else { 0 };
        }
        let b2 = self.at(pos + 2);
        if b0 & 0xF0 == 0xE0 {
            let ok = b1 & 0xC0 == 0x80
                && b2 & 0xC0 == 0x80
                && (b0 & 0x0F != 0 || b1 & 0x20 != 0)
                && !(b0 & 0x0F == 0x0D && b1 & 0x20 != 0);
            return if ok { 3 } else { 0 };
        }
        let b3 = self.at(pos + 3);
        if b0 & 0xF8 == 0xF0 {
            let requ0 = b0 & 0x07;
            let requ1 = b1 & 0x30;
            let ok = b1 & 0xC0 == 0x80
                && b2 & 0xC0 == 0x80
                && b3 & 0xC0 == 0x80
                && (requ0 != 0 || requ1 != 0)
                && (requ0 & 0x04 == 0 || (requ0.trailing_zeros() >= 2 && requ1 == 0));
            return if ok { 4 } else { 0 };
        }
        0
    }

    /// `read_string`: `*cur` is at the opening quote. Unescapes in place.
    fn read_string(&mut self, cur: &mut usize) -> Result<Val, ValueError> {
        let start = *cur + 1;
        let mut src = start;
        // Skip characters that need no copying.
        loop {
            let c = self.at(src);
            if !char_is(c, ESC_ASCII | NON_ASCII) {
                src += 1;
                continue;
            }
            if c == b'"' {
                *cur = src + 1;
                return Ok(Val {
                    tag: (((src - start) as u64) << TAG_BIT) | TYPE_STR | SUBTYPE_NOESC,
                    uni: start as u64,
                });
            }
            if c >= 0x80 {
                let len = self.utf8_seq_len(src);
                if len == 0 {
                    return Err(("invalid UTF-8 encoding in string", src));
                }
                src += len;
                continue;
            }
            break;
        }
        // An escape or control character: copy from here on.
        let mut dst = src;
        loop {
            let c = self.at(src);
            if c == b'\\' {
                src += 1;
                let escaped = self.at(src);
                let single = match escaped {
                    b'"' => Some(b'"'),
                    b'\\' => Some(b'\\'),
                    b'/' => Some(b'/'),
                    b'b' => Some(0x08),
                    b'f' => Some(0x0C),
                    b'n' => Some(b'\n'),
                    b'r' => Some(b'\r'),
                    b't' => Some(b'\t'),
                    b'u' => None,
                    _ => return Err(("invalid escaped character in string", src)),
                };
                if let Some(byte) = single {
                    self.buf[dst] = byte;
                    dst += 1;
                    src += 1;
                    continue;
                }
                src += 1;
                let Some(hi) = self.read_hex_u16(src) else {
                    return Err(("invalid escaped sequence in string", src - 2));
                };
                src += 4;
                if hi & 0xF800 != 0xD800 {
                    dst = self.put_code_point(dst, u32::from(hi));
                    continue;
                }
                if hi & 0xFC00 != 0xD800 {
                    return Err(("invalid high surrogate in string", src - 6));
                }
                if !(self.at(src) == b'\\' && self.at(src + 1) == b'u') {
                    return Err(("no low surrogate in string", src));
                }
                let Some(lo) = self.read_hex_u16(src + 2) else {
                    return Err(("invalid escaped sequence in string", src));
                };
                if lo & 0xFC00 != 0xDC00 {
                    return Err(("invalid low surrogate in string", src));
                }
                let code = (((u32::from(hi) - 0xD800) << 10) | (u32::from(lo) - 0xDC00)) + 0x10000;
                dst = self.put_code_point(dst, code);
                src += 6;
            } else if c == b'"' {
                *cur = src + 1;
                return Ok(Val {
                    tag: (((dst - start) as u64) << TAG_BIT) | TYPE_STR,
                    uni: start as u64,
                });
            } else if c < 0x20 {
                return Err(("unexpected control character in string", src));
            } else if c >= 0x80 {
                let len = self.utf8_seq_len(src);
                if len == 0 {
                    return Err(("invalid UTF-8 encoding in string", src));
                }
                self.buf.copy_within(src..src + len, dst);
                src += len;
                dst += len;
            } else {
                self.buf[dst] = c;
                src += 1;
                dst += 1;
            }
        }
    }

    /// `read_hex_u16`: four hex digits at `pos`.
    fn read_hex_u16(&self, pos: usize) -> Option<u16> {
        let mut value = 0u16;
        for i in 0..4 {
            value = (value << 4) | hex_value(self.at(pos + i))?;
        }
        Some(value)
    }

    /// Writes the UTF-8 encoding of `code` at `dst`; returns the new end.
    fn put_code_point(&mut self, dst: usize, code: u32) -> usize {
        let mut out = [0u8; 4];
        let len = if code >= 0x10000 {
            out[0] = (0xF0 | (code >> 18)) as u8;
            out[1] = (0x80 | ((code >> 12) & 0x3F)) as u8;
            out[2] = (0x80 | ((code >> 6) & 0x3F)) as u8;
            out[3] = (0x80 | (code & 0x3F)) as u8;
            4
        } else if code >= 0x800 {
            out[0] = (0xE0 | (code >> 12)) as u8;
            out[1] = (0x80 | ((code >> 6) & 0x3F)) as u8;
            out[2] = (0x80 | (code & 0x3F)) as u8;
            3
        } else if code >= 0x80 {
            out[0] = (0xC0 | (code >> 6)) as u8;
            out[1] = (0x80 | (code & 0x3F)) as u8;
            2
        } else {
            out[0] = code as u8;
            1
        };
        self.buf[dst..dst + len].copy_from_slice(&out[..len]);
        dst + len
    }

    /// Reads a non-container value at `*cur`. `Ok(None)` means no value starts here.
    fn read_scalar(
        &mut self,
        cur: &mut usize,
        root: bool,
    ) -> Result<Option<Val>, (ReadErrorCode, ValueError)> {
        let c = self.at(*cur);
        if char_is(c, NUMBER) {
            return self
                .read_number(cur)
                .map(Some)
                .map_err(|err| (ReadErrorCode::InvalidNumber, err));
        }
        if c == b'"' {
            return self
                .read_string(cur)
                .map(Some)
                .map_err(|err| (ReadErrorCode::InvalidString, err));
        }
        let literal = |len: usize, tag: u64| Val { tag, uni: len as u64 };
        if c == b't' {
            if self.matches(*cur, b"true") {
                *cur += 4;
                return Ok(Some(literal(0, TYPE_BOOL | SUBTYPE_TRUE)));
            }
            return Err((ReadErrorCode::Literal, ("invalid literal", *cur)));
        }
        if c == b'f' {
            if self.matches(*cur + 1, b"alse") {
                *cur += 5;
                return Ok(Some(literal(0, TYPE_BOOL)));
            }
            return Err((ReadErrorCode::Literal, ("invalid literal", *cur)));
        }
        if c == b'n' {
            if self.matches(*cur, b"null") {
                *cur += 4;
                return Ok(Some(literal(0, TYPE_NULL)));
            }
            if let Some(end) = self.nan_end(*cur) {
                let val = Self::raw(*cur, end);
                *cur = end;
                return Ok(Some(val));
            }
            return Err((ReadErrorCode::Literal, ("invalid literal", *cur)));
        }
        if root || matches!(c, b'i' | b'I' | b'N') {
            if let Some(end) = self.inf_or_nan_end(*cur) {
                let val = Self::raw(*cur, end);
                *cur = end;
                return Ok(Some(val));
            }
            return Err((ReadErrorCode::UnexpectedCharacter, ("unexpected character", *cur)));
        }
        Ok(None)
    }

    /// Builds the error (`return_err` of the root readers, with `is_truncated_end`).
    fn error(&self, code: ReadErrorCode, msg: &'static str, pos: usize) -> ReadError {
        if self.is_truncated_end(pos, code) {
            return ReadError {
                code: ReadErrorCode::UnexpectedEnd,
                msg: "unexpected end of data",
                pos: self.end - self.hdr,
            };
        }
        ReadError {
            code,
            msg,
            pos: pos - self.hdr,
        }
    }

    /// `is_truncated_str`.
    fn is_truncated_str(&self, cur: usize, literal: &[u8], case_sensitive: bool) -> bool {
        let len = literal.len();
        if cur + len <= self.end || self.end <= cur {
            return false;
        }
        self.buf[cur..self.end].iter().zip(literal).all(|(&c, &l)| {
            if case_sensitive {
                c == l
            } else {
                c == l || c == l.wrapping_sub(b'a').wrapping_add(b'A')
            }
        })
    }

    /// `is_truncated_end`: whether the input is valid but cut short.
    fn is_truncated_end(&self, pos: usize, code: ReadErrorCode) -> bool {
        let end = self.end;
        if pos >= end {
            return true;
        }
        if code == ReadErrorCode::Literal
            && (self.is_truncated_str(pos, b"true", true)
                || self.is_truncated_str(pos, b"false", true)
                || self.is_truncated_str(pos, b"null", true))
        {
            return true;
        }
        if matches!(
            code,
            ReadErrorCode::UnexpectedCharacter | ReadErrorCode::InvalidNumber | ReadErrorCode::Literal
        ) {
            let p = if self.at(pos) == b'-' { pos + 1 } else { pos };
            if self.is_truncated_str(p, b"infinity", false) || self.is_truncated_str(p, b"nan", false) {
                return true;
            }
        }
        if code == ReadErrorCode::UnexpectedContent
            && self.hdr + 3 <= pos
            && self.is_truncated_str(pos - 3, b"infinity", false)
        {
            return true;
        }
        if code == ReadErrorCode::InvalidString {
            let len = end - pos;
            let c0 = self.at(pos);
            if c0 == b'\\' {
                if len == 1 {
                    return true;
                }
                if len <= 5 {
                    if self.at(pos + 1) != b'u' {
                        return false;
                    }
                    return self.buf[pos + 2..end].iter().all(|&c| char_is(c, HEX));
                }
                return false;
            }
            if c0 & 0x80 != 0 {
                let (c1, c2) = (self.at(pos + 1), self.at(pos + 2));
                if len == 1 {
                    if c0 & 0xE0 == 0xC0 && c0 & 0x1E != 0 {
                        return true;
                    }
                    if c0 & 0xF0 == 0xE0 {
                        return true;
                    }
                    if c0 & 0xF8 == 0xF0 && c0 & 0x07 <= 0x04 {
                        return true;
                    }
                }
                if len == 2 {
                    if c0 & 0xF0 == 0xE0 && c1 & 0xC0 == 0x80 {
                        let pat = ((c0 & 0x0F) << 1) | ((c1 & 0x20) >> 5);
                        return (0x01..=0x1A).contains(&pat) || (0x1C..=0x1F).contains(&pat);
                    }
                    if c0 & 0xF8 == 0xF0 && c1 & 0xC0 == 0x80 {
                        let pat = ((c0 & 0x07) << 2) | ((c1 & 0x30) >> 4);
                        return (0x01..=0x10).contains(&pat);
                    }
                }
                if len == 3 && c0 & 0xF8 == 0xF0 && c1 & 0xC0 == 0x80 && c2 & 0xC0 == 0x80 {
                    let pat = ((c0 & 0x07) << 2) | ((c1 & 0x30) >> 4);
                    return (0x01..=0x10).contains(&pat);
                }
            }
        }
        false
    }

    /// Skips JSON whitespace from `cur`.
    #[inline]
    fn skip_space(&self, mut cur: usize) -> usize {
        while is_space(self.at(cur)) {
            cur += 1;
        }
        cur
    }

    /// `doc_end`: checks for content after the document.
    fn finish(&self, cur: usize, root: ValId) -> Result<ReadResult, ReadError> {
        let mut cur = cur;
        if cur < self.end && !self.stop_when_done {
            cur = self.skip_space(cur);
            if cur < self.end {
                return Err(self.error(
                    ReadErrorCode::UnexpectedContent,
                    "unexpected content after document",
                    cur,
                ));
            }
        }
        Ok(ReadResult {
            root,
            read_size: cur - self.hdr,
        })
    }

    /// `read_root_single`.
    fn read_single(&mut self, cur: usize, vals: &mut Vec<Val>) -> Result<ReadResult, ReadError> {
        let mut cur = cur;
        let root = vals.len();
        match self.read_scalar(&mut cur, true) {
            Ok(Some(val)) => vals.push(val),
            Ok(None) => {
                return Err(self.error(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur));
            }
            Err((code, (msg, pos))) => return Err(self.error(code, msg, pos)),
        }
        self.finish(cur, root)
    }

    /// `read_root_minify` (and `read_root_pretty`, which accepts the same input).
    fn read_container(&mut self, cur: usize, vals: &mut Vec<Val>) -> Result<ReadResult, ReadError> {
        let root = vals.len();
        let mut cur = cur;
        // (container slot, element count: values for arrays, keys + values for objects)
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let open = |vals: &mut Vec<Val>, is_obj: bool| {
            vals.push(Val {
                tag: if is_obj { TYPE_OBJ } else { TYPE_ARR },
                uni: 0,
            });
            vals.len() - 1
        };
        let first = self.at(cur);
        cur += 1;
        let mut state = if first == b'{' {
            stack.push((open(vals, true), 0));
            State::ObjKeyBegin
        } else {
            stack.push((open(vals, false), 0));
            State::ArrValBegin
        };
        macro_rules! fail {
            ($code:expr, $msg:expr, $pos:expr) => {
                return Err(self.error($code, $msg, $pos))
            };
        }
        loop {
            match state {
                State::ArrValBegin => {
                    let c = self.at(cur);
                    if c == b'{' {
                        cur += 1;
                        stack.push((open(vals, true), 0));
                        state = State::ObjKeyBegin;
                        continue;
                    }
                    if c == b'[' {
                        cur += 1;
                        stack.push((open(vals, false), 0));
                        continue;
                    }
                    if c == b']' {
                        // An empty array, or a trailing comma (allowed).
                        cur += 1;
                        state = Self::close(&mut stack, vals);
                        if stack.is_empty() {
                            return self.finish(cur, root);
                        }
                        continue;
                    }
                    if is_space(c) {
                        cur = self.skip_space(cur + 1);
                        continue;
                    }
                    match self.read_scalar(&mut cur, false) {
                        Ok(Some(val)) => {
                            vals.push(val);
                            if let Some(top) = stack.last_mut() {
                                top.1 += 1;
                            }
                            state = State::ArrValEnd;
                        }
                        Ok(None) => fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur),
                        Err((code, (msg, pos))) => fail!(code, msg, pos),
                    }
                }
                State::ArrValEnd => {
                    let c = self.at(cur);
                    if c == b',' {
                        cur += 1;
                        state = State::ArrValBegin;
                    } else if c == b']' {
                        cur += 1;
                        state = Self::close(&mut stack, vals);
                        if stack.is_empty() {
                            return self.finish(cur, root);
                        }
                    } else if is_space(c) {
                        cur = self.skip_space(cur + 1);
                    } else {
                        fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur);
                    }
                }
                State::ObjKeyBegin => {
                    let c = self.at(cur);
                    if c == b'"' {
                        match self.read_string(&mut cur) {
                            Ok(val) => {
                                vals.push(val);
                                if let Some(top) = stack.last_mut() {
                                    top.1 += 1;
                                }
                                state = State::ObjKeyEnd;
                            }
                            Err((msg, pos)) => fail!(ReadErrorCode::InvalidString, msg, pos),
                        }
                    } else if c == b'}' {
                        cur += 1;
                        state = Self::close(&mut stack, vals);
                        if stack.is_empty() {
                            return self.finish(cur, root);
                        }
                    } else if is_space(c) {
                        cur = self.skip_space(cur + 1);
                    } else {
                        fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur);
                    }
                }
                State::ObjKeyEnd => {
                    let c = self.at(cur);
                    if c == b':' {
                        cur += 1;
                        state = State::ObjValBegin;
                    } else if is_space(c) {
                        cur = self.skip_space(cur + 1);
                    } else {
                        fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur);
                    }
                }
                State::ObjValBegin => {
                    let c = self.at(cur);
                    if c == b'{' {
                        cur += 1;
                        stack.push((open(vals, true), 0));
                        state = State::ObjKeyBegin;
                        continue;
                    }
                    if c == b'[' {
                        cur += 1;
                        stack.push((open(vals, false), 0));
                        state = State::ArrValBegin;
                        continue;
                    }
                    if is_space(c) {
                        cur = self.skip_space(cur + 1);
                        continue;
                    }
                    match self.read_scalar(&mut cur, false) {
                        Ok(Some(val)) => {
                            vals.push(val);
                            if let Some(top) = stack.last_mut() {
                                top.1 += 1;
                            }
                            state = State::ObjValEnd;
                        }
                        Ok(None) => fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur),
                        Err((code, (msg, pos))) => fail!(code, msg, pos),
                    }
                }
                State::ObjValEnd => {
                    let c = self.at(cur);
                    if c == b',' {
                        cur += 1;
                        state = State::ObjKeyBegin;
                    } else if c == b'}' {
                        cur += 1;
                        state = Self::close(&mut stack, vals);
                        if stack.is_empty() {
                            return self.finish(cur, root);
                        }
                    } else if is_space(c) {
                        cur = self.skip_space(cur + 1);
                    } else {
                        fail!(ReadErrorCode::UnexpectedCharacter, "unexpected character", cur);
                    }
                }
            }
        }
    }

    /// Closes the innermost container (`arr_end` / `obj_end`): records its length and
    /// subtree size; the parent (if any) continues after a value.
    fn close(stack: &mut Vec<(usize, usize)>, vals: &mut [Val]) -> State {
        let Some((slot, count)) = stack.pop() else {
            return State::ArrValEnd;
        };
        let is_obj = vals[slot].tag == TYPE_OBJ;
        let len = if is_obj { count / 2 } else { count };
        vals[slot].tag = ((len as u64) << TAG_BIT) | if is_obj { TYPE_OBJ } else { TYPE_ARR };
        vals[slot].uni = (vals.len() - slot) as u64;
        let Some(top) = stack.last_mut() else {
            return State::ArrValEnd;
        };
        top.1 += 1;
        if vals[top.0].tag == TYPE_OBJ {
            State::ObjValEnd
        } else {
            State::ArrValEnd
        }
    }
}

/// Where the container state machine continues (the goto labels of yyjson).
#[derive(Clone, Copy, Debug)]
enum State {
    ArrValBegin,
    ArrValEnd,
    ObjKeyBegin,
    ObjKeyEnd,
    ObjValBegin,
    ObjValEnd,
}

/// Parses `buf[start..end]` in place, appending the document's values to `vals`
/// (`yyjson_read_opts` with `YYJSON_READ_INSITU`). `buf` must hold [`PADDING_SIZE`] zero
/// bytes after `end`. Error positions and [`ReadResult::read_size`] are relative to `start`.
pub fn read_insitu(
    buf: &mut [u8],
    start: usize,
    end: usize,
    stop_when_done: bool,
    vals: &mut Vec<Val>,
) -> Result<ReadResult, ReadError> {
    if start == end {
        return Err(ReadError {
            code: ReadErrorCode::InvalidParameter,
            msg: "input length is 0",
            pos: 0,
        });
    }
    if buf.len() < end + PADDING_SIZE {
        return Err(ReadError {
            code: ReadErrorCode::MemoryAllocation,
            msg: "memory allocation failed",
            pos: 0,
        });
    }
    let mut reader = Reader {
        buf,
        hdr: start,
        end,
        stop_when_done,
    };
    let mut cur = start;
    if is_space(reader.at(cur)) {
        cur = reader.skip_space(cur + 1);
        if cur >= end {
            return Err(ReadError {
                code: ReadErrorCode::EmptyContent,
                msg: "input data is empty",
                pos: 0,
            });
        }
    }
    let mark = vals.len();
    let result = if char_is(reader.at(cur), CONTAINER) {
        reader.read_container(cur, vals)
    } else {
        reader.read_single(cur, vals)
    };
    match result {
        Ok(result) => Ok(result),
        Err(mut err) => {
            vals.truncate(mark);
            if err.pos == 0 && err.code != ReadErrorCode::MemoryAllocation {
                let b = |i: usize| reader.at(start + i);
                let len = end - start;
                if b(0) == 0xEF && b(1) == 0xBB && b(2) == 0xBF {
                    err.msg = "byte order mark (BOM) is not supported";
                } else if len >= 4
                    && ((b(0) == 0x00 && b(1) == 0x00 && b(2) == 0xFE && b(3) == 0xFF)
                        || (b(0) == 0xFF && b(1) == 0xFE && b(2) == 0x00 && b(3) == 0x00))
                {
                    err.msg = "UTF-32 encoding is not supported";
                } else if len >= 2 && ((b(0) == 0xFE && b(1) == 0xFF) || (b(0) == 0xFF && b(1) == 0xFE)) {
                    err.msg = "UTF-16 encoding is not supported";
                }
            }
            Err(err)
        }
    }
}

/// Owns the bytes and value slots of the documents parsed by one function call (DuckDB's
/// `JSONAllocator` arena, reset per chunk).
#[derive(Debug, Default)]
pub struct JsonArena {
    pub vals: Vec<Val>,
    pub bytes: Vec<u8>,
}

impl JsonArena {
    /// Forgets every document.
    pub fn reset(&mut self) {
        self.vals.clear();
        self.bytes.clear();
    }

    /// Parses a copy of `input` (`yyjson_read_opts` without `INSITU`).
    pub fn read(&mut self, input: &[u8], stop_when_done: bool) -> Result<ReadResult, ReadError> {
        let start = self.bytes.len();
        self.bytes.extend_from_slice(input);
        self.bytes.extend_from_slice(&[0; PADDING_SIZE]);
        let end = start + input.len();
        read_insitu(&mut self.bytes, start, end, stop_when_done, &mut self.vals)
    }

    /// The documents parsed so far.
    pub fn doc(&self) -> super::Doc<'_> {
        super::Doc::new(&self.vals, &self.bytes)
    }
}
