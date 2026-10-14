use rustdb_common::Result;

use super::leb128::{encode_signed, encode_unsigned};
use crate::{FieldId, MESSAGE_TERMINATOR_FIELD_ID, SerializationOptions, SerializeObject, Serializer};

/// Writes DuckDB's binary format into a byte buffer.
#[derive(Debug)]
pub struct BinarySerializer {
    options: SerializationOptions,
    buffer: Vec<u8>,
}

impl BinarySerializer {
    /// A serializer with the given options; enums are always written as numbers.
    pub fn new(mut options: SerializationOptions) -> Self {
        options.serialize_enum_as_string = false;
        Self {
            options,
            buffer: Vec::new(),
        }
    }

    /// Serializes `value` as a top-level object (DuckDB's `BinarySerializer::Serialize`).
    pub fn serialize<T: SerializeObject + ?Sized>(
        value: &T,
        options: SerializationOptions,
    ) -> Result<Vec<u8>> {
        let mut serializer = Self::new(options);
        (&mut serializer as &mut dyn Serializer).write_object(value)?;
        Ok(serializer.into_bytes())
    }

    /// Opens the top-level object (DuckDB's `Begin`), for callers writing properties by hand.
    pub fn begin(&mut self) {
        self.on_object_begin();
    }

    /// Closes the top-level object (DuckDB's `End`).
    pub fn end(&mut self) {
        self.on_object_end();
    }

    /// The bytes written so far.
    pub fn bytes(&self) -> &[u8] {
        &self.buffer
    }

    /// The bytes written.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buffer
    }

    fn write_field_id(&mut self, field_id: FieldId) {
        self.buffer.extend_from_slice(&field_id.to_le_bytes());
    }
}

impl Serializer for BinarySerializer {
    fn options(&self) -> &SerializationOptions {
        &self.options
    }

    fn on_property_begin(&mut self, field_id: FieldId, _tag: &'static str) {
        self.write_field_id(field_id);
    }

    fn on_optional_property_begin(&mut self, field_id: FieldId, _tag: &'static str, present: bool) {
        if present {
            self.write_field_id(field_id);
        }
    }

    fn on_object_begin(&mut self) {}

    fn on_object_end(&mut self) {
        self.write_field_id(MESSAGE_TERMINATOR_FIELD_ID);
    }

    fn on_list_begin(&mut self, count: usize) {
        encode_unsigned(&mut self.buffer, count as u64);
    }

    fn on_nullable_begin(&mut self, present: bool) {
        self.write_bool(present);
    }

    fn write_null(&mut self) {
        // Absent values are represented by omitted optional properties.
    }

    fn write_bool(&mut self, value: bool) {
        self.buffer.push(u8::from(value));
    }

    fn write_u8(&mut self, value: u8) {
        encode_unsigned(&mut self.buffer, value.into());
    }

    fn write_i8(&mut self, value: i8) {
        encode_signed(&mut self.buffer, value.into());
    }

    fn write_u16(&mut self, value: u16) {
        encode_unsigned(&mut self.buffer, value.into());
    }

    fn write_i16(&mut self, value: i16) {
        encode_signed(&mut self.buffer, value.into());
    }

    fn write_u32(&mut self, value: u32) {
        encode_unsigned(&mut self.buffer, value.into());
    }

    fn write_i32(&mut self, value: i32) {
        encode_signed(&mut self.buffer, value.into());
    }

    fn write_u64(&mut self, value: u64) {
        encode_unsigned(&mut self.buffer, value);
    }

    fn write_i64(&mut self, value: i64) {
        encode_signed(&mut self.buffer, value);
    }

    fn write_i128(&mut self, value: i128) {
        // hugeint_t is written as (upper: int64, lower: uint64).
        encode_signed(&mut self.buffer, (value >> 64) as i64);
        encode_unsigned(&mut self.buffer, value as u64);
    }

    fn write_u128(&mut self, value: u128) {
        encode_unsigned(&mut self.buffer, (value >> 64) as u64);
        encode_unsigned(&mut self.buffer, value as u64);
    }

    fn write_f32(&mut self, value: f32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn write_f64(&mut self, value: f64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn write_string_bytes(&mut self, value: &[u8]) {
        encode_unsigned(&mut self.buffer, value.len() as u64);
        self.buffer.extend_from_slice(value);
    }

    fn write_data(&mut self, value: &[u8]) {
        encode_unsigned(&mut self.buffer, value.len() as u64);
        self.buffer.extend_from_slice(value);
    }
}
