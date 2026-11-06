//! `Value::Serialize` / `Value::Deserialize` (`src/common/types/value.cpp`).

use rustdb_common::blob;
use rustdb_serde::{DeserializeObject, Deserializer, Error, Result, SerializeObject, Serializer};

use super::{Payload, Value};
use crate::{GeometryStorageType, Interval, LogicalType, LogicalTypeId, PhysicalType};

/// From this version on, children of nested values omit their (implied) types.
const IMPLIED_CHILD_TYPES_VERSION: u64 = 4;

/// From this version on, GEOMETRY values are stored as WKB instead of the spatial
/// extension's format.
const WKB_GEOMETRY_VERSION: u64 = 7;

/// The type the `index`-th child of a nested value must have.
fn child_type(parent: &LogicalType, index: usize) -> Result<&LogicalType> {
    let child = match parent.physical_type() {
        PhysicalType::List | PhysicalType::Array => parent.child_type(),
        PhysicalType::Struct => parent
            .struct_children()
            .and_then(|c| c.get(index))
            .map(|(_, ty)| ty),
        _ => None,
    };
    child.ok_or_else(|| Error::internal("Parent type is not a nested type"))
}

/// Whether a child value's type is the one its parent implies (`SerializeTypeMatches`).
fn type_matches(expected: &LogicalType, actual: &LogicalType) -> bool {
    expected.id() == actual.id() && (expected.is_nested() || expected == actual)
}

impl Value {
    /// `SerializeInternal`: only the root of a nested value needs to carry its type.
    fn serialize_internal(&self, serializer: &mut dyn Serializer, serialize_type: bool) -> Result<()> {
        if serialize_type || !serializer.should_serialize(IMPLIED_CHILD_TYPES_VERSION) {
            serializer.write_property(100, "type", &self.ty)?;
        }
        serializer.write_property(101, "is_null", &self.is_null())?;
        match &self.payload {
            Payload::Null => Ok(()),
            Payload::Type(ty) => serializer.write_property(102, "value", ty),
            Payload::Bool(v) => serializer.write_property(102, "value", v),
            Payload::I8(v) => serializer.write_property(102, "value", v),
            Payload::I16(v) => serializer.write_property(102, "value", v),
            Payload::I32(v) => serializer.write_property(102, "value", v),
            Payload::I64(v) => serializer.write_property(102, "value", v),
            Payload::I128(v) => serializer.write_property(102, "value", v),
            Payload::U8(v) => serializer.write_property(102, "value", v),
            Payload::U16(v) => serializer.write_property(102, "value", v),
            Payload::U32(v) => serializer.write_property(102, "value", v),
            Payload::U64(v) => serializer.write_property(102, "value", v),
            Payload::U128(v) => serializer.write_property(102, "value", v),
            Payload::F32(v) => serializer.write_property(102, "value", v),
            Payload::F64(v) => serializer.write_property(102, "value", v),
            Payload::Interval(v) => serializer.write_property(102, "value", v),
            Payload::Bytes(bytes) => self.serialize_bytes(serializer, bytes),
            Payload::Nested(children) => self.serialize_children(serializer, children),
        }
    }

    fn serialize_bytes(&self, serializer: &mut dyn Serializer, bytes: &[u8]) -> Result<()> {
        match self.ty.id() {
            LogicalTypeId::Blob => serializer.write_property(102, "value", &blob::to_string(bytes)),
            LogicalTypeId::Geometry => {
                if !serializer.should_serialize(WKB_GEOMETRY_VERSION) {
                    // Older versions get the spatial extension's legacy format.
                    let legacy = crate::geometry::to_spatial(bytes)?;
                    return serializer.write_property(102, "value", &blob::to_string(&legacy));
                }
                serializer.write_property(102, "value", &blob::to_string(bytes))?;
                serializer.write_property(103, "geometry_format", &GeometryStorageType::Wkb)
            }
            _ => {
                serializer.on_property_begin(102, "value");
                serializer.write_string_bytes(bytes);
                serializer.on_property_end();
                Ok(())
            }
        }
    }

    fn serialize_children(&self, serializer: &mut dyn Serializer, children: &[Self]) -> Result<()> {
        serializer.write_object_property(102, "value", |serializer| {
            serializer.write_list_property(100, "children", children.len(), |serializer, i| {
                let expected = child_type(&self.ty, i)?;
                let serialize_type = expected.physical_type() == PhysicalType::Invalid;
                let child = &children[i];
                if !serialize_type && !type_matches(expected, &child.ty) {
                    return Err(Error::internal(format!(
                        "Error when serializing type - serializing a child of a nested value with type {}, \
                         but expected type {expected}",
                        child.ty
                    )));
                }
                serializer.on_object_begin();
                child.serialize_internal(serializer, serialize_type)?;
                serializer.on_object_end();
                Ok(())
            })
        })
    }

    /// Reads a value; `implied_type` is the type a nested parent dictates for it, used
    /// when the value itself carries none.
    pub fn deserialize_fields_with_type(
        deserializer: &mut dyn Deserializer,
        implied_type: Option<&LogicalType>,
    ) -> Result<Self> {
        let mut ty: LogicalType =
            deserializer.read_property_with_explicit_default(100, "type", LogicalType::INVALID)?;
        if ty.physical_type() == PhysicalType::Invalid {
            ty = implied_type
                .cloned()
                .ok_or_else(|| Error::serialization("Failed to deserialize: value without a type"))?;
        }
        let is_null: bool = deserializer.read_property(101, "is_null")?;
        if is_null {
            return Ok(Self::null(ty));
        }
        if ty.id() == LogicalTypeId::Type {
            let inner: LogicalType = deserializer.read_property(102, "value")?;
            return Ok(Self::type_value(inner));
        }
        let payload = match ty.physical_type() {
            PhysicalType::Bit => return Err(Error::internal("BIT type should not be deserialized")),
            PhysicalType::Bool => Payload::Bool(deserializer.read_property(102, "value")?),
            PhysicalType::UInt8 => Payload::U8(deserializer.read_property(102, "value")?),
            PhysicalType::Int8 => Payload::I8(deserializer.read_property(102, "value")?),
            PhysicalType::UInt16 => Payload::U16(deserializer.read_property(102, "value")?),
            PhysicalType::Int16 => Payload::I16(deserializer.read_property(102, "value")?),
            PhysicalType::UInt32 => Payload::U32(deserializer.read_property(102, "value")?),
            PhysicalType::Int32 => Payload::I32(deserializer.read_property(102, "value")?),
            PhysicalType::UInt64 => Payload::U64(deserializer.read_property(102, "value")?),
            PhysicalType::Int64 => Payload::I64(deserializer.read_property(102, "value")?),
            PhysicalType::UInt128 => Payload::U128(deserializer.read_property(102, "value")?),
            PhysicalType::Int128 => Payload::I128(deserializer.read_property(102, "value")?),
            PhysicalType::Float => Payload::F32(deserializer.read_property(102, "value")?),
            PhysicalType::Double => Payload::F64(deserializer.read_property(102, "value")?),
            PhysicalType::Interval => {
                Payload::Interval(deserializer.read_property::<Interval>(102, "value")?)
            }
            PhysicalType::Varchar => Payload::Bytes(Self::deserialize_bytes(deserializer, &ty)?.into()),
            PhysicalType::List | PhysicalType::Struct | PhysicalType::Array => {
                let children = Self::deserialize_children(deserializer, &ty)?;
                return Ok(Self::from_children(ty, children));
            }
            PhysicalType::Unknown | PhysicalType::Invalid => {
                return Err(Error::not_implemented("Unimplemented type for Deserialize"));
            }
        };
        Ok(Self { ty, payload })
    }

    fn deserialize_bytes(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Vec<u8>> {
        deserializer.on_property_begin(102, "value")?;
        let text = deserializer.read_string_bytes()?;
        deserializer.on_property_end()?;
        match ty.id() {
            LogicalTypeId::Blob => blob::from_string(&text),
            LogicalTypeId::Geometry => {
                let format = deserializer.read_property_with_explicit_default(
                    103,
                    "geometry_format",
                    GeometryStorageType::Spatial,
                )?;
                match format {
                    GeometryStorageType::Wkb => blob::from_string(&text),
                    GeometryStorageType::Spatial => crate::geometry::from_spatial(&blob::from_string(&text)?),
                    _ => Err(Error::internal(
                        "Unknown geometry format in value deserialization",
                    )),
                }
            }
            _ => Ok(text),
        }
    }

    fn deserialize_children(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Vec<Self>> {
        deserializer.read_object_property(102, "value", |deserializer| {
            let mut children = Vec::new();
            deserializer.read_list_property(100, "children", |deserializer, i| {
                let expected = child_type(ty, i)?;
                deserializer.on_object_begin()?;
                children.push(Self::deserialize_fields_with_type(deserializer, Some(expected))?);
                deserializer.on_object_end()
            })?;
            Ok(children)
        })
    }
}

impl SerializeObject for Value {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        self.serialize_internal(serializer, true)
    }
}

impl DeserializeObject for Value {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        Self::deserialize_fields_with_type(deserializer, None)
    }
}

rustdb_serde::impl_object_serde!(Value);

#[cfg(test)]
mod tests {
    use rustdb_serde::SerializationOptions;
    use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
    use rustdb_serde::json::{JsonDeserializer, JsonOptions, JsonSerializer, write_json};

    use super::*;

    fn json(value: &Value) -> String {
        let tree = JsonSerializer::serialize(value, JsonOptions::default(), SerializationOptions::default())
            .unwrap();
        write_json(&tree, false)
    }

    #[test]
    fn json_matches_duckdb() {
        assert_eq!(
            json(&Value::integer(1)),
            r#"{"type":{"id":"INTEGER","type_info":null},"is_null":false,"value":1}"#
        );
        assert_eq!(
            json(&Value::decimal(15, 2, 1)),
            r#"{"type":{"id":"DECIMAL","type_info":{"type":"DECIMAL_TYPE_INFO","alias":"","extension_info":null,"width":2,"scale":1}},"is_null":false,"value":15}"#
        );
        assert_eq!(
            json(&Value::hugeint(-1)),
            r#"{"type":{"id":"HUGEINT","type_info":null},"is_null":false,"value":{"upper":-1,"lower":18446744073709551615}}"#
        );
        assert_eq!(
            json(&Value::null(LogicalType::SQLNULL)),
            r#"{"type":{"id":"NULL","type_info":null},"is_null":true}"#
        );
    }

    #[test]
    fn nested_values_round_trip() {
        let value = Value::list(
            LogicalType::INTEGER,
            vec![Value::integer(1), Value::null(LogicalType::INTEGER)],
        );
        let options = SerializationOptions::default();
        let bytes = BinarySerializer::serialize(&value, options.clone()).unwrap();
        let read: Value = BinaryDeserializer::deserialize(&bytes).unwrap();
        assert_eq!(read, value);

        let latest = SerializationOptions {
            compatibility: rustdb_serde::SerializationCompatibility::latest(),
            ..options
        };
        let bytes = BinarySerializer::serialize(&value, latest).unwrap();
        let read: Value = BinaryDeserializer::deserialize(&bytes).unwrap();
        assert_eq!(read, value);

        let tree = JsonSerializer::serialize(&value, JsonOptions::default(), SerializationOptions::default())
            .unwrap();
        let read: Value = JsonDeserializer::deserialize(&tree).unwrap();
        assert_eq!(read, value);
    }

    #[test]
    fn blobs_use_escaped_text() {
        let value = Value::blob(&[0, b'a']);
        assert_eq!(
            json(&value),
            r#"{"type":{"id":"BLOB","type_info":null},"is_null":false,"value":"\\x00a"}"#
        );
    }
}
