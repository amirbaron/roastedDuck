//! JSON pointers (RFC 6901): `unsafe_yyjson_ptr_getx`.

use super::{Doc, ValId, ValType};

/// One token of a pointer: its text (with `~0` / `~1` escapes still in place) and the
/// number of escapes (`ptr_next_token`).
struct Token<'p> {
    text: &'p [u8],
    escapes: usize,
}

impl Token<'_> {
    /// The unescaped length.
    fn len(&self) -> usize {
        self.text.len() - self.escapes
    }

    /// `ptr_token_eq`.
    fn equals(&self, key: &[u8]) -> bool {
        if key.len() != self.len() {
            return false;
        }
        if self.escapes == 0 {
            return key == self.text;
        }
        let mut token = self.text.iter();
        for &k in key {
            let expected = match token.next() {
                Some(b'~') => match token.next() {
                    Some(b'0') => b'~',
                    _ => b'/',
                },
                Some(&c) => c,
                None => return false,
            };
            if k != expected {
                return false;
            }
        }
        true
    }

    /// `ptr_token_to_idx`: an array index (`-` is the position after the end).
    fn index(&self) -> Option<usize> {
        let text = self.text;
        if text.is_empty() || text.len() > 19 || self.escapes != 0 {
            return None;
        }
        if text[0] == b'0' {
            return (text.len() == 1).then_some(0);
        }
        if text[0] == b'-' {
            return (text.len() == 1).then_some(usize::MAX);
        }
        let mut num = 0usize;
        for &c in text {
            let digit = c.wrapping_sub(b'0');
            if digit > 9 {
                return None;
            }
            num = num * 10 + usize::from(digit);
        }
        (num != 0).then_some(num)
    }
}

/// Splits off the token after the `/` at the start of `ptr` (`ptr_next_token`); `None` on
/// a `~` not followed by `0` or `1`.
fn next_token<'p>(ptr: &mut &'p [u8]) -> Option<Token<'p>> {
    let body = &ptr[1..];
    let len = body.iter().position(|&c| c == b'/').unwrap_or(body.len());
    let text = &body[..len];
    let mut escapes = 0;
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'~' {
            if !matches!(text.get(i + 1), Some(b'0' | b'1')) {
                return None;
            }
            escapes += 1;
            i += 1;
        }
        i += 1;
    }
    *ptr = &body[len..];
    Some(Token { text, escapes })
}

/// The value at JSON pointer `ptr` (which starts with `/`) below `val`.
pub fn ptr_get(doc: &Doc<'_>, val: ValId, ptr: &[u8]) -> Option<ValId> {
    let mut rest = ptr;
    let mut val = val;
    loop {
        let token = next_token(&mut rest)?;
        val = match doc.get_type(val) {
            ValType::Obj => doc
                .obj_iter(val)
                .find(|&(key, _)| token.equals(doc.str(key)))
                .map(|(_, value)| value),
            ValType::Arr => {
                let index = token.index()?;
                doc.arr_get(val, index)
            }
            _ => None,
        }?;
        if rest.is_empty() {
            return Some(val);
        }
    }
}
