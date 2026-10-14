use std::fmt;
use std::sync::Arc;

use crate::PhysicalType;
use crate::identifier::{sql_identifier, write_quoted};

/// SQL type identifiers, numbered exactly like DuckDB's `LogicalTypeId`.
///
/// The numbering is part of the storage format and the C API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogicalTypeId {
    Invalid = 0,
    SqlNull = 1,
    Unknown = 2,
    Any = 3,
    Unbound = 4,
    Template = 5,
    Type = 6,
    Boolean = 10,
    TinyInt = 11,
    SmallInt = 12,
    Integer = 13,
    BigInt = 14,
    Date = 15,
    Time = 16,
    TimestampSec = 17,
    TimestampMs = 18,
    Timestamp = 19,
    TimestampNs = 20,
    Decimal = 21,
    Float = 22,
    Double = 23,
    Char = 24,
    Varchar = 25,
    Blob = 26,
    Interval = 27,
    UTinyInt = 28,
    USmallInt = 29,
    UInteger = 30,
    UBigInt = 31,
    TimestampTz = 32,
    TimeTz = 34,
    TimeNs = 35,
    Bit = 36,
    StringLiteral = 37,
    IntegerLiteral = 38,
    Bignum = 39,
    UHugeInt = 49,
    HugeInt = 50,
    Pointer = 51,
    Validity = 53,
    Uuid = 54,
    Geometry = 60,
    Struct = 100,
    List = 101,
    Map = 102,
    Table = 103,
    Enum = 104,
    AggregateState = 105,
    Lambda = 106,
    Union = 107,
    Array = 108,
    Variant = 109,
}

/// Extra information carried by parameterised types.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeInfo {
    Decimal {
        width: u8,
        scale: u8,
    },
    List {
        child: LogicalType,
    },
    Array {
        child: LogicalType,
        size: u32,
    },
    Struct {
        children: Vec<(String, LogicalType)>,
    },
    /// `INTEGER_LITERAL`: the literal value is kept so binding can pick the narrowest type.
    IntegerLiteral {
        value: i128,
    },
    /// ENUM: the dictionary, in declaration order (the stored value is the index).
    Enum {
        values: Vec<String>,
    },
}

/// A SQL type: an id plus optional parameters, cheap to clone.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LogicalType {
    id: LogicalTypeId,
    info: Option<Arc<TypeInfo>>,
}

impl LogicalType {
    pub const SQLNULL: Self = Self::simple(LogicalTypeId::SqlNull);
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
    pub const STRING_LITERAL: Self = Self::simple(LogicalTypeId::StringLiteral);
    pub const UNKNOWN: Self = Self::simple(LogicalTypeId::Unknown);
    pub const INVALID: Self = Self::simple(LogicalTypeId::Invalid);
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

    pub fn decimal(width: u8, scale: u8) -> Self {
        Self::with_info(LogicalTypeId::Decimal, TypeInfo::Decimal { width, scale })
    }

    pub fn list(child: LogicalType) -> Self {
        Self::with_info(LogicalTypeId::List, TypeInfo::List { child })
    }

    pub fn integer_literal(value: i128) -> Self {
        Self::with_info(LogicalTypeId::IntegerLiteral, TypeInfo::IntegerLiteral { value })
    }

    /// `STRUCT(name type, ...)`; unnamed structs have empty names.
    pub fn struct_type(children: Vec<(String, LogicalType)>) -> Self {
        Self::with_info(LogicalTypeId::Struct, TypeInfo::Struct { children })
    }

    /// `child[size]`; a size of 0 means "any size" (`child[ANY]`).
    pub fn array(child: LogicalType, size: u32) -> Self {
        Self::with_info(LogicalTypeId::Array, TypeInfo::Array { child, size })
    }

    /// `MAP(key, value)`, stored like DuckDB as `LIST(STRUCT(key, value))`.
    pub fn map(key: LogicalType, value: LogicalType) -> Self {
        let entry = Self::struct_type(vec![("key".to_owned(), key), ("value".to_owned(), value)]);
        Self::with_info(LogicalTypeId::Map, TypeInfo::List { child: entry })
    }

    /// `UNION(name type, ...)`, stored like DuckDB as a STRUCT whose first child is the
    /// `UTINYINT` tag.
    pub fn union_type(members: Vec<(String, LogicalType)>) -> Self {
        let mut children = Vec::with_capacity(members.len() + 1);
        children.push((String::new(), Self::UTINYINT));
        children.extend(members);
        Self::with_info(LogicalTypeId::Union, TypeInfo::Struct { children })
    }

    /// `ENUM('a', 'b', ...)`.
    pub fn enum_type(values: Vec<String>) -> Self {
        Self::with_info(LogicalTypeId::Enum, TypeInfo::Enum { values })
    }

    pub fn with_info(id: LogicalTypeId, info: TypeInfo) -> Self {
        Self {
            id,
            info: Some(Arc::new(info)),
        }
    }

    pub const fn id(&self) -> LogicalTypeId {
        self.id
    }

    pub fn info(&self) -> Option<&TypeInfo> {
        self.info.as_deref()
    }

    /// `(width, scale)` of a DECIMAL type.
    pub fn decimal_properties(&self) -> Option<(u8, u8)> {
        match self.info() {
            Some(TypeInfo::Decimal { width, scale }) => Some((*width, *scale)),
            _ => None,
        }
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

    /// The element type of a LIST (or the `STRUCT(key, value)` entry type of a MAP).
    pub fn list_child(&self) -> Option<&LogicalType> {
        match self.info() {
            Some(TypeInfo::List { child }) => Some(child),
            _ => None,
        }
    }

    /// `(element type, size)` of an ARRAY; a size of 0 means "any size".
    pub fn array_child(&self) -> Option<(&LogicalType, u32)> {
        match self.info() {
            Some(TypeInfo::Array { child, size }) => Some((child, *size)),
            _ => None,
        }
    }

    /// The fields of a STRUCT, or of a UNION including its leading tag field.
    pub fn struct_children(&self) -> Option<&[(String, LogicalType)]> {
        match self.info() {
            Some(TypeInfo::Struct { children }) => Some(children),
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
    pub fn union_members(&self) -> Option<&[(String, LogicalType)]> {
        match (self.id, self.info()) {
            (LogicalTypeId::Union, Some(TypeInfo::Struct { children })) => children.get(1..),
            _ => None,
        }
    }

    /// `(key type, value type)` of a MAP.
    pub fn map_key_value(&self) -> Option<(&LogicalType, &LogicalType)> {
        if self.id != LogicalTypeId::Map {
            return None;
        }
        match self.list_child()?.struct_children()? {
            [(_, key), (_, value)] => Some((key, value)),
            _ => None,
        }
    }

    /// The dictionary of an ENUM type.
    pub fn enum_values(&self) -> Option<&[String]> {
        match self.info() {
            Some(TypeInfo::Enum { values }) => Some(values),
            _ => None,
        }
    }

    /// The value of an `INTEGER_LITERAL` type.
    pub fn integer_literal_value(&self) -> Option<i128> {
        match self.info() {
            Some(TypeInfo::IntegerLiteral { value }) => Some(*value),
            _ => None,
        }
    }

    /// The type an `INTEGER_LITERAL` has when used as-is (DuckDB's `IntegerLiteral::GetType`):
    /// the narrowest of INTEGER, BIGINT and HUGEINT holding the literal.
    pub fn integer_literal_type(&self) -> Option<LogicalType> {
        let value = self.integer_literal_value()?;
        Some(if i32::try_from(value).is_ok() {
            Self::INTEGER
        } else if i64::try_from(value).is_ok() {
            Self::BIGINT
        } else {
            Self::HUGEINT
        })
    }

    /// Whether the type has child types (STRUCT, LIST, MAP, ARRAY, UNION, VARIANT).
    pub fn is_nested(&self) -> bool {
        use LogicalTypeId as L;
        matches!(
            self.id,
            L::Struct | L::List | L::Map | L::Array | L::Union | L::Variant
        )
    }

    /// Whether the type is fully specified (DuckDB's `LogicalTypeIsValid`): no `ANY`,
    /// `UNKNOWN` or `INVALID` anywhere, and parameterised types carry their parameters.
    pub fn is_valid(&self) -> bool {
        use LogicalTypeId as L;
        if matches!(
            self.id,
            L::Struct | L::Union | L::Variant | L::List | L::Map | L::Array | L::Decimal
        ) && self.info.is_none()
        {
            return false;
        }
        match self.id {
            L::Any | L::Invalid | L::Unknown => false,
            L::Struct | L::Union => self
                .struct_children()
                .is_none_or(|children| children.iter().all(|(_, ty)| ty.is_valid())),
            L::List | L::Map => self.list_child().is_none_or(Self::is_valid),
            L::Array => self.array_child().is_none_or(|(child, _)| child.is_valid()),
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
            L::Bignum | L::Varchar | L::Char | L::Blob | L::Bit | L::Type | L::Geometry => {
                PhysicalType::Varchar
            }
            L::Interval => PhysicalType::Interval,
            L::Union | L::Variant | L::Struct => PhysicalType::Struct,
            L::List | L::Map => PhysicalType::List,
            L::Array => PhysicalType::Array,
            L::Validity => PhysicalType::Bit,
            // DuckDB's `EnumTypeInfo::DictType`.
            L::Enum => match self.enum_values().map_or(0, <[_]>::len) {
                n if u8::try_from(n).is_ok() => PhysicalType::UInt8,
                n if u16::try_from(n).is_ok() => PhysicalType::UInt16,
                _ => PhysicalType::UInt32,
            },
            L::Table
            | L::Lambda
            | L::Any
            | L::Invalid
            | L::Unknown
            | L::StringLiteral
            | L::IntegerLiteral
            | L::AggregateState
            | L::Unbound
            | L::Template => PhysicalType::Invalid,
        }
    }

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
        let write_fields = |f: &mut fmt::Formatter<'_>, fields: &[(String, LogicalType)], named: bool| {
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
            (L::Decimal, Some(TypeInfo::Decimal { width, scale })) if *width > 0 => {
                write!(f, "DECIMAL({width},{scale})")
            }
            (L::List, Some(TypeInfo::List { child })) => write!(f, "{child}[]"),
            (L::Map, Some(_)) => match self.map_key_value() {
                Some((key, value)) => write!(f, "MAP({key}, {value})"),
                None => f.write_str("MAP"),
            },
            (L::Array, Some(TypeInfo::Array { child, size: 0 })) => write!(f, "{child}[ANY]"),
            (L::Array, Some(TypeInfo::Array { child, size })) => write!(f, "{child}[{size}]"),
            (L::Struct, Some(TypeInfo::Struct { children })) => {
                f.write_str("STRUCT(")?;
                write_fields(f, children, !self.is_unnamed_struct())
            }
            (L::Union, Some(TypeInfo::Struct { children })) => {
                f.write_str("UNION(")?;
                write_fields(f, children.get(1..).unwrap_or_default(), true)
            }
            (L::Enum, Some(TypeInfo::Enum { values })) => {
                f.write_str("ENUM(")?;
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(&write_quoted(value, '\''))?;
                }
                f.write_str(")")
            }
            (id, _) => f.write_str(type_id_name(id)),
        }
    }
}

/// DuckDB's `EnumUtil::ToString(LogicalTypeId)` as used by `LogicalType::ToString`.
pub(crate) const fn type_id_name(id: LogicalTypeId) -> &'static str {
    use LogicalTypeId as L;
    match id {
        L::Invalid => "INVALID",
        L::SqlNull => "\"NULL\"",
        L::Unknown => "UNKNOWN",
        L::Any => "ANY",
        L::Unbound => "UNBOUND",
        L::Template => "TEMPLATE",
        L::Type => "TYPE",
        L::Boolean => "BOOLEAN",
        L::TinyInt => "TINYINT",
        L::SmallInt => "SMALLINT",
        L::Integer => "INTEGER",
        L::BigInt => "BIGINT",
        L::Date => "DATE",
        L::Time => "TIME",
        L::TimestampSec => "TIMESTAMP_S",
        L::TimestampMs => "TIMESTAMP_MS",
        L::Timestamp => "TIMESTAMP",
        L::TimestampNs => "TIMESTAMP_NS",
        L::Decimal => "DECIMAL",
        L::Float => "FLOAT",
        L::Double => "DOUBLE",
        L::Char => "CHAR",
        L::Varchar => "VARCHAR",
        L::Blob => "BLOB",
        L::Interval => "INTERVAL",
        L::UTinyInt => "UTINYINT",
        L::USmallInt => "USMALLINT",
        L::UInteger => "UINTEGER",
        L::UBigInt => "UBIGINT",
        L::TimestampTz => "TIMESTAMP WITH TIME ZONE",
        L::TimeTz => "TIME WITH TIME ZONE",
        L::TimeNs => "TIME_NS",
        L::Bit => "BIT",
        L::StringLiteral => "STRING_LITERAL",
        L::IntegerLiteral => "INTEGER_LITERAL",
        L::Bignum => "BIGNUM",
        L::UHugeInt => "UHUGEINT",
        L::HugeInt => "HUGEINT",
        L::Pointer => "POINTER",
        L::Validity => "VALIDITY",
        L::Uuid => "UUID",
        L::Geometry => "GEOMETRY",
        L::Struct => "STRUCT",
        L::List => "LIST",
        L::Map => "MAP",
        L::Table => "TABLE",
        L::Enum => "ENUM",
        L::AggregateState => "AGGREGATE_STATE",
        L::Lambda => "LAMBDA",
        L::Union => "UNION",
        L::Array => "ARRAY",
        L::Variant => "VARIANT",
    }
}
