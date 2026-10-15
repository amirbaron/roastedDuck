//! DuckDB's binary serialization format over a byte stream (DuckDB's `BinaryDeserializer`
//! reading a `ReadStream`, used for metadata chains whose length is not known up front).

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::{Deserializer, FieldId, MESSAGE_TERMINATOR_FIELD_ID};

use crate::metadata::MetadataReader;

/// A source of bytes (DuckDB's `ReadStream`).
pub trait ReadStream {
    /// Fills `buffer` with the next bytes.
    fn read_data(&mut self, buffer: &mut [u8]) -> Result<()>;
}

impl ReadStream for MetadataReader<'_> {
    fn read_data(&mut self, buffer: &mut [u8]) -> Result<()> {
        MetadataReader::read_data(self, buffer)
    }
}

/// A byte slice as a stream.
#[derive(Debug)]
pub struct SliceStream<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> SliceStream<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub const fn position(&self) -> usize {
        self.position
    }
}

impl ReadStream for SliceStream<'_> {
    fn read_data(&mut self, buffer: &mut [u8]) -> Result<()> {
        let end = self.position + buffer.len();
        let bytes = self.data.get(self.position..end).ok_or_else(|| {
            Error::serialization("Failed to deserialize: not enough data in buffer to fulfill read request")
        })?;
        buffer.copy_from_slice(bytes);
        self.position = end;
        Ok(())
    }
}

/// Reads DuckDB's binary format from a [`ReadStream`].
pub struct StreamDeserializer<'s> {
    stream: &'s mut dyn ReadStream,
    buffered_field: Option<FieldId>,
}

impl std::fmt::Debug for StreamDeserializer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamDeserializer").finish_non_exhaustive()
    }
}

fn corrupt_varint() -> Error {
    Error::new(
        ErrorKind::Io,
        "Failed to decode LEB128 integer: data may be corrupt",
    )
}

impl<'s> StreamDeserializer<'s> {
    pub fn new(stream: &'s mut dyn ReadStream) -> Self {
        Self {
            stream,
            buffered_field: None,
        }
    }

    /// Opens the top-level object (`Begin`).
    pub fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    /// Closes the top-level object (`End`).
    pub fn end(&mut self) -> Result<()> {
        Deserializer::on_object_end(self)
    }

    fn read_byte(&mut self) -> Result<u8> {
        let mut byte = [0u8; 1];
        self.stream.read_data(&mut byte)?;
        Ok(byte[0])
    }

    fn read_raw_field(&mut self) -> Result<FieldId> {
        let mut bytes = [0u8; 2];
        self.stream.read_data(&mut bytes)?;
        Ok(FieldId::from_le_bytes(bytes))
    }

    fn peek_field(&mut self) -> Result<FieldId> {
        if let Some(field) = self.buffered_field {
            return Ok(field);
        }
        let field = self.read_raw_field()?;
        self.buffered_field = Some(field);
        Ok(field)
    }

    fn next_field(&mut self) -> Result<FieldId> {
        match self.buffered_field.take() {
            Some(field) => Ok(field),
            None => self.read_raw_field(),
        }
    }

    /// The bytes of one varint (at most 16).
    fn varint(&mut self) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(2);
        for _ in 0..16 {
            let byte = self.read_byte()?;
            bytes.push(byte);
            if byte & 0x80 == 0 {
                break;
            }
        }
        Ok(bytes)
    }

    fn read_unsigned(&mut self, bits: u32) -> Result<u64> {
        let bytes = self.varint()?;
        let mut result = 0u64;
        let mut shift = 0u32;
        for byte in bytes {
            if shift >= bits {
                return Err(corrupt_varint());
            }
            result |= u64::from(byte & 0x7F) << shift;
            shift += 7;
        }
        Ok(result)
    }

    fn read_signed(&mut self, bits: u32) -> Result<i64> {
        let bytes = self.varint()?;
        let mut result = 0u64;
        let mut shift = 0u32;
        let mut last = 0u8;
        for byte in bytes {
            if shift >= bits {
                return Err(corrupt_varint());
            }
            result |= u64::from(byte & 0x7F) << shift;
            shift += 7;
            last = byte;
        }
        if shift < bits && last & 0x40 != 0 {
            result |= u64::MAX << shift;
        }
        Ok(result as i64)
    }

    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.stream.read_data(&mut bytes)?;
        Ok(bytes)
    }

    /// Reads a blob of any length (`ReadProperty(data_ptr_t, count)` when the count is
    /// not known in advance).
    pub fn read_blob(&mut self) -> Result<Vec<u8>> {
        let len = usize::try_from(self.read_unsigned(64)?)
            .map_err(|_| Error::serialization("blob length overflow"))?;
        let mut bytes = vec![0u8; len];
        self.stream.read_data(&mut bytes)?;
        Ok(bytes)
    }
}

impl Deserializer for StreamDeserializer<'_> {
    fn deserialize_enum_from_string(&self) -> bool {
        false
    }

    fn on_property_begin(&mut self, field_id: FieldId, _tag: &'static str) -> Result<()> {
        let field = self.next_field()?;
        if field != field_id {
            return Err(Error::serialization(format!(
                "Failed to deserialize: field id mismatch, expected: {field_id}, got: {field}"
            )));
        }
        Ok(())
    }

    fn on_optional_property_begin(&mut self, field_id: FieldId, _tag: &'static str) -> Result<bool> {
        let present = self.peek_field()? == field_id;
        if present {
            self.buffered_field = None;
        }
        Ok(present)
    }

    fn on_object_begin(&mut self) -> Result<()> {
        Ok(())
    }

    fn on_object_end(&mut self) -> Result<()> {
        let field = self.next_field()?;
        if field != MESSAGE_TERMINATOR_FIELD_ID {
            return Err(Error::serialization(format!(
                "Failed to deserialize: expected end of object, but found field id: {field}"
            )));
        }
        Ok(())
    }

    fn on_list_begin(&mut self) -> Result<usize> {
        let count = self.read_unsigned(64)?;
        usize::try_from(count)
            .map_err(|_| Error::serialization("Failed to deserialize: list length overflow"))
    }

    fn on_list_end(&mut self) -> Result<()> {
        Ok(())
    }

    fn on_nullable_begin(&mut self) -> Result<bool> {
        self.read_bool()
    }

    fn on_nullable_end(&mut self) -> Result<()> {
        Ok(())
    }

    fn can_deserialize_property(&mut self, field_id: FieldId, _tag: &'static str) -> Result<bool> {
        Ok(self.peek_field()? == field_id)
    }

    fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_byte()? != 0)
    }

    fn read_u8(&mut self) -> Result<u8> {
        self.read_unsigned(8).map(|v| v as u8)
    }

    fn read_i8(&mut self) -> Result<i8> {
        self.read_signed(8).map(|v| v as i8)
    }

    fn read_u16(&mut self) -> Result<u16> {
        self.read_unsigned(16).map(|v| v as u16)
    }

    fn read_i16(&mut self) -> Result<i16> {
        self.read_signed(16).map(|v| v as i16)
    }

    fn read_u32(&mut self) -> Result<u32> {
        self.read_unsigned(32).map(|v| v as u32)
    }

    fn read_i32(&mut self) -> Result<i32> {
        self.read_signed(32).map(|v| v as i32)
    }

    fn read_u64(&mut self) -> Result<u64> {
        self.read_unsigned(64)
    }

    fn read_i64(&mut self) -> Result<i64> {
        self.read_signed(64)
    }

    fn read_i128(&mut self) -> Result<i128> {
        let upper = self.read_signed(64)?;
        let lower = self.read_unsigned(64)?;
        Ok((i128::from(upper) << 64) | i128::from(lower))
    }

    fn read_u128(&mut self) -> Result<u128> {
        let upper = self.read_unsigned(64)?;
        let lower = self.read_unsigned(64)?;
        Ok((u128::from(upper) << 64) | u128::from(lower))
    }

    fn read_f32(&mut self) -> Result<f32> {
        self.read_fixed().map(f32::from_le_bytes)
    }

    fn read_f64(&mut self) -> Result<f64> {
        self.read_fixed().map(f64::from_le_bytes)
    }

    fn read_string_bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.read_unsigned(32)? as usize;
        let mut bytes = vec![0u8; len];
        self.stream.read_data(&mut bytes)?;
        Ok(bytes)
    }

    fn read_data(&mut self, len: usize) -> Result<Vec<u8>> {
        let stored = self.read_unsigned(64)?;
        if stored != len as u64 {
            return Err(Error::serialization(format!(
                "Tried to read blob of {len} size, but only {stored} elements are available"
            )));
        }
        let mut bytes = vec![0u8; len];
        self.stream.read_data(&mut bytes)?;
        Ok(bytes)
    }
}
