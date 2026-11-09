//! An in-memory JSON document with yyjson's value model.

/// A JSON value. Numbers keep yyjson's distinction between unsigned, signed and real
/// values because DuckDB's JSON deserializer type-checks on it; objects keep key order
/// (and duplicates), since the serialized text must be reproduced byte for byte.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A non-negative integer literal.
    UInt(u64),
    /// A negative integer literal (or a signed value written by the serializer).
    SInt(i64),
    /// A literal with a fraction or exponent, or `NaN`/`Infinity`.
    Real(f64),
    /// An integer too large for 64 bits, kept verbatim (yyjson's `BIGNUM_AS_RAW`).
    Raw(String),
    /// A string.
    Str(String),
    /// A string whose original bytes are not UTF-8. yyjson retains these in the
    /// document, but its normal JSON writer rejects them.
    ByteStr(roastedduck_common::ByteString),
    /// An array.
    Array(Vec<JsonValue>),
    /// An object, with keys in document order.
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// yyjson's `yyjson_get_type_desc`, used in DuckDB's type-mismatch messages.
    pub const fn type_description(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(true) => "true",
            Self::Bool(false) => "false",
            Self::UInt(_) => "uint",
            Self::SInt(_) => "sint",
            Self::Real(_) => "real",
            Self::Raw(_) => "raw",
            Self::Str(_) | Self::ByteStr(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    /// The first value stored under `key` (yyjson's `yyjson_obj_get`); `None` for non-objects.
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Whether this is an object.
    pub const fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// Whether this is an array.
    pub const fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }
}
