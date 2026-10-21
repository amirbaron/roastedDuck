//! The key/value metadata of an Arrow field (DuckDB's `ArrowSchemaMetadata`,
//! `common/arrow/schema_metadata.cpp`), which names extension types.
//!
//! The binary form is the one of the C data interface: an `int32` pair count, then for
//! each pair the `int32` length and bytes of the key and of the value.

use std::collections::BTreeMap;
use std::ffi::c_char;

use rustdb_common::string_util::json_quote;
use rustdb_common::{Error, Result};

use crate::type_extension::ArrowExtensionMetadata;

/// The field metadata of an Arrow schema.
#[derive(Clone, Debug, Default)]
pub struct ArrowSchemaMetadata {
    /// The key/value pairs; serialized in key order.
    schema_metadata_map: BTreeMap<String, String>,
    /// The string members of the JSON object in `ARROW:extension:metadata` (only
    /// `arrow.opaque` types use it).
    extension_metadata_map: serde_json::Map<String, serde_json::Value>,
}

impl ArrowSchemaMetadata {
    /// The key naming the extension type (`ARROW_EXTENSION_NAME`).
    pub const ARROW_EXTENSION_NAME: &str = "ARROW:extension:name";
    /// The key of the extension type's parameters (`ARROW_METADATA_KEY`).
    pub const ARROW_METADATA_KEY: &str = "ARROW:extension:metadata";

    /// Reads the metadata of an imported field (`ArrowSchemaMetadata(const char *)`).
    ///
    /// # Safety
    /// `metadata` must be NULL or metadata in the C data interface's binary form.
    pub unsafe fn from_c(metadata: *const c_char) -> Result<Self> {
        let mut result = Self::default();
        if !metadata.is_null() {
            let mut reader = MetadataReader {
                cursor: metadata.cast(),
            };
            // SAFETY: the binary form holds the pairs and lengths it announces (caller
            // contract).
            unsafe {
                let pair_count = reader.int();
                for _ in 0..pair_count {
                    let key = reader.text();
                    let value = reader.text();
                    result.schema_metadata_map.insert(key, value);
                }
            }
        }
        // Errors in the extension metadata only matter for `arrow.opaque` types.
        let ignore_errors = result.option(Self::ARROW_EXTENSION_NAME)
            != ArrowExtensionMetadata::ARROW_EXTENSION_NON_CANONICAL;
        result.extension_metadata_map =
            parse_json_map(&result.option(Self::ARROW_METADATA_KEY), ignore_errors)?;
        Ok(result)
    }

    /// Sets an option (`AddOption`).
    pub fn add_option(&mut self, key: &str, value: &str) {
        self.schema_metadata_map.insert(key.to_owned(), value.to_owned());
    }

    /// An option, empty when unset (`GetOption`).
    pub fn option(&self, key: &str) -> String {
        self.schema_metadata_map.get(key).cloned().unwrap_or_default()
    }

    /// Whether the field is an extension type (`HasExtension`).
    pub fn has_extension(&self) -> bool {
        !self.option(Self::ARROW_EXTENSION_NAME).is_empty()
    }

    /// The extension the field names, with the field's format (`GetExtensionInfo`).
    pub fn extension_info(&self, format: &str) -> ArrowExtensionMetadata {
        ArrowExtensionMetadata::new(
            &self.option(Self::ARROW_EXTENSION_NAME),
            &self.extension_value("vendor_name"),
            &self.extension_value("type_name"),
            format,
        )
    }

    /// A member of the extension metadata object as text (`ComplexJSON::GetValue`).
    fn extension_value(&self, key: &str) -> String {
        match self.extension_metadata_map.get(key) {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }

    /// The metadata of a canonical extension type (`ArrowCanonicalType`).
    pub fn arrow_canonical_type(extension_name: &str) -> Self {
        let mut metadata = Self::default();
        metadata.add_option(Self::ARROW_EXTENSION_NAME, extension_name);
        metadata.add_option(Self::ARROW_METADATA_KEY, "");
        metadata
    }

    /// The metadata of a vendor's type, as an `arrow.opaque` extension
    /// (`NonCanonicalType`).
    pub fn non_canonical_type(type_name: &str, vendor_name: &str) -> Self {
        let mut metadata = Self::default();
        metadata.add_option(
            Self::ARROW_EXTENSION_NAME,
            ArrowExtensionMetadata::ARROW_EXTENSION_NON_CANONICAL,
        );
        let json = format!(
            "{{{}:{},{}:{}}}",
            json_quote("type_name"),
            json_quote(type_name),
            json_quote("vendor_name"),
            json_quote(vendor_name)
        );
        metadata.add_option(Self::ARROW_METADATA_KEY, &json);
        metadata
    }

    /// The binary form, for an exported schema (`SerializeMetadata`).
    pub fn serialize(&self) -> Box<[u8]> {
        let mut bytes = Vec::new();
        // The C data interface stores lengths as `int32`.
        let push_len = |bytes: &mut Vec<u8>, len: usize| {
            bytes.extend_from_slice(&(len as i32).to_ne_bytes());
        };
        push_len(&mut bytes, self.schema_metadata_map.len());
        for (key, value) in &self.schema_metadata_map {
            push_len(&mut bytes, key.len());
            bytes.extend_from_slice(key.as_bytes());
            push_len(&mut bytes, value.len());
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.into_boxed_slice()
    }
}

/// Reads the binary form of field metadata.
struct MetadataReader {
    cursor: *const u8,
}

impl MetadataReader {
    /// The next `int32`.
    ///
    /// # Safety
    /// The next four bytes must be readable.
    unsafe fn int(&mut self) -> i32 {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let value = self.cursor.cast::<i32>().read_unaligned();
            self.cursor = self.cursor.add(4);
            value
        }
    }

    /// The next length-prefixed string; invalid UTF-8 is replaced.
    ///
    /// # Safety
    /// The length and the bytes it announces must be readable.
    unsafe fn text(&mut self) -> String {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let len = usize::try_from(self.int()).unwrap_or(0);
            let bytes = std::slice::from_raw_parts(self.cursor, len);
            self.cursor = self.cursor.add(len);
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// The members of a JSON object (`StringUtil::ParseJSONMap`): empty for empty text, and
/// for invalid text or a non-object when `ignore_errors`.
fn parse_json_map(json: &str, ignore_errors: bool) -> Result<serde_json::Map<String, serde_json::Value>> {
    if json.is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        _ if ignore_errors => Ok(serde_json::Map::new()),
        _ => Err(Error::serialization(format!(
            "Failed to parse JSON string: {json}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_round_trips_through_the_binary_form() {
        let metadata = ArrowSchemaMetadata::non_canonical_type("hugeint", "DuckDB");
        let bytes = metadata.serialize();
        // SAFETY: `bytes` is metadata in the binary form.
        let read = unsafe { ArrowSchemaMetadata::from_c(bytes.as_ptr().cast()) }.unwrap();
        assert!(read.has_extension());
        let info = read.extension_info("w:16");
        assert_eq!(info.extension_name(), "arrow.opaque");
        assert_eq!(info.vendor_name(), "DuckDB");
        assert_eq!(info.type_name(), "hugeint");
        assert_eq!(
            read.option(ArrowSchemaMetadata::ARROW_METADATA_KEY),
            r#"{"type_name":"hugeint","vendor_name":"DuckDB"}"#
        );
    }

    #[test]
    fn invalid_opaque_metadata_is_an_error() {
        let mut metadata = ArrowSchemaMetadata::default();
        metadata.add_option(ArrowSchemaMetadata::ARROW_EXTENSION_NAME, "arrow.opaque");
        metadata.add_option(ArrowSchemaMetadata::ARROW_METADATA_KEY, "{");
        let bytes = metadata.serialize();
        // SAFETY: `bytes` is metadata in the binary form.
        let error = unsafe { ArrowSchemaMetadata::from_c(bytes.as_ptr().cast()) }.unwrap_err();
        assert_eq!(error.raw_message(), "Failed to parse JSON string: {");
    }
}
