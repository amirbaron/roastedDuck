use std::fmt;
use std::sync::Arc;

use crate::PhysicalType;

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
            // ENUM's physical type depends on its dictionary size, which is not modelled yet.
            L::Table
            | L::Lambda
            | L::Any
            | L::Invalid
            | L::Unknown
            | L::StringLiteral
            | L::Enum
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
        match (self.id, self.info()) {
            (L::Decimal, Some(TypeInfo::Decimal { width, scale })) => write!(f, "DECIMAL({width},{scale})"),
            (L::List, Some(TypeInfo::List { child })) => write!(f, "{child}[]"),
            (L::Array, Some(TypeInfo::Array { child, size })) => write!(f, "{child}[{size}]"),
            (L::Struct, Some(TypeInfo::Struct { children })) => {
                f.write_str("STRUCT(")?;
                for (i, (name, ty)) in children.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name} {ty}")?;
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
