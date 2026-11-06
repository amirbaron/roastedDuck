//! Resolving type names (DuckDB's `Binder::BindLogicalType` in `bind_create.cpp` and
//! `ExpressionBinder::BindExpression(TypeExpression &)` in `bind_type_expression.cpp`).
//!
//! The transformer leaves every type name UNBOUND. Names resolve through lazy builtin
//! and user type catalog entries before their modifiers are bound.

use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, UnboundTypeInfo};

use crate::context::{ClientContext, position_missing_entry};

/// Resolves a type written in SQL (DuckDB's `Binder::BindLogicalType`): every UNBOUND
/// type within `ty` is replaced by the builtin or user type it names.
pub fn bind_logical_type(context: &ClientContext, ty: &LogicalType) -> Result<LogicalType> {
    TypeBinder { context }.bind(ty)
}

/// [`bind_logical_type`] looking for user types in `catalog.schema` first (the binder of
/// `CREATE TABLE` searches the schema of the new table first).
pub fn bind_logical_type_in_schema(
    context: &ClientContext,
    ty: &LogicalType,
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
) -> Result<LogicalType> {
    let context = context.with_type_search_path(catalog, schema);
    TypeBinder { context: &context }.bind(ty)
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
        children: Option<&[(rustdb_common::ByteString, LogicalType)]>,
    ) -> Result<Vec<(rustdb_common::ByteString, LogicalType)>> {
        children
            .unwrap_or_default()
            .iter()
            .map(|(name, child)| Ok((name.clone(), self.bind(child)?)))
            .collect()
    }

    /// One type expression (DuckDB's `BindLogicalTypeInternal` with
    /// `BindExpression(TypeExpression &)`): a builtin type, else a user type.
    fn bind_unbound(&self, info: &UnboundTypeInfo) -> Result<LogicalType> {
        let found = self.lookup_type(info).map_err(|error| {
            // Type extension hints are plain CatalogExceptions without a query
            // position. Only the ordinary missing-type exception gets a caret.
            if error.subtype() == Some("MISSING_ENTRY") {
                position_missing_entry(error, info.query_location)
            } else {
                error
            }
        })?;
        let builtin = found.entry.internal
            && found.entry.temporary
            && rustdb_types::builtin_type_id(found.entry.name()).is_some();
        if !builtin {
            return user_type(info, &found.entry);
        }
        if !info.parameters.is_empty()
            && rustdb_types::builtin_type_has_bind_function(&info.name) == Some(false)
        {
            return Err(Error::binder(rustdb_common::byte_format!(
                "Type '{}' does not take any type parameters",
                info.name
            ))
            .with_position(info.query_location));
        }
        if info.name.eq_ignore_ascii_case("enum") {
            return bind_enum_parameters(info);
        }
        if rustdb_types::builtin_type_id(&info.name) == Some(LogicalTypeId::Varchar)
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
        if rustdb_types::builtin_type_id(&info.name) == Some(LogicalTypeId::Geometry)
            && let [parameter] = info.parameters.as_slice()
            && let rustdb_types::TypeParameterValue::Constant(crs) = &parameter.value
            && crs.logical_type().id() == LogicalTypeId::Varchar
            && let Some(crs) = crs.as_str()
        {
            return bind_geometry_crs(crs, self.context.config.ignore_unknown_crs);
        }
        // Lookup creates the root entry before its parameter expressions create
        // child entries, even when binding a modifier subsequently fails.
        let mut info = info.clone();
        for parameter in &mut info.parameters {
            if let rustdb_types::TypeParameterValue::Type(child) = &parameter.value {
                let child = self.bind_unbound(child)?;
                parameter.value =
                    rustdb_types::TypeParameterValue::Constant(rustdb_types::Value::type_value(child));
            }
        }
        let bound = info.bind_with_parameter_aliases()?;
        if bound.id() == LogicalTypeId::Unbound {
            return Err(Error::internal("builtin type entry has no default binder"));
        }
        self.bind(&bound)
    }

    /// `BindExpression(TypeExpression &)`: try the requested scope, then the search
    /// path, and report a missing unqualified type from `system.main`.
    fn lookup_type(&self, info: &UnboundTypeInfo) -> Result<crate::context::EntryLookup> {
        let (catalog, schema) = self.context.bind_schema_or_catalog(&info.catalog, &info.schema)?;
        let lookup = |catalog: &[u8], schema: &[u8], if_exists| {
            self.context
                .get_entry(catalog, schema, CatalogType::Type, &info.name, if_exists)
        };
        let valid = |entry: &crate::context::EntryLookup| matches!(&entry.entry.kind, EntryKind::Type(ty) if ty.user_type.id() != LogicalTypeId::Invalid);
        if catalog.is_empty() && !self.context.has_default_database {
            return lookup(b"system", &schema, false)?
                .ok_or_else(|| Error::internal("type lookup returned nothing"));
        }
        if let Some(entry) = lookup(&catalog, &schema, true)?.filter(valid) {
            return Ok(entry);
        }
        if !catalog.is_empty() || !schema.is_empty() {
            // A qualified name missing from its requested scope reports that scope.
            lookup(&catalog, &schema, false)?;
        }
        if let Some(entry) = lookup(&catalog, b"", true)?.filter(valid) {
            return Ok(entry);
        }
        if let Some(entry) = lookup(b"", b"", true)?.filter(valid) {
            return Ok(entry);
        }
        lookup(b"system", rustdb_catalog::DEFAULT_SCHEMA.as_bytes(), false)?
            .ok_or_else(|| Error::internal("type lookup returned nothing"))
    }
}

/// `BindGeometryType` with a connection: `GEOMETRY('crs')` with the coordinate system in
/// its most compact known form (`CoordinateReferenceSystem::TryIdentify`).
fn bind_geometry_crs(crs: &str, ignore_unknown_crs: bool) -> Result<LogicalType> {
    let Some(identified) = rustdb_types::geometry::try_identify(crs)? else {
        if ignore_unknown_crs {
            return Ok(LogicalType::GEOMETRY);
        }
        return Err(Error::binder(format!(
            "Encountered unrecognized coordinate system '{crs}' when trying to create GEOMETRY type\n\
             The coordinate system definition may be incomplete or invalid. Your options are as follows:\n\
             * Load an extension that can identify this coordinate system\n\
             * Provide a full coordinate system definition in e.g. \"PROJJSON\" or \"WKT2\" format\n\
             * Set the 'ignore_unknown_crs' configuration option to drop the coordinate system from the resulting geometry type and make this error go away"
        )));
    };
    LogicalType::geometry_with_crs(identified.definition)
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
            return Err(Error::binder(rustdb_common::byte_format!(
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
            return Err(Error::binder(rustdb_common::byte_format!(
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
        return Err(Error::binder(rustdb_common::byte_format!(
            "Type '{}' does not take any type parameters",
            info.name
        ))
        .with_position(info.query_location));
    }
    Ok(ty.user_type.clone())
}

/// The collation of `VARCHAR COLLATE name`: a named, non-NULL VARCHAR `collation`
/// modifier among at most two (`BindVarcharType`).
fn varchar_collation(info: &UnboundTypeInfo) -> Option<rustdb_common::ByteString> {
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
                value.to_varchar_bytes().ok()
            }
            _ => None,
        })
}
