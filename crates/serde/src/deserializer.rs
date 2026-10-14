//! The reading half of the framework (DuckDB's `Deserializer`).

use rustdb_common::{Error, Result};

use crate::{DefaultValue, FieldId};

/// A deserialization backend: the format-specific hooks of DuckDB's `Deserializer`.
///
/// Callers use the generic helpers on `dyn Deserializer` (`read_property`,
/// `read_property_with_default`, ...); a format implements the hooks.
pub trait Deserializer {
    /// Whether enums are stored by name (JSON) rather than by number (binary).
    fn deserialize_enum_from_string(&self) -> bool;

    /// Starts a property that must be present.
    fn on_property_begin(&mut self, field_id: FieldId, tag: &'static str) -> Result<()>;
    /// Ends a property started by `on_property_begin`.
    fn on_property_end(&mut self) -> Result<()> {
        Ok(())
    }
    /// Starts a property that may be absent; returns whether it is present.
    fn on_optional_property_begin(&mut self, field_id: FieldId, tag: &'static str) -> Result<bool>;
    /// Ends a property started by `on_optional_property_begin`.
    fn on_optional_property_end(&mut self, _present: bool) -> Result<()> {
        Ok(())
    }
    /// Starts a nested object.
    fn on_object_begin(&mut self) -> Result<()>;
    /// Ends a nested object.
    fn on_object_end(&mut self) -> Result<()>;
    /// Starts a list and returns its length.
    fn on_list_begin(&mut self) -> Result<usize>;
    /// Ends a list.
    fn on_list_end(&mut self) -> Result<()>;
    /// Starts a nullable value; returns whether it is non-NULL.
    fn on_nullable_begin(&mut self) -> Result<bool>;
    /// Ends a nullable value.
    fn on_nullable_end(&mut self) -> Result<()>;
    /// Whether the next property is `field_id` (DuckDB's `CanDeserializeProperty`).
    fn can_deserialize_property(&mut self, field_id: FieldId, tag: &'static str) -> Result<bool>;

    /// Reads a `bool`.
    fn read_bool(&mut self) -> Result<bool>;
    /// Reads a `uint8_t`.
    fn read_u8(&mut self) -> Result<u8>;
    /// Reads an `int8_t`.
    fn read_i8(&mut self) -> Result<i8>;
    /// Reads a `uint16_t`.
    fn read_u16(&mut self) -> Result<u16>;
    /// Reads an `int16_t`.
    fn read_i16(&mut self) -> Result<i16>;
    /// Reads a `uint32_t`.
    fn read_u32(&mut self) -> Result<u32>;
    /// Reads an `int32_t`.
    fn read_i32(&mut self) -> Result<i32>;
    /// Reads a `uint64_t` / `idx_t`.
    fn read_u64(&mut self) -> Result<u64>;
    /// Reads an `int64_t`.
    fn read_i64(&mut self) -> Result<i64>;
    /// Reads a `hugeint_t`.
    fn read_i128(&mut self) -> Result<i128>;
    /// Reads a `uhugeint_t`.
    fn read_u128(&mut self) -> Result<u128>;
    /// Reads a `float`.
    fn read_f32(&mut self) -> Result<f32>;
    /// Reads a `double`.
    fn read_f64(&mut self) -> Result<f64>;
    /// Reads a string as raw bytes (DuckDB strings need not be valid UTF-8).
    fn read_string_bytes(&mut self) -> Result<Vec<u8>>;
    /// Reads raw bytes of a known length (DuckDB's `ReadDataPtr`).
    fn read_data(&mut self, len: usize) -> Result<Vec<u8>>;

    /// Reads a string that must be valid UTF-8.
    fn read_string(&mut self) -> Result<String> {
        String::from_utf8(self.read_string_bytes()?)
            .map_err(|_| Error::serialization("Failed to deserialize: string is not valid UTF-8"))
    }
}

/// A value that can be read (DuckDB's `Deserializer::Read<T>`).
pub trait Deserialize: Sized {
    /// Reads a value at the deserializer's current position.
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self>;
}

/// A class with properties (C++ classes with a static `Deserialize(Deserializer &)`).
///
/// `deserialize_fields` reads the properties of an object whose begin/end markers are
/// handled by the caller (`read_object` on `dyn Deserializer`).
pub trait DeserializeObject: Sized {
    /// Reads the properties of this object.
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self>;
}

impl dyn Deserializer + '_ {
    /// `ReadProperty`: a property that must be present.
    pub fn read_property<T: Deserialize>(&mut self, field_id: FieldId, tag: &'static str) -> Result<T> {
        self.on_property_begin(field_id, tag)?;
        let value = T::deserialize(self)?;
        self.on_property_end()?;
        Ok(value)
    }

    /// `ReadPropertyWithDefault`: an absent property yields the type's default.
    pub fn read_property_with_default<T: Deserialize + DefaultValue>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
    ) -> Result<T> {
        self.read_optional(field_id, tag)
            .map(|value| value.unwrap_or_else(T::default_value))
    }

    /// `ReadPropertyWithExplicitDefault`: an absent property yields `default`.
    pub fn read_property_with_explicit_default<T: Deserialize>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        default: T,
    ) -> Result<T> {
        self.read_optional(field_id, tag)
            .map(|value| value.unwrap_or(default))
    }

    /// Reads a property that may be absent, returning `None` if it is.
    pub fn read_optional<T: Deserialize>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
    ) -> Result<Option<T>> {
        if !self.on_optional_property_begin(field_id, tag)? {
            self.on_optional_property_end(false)?;
            return Ok(None);
        }
        let value = T::deserialize(self)?;
        self.on_optional_property_end(true)?;
        Ok(Some(value))
    }

    /// `ReadDeletedProperty`: skips a property that is no longer part of the format.
    pub fn read_deleted_property<T: Deserialize>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
    ) -> Result<()> {
        self.read_optional::<T>(field_id, tag).map(drop)
    }

    /// Reads a class wrapped in an object (`Read<T>` for types with `Deserialize`).
    pub fn read_object<T: DeserializeObject>(&mut self) -> Result<T> {
        self.on_object_begin()?;
        let value = T::deserialize_fields(self)?;
        self.on_object_end()?;
        Ok(value)
    }

    /// `ReadObject`: a property holding an object read by `read`.
    pub fn read_object_property<T>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        read: impl FnOnce(&mut dyn Deserializer) -> Result<T>,
    ) -> Result<T> {
        self.on_property_begin(field_id, tag)?;
        self.on_object_begin()?;
        let value = read(self)?;
        self.on_object_end()?;
        self.on_property_end()?;
        Ok(value)
    }

    /// `ReadList`: a property holding a list whose elements are read by `read_element`.
    pub fn read_list_property(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        read_element: impl FnMut(&mut dyn Deserializer, usize) -> Result<()>,
    ) -> Result<()> {
        self.on_property_begin(field_id, tag)?;
        self.read_list(read_element)?;
        self.on_property_end()
    }

    /// Reads a list (no property framing), calling `read_element` for each index.
    pub fn read_list(
        &mut self,
        mut read_element: impl FnMut(&mut dyn Deserializer, usize) -> Result<()>,
    ) -> Result<()> {
        let count = self.on_list_begin()?;
        for index in 0..count {
            read_element(self, index)?;
        }
        self.on_list_end()
    }
}
