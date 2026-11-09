use roastedduck_common::{Error, ErrorKind, Result};

use super::leb128::{decode_signed, decode_unsigned};
use crate::{DeserializeObject, Deserializer, FieldId, MESSAGE_TERMINATOR_FIELD_ID};

/// Reads DuckDB's binary format from a byte slice.
#[derive(Debug)]
pub struct BinaryDeserializer<'a> {
    data: &'a [u8],
    position: usize,
    /// One field id of look-ahead, used to detect absent optional properties.
    buffered_field: Option<FieldId>,
}

impl<'a> BinaryDeserializer<'a> {
    /// A deserializer reading `data` from the start.
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffered_field: None,
        }
    }

    /// Reads a top-level object (DuckDB's `BinaryDeserializer::Deserialize<T>`).
    pub fn deserialize<T: DeserializeObject>(data: &'a [u8]) -> Result<T> {
        let mut deserializer = Self::new(data);
        let value = (&mut deserializer as &mut dyn Deserializer).read_object()?;
        Ok(value)
    }

    /// Opens the top-level object (DuckDB's `Begin`).
    pub fn begin(&mut self) -> Result<()> {
        self.on_object_begin()
    }

    /// Closes the top-level object (DuckDB's `End`).
    pub fn end(&mut self) -> Result<()> {
        self.on_object_end()
    }

    /// Bytes consumed so far.
    pub const fn position(&self) -> usize {
        self.position
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| {
                Error::serialization(
                    "Failed to deserialize: not enough data in buffer to fulfill read request",
                )
            })?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn read_raw_field(&mut self) -> Result<FieldId> {
        let bytes = self.read_bytes(2)?;
        Ok(FieldId::from_le_bytes([bytes[0], bytes[1]]))
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

    /// The bytes of one varint (at most 16, like DuckDB's `VarIntDecode`).
    fn varint_bytes(&mut self) -> Result<&'a [u8]> {
        let start = self.position;
        for _ in 0..16 {
            let byte = self.read_bytes(1)?[0];
            if byte & 0x80 == 0 {
                break;
            }
        }
        Ok(&self.data[start..self.position])
    }

    fn read_unsigned(&mut self, bits: u32) -> Result<u64> {
        let bytes = self.varint_bytes()?;
        decode_unsigned(bytes, bits).ok_or_else(corrupt_varint)
    }

    fn read_signed(&mut self, bits: u32) -> Result<i64> {
        let bytes = self.varint_bytes()?;
        decode_signed(bytes, bits).ok_or_else(corrupt_varint)
    }

    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut array = [0u8; N];
        array.copy_from_slice(self.read_bytes(N)?);
        Ok(array)
    }
}

fn corrupt_varint() -> Error {
    Error::new(
        ErrorKind::Io,
        "Failed to decode LEB128 integer: data may be corrupt",
    )
}

impl Deserializer for BinaryDeserializer<'_> {
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
        Ok(self.read_bytes(1)?[0] != 0)
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
        self.read_bytes(len).map(<[u8]>::to_vec)
    }

    fn read_data(&mut self, len: usize) -> Result<Vec<u8>> {
        let stored = self.read_unsigned(64)?;
        if stored != len as u64 {
            return Err(Error::serialization(format!(
                "Tried to read blob of {len} size, but only {stored} elements are available"
            )));
        }
        self.read_bytes(len).map(<[u8]>::to_vec)
    }
}
