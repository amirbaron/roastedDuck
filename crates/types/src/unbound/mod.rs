//! Unbound types: type names the parser could not resolve (DuckDB's `UnboundTypeInfo`
//! holding a `TypeExpression`), their serialization, and the binding of builtin names.

mod default_bind;

pub use default_bind::{builtin_type_has_bind_function, builtin_type_id, builtin_type_names};

use rustdb_serde::{
    DeserializeObject, Deserializer, Error, OptionalIdx, Result, SerializeObject, Serializer, write_enum,
};

use crate::Value;
use rustdb_common::ByteString;

/// A type name the parser could not resolve yet (DuckDB's `UnboundTypeInfo` holding a
/// `TypeExpression`): `catalog.schema.name(parameters)`, bound later by the binder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnboundTypeInfo {
    /// The catalog qualifier, or empty.
    pub catalog: ByteString,
    /// The schema qualifier, or empty.
    pub schema: ByteString,
    /// The type name as written (normalized for builtin types).
    pub name: ByteString,
    /// The type modifiers in parentheses.
    pub parameters: Vec<TypeParameter>,
    /// Position of the type name in the query text.
    pub query_location: Option<u64>,
}

/// One parameter of an unbound type: `DECIMAL(18, 3)`, `STRUCT(a INTEGER)`, `MAP(k := INT, ...)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeParameter {
    /// The parameter name (the field name of a STRUCT member), or empty.
    pub name: ByteString,
    pub value: TypeParameterValue,
    /// Position of the parameter in the query text.
    pub query_location: Option<u64>,
}

/// The value of a [`TypeParameter`]: the transformer only produces types and constants.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeParameterValue {
    /// A nested type, as in `STRUCT(a INTEGER)` or `MAP(VARCHAR, INTEGER)`.
    Type(UnboundTypeInfo),
    /// A constant, as in `DECIMAL(18, 3)`.
    Constant(Value),
}

/// `ExpressionClass` / `ExpressionType` members of the two parsed-expression kinds an
/// unbound type is made of. The parse-tree crate checks these against its generated enums.
pub mod expression_ids {
    /// `ExpressionClass::CONSTANT`.
    pub const CLASS_CONSTANT: (&str, u8) = ("CONSTANT", 7);
    /// `ExpressionClass::TYPE`.
    pub const CLASS_TYPE: (&str, u8) = ("TYPE", 21);
    /// `ExpressionType::VALUE_CONSTANT`.
    pub const TYPE_VALUE_CONSTANT: (&str, u8) = ("VALUE_CONSTANT", 75);
    /// `ExpressionType::TYPE`.
    pub const TYPE_TYPE: (&str, u8) = ("TYPE", 207);
}

/// The serialization version that writes unbound types as parsed expressions instead of
/// legacy `USER` types.
pub(crate) const UNBOUND_EXPRESSION_VERSION: u64 = 7;

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
fn read_expression_base(deserializer: &mut dyn Deserializer) -> Result<(bool, ByteString, Option<u64>)> {
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
    fn serialize_expression(&self, serializer: &mut dyn Serializer, alias: &[u8]) -> Result<()> {
        write_id(serializer, 100, "class", expression_ids::CLASS_TYPE);
        write_id(serializer, 101, "type", expression_ids::TYPE_TYPE);
        serializer.write_optional_with(102, "alias", alias.is_empty(), |serializer| {
            serializer.write_string_bytes(alias);
            Ok(())
        })?;
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
                self.serialize_expression(serializer, b"")?;
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
                name: ByteString::new(),
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

impl UnboundTypeInfo {
    /// Native `TypeExpression` SQL text, also used by `ParsedExpression::GetName` during
    /// legacy default binding when a parameter has no explicit alias.
    pub fn to_sql_bytes(&self) -> rustdb_common::Result<ByteString> {
        use crate::identifier::{sql_identifier_bytes, type_identifier_bytes};
        use rustdb_common::{byte_format, byte_write};
        let mut output = ByteString::new();
        if !self.catalog.is_empty() {
            let _ = byte_write!(output, "{}.", sql_identifier_bytes(&self.catalog));
        }
        if !self.schema.is_empty() {
            let _ = byte_write!(output, "{}.", sql_identifier_bytes(&self.schema));
        }
        let render = |parameter: &TypeParameter| match &parameter.value {
            TypeParameterValue::Type(ty) => ty.to_sql_bytes(),
            TypeParameterValue::Constant(value) => value.try_to_sql_string(),
        };
        if output.is_empty() {
            match (
                self.name.as_bytes().to_ascii_uppercase().as_slice(),
                self.parameters.as_slice(),
            ) {
                (b"LIST", [child]) => return Ok(byte_format!("{}[]", render(child)?)),
                (b"ARRAY", [child, size]) => {
                    return Ok(byte_format!("{}[{}]", render(child)?, render(size)?));
                }
                (b"STRUCT" | b"UNION", []) => return Ok(self.name.as_bytes().to_ascii_uppercase().into()),
                (b"STRUCT" | b"UNION", parameters) => {
                    output.push_bytes(self.name.as_bytes().to_ascii_uppercase());
                    output.push(b'(');
                    for (i, parameter) in parameters.iter().enumerate() {
                        if i > 0 {
                            output.push_bytes(", ");
                        }
                        let _ = byte_write!(
                            output,
                            "{} {}",
                            sql_identifier_bytes(&parameter.name),
                            render(parameter)?
                        );
                    }
                    output.push(b')');
                    return Ok(output);
                }
                (b"MAP", [key, value]) => {
                    return Ok(byte_format!("MAP({}, {})", render(key)?, render(value)?));
                }
                (b"VARCHAR", [.., last]) if last.name.eq_ignore_ascii_case("collation") => {
                    if let TypeParameterValue::Constant(value) = &last.value {
                        return Ok(byte_format!(
                            "VARCHAR COLLATE {}",
                            sql_identifier_bytes(&value.to_varchar_bytes()?)
                        ));
                    }
                }
                (b"INTERVAL", [_, ..]) => return Ok("INTERVAL".into()),
                _ => {}
            }
        }
        output.push_bytes(type_identifier_bytes(&self.name));
        if !self.parameters.is_empty() {
            output.push(b'(');
            for (i, parameter) in self.parameters.iter().enumerate() {
                if i > 0 {
                    output.push_bytes(", ");
                }
                output.push_bytes(render(parameter)?);
            }
            output.push(b')');
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use rustdb_serde::SerializationOptions;
    use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};

    use super::*;
    use crate::{LogicalType, LogicalTypeId, TypeInfo};

    fn round_trip(ty: &LogicalType, options: SerializationOptions) -> LogicalType {
        let bytes = BinarySerializer::serialize(ty, options).unwrap();
        BinaryDeserializer::deserialize(&bytes).unwrap()
    }

    #[test]
    fn unbound_types_round_trip_in_both_encodings() {
        let unbound = LogicalType::with_info(
            LogicalTypeId::Unbound,
            TypeInfo::Unbound(UnboundTypeInfo {
                catalog: ByteString::new(),
                schema: "s".into(),
                name: "mytype".into(),
                parameters: vec![TypeParameter {
                    name: ByteString::new(),
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
