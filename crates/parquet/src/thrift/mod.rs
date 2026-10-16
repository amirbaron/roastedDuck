//! Apache Thrift's compact protocol (DuckDB uses `TCompactProtocolT` from
//! `third_party/thrift`), the serialization of every Parquet metadata structure.
//!
//! Structures are declared with [`thrift_struct!`], which generates the Rust struct and its
//! reader and writer from the field list of `parquet.thrift`. Writers emit fields in
//! declaration order and skip unset optional fields, exactly like the C++ generated code,
//! so metadata written by rustdb is byte-identical to DuckDB's for the same values.

mod types;

pub use types::*;

use rustdb_common::{Error, Result};

/// Compact protocol type ids (`TCompactProtocol::Types`).
pub mod compact_type {
    pub const STOP: u8 = 0;
    pub const BOOLEAN_TRUE: u8 = 1;
    pub const BOOLEAN_FALSE: u8 = 2;
    pub const BYTE: u8 = 3;
    pub const I16: u8 = 4;
    pub const I32: u8 = 5;
    pub const I64: u8 = 6;
    pub const DOUBLE: u8 = 7;
    pub const BINARY: u8 = 8;
    pub const LIST: u8 = 9;
    pub const SET: u8 = 10;
    pub const MAP: u8 = 11;
    pub const STRUCT: u8 = 12;
}

/// The deepest nesting of structures and containers accepted when reading (Thrift's
/// default recursion limit).
const MAX_RECURSION_DEPTH: u32 = 64;

/// The error thrown by the C++ protocol for malformed input
/// (`TProtocolException::INVALID_DATA`).
fn invalid_data() -> Error {
    Error::new(rustdb_common::ErrorKind::Invalid, "TProtocolException: Invalid data")
}

/// Reading past the end of the input (`TTransportException::END_OF_FILE`).
fn end_of_input() -> Error {
    Error::new(
        rustdb_common::ErrorKind::Invalid,
        "TTransportException: No more data to read.",
    )
}

/// Reads compact-protocol values from a byte slice.
#[derive(Debug)]
pub struct CompactReader<'a> {
    data: &'a [u8],
    position: usize,
    /// The id of the last field read in each open structure.
    last_field_ids: Vec<i16>,
    /// The value of a boolean field, carried in the field header.
    pending_bool: Option<bool>,
}

impl<'a> CompactReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            last_field_ids: Vec::new(),
            pending_bool: None,
        }
    }

    /// The number of bytes consumed so far.
    pub fn position(&self) -> usize {
        self.position
    }

    fn read_u8(&mut self) -> Result<u8> {
        let byte = *self.data.get(self.position).ok_or_else(end_of_input)?;
        self.position += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(len).ok_or_else(end_of_input)?;
        let bytes = self.data.get(self.position..end).ok_or_else(end_of_input)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = self.read_u8()?;
            if shift < 64 {
                result |= u64::from(byte & 0x7f) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 63 {
                // `readVarint64` gives up after ten bytes.
                return Err(invalid_data());
            }
        }
    }

    fn read_zigzag(&mut self) -> Result<i64> {
        let value = self.read_varint()?;
        Ok((value >> 1) as i64 ^ -((value & 1) as i64))
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        if let Some(value) = self.pending_bool.take() {
            return Ok(value);
        }
        Ok(self.read_u8()? == compact_type::BOOLEAN_TRUE)
    }

    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_zigzag()? as i16)
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_zigzag()? as i32)
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        self.read_zigzag()
    }

    pub fn read_double(&mut self) -> Result<f64> {
        let bytes = self.read_bytes(8)?;
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        Ok(f64::from_le_bytes(array))
    }

    pub fn read_binary(&mut self) -> Result<Vec<u8>> {
        let len = usize::try_from(self.read_varint()?).map_err(|_| invalid_data())?;
        Ok(self.read_bytes(len)?.to_vec())
    }

    pub fn read_string(&mut self) -> Result<String> {
        let len = usize::try_from(self.read_varint()?).map_err(|_| invalid_data())?;
        let bytes = self.read_bytes(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Begins a list: its element type and size.
    pub fn read_list_begin(&mut self) -> Result<(u8, usize)> {
        let header = self.read_u8()?;
        let element_type = header & 0x0f;
        let mut size = u64::from(header >> 4);
        if size == 15 {
            size = self.read_varint()?;
        }
        let size = usize::try_from(size).map_err(|_| invalid_data())?;
        // Every element takes at least one byte (except booleans packed in nothing).
        if size > self.data.len() - self.position && element_type != compact_type::BOOLEAN_TRUE {
            return Err(end_of_input());
        }
        Ok((element_type, size))
    }

    fn struct_begin(&mut self) -> Result<()> {
        if self.last_field_ids.len() as u32 >= MAX_RECURSION_DEPTH {
            return Err(Error::new(
                rustdb_common::ErrorKind::Invalid,
                "TProtocolException: Exceeded recursion depth",
            ));
        }
        self.last_field_ids.push(0);
        Ok(())
    }

    fn struct_end(&mut self) {
        self.last_field_ids.pop();
    }

    /// The next field header: `None` at the end of the structure, else its id and type.
    fn read_field_begin(&mut self) -> Result<Option<(i16, u8)>> {
        let header = self.read_u8()?;
        let field_type = header & 0x0f;
        if field_type == compact_type::STOP {
            return Ok(None);
        }
        let modifier = i16::from(header >> 4);
        let last = *self.last_field_ids.last().ok_or_else(invalid_data)?;
        let id = if modifier == 0 {
            // Not enough room in the header byte: the id follows as a zigzag varint.
            self.read_zigzag()? as i16
        } else {
            last.wrapping_add(modifier)
        };
        if let Some(last) = self.last_field_ids.last_mut() {
            *last = id;
        }
        match field_type {
            compact_type::BOOLEAN_TRUE => self.pending_bool = Some(true),
            compact_type::BOOLEAN_FALSE => self.pending_bool = Some(false),
            _ => {}
        }
        Ok(Some((id, field_type)))
    }

    /// Reads a structure, calling `field` for every field with its id and wire type.
    pub fn read_struct(&mut self, mut field: impl FnMut(&mut Self, i16, u8) -> Result<()>) -> Result<()> {
        self.struct_begin()?;
        while let Some((id, field_type)) = self.read_field_begin()? {
            field(self, id, field_type)?;
            self.pending_bool = None;
        }
        self.struct_end();
        Ok(())
    }

    /// Skips a value of `field_type` (`TProtocol::skip`).
    pub fn skip(&mut self, field_type: u8) -> Result<()> {
        self.skip_depth(field_type, 0)
    }

    fn skip_depth(&mut self, field_type: u8, depth: u32) -> Result<()> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(invalid_data());
        }
        match field_type {
            compact_type::BOOLEAN_TRUE | compact_type::BOOLEAN_FALSE => {
                self.read_bool()?;
            }
            compact_type::BYTE => {
                self.read_u8()?;
            }
            compact_type::I16 | compact_type::I32 | compact_type::I64 => {
                self.read_varint()?;
            }
            compact_type::DOUBLE => {
                self.read_bytes(8)?;
            }
            compact_type::BINARY => {
                let len = usize::try_from(self.read_varint()?).map_err(|_| invalid_data())?;
                self.read_bytes(len)?;
            }
            compact_type::STRUCT => {
                self.struct_begin()?;
                while let Some((_, inner)) = self.read_field_begin()? {
                    self.skip_depth(inner, depth + 1)?;
                    self.pending_bool = None;
                }
                self.struct_end();
            }
            compact_type::LIST | compact_type::SET => {
                let (element_type, size) = self.read_list_begin()?;
                for _ in 0..size {
                    self.skip_depth(element_type, depth + 1)?;
                }
            }
            compact_type::MAP => {
                let size = usize::try_from(self.read_varint()?).map_err(|_| invalid_data())?;
                if size > 0 {
                    let types = self.read_u8()?;
                    for _ in 0..size {
                        self.skip_depth(types >> 4, depth + 1)?;
                        self.skip_depth(types & 0x0f, depth + 1)?;
                    }
                }
            }
            _ => return Err(invalid_data()),
        }
        Ok(())
    }
}

/// Writes compact-protocol values into a byte vector.
#[derive(Debug, Default)]
pub struct CompactWriter {
    out: Vec<u8>,
    last_field_ids: Vec<i16>,
}

impl CompactWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes written.
    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    fn write_varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn write_zigzag(&mut self, value: i64) {
        self.write_varint(((value << 1) ^ (value >> 63)) as u64);
    }

    pub fn write_i8(&mut self, value: i8) {
        self.out.push(value as u8);
    }

    pub fn write_i16(&mut self, value: i16) {
        self.write_zigzag(i64::from(value));
    }

    pub fn write_i32(&mut self, value: i32) {
        self.write_zigzag(i64::from(value));
    }

    pub fn write_i64(&mut self, value: i64) {
        self.write_zigzag(value);
    }

    pub fn write_double(&mut self, value: f64) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_binary(&mut self, value: &[u8]) {
        self.write_varint(value.len() as u64);
        self.out.extend_from_slice(value);
    }

    /// A boolean outside a field header (a list element).
    pub fn write_bool_element(&mut self, value: bool) {
        self.out.push(if value {
            compact_type::BOOLEAN_TRUE
        } else {
            compact_type::BOOLEAN_FALSE
        });
    }

    pub fn write_list_begin(&mut self, element_type: u8, size: usize) {
        if size < 15 {
            self.out.push(((size as u8) << 4) | element_type);
        } else {
            self.out.push(0xf0 | element_type);
            self.write_varint(size as u64);
        }
    }

    pub fn write_struct_begin(&mut self) {
        self.last_field_ids.push(0);
    }

    pub fn write_struct_end(&mut self) {
        self.out.push(compact_type::STOP);
        self.last_field_ids.pop();
    }

    /// A field header; `field_type` is `BOOLEAN_TRUE`/`BOOLEAN_FALSE` for boolean fields,
    /// whose value it carries.
    pub fn write_field_begin(&mut self, id: i16, field_type: u8) {
        let last = self.last_field_ids.last().copied().unwrap_or(0);
        if id > last && id - last <= 15 {
            self.out.push((((id - last) as u8) << 4) | field_type);
        } else {
            self.out.push(field_type);
            self.write_i16(id);
        }
        if let Some(last) = self.last_field_ids.last_mut() {
            *last = id;
        }
    }
}

/// A value with a Thrift type: how it is read, written and tagged on the wire.
pub trait ThriftValue: Sized {
    /// The compact type id of the value (for booleans: `BOOLEAN_TRUE`).
    const TYPE: u8;

    fn read(reader: &mut CompactReader<'_>) -> Result<Self>;

    fn write(&self, writer: &mut CompactWriter);

    /// Writes the value as field `id` of the enclosing structure.
    fn write_field(&self, writer: &mut CompactWriter, id: i16) {
        writer.write_field_begin(id, Self::TYPE);
        self.write(writer);
    }

    /// Whether a field of wire type `field_type` holds this type.
    fn accepts(field_type: u8) -> bool {
        field_type == Self::TYPE
    }
}

impl ThriftValue for bool {
    const TYPE: u8 = compact_type::BOOLEAN_TRUE;

    fn read(reader: &mut CompactReader<'_>) -> Result<Self> {
        reader.read_bool()
    }

    fn write(&self, writer: &mut CompactWriter) {
        writer.write_bool_element(*self);
    }

    fn write_field(&self, writer: &mut CompactWriter, id: i16) {
        let field_type = if *self {
            compact_type::BOOLEAN_TRUE
        } else {
            compact_type::BOOLEAN_FALSE
        };
        writer.write_field_begin(id, field_type);
    }

    fn accepts(field_type: u8) -> bool {
        matches!(
            field_type,
            compact_type::BOOLEAN_TRUE | compact_type::BOOLEAN_FALSE
        )
    }
}

macro_rules! thrift_primitive {
    ($ty:ty, $type_id:expr, $read:ident, $write:ident) => {
        impl ThriftValue for $ty {
            const TYPE: u8 = $type_id;

            fn read(reader: &mut CompactReader<'_>) -> Result<Self> {
                reader.$read()
            }

            fn write(&self, writer: &mut CompactWriter) {
                writer.$write(*self);
            }
        }
    };
}

thrift_primitive!(i8, compact_type::BYTE, read_i8, write_i8);
thrift_primitive!(i16, compact_type::I16, read_i16, write_i16);
thrift_primitive!(i32, compact_type::I32, read_i32, write_i32);
thrift_primitive!(i64, compact_type::I64, read_i64, write_i64);
thrift_primitive!(f64, compact_type::DOUBLE, read_double, write_double);

impl ThriftValue for String {
    const TYPE: u8 = compact_type::BINARY;

    fn read(reader: &mut CompactReader<'_>) -> Result<Self> {
        reader.read_string()
    }

    fn write(&self, writer: &mut CompactWriter) {
        writer.write_binary(self.as_bytes());
    }
}

/// A Thrift `binary` value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Binary(pub Vec<u8>);

impl ThriftValue for Binary {
    const TYPE: u8 = compact_type::BINARY;

    fn read(reader: &mut CompactReader<'_>) -> Result<Self> {
        reader.read_binary().map(Binary)
    }

    fn write(&self, writer: &mut CompactWriter) {
        writer.write_binary(&self.0);
    }
}

impl std::ops::Deref for Binary {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl<T: ThriftValue> ThriftValue for Vec<T> {
    const TYPE: u8 = compact_type::LIST;

    fn read(reader: &mut CompactReader<'_>) -> Result<Self> {
        // Like the generated C++ code, the element type in the header is not checked.
        let (_, size) = reader.read_list_begin()?;
        let mut result = Vec::with_capacity(size.min(1024));
        for _ in 0..size {
            result.push(T::read(reader)?);
        }
        Ok(result)
    }

    fn write(&self, writer: &mut CompactWriter) {
        writer.write_list_begin(T::TYPE, self.len());
        for value in self {
            value.write(writer);
        }
    }
}

/// Declares a Thrift enumeration: an `i32` newtype with a constant per value, which keeps
/// values outside the declared set (as the C++ code does).
macro_rules! thrift_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $value:expr),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub i32);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $variant: Self = Self($value);)*

            /// The name of the value (`EnumUtil::ToChars` / Thrift's `operator<<`), or
            /// `None` for an undeclared value.
            pub fn name(self) -> Option<&'static str> {
                match self.0 {
                    $($value => Some(stringify!($variant)),)*
                    _ => None,
                }
            }
        }

        impl $crate::thrift::ThriftValue for $name {
            const TYPE: u8 = $crate::thrift::compact_type::I32;

            fn read(reader: &mut $crate::thrift::CompactReader<'_>) -> rustdb_common::Result<Self> {
                reader.read_i32().map(Self)
            }

            fn write(&self, writer: &mut $crate::thrift::CompactWriter) {
                writer.write_i32(self.0);
            }
        }
    };
}

/// The Rust type of a field: optional fields are `Option`s.
macro_rules! thrift_field_type {
    (required, $ty:ty) => { $ty };
    (optional, $ty:ty) => { Option<$ty> };
}

/// The default value of a field.
macro_rules! thrift_field_default {
    (required) => { Default::default() };
    (required, $default:expr) => { $default };
    (optional) => { None };
    (optional, $default:expr) => { Some($default) };
}

/// Stores a read field value.
macro_rules! thrift_field_store {
    (required, $slot:expr, $value:expr) => { $slot = $value };
    (optional, $slot:expr, $value:expr) => { $slot = Some($value) };
}

/// Whether a field is required.
macro_rules! thrift_field_required {
    (required) => {
        true
    };
    (optional) => {
        false
    };
}

/// Writes a field (an optional field only when set).
macro_rules! thrift_field_write {
    (required, $writer:expr, $id:expr, $value:expr) => {
        $crate::thrift::ThriftValue::write_field(&$value, $writer, $id)
    };
    (optional, $writer:expr, $id:expr, $value:expr) => {
        if let Some(value) = &$value {
            $crate::thrift::ThriftValue::write_field(value, $writer, $id);
        }
    };
}

/// Declares a Thrift structure (or union, whose members are all optional) with its
/// reader and writer. Fields are `id: required|optional name: Type [= default]`.
macro_rules! thrift_struct {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($(#[$field_meta:meta])* $id:literal : $req:ident $field:ident : $ty:ty $(= $default:expr)?),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $crate::thrift::thrift_field_type!($req, $ty),)*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    $($field: $crate::thrift::thrift_field_default!($req $(, $default)?),)*
                }
            }
        }

        impl $crate::thrift::ThriftValue for $name {
            const TYPE: u8 = $crate::thrift::compact_type::STRUCT;

            #[allow(unused_mut, unused_variables)]
            fn read(reader: &mut $crate::thrift::CompactReader<'_>) -> rustdb_common::Result<Self> {
                let mut result = Self::default();
                let mut seen: u64 = 0;
                reader.read_struct(|reader, id, field_type| {
                    match id {
                        $($id if <$ty as $crate::thrift::ThriftValue>::accepts(field_type) => {
                            $crate::thrift::thrift_field_store!(
                                $req,
                                result.$field,
                                <$ty as $crate::thrift::ThriftValue>::read(reader)?
                            );
                            seen |= 1 << $id;
                        })*
                        _ => reader.skip(field_type)?,
                    }
                    Ok(())
                })?;
                $(
                    if $crate::thrift::thrift_field_required!($req) && seen & (1 << $id) == 0 {
                        return Err(rustdb_common::Error::new(
                            rustdb_common::ErrorKind::Invalid,
                            "TProtocolException: Invalid data",
                        ));
                    }
                )*
                Ok(result)
            }

            #[allow(unused_variables)]
            fn write(&self, writer: &mut $crate::thrift::CompactWriter) {
                writer.write_struct_begin();
                $($crate::thrift::thrift_field_write!($req, writer, $id, self.$field);)*
                writer.write_struct_end();
            }
        }
    };
}

pub(crate) use {
    thrift_enum, thrift_field_default, thrift_field_required, thrift_field_store, thrift_field_type,
    thrift_field_write, thrift_struct,
};

/// Reads one Thrift value from the start of `data`, returning it and its encoded length.
pub fn read_from<T: ThriftValue>(data: &[u8]) -> Result<(T, usize)> {
    let mut reader = CompactReader::new(data);
    let value = T::read(&mut reader)?;
    Ok((value, reader.position()))
}

/// The compact encoding of a Thrift value.
pub fn write_to_vec<T: ThriftValue>(value: &T) -> Vec<u8> {
    let mut writer = CompactWriter::new();
    value.write(&mut writer);
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_metadata() {
        let element = SchemaElement {
            type_: Some(Type::INT64),
            name: "a".to_owned(),
            repetition_type: Some(FieldRepetitionType::OPTIONAL),
            field_id: Some(300),
            logical_type: Some(LogicalType {
                integer: Some(IntType {
                    bit_width: 64,
                    is_signed: true,
                }),
                ..LogicalType::default()
            }),
            ..SchemaElement::default()
        };
        let bytes = write_to_vec(&element);
        let (read, len): (SchemaElement, usize) = read_from(&bytes).unwrap();
        assert_eq!(len, bytes.len());
        assert_eq!(read, element);
    }

    #[test]
    fn data_page_v2_compressed_defaults_to_true() {
        let header = DataPageHeaderV2::default();
        assert_eq!(header.is_compressed, Some(true));
    }

    #[test]
    fn skips_unknown_fields_and_checks_required() {
        // A KeyValue with an unknown field 7 (i32 = 5) before the key.
        let mut writer = CompactWriter::new();
        writer.write_struct_begin();
        writer.write_field_begin(7, compact_type::I32);
        writer.write_i32(5);
        writer.write_field_begin(1, compact_type::BINARY);
        writer.write_binary(b"k");
        writer.write_struct_end();
        let (value, _): (KeyValue, usize) = read_from(&writer.into_bytes()).unwrap();
        assert_eq!(value.key, "k");
        assert!(read_from::<KeyValue>(&[0]).is_err());
    }
}
