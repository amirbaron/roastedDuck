//! What a piece of buffer-managed memory is used for (DuckDB's `MemoryTag`).

/// The subsystem an allocation belongs to; reported by `duckdb_memory()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum MemoryTag {
    BaseTable = 0,
    HashTable = 1,
    ParquetReader = 2,
    CsvReader = 3,
    OrderBy = 4,
    ArtIndex = 5,
    ColumnData = 6,
    Metadata = 7,
    OverflowStrings = 8,
    InMemoryTable = 9,
    Allocator = 10,
    Extension = 11,
    Transaction = 12,
    ExternalFileCache = 13,
    Window = 14,
    ObjectCache = 15,
}

/// Number of [`MemoryTag`] values.
pub const MEMORY_TAG_COUNT: usize = 16;

impl MemoryTag {
    /// Every tag, in numeric order.
    pub const ALL: [Self; MEMORY_TAG_COUNT] = [
        Self::BaseTable,
        Self::HashTable,
        Self::ParquetReader,
        Self::CsvReader,
        Self::OrderBy,
        Self::ArtIndex,
        Self::ColumnData,
        Self::Metadata,
        Self::OverflowStrings,
        Self::InMemoryTable,
        Self::Allocator,
        Self::Extension,
        Self::Transaction,
        Self::ExternalFileCache,
        Self::Window,
        Self::ObjectCache,
    ];

    /// The name shown by `duckdb_memory()` (`EnumUtil::ToString`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::BaseTable => "BASE_TABLE",
            Self::HashTable => "HASH_TABLE",
            Self::ParquetReader => "PARQUET_READER",
            Self::CsvReader => "CSV_READER",
            Self::OrderBy => "ORDER_BY",
            Self::ArtIndex => "ART_INDEX",
            Self::ColumnData => "COLUMN_DATA",
            Self::Metadata => "METADATA",
            Self::OverflowStrings => "OVERFLOW_STRINGS",
            Self::InMemoryTable => "IN_MEMORY_TABLE",
            Self::Allocator => "ALLOCATOR",
            Self::Extension => "EXTENSION",
            Self::Transaction => "TRANSACTION",
            Self::ExternalFileCache => "EXTERNAL_FILE_CACHE",
            Self::Window => "WINDOW",
            Self::ObjectCache => "OBJECT_CACHE",
        }
    }

    /// The tag's index into per-tag counters.
    pub const fn index(self) -> usize {
        self as usize
    }
}
