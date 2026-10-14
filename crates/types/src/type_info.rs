//! The hand-written type infos (DuckDB's `custom_implementation` classes in `types.json`)
//! and the serialization of [`LogicalType`] itself.

use rustdb_serde::{
    Deserialize, DeserializeObject, Deserializer, Error, OptionalIdx, Result, SerializeObject, Serializer,
    write_enum,
};

use crate::{ExtraTypeInfo, LogicalType, LogicalTypeId, PhysicalType, Value};

/// The members of an ENUM type, in definition order (DuckDB's `EnumTypeInfo`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnumTypeInfo {
    pub values: Vec<String>,
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
            serializer.write_str(&self.values[i]);
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
            values.push(deserializer.read_string()?);
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
        let definition =
            deserializer.read_property_with_explicit_default(100, "definition", String::new())?;
        Ok(Self { definition })
    }
}

rustdb_serde::impl_object_serde!(CoordinateReferenceSystem);

/// A type name the parser could not resolve yet (DuckDB's `UnboundTypeInfo` holding a
/// `TypeExpression`): `catalog.schema.name(parameters)`, bound later by the binder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnboundTypeInfo {
    pub catalog: String,
    pub schema: String,
    pub name: String,
    pub parameters: Vec<TypeParameter>,
    /// Position of the type name in the query text.
    pub query_location: Option<u64>,
}

/// One parameter of an unbound type: `DECIMAL(18, 3)`, `STRUCT(a INTEGER)`, `MAP(k := INT, ...)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeParameter {
    /// The parameter name (the field name of a STRUCT member), or empty.
    pub name: String,
    pub value: TypeParameterValue,
    pub query_location: Option<u64>,
}

/// The value of a [`TypeParameter`]: the transformer only produces types and constants.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeParameterValue {
    Type(UnboundTypeInfo),
    Constant(Value),
}

/// `ExpressionClass` / `ExpressionType` members of the two parsed-expression kinds an
/// unbound type is made of. The parse-tree crate checks these against its generated enums.
pub mod expression_ids {
    pub const CLASS_CONSTANT: (&str, u8) = ("CONSTANT", 7);
    pub const CLASS_TYPE: (&str, u8) = ("TYPE", 21);
    pub const TYPE_VALUE_CONSTANT: (&str, u8) = ("VALUE_CONSTANT", 75);
    pub const TYPE_TYPE: (&str, u8) = ("TYPE", 207);
}

/// The serialization version that writes unbound types as parsed expressions instead of
/// legacy `USER` types.
const UNBOUND_EXPRESSION_VERSION: u64 = 7;

fn write_id(serializer: &mut dyn Serializer, field_id: u16, tag: &'static str, id: (&str, u8)) {
    serializer.on_property_begin(field_id, tag);
    write_enum(serializer, id.0, |serializer| serializer.write_u8(id.1));
    serializer.on_property_end();
}

/// Reads an `ExpressionClass` property and tells whether it is `id`.
fn read_id_is(
    deserializer: &mut dyn Deserializer,
    field_id: u16,
    tag: &'static str,
    id: (&str, u8),
) -> Result<bool> {
    deserializer.on_property_begin(field_id, tag)?;
    let matches = if deserializer.deserialize_enum_from_string() {
        deserializer.read_string()?.eq_ignore_ascii_case(id.0)
    } else {
        deserializer.read_u8()? == id.1
    };
    deserializer.on_property_end()?;
    Ok(matches)
}

/// The `ParsedExpression` members of a type parameter: whether it is a constant, its
/// alias and its query location.
fn read_expression_base(deserializer: &mut dyn Deserializer) -> Result<(bool, String, Option<u64>)> {
    let is_constant = read_id_is(deserializer, 100, "class", expression_ids::CLASS_CONSTANT)?;
    deserializer.on_property_begin(101, "type")?;
    // The expression type follows from the class.
    if deserializer.deserialize_enum_from_string() {
        deserializer.read_string()?;
    } else {
        deserializer.read_u8()?;
    }
    deserializer.on_property_end()?;
    let alias = deserializer.read_property_with_default(102, "alias")?;
    let query_location = deserializer
        .read_property_with_explicit_default(103, "query_location", OptionalIdx(None))?
        .0;
    Ok((is_constant, alias, query_location))
}

impl UnboundTypeInfo {
    /// Writes this type as a `TypeExpression` (a `ParsedExpression` object's members).
    fn serialize_expression(&self, serializer: &mut dyn Serializer, alias: &str) -> Result<()> {
        write_id(serializer, 100, "class", expression_ids::CLASS_TYPE);
        write_id(serializer, 101, "type", expression_ids::TYPE_TYPE);
        serializer.write_optional(102, "alias", alias.is_empty(), alias)?;
        serializer.write_property_with_explicit_default(
            103,
            "query_location",
            &OptionalIdx(self.query_location),
            &OptionalIdx(None),
        )?;
        serializer.write_property_with_default(200, "catalog", &self.catalog)?;
        serializer.write_property_with_default(201, "schema", &self.schema)?;
        serializer.write_property_with_default(202, "type_name", &self.name)?;
        serializer.write_optional_with(203, "children", self.parameters.is_empty(), |serializer| {
            serializer.on_list_begin(self.parameters.len());
            for parameter in &self.parameters {
                serializer.on_nullable_begin(true);
                serializer.on_object_begin();
                match &parameter.value {
                    TypeParameterValue::Type(ty) => ty.serialize_expression(serializer, &parameter.name)?,
                    TypeParameterValue::Constant(value) => {
                        write_id(serializer, 100, "class", expression_ids::CLASS_CONSTANT);
                        write_id(serializer, 101, "type", expression_ids::TYPE_VALUE_CONSTANT);
                        serializer.write_property_with_default(102, "alias", &parameter.name)?;
                        serializer.write_property_with_explicit_default(
                            103,
                            "query_location",
                            &OptionalIdx(parameter.query_location),
                            &OptionalIdx(None),
                        )?;
                        serializer.write_property(200, "value", value)?;
                    }
                }
                serializer.on_object_end();
                serializer.on_nullable_end();
            }
            serializer.on_list_end();
            Ok(())
        })
    }

    /// Reads the `TypeExpression` members following the `ParsedExpression` base.
    fn deserialize_type_fields(
        deserializer: &mut dyn Deserializer,
        query_location: Option<u64>,
    ) -> Result<Self> {
        let catalog = deserializer.read_property_with_default(200, "catalog")?;
        let schema = deserializer.read_property_with_default(201, "schema")?;
        let name = deserializer.read_property_with_default(202, "type_name")?;
        let mut parameters = Vec::new();
        let present = deserializer.on_optional_property_begin(203, "children")?;
        if present {
            let count = deserializer.on_list_begin()?;
            for _ in 0..count {
                if !deserializer.on_nullable_begin()? {
                    return Err(Error::serialization("Failed to deserialize: NULL type parameter"));
                }
                deserializer.on_object_begin()?;
                parameters.push(TypeParameter::deserialize_expression(deserializer)?);
                deserializer.on_object_end()?;
                deserializer.on_nullable_end()?;
            }
            deserializer.on_list_end()?;
        }
        deserializer.on_optional_property_end(present)?;
        Ok(Self {
            catalog,
            schema,
            name,
            parameters,
            query_location,
        })
    }
}

impl TypeParameter {
    fn deserialize_expression(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let (is_constant, name, query_location) = read_expression_base(deserializer)?;
        let value = if is_constant {
            TypeParameterValue::Constant(deserializer.read_property(200, "value")?)
        } else {
            TypeParameterValue::Type(UnboundTypeInfo::deserialize_type_fields(
                deserializer,
                query_location,
            )?)
        };
        Ok(Self {
            name,
            value,
            query_location,
        })
    }
}

impl SerializeObject for UnboundTypeInfo {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        if serializer.should_serialize(UNBOUND_EXPRESSION_VERSION) {
            return serializer.write_optional_with(204, "expr", false, |serializer| {
                serializer.on_nullable_begin(true);
                serializer.on_object_begin();
                self.serialize_expression(serializer, "")?;
                serializer.on_object_end();
                serializer.on_nullable_end();
                Ok(())
            });
        }
        // Older readers expect a "USER" type: a name with constant modifiers.
        let mut modifiers = Vec::with_capacity(self.parameters.len());
        for parameter in &self.parameters {
            match &parameter.value {
                TypeParameterValue::Constant(value) => modifiers.push(value.clone()),
                TypeParameterValue::Type(_) => {
                    return Err(Error::serialization(format!(
                        "Cannot serialize non-constant type parameter when targeting serialization version {}",
                        serializer.options().compatibility.duckdb_version
                    )));
                }
            }
        }
        serializer.write_property_with_default(200, "name", &self.name)?;
        serializer.write_property_with_default(201, "catalog", &self.catalog)?;
        serializer.write_property_with_default(202, "schema", &self.schema)?;
        serializer.write_property_with_default(203, "user_type_modifiers", &modifiers)
    }
}

impl DeserializeObject for UnboundTypeInfo {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        if deserializer.on_optional_property_begin(204, "expr")? {
            let present = deserializer.on_nullable_begin()?;
            let info = if present {
                deserializer.on_object_begin()?;
                let (is_constant, _alias, query_location) = read_expression_base(deserializer)?;
                if is_constant {
                    return Err(Error::serialization(
                        "Failed to deserialize: an unbound type must be a type expression",
                    ));
                }
                let info = Self::deserialize_type_fields(deserializer, query_location)?;
                deserializer.on_object_end()?;
                Some(info)
            } else {
                None
            };
            deserializer.on_nullable_end()?;
            deserializer.on_optional_property_end(true)?;
            if let Some(info) = info {
                return Ok(info);
            }
        } else {
            deserializer.on_optional_property_end(false)?;
        }
        // A legacy "USER" type.
        let name = deserializer.read_property_with_default(200, "name")?;
        let catalog = deserializer.read_property_with_default(201, "catalog")?;
        let schema = deserializer.read_property_with_default(202, "schema")?;
        let modifiers: Vec<Value> = deserializer.read_property_with_default(203, "user_type_modifiers")?;
        let parameters = modifiers
            .into_iter()
            .map(|value| TypeParameter {
                name: String::new(),
                value: TypeParameterValue::Constant(value),
                query_location: None,
            })
            .collect();
        Ok(Self {
            catalog,
            schema,
            name,
            parameters,
            query_location: None,
        })
    }
}

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
        // DuckDB first tries to bind UNBOUND builtin types here when targeting older
        // versions (`UnboundType::TryDefaultBind`); that needs the default type catalog,
        // so unbound types are written in their legacy `USER` form.
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
        if ty.id() == LogicalTypeId::Blob && ty.alias() == Some("GEOMETRY") {
            return Ok(Self::GEOMETRY);
        }
        Ok(ty)
    }
}

rustdb_serde::impl_object_serde!(LogicalType);

#[cfg(test)]
mod tests {
    use rustdb_serde::SerializationOptions;
    use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};

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

    #[test]
    fn unbound_types_round_trip_in_both_encodings() {
        let unbound = LogicalType::with_info(
            LogicalTypeId::Unbound,
            TypeInfo::Unbound(UnboundTypeInfo {
                catalog: String::new(),
                schema: "s".into(),
                name: "mytype".into(),
                parameters: vec![TypeParameter {
                    name: String::new(),
                    value: TypeParameterValue::Constant(Value::integer(3)),
                    query_location: None,
                }],
                query_location: None,
            }),
        );
        assert_eq!(round_trip(&unbound, SerializationOptions::default()), unbound);
        let latest = SerializationOptions {
            compatibility: rustdb_serde::SerializationCompatibility::latest(),
            ..SerializationOptions::default()
        };
        assert_eq!(round_trip(&unbound, latest), unbound);
    }
}
