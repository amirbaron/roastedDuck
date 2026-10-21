use std::fmt;
use std::sync::Arc;

use crate::identifier::{sql_identifier, write_quoted};
use crate::{
    AnyTypeInfo, ArrayTypeInfo, DecimalTypeInfo, EnumTypeInfo, ExtraTypeInfo, IntegerLiteralTypeInfo,
    ListTypeInfo, LogicalTypeId, PhysicalType, StructTypeInfo, TypeInfo, Value,
};

/// A SQL type: an id plus optional parameters, cheap to clone.
///
/// The parameters are DuckDB's `ExtraTypeInfo`: the subclass-specific data in
/// [`TypeInfo`] plus the members every info carries (an alias such as `JSON` on a
/// `VARCHAR`, and extension metadata).
///
/// Equality is DuckDB's `LogicalType::operator==`: a VARCHAR's collation does not count,
/// and a parameterless info (STRING or GENERIC) equals no info unless it carries an alias
/// or extension metadata.
#[derive(Clone)]
pub struct LogicalType {
    id: LogicalTypeId,
    info: Option<Info>,
}

/// The parameters of a [`LogicalType`].
#[derive(Clone)]
enum Info {
    /// Parameters shared through a reference count.
    Shared(Arc<ExtraTypeInfo>),
    /// The interned parameters of a plain DECIMAL. Vectors clone their type all the
    /// time; with a shared reference count, threads cloning the same DECIMAL type would
    /// contend on it.
    Static(&'static ExtraTypeInfo),
}

impl Info {
    fn new(info: ExtraTypeInfo) -> Self {
        if let TypeInfo::Decimal(DecimalTypeInfo { width, scale }) = info.kind
            && info.alias.is_empty()
            && info.extension_info.is_none()
            && let Some(interned) = interned_decimal(width, scale)
        {
            return Self::Static(interned);
        }
        Self::Shared(Arc::new(info))
    }

    fn get(&self) -> &ExtraTypeInfo {
        match self {
            Self::Shared(info) => info,
            Self::Static(info) => info,
        }
    }

    fn into_inner(self) -> ExtraTypeInfo {
        match self {
            Self::Shared(info) => Arc::unwrap_or_clone(info),
            Self::Static(info) => info.clone(),
        }
    }
}

/// The largest DECIMAL width (`DecimalType::MAX_WIDTH_DECIMAL`).
const MAX_DECIMAL_WIDTH: u8 = 38;

/// The parameters of every plain `DECIMAL(width, scale)`, created once.
fn interned_decimal(width: u8, scale: u8) -> Option<&'static ExtraTypeInfo> {
    static DECIMALS: std::sync::OnceLock<Box<[ExtraTypeInfo]>> = std::sync::OnceLock::new();
    if width > MAX_DECIMAL_WIDTH || scale > MAX_DECIMAL_WIDTH {
        return None;
    }
    let side = usize::from(MAX_DECIMAL_WIDTH) + 1;
    let all = DECIMALS.get_or_init(|| {
        (0..side * side)
            .map(|index| ExtraTypeInfo {
                alias: String::new(),
                extension_info: None,
                kind: TypeInfo::Decimal(DecimalTypeInfo {
                    width: (index / side) as u8,
                    scale: (index % side) as u8,
                }),
            })
            .collect()
    });
    all.get(usize::from(width) * side + usize::from(scale))
}

impl LogicalType {
    // Parameterless types, named like DuckDB's `LogicalType::INTEGER` etc.
    pub const INVALID: Self = Self::simple(LogicalTypeId::Invalid);
    pub const SQLNULL: Self = Self::simple(LogicalTypeId::SqlNull);
    pub const UNKNOWN: Self = Self::simple(LogicalTypeId::Unknown);
    pub const BOOLEAN: Self = Self::simple(LogicalTypeId::Boolean);
    pub const TINYINT: Self = Self::simple(LogicalTypeId::TinyInt);
    pub const SMALLINT: Self = Self::simple(LogicalTypeId::SmallInt);
    pub const INTEGER: Self = Self::simple(LogicalTypeId::Integer);
    pub const BIGINT: Self = Self::simple(LogicalTypeId::BigInt);
    pub const HUGEINT: Self = Self::simple(LogicalTypeId::HugeInt);
    pub const UTINYINT: Self = Self::simple(LogicalTypeId::UTinyInt);
    pub const USMALLINT: Self = Self::simple(LogicalTypeId::USmallInt);
    pub const UINTEGER: Self = Self::simple(LogicalTypeId::UInteger);
    pub const UBIGINT: Self = Self::simple(LogicalTypeId::UBigInt);
    pub const UHUGEINT: Self = Self::simple(LogicalTypeId::UHugeInt);
    pub const FLOAT: Self = Self::simple(LogicalTypeId::Float);
    pub const DOUBLE: Self = Self::simple(LogicalTypeId::Double);
    pub const VARCHAR: Self = Self::simple(LogicalTypeId::Varchar);
    pub const BLOB: Self = Self::simple(LogicalTypeId::Blob);
    pub const DATE: Self = Self::simple(LogicalTypeId::Date);
    pub const TIME: Self = Self::simple(LogicalTypeId::Time);
    pub const TIMESTAMP: Self = Self::simple(LogicalTypeId::Timestamp);
    pub const INTERVAL: Self = Self::simple(LogicalTypeId::Interval);
    pub const GEOMETRY: Self = Self::simple(LogicalTypeId::Geometry);
    pub const STRING_LITERAL: Self = Self::simple(LogicalTypeId::StringLiteral);
    pub const ANY: Self = Self::simple(LogicalTypeId::Any);
    pub const CHAR: Self = Self::simple(LogicalTypeId::Char);
    pub const TIME_NS: Self = Self::simple(LogicalTypeId::TimeNs);
    pub const TIME_TZ: Self = Self::simple(LogicalTypeId::TimeTz);
    pub const TIMESTAMP_S: Self = Self::simple(LogicalTypeId::TimestampSec);
    pub const TIMESTAMP_MS: Self = Self::simple(LogicalTypeId::TimestampMs);
    pub const TIMESTAMP_NS: Self = Self::simple(LogicalTypeId::TimestampNs);
    pub const TIMESTAMP_TZ: Self = Self::simple(LogicalTypeId::TimestampTz);
    pub const UUID: Self = Self::simple(LogicalTypeId::Uuid);
    pub const BIT: Self = Self::simple(LogicalTypeId::Bit);
    pub const BIGNUM: Self = Self::simple(LogicalTypeId::Bignum);
    pub const POINTER: Self = Self::simple(LogicalTypeId::Pointer);

    /// DuckDB's maximum DECIMAL width.
    pub const MAX_DECIMAL_WIDTH: u8 = 38;

    /// A type without parameters.
    pub const fn simple(id: LogicalTypeId) -> Self {
        Self { id, info: None }
    }

    /// `DECIMAL(width, scale)`.
    pub fn decimal(width: u8, scale: u8) -> Self {
        Self::with_info(
            LogicalTypeId::Decimal,
            TypeInfo::Decimal(DecimalTypeInfo { width, scale }),
        )
    }

    /// `child[]`.
    pub fn list(child: Self) -> Self {
        Self::with_info(
            LogicalTypeId::List,
            TypeInfo::List(ListTypeInfo { child_type: child }),
        )
    }

    /// `child[size]`.
    pub fn array(child: Self, size: u32) -> Self {
        Self::with_info(
            LogicalTypeId::Array,
            TypeInfo::Array(ArrayTypeInfo {
                child_type: child,
                size,
            }),
        )
    }

    /// `STRUCT(name type, ...)`.
    pub fn struct_type(children: Vec<(String, Self)>) -> Self {
        Self::with_info(
            LogicalTypeId::Struct,
            TypeInfo::Struct(StructTypeInfo {
                child_types: children,
            }),
        )
    }

    /// `AGGREGATE_STATE<function(arguments)::return_type>`: the exported state of an
    /// aggregate, stored as its raw bytes.
    pub fn aggregate_state(function_name: impl Into<String>, return_type: Self, bound_argument_types: Vec<Self>) -> Self {
        Self::with_info(
            LogicalTypeId::AggregateState,
            TypeInfo::AggregateState(crate::AggregateStateTypeInfo {
                function_name: function_name.into(),
                return_type,
                bound_argument_types,
            }),
        )
    }

    /// `MAP(key, value)`: a list of `{key, value}` structs.
    pub fn map(key: Self, value: Self) -> Self {
        let entry = Self::struct_type(vec![("key".to_owned(), key), ("value".to_owned(), value)]);
        Self::with_info(
            LogicalTypeId::Map,
            TypeInfo::List(ListTypeInfo { child_type: entry }),
        )
    }

    /// `UNION(name type, ...)`: a struct whose first, unnamed field is the member tag.
    pub fn union_type(members: Vec<(String, Self)>) -> Self {
        let mut children = Vec::with_capacity(members.len() + 1);
        children.push((String::new(), Self::UTINYINT));
        children.extend(members);
        Self::with_info(
            LogicalTypeId::Union,
            TypeInfo::Struct(StructTypeInfo {
                child_types: children,
            }),
        )
    }

    /// `VARIANT`: stored as keys, a children index, values and the data blob.
    pub fn variant() -> Self {
        let entries = |fields: [(&str, Self); 2]| {
            Self::list(Self::struct_type(
                fields
                    .into_iter()
                    .map(|(name, ty)| (name.to_owned(), ty))
                    .collect(),
            ))
        };
        let children = vec![
            ("keys".to_owned(), Self::list(Self::VARCHAR)),
            (
                "children".to_owned(),
                entries([("keys_index", Self::UINTEGER), ("values_index", Self::UINTEGER)]),
            ),
            (
                "values".to_owned(),
                entries([("type_id", Self::UTINYINT), ("byte_offset", Self::UINTEGER)]),
            ),
            ("data".to_owned(), Self::BLOB),
        ];
        Self::with_info(
            LogicalTypeId::Variant,
            TypeInfo::Struct(StructTypeInfo {
                child_types: children,
            }),
        )
    }

    /// `GEOMETRY(crs)` (`LogicalType::GEOMETRY(const string &)`): the definition is kept in
    /// the normalized form `CoordinateReferenceSystem` parses it to; an empty one gives
    /// plain GEOMETRY. Fails on malformed WKT.
    pub fn geometry_with_crs(definition: impl Into<String>) -> rustdb_common::Result<Self> {
        let definition = definition.into();
        if definition.is_empty() {
            return Ok(Self::GEOMETRY);
        }
        let crs = crate::geometry::Crs::parse(&definition)?;
        Ok(Self::with_info(
            LogicalTypeId::Geometry,
            TypeInfo::Geo(crate::GeoTypeInfo {
                crs: crate::CoordinateReferenceSystem {
                    definition: crs.definition,
                },
            }),
        ))
    }

    /// `INTEGER_LITERAL`: the literal value is kept so binding can pick the narrowest type.
    pub fn integer_literal(constant: Value) -> Self {
        Self::with_info(
            LogicalTypeId::IntegerLiteral,
            TypeInfo::IntegerLiteral(IntegerLiteralTypeInfo {
                constant_value: constant,
            }),
        )
    }

    /// `ANY` with a custom implicit cast score (DuckDB's `LogicalType::ANY_PARAMS`): an
    /// overload taking it loses to every overload whose casts cost less than `cast_score`.
    pub fn any_params(target_type: Self, cast_score: u64) -> Self {
        Self::with_info(
            LogicalTypeId::Any,
            TypeInfo::Any(AnyTypeInfo {
                target_type,
                cast_score,
            }),
        )
    }

    /// `ENUM('a', 'b', ...)`.
    pub fn enum_type(values: Vec<String>) -> Self {
        Self::with_info(LogicalTypeId::Enum, TypeInfo::Enum(EnumTypeInfo { values }))
    }

    /// A type with parameters (and no alias or extension metadata).
    pub fn with_info(id: LogicalTypeId, info: TypeInfo) -> Self {
        Self::from_parts(
            id,
            Some(ExtraTypeInfo {
                alias: String::new(),
                extension_info: None,
                kind: info,
            }),
        )
    }

    /// A type from an id and DuckDB's complete `ExtraTypeInfo`.
    pub fn from_parts(id: LogicalTypeId, info: Option<ExtraTypeInfo>) -> Self {
        Self {
            id,
            info: info.map(Info::new),
        }
    }

    /// This type under an alias (`JSON` is `VARCHAR` aliased as `JSON`).
    #[must_use]
    pub fn with_alias(self, alias: impl Into<String>) -> Self {
        let mut info = self.info.map_or_else(
            || ExtraTypeInfo {
                alias: String::new(),
                extension_info: None,
                kind: TypeInfo::Generic,
            },
            Info::into_inner,
        );
        info.alias = alias.into();
        Self::from_parts(self.id, Some(info))
    }

    /// The type id.
    pub const fn id(&self) -> LogicalTypeId {
        self.id
    }

    /// The subclass-specific parameters, if any.
    pub fn info(&self) -> Option<&TypeInfo> {
        self.info.as_ref().map(|info| &info.get().kind)
    }

    /// DuckDB's complete `ExtraTypeInfo`, including alias and extension metadata.
    pub fn extra_info(&self) -> Option<&ExtraTypeInfo> {
        self.info.as_ref().map(Info::get)
    }

    /// The alias, if one is set.
    pub fn alias(&self) -> Option<&str> {
        self.extra_info()
            .map(|info| info.alias.as_str())
            .filter(|alias| !alias.is_empty())
    }

    /// `(width, scale)` of a DECIMAL type.
    pub fn decimal_properties(&self) -> Option<(u8, u8)> {
        match self.info() {
            Some(TypeInfo::Decimal(info)) => Some((info.width, info.scale)),
            _ => None,
        }
    }

    /// The element type of a LIST, MAP or ARRAY.
    pub fn child_type(&self) -> Option<&Self> {
        match self.info() {
            Some(TypeInfo::List(info)) => Some(&info.child_type),
            Some(TypeInfo::Array(info)) => Some(&info.child_type),
            _ => None,
        }
    }

    /// The `(name, type)` fields of a STRUCT or UNION (for UNION, the tag comes first).
    pub fn struct_children(&self) -> Option<&[(String, Self)]> {
        match self.info() {
            Some(TypeInfo::Struct(info)) => Some(&info.child_types),
            _ => None,
        }
    }

    /// `(element type, size)` of an ARRAY; a size of 0 means "any size".
    pub fn array_child(&self) -> Option<(&Self, u32)> {
        match self.info() {
            Some(TypeInfo::Array(info)) => Some((&info.child_type, info.size)),
            _ => None,
        }
    }

    /// Whether this is a STRUCT whose fields have no names (`ROW(1, 2)`).
    pub fn is_unnamed_struct(&self) -> bool {
        self.struct_children()
            .and_then(<[_]>::first)
            .is_some_and(|(name, _)| name.is_empty())
    }

    /// The members of a UNION (without the tag field).
    pub fn union_members(&self) -> Option<&[(String, Self)]> {
        match (self.id, self.info()) {
            (LogicalTypeId::Union, Some(TypeInfo::Struct(info))) => info.child_types.get(1..),
            _ => None,
        }
    }

    /// `(key type, value type)` of a MAP.
    pub fn map_key_value(&self) -> Option<(&Self, &Self)> {
        if self.id != LogicalTypeId::Map {
            return None;
        }
        match self.child_type()?.struct_children()? {
            [(_, key), (_, value)] => Some((key, value)),
            _ => None,
        }
    }

    /// The members of an ENUM type, in definition order.
    pub fn enum_values(&self) -> Option<&[String]> {
        match self.info() {
            Some(TypeInfo::Enum(info)) => Some(&info.values),
            _ => None,
        }
    }

    /// The constant of an `INTEGER_LITERAL` type.
    pub fn integer_literal_value(&self) -> Option<&Value> {
        match self.info() {
            Some(TypeInfo::IntegerLiteral(info)) => Some(&info.constant_value),
            _ => None,
        }
    }

    /// The type an `INTEGER_LITERAL` has when used as-is (DuckDB's `IntegerLiteral::GetType`).
    pub fn integer_literal_type(&self) -> Option<&Self> {
        self.integer_literal_value().map(Value::logical_type)
    }

    /// `(width, scale)` of the DECIMAL that holds every value of a numeric type
    /// (DuckDB's `GetDecimalProperties`).
    pub fn numeric_decimal_properties(&self) -> Option<(u8, u8)> {
        use LogicalTypeId as L;
        let width = match self.id {
            L::Boolean => 1,
            L::TinyInt | L::UTinyInt => 3,
            L::SmallInt | L::USmallInt => 5,
            L::Integer | L::UInteger => 10,
            L::BigInt => 19,
            L::UBigInt => 20,
            L::HugeInt | L::UHugeInt => 38,
            L::Decimal => return self.decimal_properties(),
            L::IntegerLiteral => return self.integer_literal_type()?.numeric_decimal_properties(),
            _ => return None,
        };
        Some((width, 0))
    }

    /// Whether the type is fully specified (DuckDB's `LogicalTypeIsValid`): no `ANY`,
    /// `UNKNOWN` or `INVALID` anywhere, and parameterised types carry their parameters.
    pub fn is_valid(&self) -> bool {
        use LogicalTypeId as L;
        if matches!(
            self.id,
            L::Struct | L::Union | L::Variant | L::List | L::Map | L::Array | L::Decimal
        ) && self.info().is_none()
        {
            return false;
        }
        match self.id {
            L::Any | L::Invalid | L::Unknown => false,
            L::Struct | L::Union => self
                .struct_children()
                .is_none_or(|children| children.iter().all(|(_, ty)| ty.is_valid())),
            L::List | L::Map | L::Array => self.child_type().is_none_or(Self::is_valid),
            _ => true,
        }
    }

    /// The physical representation (DuckDB's `GetInternalType`).
    pub fn physical_type(&self) -> PhysicalType {
        use LogicalTypeId as L;
        match self.id {
            L::Boolean => PhysicalType::Bool,
            L::TinyInt => PhysicalType::Int8,
            L::UTinyInt => PhysicalType::UInt8,
            L::SmallInt => PhysicalType::Int16,
            L::USmallInt => PhysicalType::UInt16,
            L::SqlNull | L::Date | L::Integer => PhysicalType::Int32,
            L::UInteger => PhysicalType::UInt32,
            L::BigInt
            | L::Time
            | L::TimeNs
            | L::Timestamp
            | L::TimestampSec
            | L::TimestampNs
            | L::TimestampMs
            | L::TimeTz
            | L::TimestampTz => PhysicalType::Int64,
            L::UBigInt | L::Pointer => PhysicalType::UInt64,
            L::UHugeInt => PhysicalType::UInt128,
            L::HugeInt | L::Uuid => PhysicalType::Int128,
            L::Float => PhysicalType::Float,
            L::Double => PhysicalType::Double,
            L::Decimal => match self.decimal_properties() {
                Some((w, _)) if w <= 4 => PhysicalType::Int16,
                Some((w, _)) if w <= 9 => PhysicalType::Int32,
                Some((w, _)) if w <= 18 => PhysicalType::Int64,
                Some((w, _)) if w <= 38 => PhysicalType::Int128,
                _ => PhysicalType::Invalid,
            },
            L::Bignum | L::Varchar | L::Char | L::Blob | L::Bit | L::Type | L::Geometry | L::AggregateState => {
                PhysicalType::Varchar
            }
            L::Interval => PhysicalType::Interval,
            L::Union | L::Variant | L::Struct => PhysicalType::Struct,
            L::List | L::Map => PhysicalType::List,
            L::Array => PhysicalType::Array,
            L::Validity => PhysicalType::Bit,
            L::Enum => match self.info() {
                Some(TypeInfo::Enum(info)) => info.dictionary_type(),
                _ => PhysicalType::Invalid,
            },
            L::Table
            | L::Lambda
            | L::Any
            | L::Invalid
            | L::Unknown
            | L::StringLiteral
            | L::IntegerLiteral
            | L::Unbound
            | L::Template => PhysicalType::Invalid,
        }
    }

    /// Whether this is an integer, floating-point or decimal type.
    pub fn is_numeric(&self) -> bool {
        use LogicalTypeId as L;
        matches!(
            self.id,
            L::TinyInt
                | L::SmallInt
                | L::Integer
                | L::BigInt
                | L::HugeInt
                | L::Float
                | L::Double
                | L::Decimal
                | L::UTinyInt
                | L::USmallInt
                | L::UInteger
                | L::UBigInt
                | L::UHugeInt
        )
    }

    /// Whether this is a signed or unsigned integer type.
    pub fn is_integral(&self) -> bool {
        use LogicalTypeId as L;
        matches!(
            self.id,
            L::TinyInt
                | L::SmallInt
                | L::Integer
                | L::BigInt
                | L::HugeInt
                | L::UTinyInt
                | L::USmallInt
                | L::UInteger
                | L::UBigInt
                | L::UHugeInt
        )
    }

    /// Whether this is a LIST, MAP, STRUCT, UNION, ARRAY or VARIANT.
    pub const fn is_nested(&self) -> bool {
        use LogicalTypeId as L;
        matches!(
            self.id,
            L::List | L::Map | L::Struct | L::Union | L::Array | L::Variant
        )
    }
}

/// Whether `info` is a STRING or GENERIC info, whose contents do not take part in equality
/// (`ExtraTypeInfo::Equals`).
fn info_without_contents(info: &ExtraTypeInfo) -> bool {
    matches!(info.kind, TypeInfo::String(_) | TypeInfo::Generic)
}

impl PartialEq for LogicalType {
    /// `LogicalType::operator==` and `EqualTypeInfo`.
    fn eq(&self, other: &Self) -> bool {
        if self.id != other.id {
            return false;
        }
        match (self.extra_info(), other.extra_info()) {
            (None, None) => true,
            (Some(info), None) | (None, Some(info)) => {
                info_without_contents(info) && info.alias.is_empty() && info.extension_info.is_none()
            }
            (Some(left), Some(right)) => {
                if left.alias != right.alias || left.extension_info != right.extension_info {
                    return false;
                }
                if info_without_contents(left) {
                    return true;
                }
                left.kind == right.kind
            }
        }
    }
}

impl Eq for LogicalType {}

impl std::hash::Hash for LogicalType {
    /// Consistent with [`PartialEq`]: the contents of STRING and GENERIC infos are skipped.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        match self.extra_info() {
            None => "".hash(state),
            Some(info) => {
                info.alias.hash(state);
                if !info_without_contents(info) {
                    info.kind.hash(state);
                }
            }
        }
    }
}

impl Default for LogicalType {
    /// `INVALID`, like DuckDB's default-constructed `LogicalType`.
    fn default() -> Self {
        Self::INVALID
    }
}

impl fmt::Debug for LogicalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for LogicalType {
    /// DuckDB's `LogicalType::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use LogicalTypeId as L;
        if let Some(alias) = self.alias() {
            return f.write_str(alias);
        }
        let write_fields = |f: &mut fmt::Formatter<'_>, fields: &[(String, Self)], named: bool| {
            for (i, (name, ty)) in fields.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                if named {
                    write!(f, "{} ", sql_identifier(name))?;
                }
                write!(f, "{ty}")?;
            }
            f.write_str(")")
        };
        match (self.id, self.info()) {
            (L::Decimal, Some(TypeInfo::Decimal(info))) if info.width > 0 => {
                write!(f, "DECIMAL({},{})", info.width, info.scale)
            }
            (L::List, Some(TypeInfo::List(info))) => write!(f, "{}[]", info.child_type),
            (L::Map, Some(_)) => match self.map_key_value() {
                Some((key, value)) => write!(f, "MAP({key}, {value})"),
                None => f.write_str("MAP"),
            },
            (L::Array, Some(TypeInfo::Array(info))) if info.size == 0 => {
                write!(f, "{}[ANY]", info.child_type)
            }
            (L::Array, Some(TypeInfo::Array(info))) => write!(f, "{}[{}]", info.child_type, info.size),
            (L::Struct, Some(TypeInfo::Struct(info))) => {
                f.write_str("STRUCT(")?;
                write_fields(f, &info.child_types, !self.is_unnamed_struct())
            }
            (L::Union, Some(TypeInfo::Struct(info))) => {
                f.write_str("UNION(")?;
                write_fields(f, info.child_types.get(1..).unwrap_or_default(), true)
            }
            (L::Enum, Some(TypeInfo::Enum(info))) => {
                f.write_str("ENUM(")?;
                for (i, value) in info.values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(&write_quoted(value, '\''))?;
                }
                f.write_str(")")
            }
            (L::Template, Some(TypeInfo::Template(info))) => f.write_str(&info.name),
            (L::Template, None) => f.write_str("T"),
            // `AggregateStateType::GetTypeName`.
            (L::AggregateState, Some(TypeInfo::AggregateState(info))) => {
                write!(f, "AGGREGATE_STATE<{}(", info.function_name)?;
                for (i, argument) in info.bound_argument_types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{argument}")?;
                }
                write!(f, ")::{}>", info.return_type)
            }
            (L::AggregateState, None) => f.write_str("AGGREGATE_STATE<?>"),
            (L::Geometry, Some(TypeInfo::Geo(info))) if !info.crs.definition.is_empty() => {
                write!(f, "GEOMETRY({})", write_quoted(&info.crs.definition, '\''))
            }
            (id, _) => f.write_str(type_id_name(id)),
        }
    }
}

/// DuckDB's `EnumUtil::ToString(LogicalTypeId)` as used by `LogicalType::ToString`.
pub(crate) const fn type_id_name(id: LogicalTypeId) -> &'static str {
    match id {
        LogicalTypeId::SqlNull => "\"NULL\"",
        other => other.name(),
    }
}
