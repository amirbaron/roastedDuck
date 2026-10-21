//! Binding builtin type names without a catalog (DuckDB's `UnboundType::TryDefaultBind`
//! and `DefaultTypeGenerator`'s table of builtin types and their bind functions).
//!
//! The binder resolves user-defined types through the catalog; this covers the names
//! every database knows. Serialization uses it too: when targeting versions before
//! v1.5, DuckDB writes an unbound builtin type as the bound type.

use rustdb_common::{CaseInsensitiveSet, Error, Result};

use super::{TypeParameter, TypeParameterValue, UnboundTypeInfo};
use crate::{EnumTypeInfo, LogicalType, LogicalTypeId, TypeInfo, Value};

/// The widest DECIMAL (`Decimal::MAX_WIDTH_DECIMAL`).
const MAX_DECIMAL_WIDTH: u8 = 38;
/// The largest fixed-size ARRAY (`ArrayType::MAX_ARRAY_SIZE`).
const MAX_ARRAY_SIZE: i64 = 100_000;
/// The most members a UNION may have (`UnionType::MAX_UNION_MEMBERS`).
const MAX_UNION_MEMBERS: usize = 255;

/// A type modifier with nested types bound (DuckDB's `TypeArgument`).
struct TypeArgument {
    /// The parameter's name: its alias (`ParsedExpression::GetAlias`), empty without one.
    name: String,
    value: Value,
}

impl TypeArgument {
    fn is_named(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    fn type_value(&self) -> Option<&LogicalType> {
        self.value.as_type()
    }
}

type BindFunction = fn(&[TypeArgument]) -> Result<LogicalType>;

/// DuckDB's `BUILTIN_TYPES`: name, type id, and the function validating modifiers.
const BUILTIN_TYPES: &[(&str, LogicalTypeId, Option<BindFunction>)] = &[
    ("decimal", LogicalTypeId::Decimal, Some(bind_decimal)),
    ("dec", LogicalTypeId::Decimal, Some(bind_decimal)),
    ("numeric", LogicalTypeId::Decimal, Some(bind_decimal)),
    ("time", LogicalTypeId::Time, None),
    ("time_ns", LogicalTypeId::TimeNs, None),
    ("date", LogicalTypeId::Date, None),
    ("timestamp", LogicalTypeId::Timestamp, Some(bind_timestamp)),
    ("datetime", LogicalTypeId::Timestamp, Some(bind_timestamp)),
    ("timestamp_us", LogicalTypeId::Timestamp, None),
    ("timestamp_ms", LogicalTypeId::TimestampMs, None),
    ("timestamp_ns", LogicalTypeId::TimestampNs, None),
    ("timestamp_s", LogicalTypeId::TimestampSec, None),
    ("timestamptz", LogicalTypeId::TimestampTz, None),
    ("timestamp with time zone", LogicalTypeId::TimestampTz, None),
    ("timetz", LogicalTypeId::TimeTz, None),
    ("time with time zone", LogicalTypeId::TimeTz, None),
    ("interval", LogicalTypeId::Interval, Some(bind_interval)),
    ("varchar", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("bpchar", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("string", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("char", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("nvarchar", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("text", LogicalTypeId::Varchar, Some(bind_varchar)),
    ("blob", LogicalTypeId::Blob, None),
    ("bytea", LogicalTypeId::Blob, None),
    ("varbinary", LogicalTypeId::Blob, None),
    ("binary", LogicalTypeId::Blob, None),
    ("hugeint", LogicalTypeId::HugeInt, None),
    ("int128", LogicalTypeId::HugeInt, None),
    ("uhugeint", LogicalTypeId::UHugeInt, None),
    ("uint128", LogicalTypeId::UHugeInt, None),
    ("bigint", LogicalTypeId::BigInt, None),
    ("oid", LogicalTypeId::BigInt, None),
    ("long", LogicalTypeId::BigInt, None),
    ("int8", LogicalTypeId::BigInt, None),
    ("int64", LogicalTypeId::BigInt, None),
    ("ubigint", LogicalTypeId::UBigInt, None),
    ("uint64", LogicalTypeId::UBigInt, None),
    ("integer", LogicalTypeId::Integer, None),
    ("int", LogicalTypeId::Integer, None),
    ("int4", LogicalTypeId::Integer, None),
    ("signed", LogicalTypeId::Integer, None),
    ("integral", LogicalTypeId::Integer, None),
    ("int32", LogicalTypeId::Integer, None),
    ("uinteger", LogicalTypeId::UInteger, None),
    ("uint32", LogicalTypeId::UInteger, None),
    ("smallint", LogicalTypeId::SmallInt, None),
    ("int2", LogicalTypeId::SmallInt, None),
    ("short", LogicalTypeId::SmallInt, None),
    ("int16", LogicalTypeId::SmallInt, None),
    ("usmallint", LogicalTypeId::USmallInt, None),
    ("uint16", LogicalTypeId::USmallInt, None),
    ("tinyint", LogicalTypeId::TinyInt, None),
    ("int1", LogicalTypeId::TinyInt, None),
    ("utinyint", LogicalTypeId::UTinyInt, None),
    ("uint8", LogicalTypeId::UTinyInt, None),
    ("struct", LogicalTypeId::Struct, Some(bind_struct)),
    ("row", LogicalTypeId::Struct, Some(bind_struct)),
    ("list", LogicalTypeId::List, Some(bind_list)),
    ("array", LogicalTypeId::Array, Some(bind_array)),
    ("map", LogicalTypeId::Map, Some(bind_map)),
    ("union", LogicalTypeId::Union, Some(bind_union)),
    ("bit", LogicalTypeId::Bit, Some(bind_bit)),
    ("bitstring", LogicalTypeId::Bit, Some(bind_bit)),
    ("variant", LogicalTypeId::Variant, Some(bind_variant)),
    ("bignum", LogicalTypeId::Bignum, None),
    ("varint", LogicalTypeId::Bignum, None),
    ("boolean", LogicalTypeId::Boolean, None),
    ("bool", LogicalTypeId::Boolean, None),
    ("logical", LogicalTypeId::Boolean, None),
    ("uuid", LogicalTypeId::Uuid, None),
    ("guid", LogicalTypeId::Uuid, None),
    ("enum", LogicalTypeId::Enum, Some(bind_enum)),
    ("null", LogicalTypeId::SqlNull, None),
    ("float", LogicalTypeId::Float, None),
    ("real", LogicalTypeId::Float, None),
    ("float4", LogicalTypeId::Float, None),
    ("double", LogicalTypeId::Double, None),
    ("float8", LogicalTypeId::Double, None),
    ("geometry", LogicalTypeId::Geometry, Some(bind_geometry)),
    ("type", LogicalTypeId::Type, None),
];

/// The id of the builtin type called `name`, compared case-insensitively (DuckDB's
/// `DefaultTypeGenerator::GetDefaultType`); `None` for user-defined names.
pub fn builtin_type_id(name: &str) -> Option<LogicalTypeId> {
    BUILTIN_TYPES
        .iter()
        .find(|(builtin, _, _)| builtin.eq_ignore_ascii_case(name))
        .map(|&(_, id, _)| id)
}

/// The names of the builtin types, lowercase (DuckDB's `DefaultTypeGenerator::GetDefaultEntries`).
pub fn builtin_type_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_TYPES.iter().map(|&(name, _, _)| name)
}

impl UnboundTypeInfo {
    /// Binds this type, and the types among its parameters, where they name builtin types
    /// (DuckDB's `TryDefaultBindTypeExpression`). A name that is not builtin stays an
    /// UNBOUND type; invalid modifiers are DuckDB's binder errors.
    pub fn default_bind(&self) -> Result<LogicalType> {
        let arguments = self
            .parameters
            .iter()
            .map(TypeParameter::bind_argument)
            .collect::<Result<Vec<_>>>()?;
        let Some(&(_, id, bind)) = BUILTIN_TYPES
            .iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(&self.name))
        else {
            return Ok(LogicalType::with_info(
                LogicalTypeId::Unbound,
                TypeInfo::Unbound(self.clone()),
            ));
        };
        match bind {
            Some(bind) => bind(&arguments),
            None if arguments.is_empty() => Ok(LogicalType::simple(id)),
            None => Err(Error::binder(format!(
                "Type '{}' does not take any type parameters",
                self.name
            ))),
        }
    }
}

impl TypeParameter {
    fn bind_argument(&self) -> Result<TypeArgument> {
        let value = match &self.value {
            TypeParameterValue::Type(ty) => Value::type_value(ty.default_bind()?),
            TypeParameterValue::Constant(value) => value.clone(),
        };
        Ok(TypeArgument {
            name: self.name.clone(),
            value,
        })
    }
}

/// `Value::DefaultTryCastAs(UTINYINT)` for the modifiers the parser produces: integers and
/// numeric strings.
fn to_u8(value: &Value) -> Option<u8> {
    if value.logical_type().is_integral() {
        return value.as_i128().and_then(|v| u8::try_from(v).ok());
    }
    if value.logical_type().id() == LogicalTypeId::Varchar {
        let text = std::str::from_utf8(value.as_bytes()?).ok()?.trim();
        return text.strip_prefix('+').unwrap_or(text).parse().ok();
    }
    None
}

fn bind_decimal(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let (mut width, mut scale) = (18, 3);
    if let Some(argument) = arguments.first() {
        if argument.value.is_null() {
            return Err(Error::binder("DECIMAL type width cannot be NULL"));
        }
        width = to_u8(&argument.value).ok_or_else(|| {
            Error::binder(format!(
                "DECIMAL type width must be between 1 and {MAX_DECIMAL_WIDTH}"
            ))
        })?;
        scale = 0;
    }
    if let Some(argument) = arguments.get(1) {
        if argument.value.is_null() {
            return Err(Error::binder("DECIMAL type scale cannot be NULL"));
        }
        scale = to_u8(&argument.value).ok_or_else(|| {
            Error::binder(format!(
                "DECIMAL type scale must be between 0 and {MAX_DECIMAL_WIDTH}"
            ))
        })?;
    }
    if arguments.len() > 2 {
        return Err(Error::binder(
            "DECIMAL type can take at most two type modifiers, width and scale",
        ));
    }
    if !(1..=MAX_DECIMAL_WIDTH).contains(&width) {
        return Err(Error::binder(format!(
            "DECIMAL type width must be between 1 and {MAX_DECIMAL_WIDTH}"
        )));
    }
    if scale > width {
        return Err(Error::binder("DECIMAL type scale cannot be greater than width"));
    }
    Ok(LogicalType::decimal(width, scale))
}

fn bind_timestamp(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let [argument] = arguments else {
        return if arguments.is_empty() {
            Ok(LogicalType::TIMESTAMP)
        } else {
            Err(Error::binder("TIMESTAMP type takes at most one type modifier"))
        };
    };
    if argument.value.is_null() {
        return Err(Error::binder("TIMESTAMP type precision cannot be NULL"));
    }
    let precision = to_u8(&argument.value)
        .ok_or_else(|| Error::binder("TIMESTAMP type precision must be between 0 and 9"))?;
    Ok(match precision {
        0 => LogicalType::simple(LogicalTypeId::TimestampSec),
        1..=3 => LogicalType::simple(LogicalTypeId::TimestampMs),
        4..=6 => LogicalType::TIMESTAMP,
        7..=9 => LogicalType::simple(LogicalTypeId::TimestampNs),
        _ => {
            return Err(Error::binder(
                "TIMESTAMP only supports until nano-second precision (9)",
            ));
        }
    })
}

fn bind_varchar(arguments: &[TypeArgument]) -> Result<LogicalType> {
    // A length modifier is accepted and ignored; a collation needs a connection.
    if arguments.len() <= 2
        && arguments.iter().any(|argument| {
            argument.is_named("collation")
                && argument.value.logical_type().id() == LogicalTypeId::Varchar
                && !argument.value.is_null()
        })
    {
        return Err(Error::binder(
            "Cannot bind varchar with collation without a connection",
        ));
    }
    if arguments.len() > 1 {
        return Err(Error::binder("VARCHAR type takes at most one type modifier"));
    }
    Ok(LogicalType::VARCHAR)
}

fn bind_bit(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if arguments.len() > 1 {
        return Err(Error::binder("BIT type takes at most one type modifier"));
    }
    Ok(LogicalType::simple(LogicalTypeId::Bit))
}

fn bind_interval(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if arguments.len() > 1 {
        return Err(Error::binder("INTERVAL type takes at most one type modifier"));
    }
    Ok(LogicalType::INTERVAL)
}

fn bind_enum(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if arguments.is_empty() {
        return Err(Error::binder("ENUM type requires at least one argument"));
    }
    let mut values = Vec::with_capacity(arguments.len());
    for (index, argument) in arguments.iter().enumerate() {
        if !argument.name.is_empty() {
            return Err(Error::binder(format!(
                "ENUM type arguments cannot have names (argument {} has name \"{}\")",
                index + 1,
                argument.name
            )));
        }
        if argument.value.logical_type().id() != LogicalTypeId::Varchar {
            return Err(Error::binder("ENUM type requires a set of VARCHAR arguments"));
        }
        let Some(bytes) = argument.value.as_bytes() else {
            return Err(Error::binder(format!(
                "ENUM type arguments cannot be NULL (argument {} is NULL)",
                index + 1
            )));
        };
        values.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(LogicalType::with_info(
        LogicalTypeId::Enum,
        TypeInfo::Enum(EnumTypeInfo { values }),
    ))
}

fn bind_list(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let [argument] = arguments else {
        return Err(Error::binder("LIST type requires exactly one type modifier"));
    };
    if argument.value.is_null() {
        return Err(Error::binder("LIST type modifier cannot be NULL"));
    }
    let child = argument.type_value().ok_or_else(|| {
        Error::binder(format!(
            "LIST type modifier must be a type, but got {}",
            argument.value.to_varchar().unwrap_or_default()
        ))
    })?;
    Ok(LogicalType::list(child.clone()))
}

fn bind_array(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let [element, size] = arguments else {
        return Err(Error::binder("ARRAY type requires exactly two type modifiers"));
    };
    if element.value.is_null() {
        return Err(Error::binder("ARRAY type modifier cannot be NULL"));
    }
    let child = element.type_value().ok_or_else(|| {
        Error::binder(format!(
            "ARRAY type modifier must be a type, but got {}",
            element.value.to_varchar().unwrap_or_default()
        ))
    })?;
    if size.value.is_null() {
        return Err(Error::binder("ARRAY type size modifier cannot be NULL"));
    }
    if !size.value.logical_type().is_integral() {
        return Err(Error::binder("ARRAY type size modifier must be an integral type"));
    }
    let size = size
        .value
        .as_i128()
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| Error::binder("ARRAY type size modifier must be a BIGINT"))?;
    if size < 1 {
        return Err(Error::binder("ARRAY type size must be at least 1"));
    }
    if size > MAX_ARRAY_SIZE {
        return Err(Error::binder(format!(
            "ARRAY type size must be at most {MAX_ARRAY_SIZE}"
        )));
    }
    Ok(LogicalType::array(child.clone(), size as u32))
}

/// The child types with their names, rejecting duplicate names (case-insensitively)
/// unless the children are `anonymous`.
fn named_children(
    arguments: &[TypeArgument],
    duplicate: &str,
    anonymous: bool,
) -> Result<Vec<(String, LogicalType)>> {
    let mut names = CaseInsensitiveSet::new();
    let mut children = Vec::with_capacity(arguments.len());
    for argument in arguments {
        if !anonymous && !names.insert(argument.name.as_str()) {
            return Err(Error::binder(format!("{duplicate} \"{}\"", argument.name)));
        }
        let ty = argument
            .type_value()
            .ok_or_else(|| Error::internal("type modifier is not a type"))?;
        children.push((argument.name.clone(), ty.clone()));
    }
    Ok(children)
}

fn bind_struct(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if arguments.is_empty() {
        return Err(Error::binder("STRUCT type requires at least one child type"));
    }
    for argument in arguments {
        if argument.value.logical_type().id() != LogicalTypeId::Type {
            return Err(Error::binder("STRUCT type arguments must be types"));
        }
        if argument.value.is_null() {
            return Err(Error::binder("STRUCT type arguments cannot be NULL"));
        }
    }
    let all_named = arguments.iter().all(|argument| !argument.name.is_empty());
    let all_anonymous = arguments.iter().all(|argument| argument.name.is_empty());
    if !all_named && !all_anonymous {
        return Err(Error::binder(
            "STRUCT type arguments must either all have names or all be anonymous",
        ));
    }
    let children = named_children(arguments, "Duplicate STRUCT type argument name", all_anonymous)?;
    Ok(LogicalType::struct_type(children))
}

fn bind_map(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let [key, value] = arguments else {
        return Err(Error::binder(
            "MAP type requires exactly two type modifiers: key type and value type",
        ));
    };
    if key.value.logical_type().id() != LogicalTypeId::Type
        || value.value.logical_type().id() != LogicalTypeId::Type
    {
        return Err(Error::binder("MAP type modifiers must be types"));
    }
    let key = key
        .type_value()
        .ok_or_else(|| Error::binder("MAP type key type modifier cannot be NULL"))?;
    let value = value
        .type_value()
        .ok_or_else(|| Error::binder("MAP type value type modifier cannot be NULL"))?;
    Ok(LogicalType::map(key.clone(), value.clone()))
}

fn bind_union(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if arguments.is_empty() {
        return Err(Error::binder("UNION type requires at least one type modifier"));
    }
    if arguments.len() > MAX_UNION_MEMBERS {
        return Err(Error::binder(format!(
            "UNION type supports at most {MAX_UNION_MEMBERS} type modifiers"
        )));
    }
    for argument in arguments {
        if argument.name.is_empty() {
            return Err(Error::binder("UNION type modifiers must have names"));
        }
        if argument.value.logical_type().id() != LogicalTypeId::Type {
            return Err(Error::binder("UNION type modifiers must be types"));
        }
        if argument.value.is_null() {
            return Err(Error::binder("UNION type modifiers cannot be NULL"));
        }
    }
    let members = named_children(arguments, "Duplicate UNION type member name", false)?;
    Ok(LogicalType::union_type(members))
}

fn bind_variant(arguments: &[TypeArgument]) -> Result<LogicalType> {
    if !arguments.is_empty() {
        return Err(Error::binder("Type 'VARIANT' does not take any type parameters"));
    }
    Ok(LogicalType::variant())
}

fn bind_geometry(arguments: &[TypeArgument]) -> Result<LogicalType> {
    let [crs] = arguments else {
        return if arguments.is_empty() {
            Ok(LogicalType::GEOMETRY)
        } else {
            Err(Error::binder(
                "GEOMETRY type takes a single optional type modifier with a coordinate system definition",
            ))
        };
    };
    if crs.value.logical_type().id() != LogicalTypeId::Varchar {
        return Err(Error::binder(
            "GEOMETRY type modifier must be a string with a coordinate system definition",
        ));
    }
    if crs.value.is_null() {
        return Err(Error::binder("GEOMETRY type modifier cannot be NULL"));
    }
    Err(Error::binder(
        "Cannot create GEOMETRY type with coordinate system without a connection",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbound(name: &str, parameters: Vec<TypeParameter>) -> UnboundTypeInfo {
        UnboundTypeInfo {
            catalog: String::new(),
            schema: String::new(),
            name: name.to_owned(),
            parameters,
            query_location: None,
        }
    }

    fn constant(value: Value) -> TypeParameter {
        TypeParameter {
            name: String::new(),
            value: TypeParameterValue::Constant(value),
            query_location: None,
        }
    }

    fn named_type(name: &str, ty: UnboundTypeInfo) -> TypeParameter {
        TypeParameter {
            name: name.to_owned(),
            value: TypeParameterValue::Type(ty),
            query_location: None,
        }
    }

    #[test]
    fn binds_builtin_types() {
        let bind = |ty: UnboundTypeInfo| ty.default_bind().unwrap();
        assert_eq!(bind(unbound("INTEGER", vec![])), LogicalType::INTEGER);
        assert_eq!(bind(unbound("int4", vec![])), LogicalType::INTEGER);
        assert_eq!(
            bind(unbound("DECIMAL", vec![constant(Value::integer(3))])),
            LogicalType::decimal(3, 0)
        );
        assert_eq!(
            bind(unbound(
                "NUMERIC",
                vec![constant(Value::varchar("5")), constant(Value::integer(2))]
            )),
            LogicalType::decimal(5, 2)
        );
        assert_eq!(bind(unbound("decimal", vec![])), LogicalType::decimal(18, 3));
        assert_eq!(
            bind(unbound("TIMESTAMP", vec![constant(Value::integer(3))])).id(),
            LogicalTypeId::TimestampMs
        );
        assert_eq!(
            bind(unbound("VARCHAR", vec![constant(Value::integer(10))])),
            LogicalType::VARCHAR
        );
        assert_eq!(
            bind(unbound(
                "STRUCT",
                vec![named_type("a", unbound("INTEGER", vec![]))]
            )),
            LogicalType::struct_type(vec![("a".into(), LogicalType::INTEGER)])
        );
        assert_eq!(
            bind(unbound("list", vec![named_type("", unbound("VARCHAR", vec![]))])),
            LogicalType::list(LogicalType::VARCHAR)
        );
        // A user type stays unbound, also nested.
        let user = unbound("mytype", vec![]);
        assert_eq!(bind(user.clone()).id(), LogicalTypeId::Unbound);
        let list = bind(unbound("list", vec![named_type("", user)]));
        assert_eq!(
            list.child_type().map(LogicalType::id),
            Some(LogicalTypeId::Unbound)
        );
    }

    #[test]
    fn reports_duckdb_binder_errors() {
        let error = |ty: UnboundTypeInfo| ty.default_bind().unwrap_err().to_string();
        assert_eq!(
            error(unbound("DECIMAL", vec![constant(Value::integer(50))])),
            "Binder Error: DECIMAL type width must be between 1 and 38"
        );
        assert_eq!(
            error(unbound("INTEGER", vec![constant(Value::integer(1))])),
            "Binder Error: Type 'INTEGER' does not take any type parameters"
        );
        // Constants without an alias are named after their SQL text.
        assert_eq!(
            error(unbound("ENUM", vec![constant(Value::varchar("a"))])),
            "Binder Error: ENUM type arguments cannot have names (argument 1 has name \"'a'\")"
        );
    }
}
