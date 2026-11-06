//! Native formatting dispatch. Byte-owning SQL nodes opt in independently of
//! their checked Rust Display interface; ordinary text and numbers use Display.

use std::fmt::Display;

use crate::ByteString;

/// A byte representation that must bypass Rust's text formatter.
pub trait NativeByteFormat {
    fn append_native_bytes(&self, output: &mut ByteString);
}

impl NativeByteFormat for ByteString {
    fn append_native_bytes(&self, output: &mut ByteString) {
        output.push_bytes(self);
    }
}

impl NativeByteFormat for [u8] {
    fn append_native_bytes(&self, output: &mut ByteString) {
        output.push_bytes(self);
    }
}
impl NativeByteFormat for Vec<u8> {
    fn append_native_bytes(&self, output: &mut ByteString) {
        output.push_bytes(self);
    }
}

impl<T: NativeByteFormat + ?Sized> NativeByteFormat for &T {
    fn append_native_bytes(&self, output: &mut ByteString) {
        (*self).append_native_bytes(output);
    }
}

impl<T: NativeByteFormat + ?Sized> NativeByteFormat for &mut T {
    fn append_native_bytes(&self, output: &mut ByteString) {
        (**self).append_native_bytes(output);
    }
}

impl<T: NativeByteFormat + ?Sized> NativeByteFormat for Box<T> {
    fn append_native_bytes(&self, output: &mut ByteString) {
        (**self).append_native_bytes(output);
    }
}

impl<T: NativeByteFormat + ?Sized> NativeByteFormat for std::sync::Arc<T> {
    fn append_native_bytes(&self, output: &mut ByteString) {
        (**self).append_native_bytes(output);
    }
}

/// Macro implementation detail: the two reference levels select native formatting
/// when available and otherwise fall back to the ordinary Display contract.
#[doc(hidden)]
#[derive(Debug)]
pub struct FormatArgument<'a, T: ?Sized>(pub &'a T);

#[doc(hidden)]
pub trait AppendBytes {
    fn append_bytes(self, output: &mut ByteString);
}

impl<T: NativeByteFormat + ?Sized> AppendBytes for &FormatArgument<'_, T> {
    fn append_bytes(self, output: &mut ByteString) {
        self.0.append_native_bytes(output);
    }
}

impl<T: Display + ?Sized> AppendBytes for &&FormatArgument<'_, T> {
    fn append_bytes(self, output: &mut ByteString) {
        output.push_bytes(self.0.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{byte_format, byte_write};
    use std::cell::Cell;

    struct NativeNode(ByteString);
    impl Display for NativeNode {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("text-only-view")
        }
    }
    impl NativeByteFormat for NativeNode {
        fn append_native_bytes(&self, output: &mut ByteString) {
            output.push_bytes(&self.0);
        }
    }

    #[test]
    fn native_nodes_bypass_existing_text_display_recursively() {
        let name = ByteString::from(b"CaF\xe9\0\xff");
        let node = NativeNode(name.clone());
        assert_eq!(
            byte_format!("({node}) = {}", &name).as_bytes(),
            b"(CaF\xe9\0\xff) = CaF\xe9\0\xff"
        );
        assert_eq!(byte_format!("{}", &&node).as_bytes(), name.as_bytes());
        let mut native = NativeNode(name.clone());
        assert_eq!(byte_format!("{}", &mut native).as_bytes(), name.as_bytes());
    }

    #[test]
    fn fields_capture_text_numbers_and_explicit_arguments_once() {
        let count = Cell::new(0);
        let name = "café";
        let rendered = byte_format!(
            "{{{name}}}: {0}/{0}; {answer}",
            {
                count.set(count.get() + 1);
                42
            },
            answer = 7
        );
        assert_eq!(rendered.to_str().unwrap(), "{café}: 42/42; 7");
        assert_eq!(count.get(), 1);
        assert_eq!(byte_format!("{}", format!("{:04}", 3)).as_bytes(), b"0003");
        let index = &3usize;
        assert_eq!(byte_format!("{}", index + 1).as_bytes(), b"4");
        assert_eq!(byte_format!("{}", 2 + 3 * 4).as_bytes(), b"14");
    }

    #[test]
    fn writer_retains_existing_raw_prefix_and_field_bytes() {
        let mut out = ByteString::from(b"\xff:");
        let value = ByteString::from(b"a\0b\xe9");
        byte_write!(out, "[{value}] {}", 42).unwrap();
        assert_eq!(out.as_bytes(), b"\xff:[a\0b\xe9] 42");
    }
}
