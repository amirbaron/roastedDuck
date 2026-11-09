//! The hand-written type infos (DuckDB's `custom_implementation` classes in `types.json`)
//! and the serialization of [`LogicalType`] itself.

use roastedduck_serde::{
    Deserialize, DeserializeObject, Deserializer, Error, Result, SerializeObject, Serializer,
};

use crate::unbound::UNBOUND_EXPRESSION_VERSION;
use crate::{ExtraTypeInfo, LogicalType, LogicalTypeId, PhysicalType, TypeInfo};

/// The members of an ENUM type, in definition order (DuckDB's `EnumTypeInfo`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnumTypeInfo {
    /// The member names; a member's position is its stored value.
    pub values: Vec<roastedduck_common::ByteString>,
}

impl EnumTypeInfo {
    /// The integer type storing positions into the dictionary (`EnumTypeInfo::DictType`).
    pub fn dictionary_type(&self) -> PhysicalType {
        match self.values.len() {
            0..=0xFF => PhysicalType::UInt8,
            0x100..=0xFFFF => PhysicalType::UInt16,
            _ => PhysicalType::UInt32,
        }
    }
}

impl SerializeObject for EnumTypeInfo {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(200, "values_count", &(self.values.len() as u64))?;
        serializer.write_list_property(201, "values", self.values.len(), |serializer, i| {
            serializer.write_string_bytes(&self.values[i]);
            Ok(())
        })
    }
}

impl DeserializeObject for EnumTypeInfo {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let count: u64 = deserializer.read_property(200, "values_count")?;
        if count > u64::from(u32::MAX) {
            return Err(Error::internal(format!(
                "Enum size must be lower than {}",
                u32::MAX
            )));
        }
        let mut values = Vec::with_capacity(count.min(4096) as usize);
        deserializer.read_list_property(201, "values", |deserializer, _| {
            values.push(deserializer.read_string_bytes()?.into());
            Ok(())
        })?;
        Ok(Self { values })
    }
}

/// A coordinate reference system of a GEOMETRY type (DuckDB's `CoordinateReferenceSystem`).
///
/// Only the definition text is kept; DuckDB derives the CRS kind by parsing it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CoordinateReferenceSystem {
    pub definition: String,
}

impl SerializeObject for CoordinateReferenceSystem {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property_with_explicit_default(100, "definition", &self.definition, &String::new())
    }
}

impl DeserializeObject for CoordinateReferenceSystem {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let definition: String =
            deserializer.read_property_with_explicit_default(100, "definition", String::new())?;
        // `CoordinateReferenceSystem::Deserialize`: parsed again; unparsable is invalid.
        let definition = crate::geometry::Crs::parse_lenient(&definition).definition;
        Ok(Self { definition })
    }
}

roastedduck_serde::impl_object_serde!(CoordinateReferenceSystem);

/// The version from which GEOMETRY is written as itself rather than as the spatial
/// extension's `BLOB` alias.
const GEOMETRY_VERSION: u64 = 7;

/// The spatial extension's legacy geometry type (`Geometry::GetSpatialGeometryType`).
fn legacy_geometry_type() -> LogicalType {
    LogicalType::BLOB.with_alias("GEOMETRY")
}

impl SerializeObject for LogicalType {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        if self.id() == LogicalTypeId::Geometry && !serializer.should_serialize(GEOMETRY_VERSION) {
            // Drops any CRS, like DuckDB.
            return legacy_geometry_type().serialize_fields(serializer);
        }
        if self.id() == LogicalTypeId::Unbound
            && !serializer.should_serialize(UNBOUND_EXPRESSION_VERSION)
            && let Some(TypeInfo::Unbound(unbound)) = self.info()
        {
            // Older readers know builtin types only bound; other names (and any binding
            // error) fall back to the legacy `USER` form below, like DuckDB.
            if let Ok(bound) = unbound.default_bind()
                && !matches!(bound.id(), LogicalTypeId::Invalid | LogicalTypeId::Unbound)
            {
                return bound.serialize_fields(serializer);
            }
        }
        serializer.write_property(100, "id", &self.id())?;
        let info = self.extra_info();
        serializer.write_optional_with(101, "type_info", info.is_none(), |serializer| {
            serializer.on_nullable_begin(info.is_some());
            if let Some(info) = info {
                serializer.write_object(info)?;
            }
            serializer.on_nullable_end();
            Ok(())
        })
    }
}

/// The nullable `shared_ptr<ExtraTypeInfo>` property of a type; an `INVALID_TYPE_INFO`
/// reads as no info at all.
struct TypeInfoProperty(Option<ExtraTypeInfo>);

impl Deserialize for TypeInfoProperty {
    fn deserialize(deserializer: &mut dyn Deserializer) -> Result<Self> {
        if !deserializer.on_nullable_begin()? {
            deserializer.on_nullable_end()?;
            return Ok(Self(None));
        }
        deserializer.on_object_begin()?;
        let info = ExtraTypeInfo::deserialize_fields(deserializer)?;
        deserializer.on_object_end()?;
        deserializer.on_nullable_end()?;
        Ok(Self(info))
    }
}

impl DeserializeObject for LogicalType {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let id: LogicalTypeId = deserializer.read_property(100, "id")?;
        let info = deserializer
            .read_optional::<TypeInfoProperty>(101, "type_info")?
            .and_then(|property| property.0);
        let ty = Self::from_parts(id, info);
        if ty.id() == LogicalTypeId::Blob && ty.alias().is_some_and(|alias| alias == "GEOMETRY") {
            return Ok(Self::GEOMETRY);
        }
        Ok(ty)
    }
}

roastedduck_serde::impl_object_serde!(LogicalType);

#[cfg(test)]
mod tests {
    use roastedduck_serde::SerializationOptions;
    use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};

    use super::*;
    use crate::TypeInfo;

    fn round_trip(ty: &LogicalType, options: SerializationOptions) -> LogicalType {
        let bytes = BinarySerializer::serialize(ty, options).unwrap();
        BinaryDeserializer::deserialize(&bytes).unwrap()
    }

    #[test]
    fn types_round_trip_in_binary() {
        let enum_type = LogicalType::with_info(
            LogicalTypeId::Enum,
            TypeInfo::Enum(EnumTypeInfo {
                values: vec!["a".into(), "b".into()],
            }),
        );
        for ty in [
            LogicalType::INTEGER,
            LogicalType::decimal(18, 3),
            LogicalType::list(LogicalType::VARCHAR),
            LogicalType::map(LogicalType::INTEGER, LogicalType::VARCHAR),
            LogicalType::array(LogicalType::DOUBLE, 3),
            LogicalType::VARCHAR.with_alias("JSON"),
            enum_type,
        ] {
            assert_eq!(round_trip(&ty, SerializationOptions::default()), ty);
        }
    }

    #[test]
    fn decimal_bytes_match_duckdb() {
        // id=21 (DECIMAL), type_info { type=1 (DECIMAL_TYPE_INFO), width=18, scale=3 }
        let bytes =
            BinarySerializer::serialize(&LogicalType::decimal(18, 3), SerializationOptions::default())
                .unwrap();
        assert_eq!(
            bytes,
            [
                0x64, 0x00, 21, 0x65, 0x00, 1, 0x64, 0x00, 2, 0xC8, 0x00, 18, 0xC9, 0x00, 3, 0xFF, 0xFF,
                0xFF, 0xFF
            ]
        );
    }

    #[test]
    fn geometry_uses_the_legacy_alias_for_old_versions() {
        let bytes =
            BinarySerializer::serialize(&LogicalType::GEOMETRY, SerializationOptions::default()).unwrap();
        let read: LogicalType = BinaryDeserializer::deserialize(&bytes).unwrap();
        assert_eq!(read, LogicalType::GEOMETRY);
        let legacy: LogicalType = BinaryDeserializer::deserialize(
            &BinarySerializer::serialize(&legacy_geometry_type(), SerializationOptions::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(legacy, LogicalType::GEOMETRY);
    }
}
