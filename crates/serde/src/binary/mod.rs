//! DuckDB's binary format (`BinarySerializer` / `BinaryDeserializer`), used by the WAL,
//! the catalog and plan serialization.
//!
//! Layout: every property is a little-endian `u16` field id followed by its value;
//! objects end with the field id `0xFFFF`; properties holding their default value are
//! omitted; integers are LEB128 varints (signed types use the signed encoding); floats
//! are raw little-endian; strings and lists are length-prefixed; nullable values carry a
//! one-byte presence flag.

mod deserializer;
mod leb128;
mod serializer;

pub use deserializer::BinaryDeserializer;
pub use serializer::BinarySerializer;
