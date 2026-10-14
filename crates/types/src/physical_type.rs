/// In-memory representation of a value, numbered like DuckDB's `PhysicalType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PhysicalType {
    Bool = 1,
    UInt8 = 2,
    Int8 = 3,
    UInt16 = 4,
    Int16 = 5,
    UInt32 = 6,
    Int32 = 7,
    UInt64 = 8,
    Int64 = 9,
    Float = 11,
    Double = 12,
    Interval = 21,
    List = 23,
    Struct = 24,
    Array = 29,
    Varchar = 200,
    UInt128 = 203,
    Int128 = 204,
    Unknown = 205,
    Bit = 206,
    Invalid = 255,
}

impl PhysicalType {
    /// Width in bytes of one value in a flat vector (DuckDB's `GetTypeIdSize`).
    pub const fn size(self) -> usize {
        match self {
            Self::Bool | Self::UInt8 | Self::Int8 => 1,
            Self::UInt16 | Self::Int16 => 2,
            Self::UInt32 | Self::Int32 | Self::Float => 4,
            Self::UInt64 | Self::Int64 | Self::Double => 8,
            Self::Interval | Self::Varchar | Self::UInt128 | Self::Int128 | Self::List => 16,
            Self::Struct | Self::Array | Self::Unknown | Self::Bit | Self::Invalid => 0,
        }
    }
}
