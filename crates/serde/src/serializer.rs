//! The writing half of the framework (DuckDB's `Serializer`).

use rustdb_common::Result;

use crate::{DefaultValue, FieldId, SerializationOptions};

/// A serialization backend: the format-specific hooks of DuckDB's `Serializer`.
///
/// Callers use the generic helpers on `dyn Serializer` (`write_property`,
/// `write_property_with_default`, ...) rather than these hooks; the hooks are what a
/// format implements. The trait is object safe so that generated code is compiled once
/// and shared by every format.
pub trait Serializer {
    /// The options in effect; enum and default-value handling depend on them.
    fn options(&self) -> &SerializationOptions;

    /// Starts a property that is always written.
    fn on_property_begin(&mut self, field_id: FieldId, tag: &'static str);
    /// Ends a property started by `on_property_begin`.
    fn on_property_end(&mut self) {}
    /// Starts a property that is left out when not `present` (it holds its default).
    fn on_optional_property_begin(&mut self, field_id: FieldId, tag: &'static str, present: bool);
    /// Ends a property started by `on_optional_property_begin`.
    fn on_optional_property_end(&mut self, _present: bool) {}
    /// Starts a nested object.
    fn on_object_begin(&mut self);
    /// Ends a nested object.
    fn on_object_end(&mut self);
    /// Starts a list of `count` elements.
    fn on_list_begin(&mut self, count: usize);
    /// Ends a list.
    fn on_list_end(&mut self) {}
    /// Starts a value that may be NULL (a C++ pointer); a present value follows.
    fn on_nullable_begin(&mut self, present: bool);
    /// Ends a nullable value.
    fn on_nullable_end(&mut self) {}

    /// Writes an explicit NULL.
    fn write_null(&mut self);
    /// Writes a `bool`.
    fn write_bool(&mut self, value: bool);
    /// Writes a `uint8_t`.
    fn write_u8(&mut self, value: u8);
    /// Writes an `int8_t`.
    fn write_i8(&mut self, value: i8);
    /// Writes a `uint16_t`.
    fn write_u16(&mut self, value: u16);
    /// Writes an `int16_t`.
    fn write_i16(&mut self, value: i16);
    /// Writes a `uint32_t`.
    fn write_u32(&mut self, value: u32);
    /// Writes an `int32_t`.
    fn write_i32(&mut self, value: i32);
    /// Writes a `uint64_t` / `idx_t`.
    fn write_u64(&mut self, value: u64);
    /// Writes an `int64_t`.
    fn write_i64(&mut self, value: i64);
    /// Writes a `hugeint_t`.
    fn write_i128(&mut self, value: i128);
    /// Writes a `uhugeint_t`.
    fn write_u128(&mut self, value: u128);
    /// Writes a `float`.
    fn write_f32(&mut self, value: f32);
    /// Writes a `double`.
    fn write_f64(&mut self, value: f64);
    /// Writes a string whose bytes need not be valid UTF-8 (DuckDB strings are byte strings).
    fn write_string_bytes(&mut self, value: &[u8]);
    /// Writes raw bytes (DuckDB's `WriteDataPtr`).
    fn write_data(&mut self, value: &[u8]);

    /// Writes a string.
    fn write_str(&mut self, value: &str) {
        self.write_string_bytes(value.as_bytes());
    }
}

/// A value that can be written (DuckDB's `Serializer::WriteValue` overloads).
pub trait Serialize {
    /// Writes this value at the serializer's current position.
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()>;
}

/// A class with properties (C++ classes with a `Serialize(Serializer &)` method).
///
/// `serialize_fields` writes the properties only; wrapping them in an object is the
/// caller's job (`write_object` on `dyn Serializer`), exactly as in DuckDB where
/// `WriteValue` brackets `Serialize` with `OnObjectBegin/End`.
pub trait SerializeObject {
    /// Writes the properties of this object.
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()>;
}

impl dyn Serializer + '_ {
    /// Whether a property introduced in `version` may be written.
    pub fn should_serialize(&self, version: u64) -> bool {
        self.options().should_serialize(version)
    }

    /// `WriteProperty`: a property that is always written.
    pub fn write_property<T: Serialize + ?Sized>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        value: &T,
    ) -> Result<()> {
        self.on_property_begin(field_id, tag);
        value.serialize(self)?;
        self.on_property_end();
        Ok(())
    }

    /// `WritePropertyWithDefault`: skipped when it holds the type's default value.
    pub fn write_property_with_default<T: Serialize + DefaultValue + ?Sized>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        value: &T,
    ) -> Result<()> {
        self.write_optional(field_id, tag, value.is_default(), value)
    }

    /// `WritePropertyWithDefault` with an explicit default value.
    pub fn write_property_with_explicit_default<T: Serialize + PartialEq + ?Sized>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        value: &T,
        default: &T,
    ) -> Result<()> {
        self.write_optional(field_id, tag, value == default, value)
    }

    /// `WritePropertyWithDefault` where the caller decides whether `value` is the default.
    pub fn write_optional<T: Serialize + ?Sized>(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        is_default: bool,
        value: &T,
    ) -> Result<()> {
        self.write_optional_with(field_id, tag, is_default, |serializer| {
            value.serialize(serializer)
        })
    }

    /// Like `write_optional`, with the value written by `write`.
    pub fn write_optional_with(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        is_default: bool,
        write: impl FnOnce(&mut dyn Serializer) -> Result<()>,
    ) -> Result<()> {
        if is_default && !self.options().serialize_default_values {
            self.on_optional_property_begin(field_id, tag, false);
            self.on_optional_property_end(false);
            return Ok(());
        }
        self.on_optional_property_begin(field_id, tag, true);
        write(self)?;
        self.on_optional_property_end(true);
        Ok(())
    }

    /// Writes a class as an object (`WriteValue` for types with a `Serialize` method).
    pub fn write_object<T: SerializeObject + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.on_object_begin();
        value.serialize_fields(self)?;
        self.on_object_end();
        Ok(())
    }

    /// An object whose properties `write` writes (`WriteValue` of a class, at the current
    /// position).
    pub fn write_object_with(&mut self, write: impl FnOnce(&mut dyn Serializer) -> Result<()>) -> Result<()> {
        self.on_object_begin();
        write(self)?;
        self.on_object_end();
        Ok(())
    }

    /// `WriteObject`: a property holding an object built by `write`.
    pub fn write_object_property(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        write: impl FnOnce(&mut dyn Serializer) -> Result<()>,
    ) -> Result<()> {
        self.on_property_begin(field_id, tag);
        self.on_object_begin();
        write(self)?;
        self.on_object_end();
        self.on_property_end();
        Ok(())
    }

    /// `WriteList`: a property holding `count` elements, each written by `write_element`.
    pub fn write_list_property(
        &mut self,
        field_id: FieldId,
        tag: &'static str,
        count: usize,
        mut write_element: impl FnMut(&mut dyn Serializer, usize) -> Result<()>,
    ) -> Result<()> {
        self.on_property_begin(field_id, tag);
        self.on_list_begin(count);
        for index in 0..count {
            write_element(self, index)?;
        }
        self.on_list_end();
        self.on_property_end();
        Ok(())
    }

    /// Writes a sequence as a list (`WriteValue(vector<T>)`).
    pub fn write_list<'a, T: Serialize + 'a>(
        &mut self,
        items: impl ExactSizeIterator<Item = &'a T>,
    ) -> Result<()> {
        self.on_list_begin(items.len());
        for item in items {
            item.serialize(self)?;
        }
        self.on_list_end();
        Ok(())
    }
}
