//! Resolving type names (DuckDB's `Binder::BindLogicalType` in `bind_create.cpp` and
//! `ExpressionBinder::BindExpression(TypeExpression &)` in `bind_type_expression.cpp`).
//!
//! The transformer leaves every type name UNBOUND. Builtin names bind without the
//! catalog (`DefaultTypeGenerator`); other names are user types (`TypeCatalogEntry`)
//! looked up through the search path.

use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, UnboundTypeInfo};

use crate::context::{ClientContext, position_missing_entry};

/// Resolves a type written in SQL (DuckDB's `Binder::BindLogicalType`): every UNBOUND
/// type within `ty` is replaced by the builtin or user type it names.
pub fn bind_logical_type(context: &ClientContext, ty: &LogicalType) -> Result<LogicalType> {
    TypeBinder {
        context,
        schema: None,
    }
    .bind(ty)
}

/// [`bind_logical_type`] looking for user types in `catalog.schema` first (the binder of
/// `CREATE TABLE` searches the schema of the new table first).
pub fn bind_logical_type_in_schema(
    context: &ClientContext,
    ty: &LogicalType,
    catalog: &str,
    schema: &str,
) -> Result<LogicalType> {
    TypeBinder {
        context,
        schema: Some((catalog, schema)),
    }
    .bind(ty)
}

/// Whether `ty` contains an UNBOUND type (`TypeVisitor::Contains`).
fn contains_unbound(ty: &LogicalType) -> bool {
    match ty.id() {
        LogicalTypeId::Unbound => true,
        LogicalTypeId::List => ty.child_type().is_some_and(contains_unbound),
        LogicalTypeId::Array => ty.array_child().is_some_and(|(child, _)| contains_unbound(child)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| contains_unbound(key) || contains_unbound(value)),
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| contains_unbound(child))),
        LogicalTypeId::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| contains_unbound(member))),
        _ => false,
    }
}

struct TypeBinder<'a> {
    context: &'a ClientContext,
    /// The catalog and schema searched first for unqualified user types.
    schema: Option<(&'a str, &'a str)>,
}

impl TypeBinder<'_> {
    /// `TypeVisitor::VisitReplace` of the UNBOUND types.
    fn bind(&self, ty: &LogicalType) -> Result<LogicalType> {
        if !contains_unbound(ty) {
            return Ok(ty.clone());
        }
        let bound = match ty.id() {
            LogicalTypeId::Unbound => {
                let Some(TypeInfo::Unbound(info)) = ty.info() else {
                    return Err(Error::internal("UNBOUND type without a type expression"));
                };
                return self.bind_unbound(info);
            }
            LogicalTypeId::List => match ty.child_type() {
                Some(child) => LogicalType::list(self.bind(child)?),
                None => ty.clone(),
            },
            LogicalTypeId::Array => match ty.array_child() {
                Some((child, size)) => LogicalType::array(self.bind(child)?, size),
                None => ty.clone(),
            },
            LogicalTypeId::Map => match ty.map_key_value() {
                Some((key, value)) => LogicalType::map(self.bind(key)?, self.bind(value)?),
                None => ty.clone(),
            },
            LogicalTypeId::Struct => LogicalType::struct_type(self.bind_children(ty.struct_children())?),
            LogicalTypeId::Union => LogicalType::union_type(self.bind_children(ty.union_members())?),
            _ => ty.clone(),
        };
        Ok(match ty.alias() {
            Some(alias) => bound.with_alias(alias),
            None => bound,
        })
    }

    fn bind_children(
        &self,
        children: Option<&[(String, LogicalType)]>,
    ) -> Result<Vec<(String, LogicalType)>> {
        children
            .unwrap_or_default()
            .iter()
            .map(|(name, child)| Ok((name.clone(), self.bind(child)?)))
            .collect()
    }

    /// One type expression (DuckDB's `BindLogicalTypeInternal` with
    /// `BindExpression(TypeExpression &)`): a builtin type, else a user type.
    fn bind_unbound(&self, info: &UnboundTypeInfo) -> Result<LogicalType> {
        let unqualified = info.catalog.is_empty() && info.schema.is_empty();
        if unqualified
            && let Some((catalog, schema)) = self.schema
            && let Some(found) =
                self.context
                    .get_entry(catalog, schema, CatalogType::Type, &info.name, true)?
        {
            return user_type(info, &found.entry);
        }
        let qualified_in_main =
            info.schema.is_empty() || info.schema.eq_ignore_ascii_case(rustdb_catalog::DEFAULT_SCHEMA);
        if qualified_in_main && info.name.eq_ignore_ascii_case("enum") {
            return bind_enum_parameters(info);
        }
        if qualified_in_main
            && rustdb_types::builtin_type_id(&info.name) == Some(LogicalTypeId::Varchar)
            && let Some(collation) = varchar_collation(info)
        {
            // `BindVarcharType`: a valid collation names a collated VARCHAR.
            let ty = LogicalType::with_info(
                LogicalTypeId::Varchar,
                rustdb_types::TypeInfo::String(rustdb_types::StringTypeInfo { collation }),
            );
            crate::collation_binding::push_collation(
                self.context,
                crate::expression::Expression::constant(rustdb_types::Value::varchar("")),
                &ty,
            )?;
            return Ok(ty);
        }
        if qualified_in_main {
            let bound = info.default_bind()?;
            if bound.id() != LogicalTypeId::Unbound {
                // The modifiers of a builtin type may name user types.
                return self.bind(&bound);
            }
        }
        let (mut catalog, mut schema) = (info.catalog.clone(), info.schema.clone());
        if catalog.is_empty() && !schema.is_empty() && self.context.catalog(&schema).is_some() {
            // `db.type`: a catalog, not a schema (`BindSchemaOrCatalog`).
            catalog = std::mem::take(&mut schema);
        }
        let found = self
            .context
            .get_entry(&catalog, &schema, CatalogType::Type, &info.name, false)
            .map_err(|error| position_missing_entry(error, info.query_location))?
            .ok_or_else(|| Error::internal("type lookup returned nothing"))?;
        user_type(info, &found.entry)
    }
}

/// `ENUM(...)` through the binder (`BindEnumType` with the modifiers named by their
/// aliases, as `BindExpression(TypeExpression &)` passes them).
fn bind_enum_parameters(info: &UnboundTypeInfo) -> Result<LogicalType> {
    use rustdb_types::TypeParameterValue;
    if info.parameters.is_empty() {
        return Err(Error::binder("ENUM type requires at least one argument"));
    }
    let mut values = Vec::with_capacity(info.parameters.len());
    for (index, parameter) in info.parameters.iter().enumerate() {
        if !parameter.name.is_empty() {
            return Err(Error::binder(format!(
                "ENUM type arguments cannot have names (argument {} has name \"{}\")",
                index + 1,
                parameter.name
            )));
        }
        let TypeParameterValue::Constant(value) = &parameter.value else {
            return Err(Error::binder("ENUM type requires a set of VARCHAR arguments"));
        };
        if value.logical_type().id() != LogicalTypeId::Varchar {
            return Err(Error::binder("ENUM type requires a set of VARCHAR arguments"));
        }
        let Some(text) = value.as_str() else {
            return Err(Error::binder(format!(
                "ENUM type arguments cannot be NULL (argument {} is NULL)",
                index + 1
            )));
        };
        values.push(text.to_owned());
    }
    Ok(LogicalType::enum_type(values))
}

/// The type of a user type entry; user types take no modifiers.
fn user_type(info: &UnboundTypeInfo, entry: &rustdb_catalog::CatalogEntry) -> Result<LogicalType> {
    let EntryKind::Type(ty) = &entry.kind else {
        return Err(Error::internal("type lookup found another kind of entry"));
    };
    if !info.parameters.is_empty() {
        return Err(
            Error::binder(format!("Type '{}' does not take any type parameters", info.name))
                .with_position(info.query_location),
        );
    }
    Ok(ty.user_type.clone())
}

/// The collation of `VARCHAR COLLATE name`: a named, non-NULL VARCHAR `collation`
/// modifier among at most two (`BindVarcharType`).
fn varchar_collation(info: &UnboundTypeInfo) -> Option<String> {
    if info.parameters.is_empty() || info.parameters.len() > 2 {
        return None;
    }
    info.parameters
        .iter()
        .find_map(|parameter| match &parameter.value {
            rustdb_types::TypeParameterValue::Constant(value)
                if parameter.name.eq_ignore_ascii_case("collation")
                    && value.logical_type().id() == LogicalTypeId::Varchar
                    && !value.is_null() =>
            {
                value.to_varchar().ok()
            }
            _ => None,
        })
}
