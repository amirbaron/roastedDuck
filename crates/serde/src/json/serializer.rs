use rustdb_common::{Error, Result};

use super::JsonValue;
use crate::{FieldId, SerializationOptions, SerializeObject, Serializer};

/// The knobs of `json_serialize_sql(sql, skip_null := .., skip_empty := .., skip_default := ..)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonOptions {
    /// Omit properties whose value is NULL.
    pub skip_null: bool,
    /// Omit empty strings, lists and objects.
    pub skip_empty: bool,
    /// Omit properties holding their default value.
    pub skip_default: bool,
}

/// Where a finished container goes once it is closed.
#[derive(Debug)]
enum Slot {
    /// Appended to the enclosing array, or added to the enclosing object under this tag.
    Attach(&'static str),
    /// An empty list skipped because of `skip_empty`; still tracked so nesting stays balanced.
    Detached,
    /// The document root.
    Root,
}

#[derive(Debug)]
struct Frame {
    value: JsonValue,
    slot: Slot,
    /// Objects opened through `on_object_begin` are dropped when empty under `skip_empty`.
    removable: bool,
}

/// Builds the JSON document DuckDB's `JsonSerializer` produces (on top of yyjson).
///
/// Containers are attached to their parent when they are closed; this is equivalent to
/// yyjson attaching them when opened because nothing else is written to the parent in
/// between, and it makes dropping empty objects (`skip_empty`) a matter of not attaching.
#[derive(Debug)]
pub struct JsonSerializer {
    options: SerializationOptions,
    skip_null: bool,
    skip_empty: bool,
    stack: Vec<Frame>,
    current_tag: &'static str,
    invalid_utf8: bool,
}

impl JsonSerializer {
    /// A serializer whose root is an empty object; enums are written by name.
    pub fn new(json: JsonOptions, mut options: SerializationOptions) -> Self {
        options.serialize_enum_as_string = true;
        options.serialize_default_values = !json.skip_default;
        Self {
            options,
            skip_null: json.skip_null,
            skip_empty: json.skip_empty,
            stack: vec![Frame {
                value: JsonValue::Object(Vec::new()),
                slot: Slot::Root,
                removable: false,
            }],
            current_tag: "",
            invalid_utf8: false,
        }
    }

    /// Serializes the properties of `value` into a root object
    /// (DuckDB's `JsonSerializer::Serialize`).
    pub fn serialize<T: SerializeObject + ?Sized>(
        value: &T,
        json: JsonOptions,
        options: SerializationOptions,
    ) -> Result<JsonValue> {
        let mut serializer = Self::new(json, options);
        value.serialize_fields(&mut serializer)?;
        serializer.finish()
    }

    /// The finished root object.
    pub fn finish(mut self) -> Result<JsonValue> {
        if self.invalid_utf8 {
            return Err(Error::serialization(
                "Failed to serialize json, perhaps the query contains invalid utf8 characters?",
            ));
        }
        match self.stack.pop() {
            Some(frame) if self.stack.is_empty() => Ok(frame.value),
            _ => Err(Error::internal(
                "JSON serializer finished with unbalanced nesting",
            )),
        }
    }

    fn push_value(&mut self, value: JsonValue) {
        let tag = self.current_tag;
        if let Some(frame) = self.stack.last_mut() {
            match &mut frame.value {
                JsonValue::Array(items) => items.push(value),
                JsonValue::Object(entries) => entries.push((tag.to_owned(), value)),
                _ => {}
            }
        }
    }

    fn open(&mut self, value: JsonValue, slot: Slot, removable: bool) {
        self.stack.push(Frame {
            value,
            slot,
            removable,
        });
    }

    fn close(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let empty = matches!(&frame.value, JsonValue::Object(entries) if entries.is_empty());
        if frame.removable && empty && self.skip_empty {
            return;
        }
        if let Slot::Attach(tag) = frame.slot {
            let saved = std::mem::replace(&mut self.current_tag, tag);
            self.push_value(frame.value);
            self.current_tag = saved;
        }
    }
}

impl Serializer for JsonSerializer {
    fn options(&self) -> &SerializationOptions {
        &self.options
    }

    fn on_property_begin(&mut self, _field_id: FieldId, tag: &'static str) {
        self.current_tag = tag;
    }

    fn on_optional_property_begin(&mut self, _field_id: FieldId, tag: &'static str, _present: bool) {
        self.current_tag = tag;
    }

    fn on_object_begin(&mut self) {
        let tag = self.current_tag;
        self.open(JsonValue::Object(Vec::new()), Slot::Attach(tag), true);
    }

    fn on_object_end(&mut self) {
        self.close();
    }

    fn on_list_begin(&mut self, count: usize) {
        let slot = if count == 0 && self.skip_empty {
            Slot::Detached
        } else {
            Slot::Attach(self.current_tag)
        };
        self.open(JsonValue::Array(Vec::new()), slot, false);
    }

    fn on_list_end(&mut self) {
        self.close();
    }

    fn on_nullable_begin(&mut self, present: bool) {
        if !present && !self.skip_null {
            self.write_null();
        }
    }

    fn write_null(&mut self) {
        if !self.skip_null {
            self.push_value(JsonValue::Null);
        }
    }

    fn write_bool(&mut self, value: bool) {
        self.push_value(JsonValue::Bool(value));
    }

    fn write_u8(&mut self, value: u8) {
        self.push_value(JsonValue::UInt(value.into()));
    }

    fn write_i8(&mut self, value: i8) {
        self.push_value(JsonValue::SInt(value.into()));
    }

    fn write_u16(&mut self, value: u16) {
        self.push_value(JsonValue::UInt(value.into()));
    }

    fn write_i16(&mut self, value: i16) {
        self.push_value(JsonValue::SInt(value.into()));
    }

    fn write_u32(&mut self, value: u32) {
        self.push_value(JsonValue::UInt(value.into()));
    }

    fn write_i32(&mut self, value: i32) {
        self.push_value(JsonValue::SInt(value.into()));
    }

    fn write_u64(&mut self, value: u64) {
        self.push_value(JsonValue::UInt(value));
    }

    fn write_i64(&mut self, value: i64) {
        self.push_value(JsonValue::SInt(value));
    }

    fn write_i128(&mut self, value: i128) {
        let tag = self.current_tag;
        self.open(JsonValue::Object(Vec::new()), Slot::Attach(tag), false);
        self.current_tag = "upper";
        self.write_i64((value >> 64) as i64);
        self.current_tag = "lower";
        self.write_u64(value as u64);
        self.close();
    }

    fn write_u128(&mut self, value: u128) {
        let tag = self.current_tag;
        self.open(JsonValue::Object(Vec::new()), Slot::Attach(tag), false);
        self.current_tag = "upper";
        self.write_u64((value >> 64) as u64);
        self.current_tag = "lower";
        self.write_u64(value as u64);
        self.close();
    }

    fn write_f32(&mut self, value: f32) {
        self.push_value(JsonValue::Real(value.into()));
    }

    fn write_f64(&mut self, value: f64) {
        self.push_value(JsonValue::Real(value));
    }

    fn write_string_bytes(&mut self, value: &[u8]) {
        if self.skip_empty && value.is_empty() {
            return;
        }
        if let Ok(text) = std::str::from_utf8(value) {
            self.push_value(JsonValue::Str(text.to_owned()));
        } else {
            self.invalid_utf8 = true;
            self.push_value(JsonValue::Str(String::from_utf8_lossy(value).into_owned()));
        }
    }

    fn write_data(&mut self, value: &[u8]) {
        let text = rustdb_common::blob::to_string(value);
        self.push_value(JsonValue::Str(text));
    }
}
