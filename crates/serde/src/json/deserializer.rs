use rustdb_common::{Error, Result};

use super::{JsonValue, write_json};
use crate::{DeserializeObject, Deserializer, FieldId};

#[derive(Debug)]
struct Frame<'a> {
    value: &'a JsonValue,
    /// Position of the next element when `value` is an array.
    next: usize,
}

/// Reads a JSON document produced by DuckDB's `JsonSerializer`
/// (DuckDB's `JsonDeserializer`, including its error messages).
#[derive(Debug)]
pub struct JsonDeserializer<'a> {
    stack: Vec<Frame<'a>>,
    current_tag: &'static str,
}

impl<'a> JsonDeserializer<'a> {
    /// A deserializer positioned inside `root`, which must be the object holding the
    /// top-level properties.
    pub fn new(root: &'a JsonValue) -> Self {
        Self {
            stack: vec![Frame { value: root, next: 0 }],
            current_tag: "",
        }
    }

    /// Reads the properties of `root` as a `T` (DuckDB's `T::Deserialize(json_deserializer)`).
    pub fn deserialize<T: DeserializeObject>(root: &'a JsonValue) -> Result<T> {
        let mut deserializer = Self::new(root);
        T::deserialize_fields(&mut deserializer)
    }

    fn current(&self) -> Result<&Frame<'a>> {
        self.stack
            .last()
            .ok_or_else(|| Error::internal("JSON deserializer stack is empty"))
    }

    /// The value under the current tag (objects) or the next element (arrays).
    fn next_value(&mut self) -> Result<&'a JsonValue> {
        let tag = self.current_tag;
        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| Error::internal("JSON deserializer stack is empty"))?;
        match frame.value {
            JsonValue::Object(_) => frame.value.get(tag).ok_or_else(|| {
                Error::parser(format!(
                    "Expected but did not find property '{tag}' in json object: '{}'",
                    write_json(frame.value, false)
                ))
            }),
            JsonValue::Array(items) => {
                let value = items.get(frame.next).ok_or_else(|| {
                    Error::parser(format!(
                        "Expected but did not find another value after exhausting json array: '{}'",
                        write_json(frame.value, false)
                    ))
                })?;
                frame.next += 1;
                Ok(value)
            }
            _ => Err(Error::internal("Cannot get value from non-array/object")),
        }
    }

    fn type_error(&self, value: &JsonValue, expected: &str) -> Error {
        let actual = value.type_description();
        match self.stack.last().map(|frame| frame.value) {
            Some(JsonValue::Object(_)) => Error::parser(format!(
                "property '{}' expected type '{expected}', but got type: '{actual}'",
                self.current_tag
            )),
            Some(JsonValue::Array(_)) => Error::parser(format!(
                "Sequence expect child of type '{expected}', but got type: {actual}"
            )),
            _ => Error::internal("cannot get nested value from non object or array-type"),
        }
    }

    /// An integer accepted where yyjson's `yyjson_is_int` holds, as `yyjson_get_sint`.
    fn read_signed(&mut self, expected: &str) -> Result<i64> {
        let value = self.next_value()?;
        match *value {
            JsonValue::SInt(v) => Ok(v),
            JsonValue::UInt(v) => Ok(v as i64),
            _ => Err(self.type_error(value, expected)),
        }
    }

    /// An integer accepted where `yyjson_is_uint` holds.
    fn read_unsigned(&mut self, expected: &str) -> Result<u64> {
        let value = self.next_value()?;
        match *value {
            JsonValue::UInt(v) => Ok(v),
            _ => Err(self.type_error(value, expected)),
        }
    }

    fn read_real(&mut self, expected: &str) -> Result<f64> {
        let value = self.next_value()?;
        match *value {
            JsonValue::Real(v) => Ok(v),
            _ => Err(self.type_error(value, expected)),
        }
    }

    fn read_str(&mut self) -> Result<&'a str> {
        let value = self.next_value()?;
        match value {
            JsonValue::Str(s) => Ok(s),
            JsonValue::ByteStr(s) => s
                .to_str()
                .map_err(|_| Error::serialization("Failed to deserialize: string is not valid UTF-8")),
            _ => Err(self.type_error(value, "string")),
        }
    }

    fn enter(&mut self, expected: &str, check: fn(&JsonValue) -> bool) -> Result<&'a JsonValue> {
        let value = self.next_value()?;
        if !check(value) {
            return Err(self.type_error(value, expected));
        }
        self.stack.push(Frame { value, next: 0 });
        Ok(value)
    }

    fn leave(&mut self) {
        self.stack.pop();
    }

    fn has_property(&self, tag: &str) -> Result<bool> {
        Ok(self.current()?.value.get(tag).is_some())
    }
}

impl Deserializer for JsonDeserializer<'_> {
    fn deserialize_enum_from_string(&self) -> bool {
        true
    }

    fn on_property_begin(&mut self, _field_id: FieldId, tag: &'static str) -> Result<()> {
        self.current_tag = tag;
        Ok(())
    }

    fn on_optional_property_begin(&mut self, _field_id: FieldId, tag: &'static str) -> Result<bool> {
        let present = self.has_property(tag)?;
        if present {
            self.current_tag = tag;
        }
        Ok(present)
    }

    fn on_object_begin(&mut self) -> Result<()> {
        self.enter("object", JsonValue::is_object).map(drop)
    }

    fn on_object_end(&mut self) -> Result<()> {
        self.leave();
        Ok(())
    }

    fn on_list_begin(&mut self) -> Result<usize> {
        let value = self.enter("array", JsonValue::is_array)?;
        Ok(match value {
            JsonValue::Array(items) => items.len(),
            _ => 0,
        })
    }

    fn on_list_end(&mut self) -> Result<()> {
        self.leave();
        Ok(())
    }

    fn on_nullable_begin(&mut self) -> Result<bool> {
        // Peek: only a NULL is consumed here; a present value is read by the caller.
        let frame = self.current()?;
        let saved = frame.next;
        let value = self.next_value()?;
        let present = !matches!(value, JsonValue::Null);
        if present && let Some(frame) = self.stack.last_mut() {
            frame.next = saved;
        }
        Ok(present)
    }

    fn on_nullable_end(&mut self) -> Result<()> {
        Ok(())
    }

    fn can_deserialize_property(&mut self, _field_id: FieldId, tag: &'static str) -> Result<bool> {
        self.has_property(tag)
    }

    fn read_bool(&mut self) -> Result<bool> {
        let value = self.next_value()?;
        match *value {
            JsonValue::Bool(v) => Ok(v),
            _ => Err(self.type_error(value, "bool")),
        }
    }

    fn read_u8(&mut self) -> Result<u8> {
        self.read_unsigned("uint8_t").map(|v| v as u8)
    }

    fn read_i8(&mut self) -> Result<i8> {
        self.read_signed("int8_t").map(|v| v as i8)
    }

    fn read_u16(&mut self) -> Result<u16> {
        self.read_unsigned("uint16_t").map(|v| v as u16)
    }

    fn read_i16(&mut self) -> Result<i16> {
        self.read_signed("int16_t").map(|v| v as i16)
    }

    fn read_u32(&mut self) -> Result<u32> {
        self.read_unsigned("uint32_t").map(|v| v as u32)
    }

    fn read_i32(&mut self) -> Result<i32> {
        self.read_signed("int32_t").map(|v| v as i32)
    }

    fn read_u64(&mut self) -> Result<u64> {
        self.read_unsigned("uint64_t")
    }

    fn read_i64(&mut self) -> Result<i64> {
        self.read_signed("int64_t")
    }

    fn read_i128(&mut self) -> Result<i128> {
        self.enter("object", JsonValue::is_object)?;
        self.current_tag = "upper";
        let upper = self.read_i64()?;
        self.current_tag = "lower";
        let lower = self.read_u64()?;
        self.leave();
        Ok((i128::from(upper) << 64) | i128::from(lower))
    }

    fn read_u128(&mut self) -> Result<u128> {
        self.enter("object", JsonValue::is_object)?;
        self.current_tag = "upper";
        let upper = self.read_u64()?;
        self.current_tag = "lower";
        let lower = self.read_u64()?;
        self.leave();
        Ok((u128::from(upper) << 64) | u128::from(lower))
    }

    fn read_f32(&mut self) -> Result<f32> {
        self.read_real("float").map(|v| v as f32)
    }

    fn read_f64(&mut self) -> Result<f64> {
        self.read_real("double")
    }

    fn read_string_bytes(&mut self) -> Result<Vec<u8>> {
        let value = self.next_value()?;
        match value {
            JsonValue::Str(s) => Ok(s.as_bytes().to_vec()),
            JsonValue::ByteStr(s) => Ok(s.as_bytes().to_vec()),
            _ => Err(self.type_error(value, "string")),
        }
    }

    fn read_string(&mut self) -> Result<String> {
        self.read_str().map(str::to_owned)
    }

    fn read_data(&mut self, len: usize) -> Result<Vec<u8>> {
        // DuckDB writes data pointers as escaped blob text (`Blob::ToString`).
        let bytes = rustdb_common::blob::from_string(self.read_str()?.as_bytes())?;
        if bytes.len() != len {
            return Err(Error::serialization(format!(
                "Tried to read blob of {len} size, but only {} elements are available",
                bytes.len()
            )));
        }
        Ok(bytes)
    }
}
