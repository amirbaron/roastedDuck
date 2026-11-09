//! Type names (`transform_typename.cpp`).
//!
//! DuckDB's transformer turns every type name into an UNBOUND type holding a
//! `TypeExpression`; here that expression is an [`UnboundTypeInfo`] directly.

use roastedduck_ast::{ExpressionClass, ParsedExpressionKind};
use roastedduck_common::{Error, Result};
use roastedduck_pg_parser::nodes::{Node, TypeName, Value};
use roastedduck_types::{
    LogicalType, LogicalTypeId, TypeInfo, TypeParameter, TypeParameterValue, UnboundTypeInfo,
    Value as DuckValue, builtin_type_id,
};

use super::{Transformer, node_string};

/// An UNBOUND type with this type expression (`LogicalType::UNBOUND`).
pub(crate) fn unbound_type(info: UnboundTypeInfo) -> LogicalType {
    LogicalType::with_info(LogicalTypeId::Unbound, TypeInfo::Unbound(info))
}

impl<'a> Transformer<'a> {
    /// `TransformTypeExpressionInternal`: the qualified name and the type modifiers.
    pub(crate) fn transform_type_expression_internal(
        &mut self,
        type_name: &'a TypeName,
    ) -> Result<UnboundTypeInfo> {
        let names: Vec<&[u8]> = type_name.names.iter().map(node_string).collect::<Result<_>>()?;
        let (catalog, schema, name) = match names.as_slice() {
            [] => return Err(Error::parser("Type name cannot be empty")),
            [name] => (b"".as_slice(), b"".as_slice(), *name),
            [schema, name] => (b"".as_slice(), *schema, *name),
            [catalog, schema, name] => (*catalog, *schema, *name),
            _ => {
                return Err(Error::parser(
                    "Too many qualifications for type name - expected [catalog.schema.name] or [schema.name]",
                ));
            }
        };
        // Normalize builtin type names to DuckDB's names (`int4` becomes `INTEGER`).
        let name = builtin_type_id(name).map_or_else(
            || roastedduck_common::ByteString::from(name),
            |id| id.name().into(),
        );
        let mut parameters = Vec::with_capacity(type_name.typmods.len());
        for typemod in &type_name.typmods {
            let mut parameter_name = roastedduck_common::ByteString::new();
            let mut node = typemod;
            if let Node::List(pair) = typemod {
                let [name_node, value_node] = pair.as_slice() else {
                    return Err(Error::parser(
                        "Expected type modifier to be a pair of (name, value)",
                    ));
                };
                node = value_node;
                match name_node {
                    Node::List(nil) if nil.is_empty() => {}
                    Node::Value(Value::String(text)) => parameter_name.clone_from(text),
                    _ => return Err(Error::parser("Expected a constant as type modifier name")),
                }
            }
            if let Node::TypeName(inner) = node {
                let ty = self.transform_type_expression(inner)?;
                parameters.push(TypeParameter {
                    name: parameter_name,
                    query_location: ty.query_location,
                    value: TypeParameterValue::Type(ty),
                });
            } else {
                let expr = self.transform_expression(node)?;
                if expr.expression_class() != ExpressionClass::Constant {
                    return Err(Error::parser("Expected a constant as type modifier"));
                }
                let query_location = expr.query_location;
                let ParsedExpressionKind::Constant(constant) = expr.kind else {
                    return Err(Error::internal("Constant expression without a value"));
                };
                parameters.push(TypeParameter {
                    name: parameter_name,
                    value: TypeParameterValue::Constant(constant.value),
                    query_location,
                });
            }
        }
        Ok(UnboundTypeInfo {
            catalog: catalog.to_owned().into(),
            schema: schema.to_owned().into(),
            name,
            parameters,
            query_location: u64::try_from(type_name.location).ok(),
        })
    }

    /// `TransformTypeExpression`: the type with its array bounds (`INT[]`, `INT[3]`).
    pub(crate) fn transform_type_expression(&mut self, type_name: &'a TypeName) -> Result<UnboundTypeInfo> {
        self.nested(|t| {
            let mut result = t.transform_type_expression_internal(type_name)?;
            for (extra, bound) in type_name.array_bounds.iter().enumerate() {
                t.check_depth(extra as u64)?;
                let Node::Value(Value::Integer(size)) = bound else {
                    return Err(Error::parser("ARRAY bounds must only contain expressions"));
                };
                let element = TypeParameter {
                    name: String::new().into(),
                    query_location: result.query_location,
                    value: TypeParameterValue::Type(result),
                };
                let mut parameters = vec![element];
                let name = if *size == -1 {
                    "list"
                } else {
                    parameters.push(TypeParameter {
                        name: String::new().into(),
                        value: TypeParameterValue::Constant(DuckValue::bigint(*size)),
                        query_location: None,
                    });
                    "array"
                };
                result = UnboundTypeInfo {
                    catalog: String::new().into(),
                    schema: String::new().into(),
                    name: name.to_owned().into(),
                    parameters,
                    query_location: None,
                };
            }
            if let Ok(location) = u64::try_from(type_name.location) {
                result.query_location = Some(location);
            }
            Ok(result)
        })
    }

    /// `TransformTypeName`: an UNBOUND type for the binder to resolve.
    pub(crate) fn transform_type_name(&mut self, type_name: &'a TypeName) -> Result<LogicalType> {
        Ok(unbound_type(self.transform_type_expression(type_name)?))
    }
}
