//! Owned byte strings for DuckDB's `std::string` fields, including identifiers.

use std::borrow::Borrow;
use std::ops::{Deref, DerefMut};

/// A byte-owning string without an implicit UTF-8 presentation.
///
/// Names and serialized strings can contain invalid UTF-8. VARCHAR construction
/// validates separately. There is deliberately no text dereference or Display
/// implementation: native formatting must write the original bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteString(Vec<u8>);

impl ByteString {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn join<S: AsRef<[u8]>>(parts: &[S], separator: impl AsRef<[u8]>) -> Self {
        let mut out = Self::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                out.push_bytes(separator.as_ref());
            }
            out.push_bytes(part);
        }
        out
    }

    #[must_use]
    pub fn replace_byte(&self, byte: u8, replacement: &[u8]) -> Self {
        let mut out = Self::new();
        for &current in self.as_bytes() {
            if current == byte {
                out.push_bytes(replacement);
            } else {
                out.push(current);
            }
        }
        out
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn push(&mut self, byte: u8) {
        self.0.push(byte);
    }

    pub fn push_bytes(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.extend_from_slice(bytes.as_ref());
    }

    pub fn eq_ignore_ascii_case(&self, other: impl AsRef<[u8]>) -> bool {
        self.0.eq_ignore_ascii_case(other.as_ref())
    }

    /// A checked text view, for interfaces that actually require UTF-8.
    pub fn to_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.0)
    }

    pub fn into_string(self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.0)
    }

    /// DuckDB identifier folding changes ASCII letters and leaves other bytes intact.
    #[must_use]
    pub fn to_ascii_lowercase(&self) -> Self {
        Self(self.0.to_ascii_lowercase())
    }
}

impl<T: AsRef<[u8]>> std::ops::AddAssign<T> for ByteString {
    fn add_assign(&mut self, bytes: T) {
        self.push_bytes(bytes);
    }
}

impl std::fmt::Write for ByteString {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_bytes(text);
        Ok(())
    }
}

impl From<ByteString> for std::borrow::Cow<'static, [u8]> {
    fn from(value: ByteString) -> Self {
        Self::Owned(value.into_bytes())
    }
}

impl From<Vec<u8>> for ByteString {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<String> for ByteString {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}

impl From<&ByteString> for ByteString {
    fn from(value: &ByteString) -> Self {
        value.clone()
    }
}

impl From<std::borrow::Cow<'_, str>> for ByteString {
    fn from(value: std::borrow::Cow<'_, str>) -> Self {
        match value {
            std::borrow::Cow::Borrowed(text) => text.into(),
            std::borrow::Cow::Owned(text) => text.into(),
        }
    }
}

impl From<&str> for ByteString {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl From<&String> for ByteString {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl PartialEq<str> for ByteString {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<String> for ByteString {
    fn eq(&self, other: &String) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}
impl PartialEq<ByteString> for String {
    fn eq(&self, other: &ByteString) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for ByteString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl From<&[u8]> for ByteString {
    fn from(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for ByteString {
    fn from(value: &[u8; N]) -> Self {
        Self(value.to_vec())
    }
}

impl AsRef<[u8]> for ByteString {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<[u8]> for ByteString {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for ByteString {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl DerefMut for ByteString {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use super::ByteString;
    use std::collections::HashMap;

    #[test]
    fn names_keep_raw_bytes_and_fold_ascii_only() {
        let name = ByteString::from(b"CaF\xe9\0\xff");
        assert_eq!(name.as_bytes(), b"CaF\xe9\0\xff");
        assert_eq!(name.to_ascii_lowercase().as_bytes(), b"caf\xe9\0\xff");
        assert!(name.to_str().is_err());
        assert_eq!(name.into_string().unwrap_err().into_bytes(), b"CaF\xe9\0\xff");
        let text = ByteString::from("café");
        assert_eq!(text.to_str().unwrap(), "café");
    }

    #[test]
    fn raw_borrowed_keys_have_the_same_hash_and_equality() {
        let mut names = HashMap::new();
        names.insert(ByteString::from(b"caf\xe9"), 42);
        assert_eq!(names.get(b"caf\xe9".as_slice()), Some(&42));
        assert_eq!(names.get("café".as_bytes()), None);
    }
}
