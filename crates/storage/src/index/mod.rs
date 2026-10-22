//! Indexes over table data (DuckDB's `src/execution/index`): the adaptive radix tree
//! that enforces PRIMARY KEY, UNIQUE and FOREIGN KEY constraints and serves index scans,
//! its fixed-size allocators, and the per-table index list.

pub mod art;
pub mod conflict_manager;
pub mod fixed_size_allocator;
pub mod table_index_list;
pub mod unbound_index;

pub use art::{Art, ArtKey, IndexKeyExpressions};
pub use conflict_manager::ConflictManager;
pub use fixed_size_allocator::{
    BufferLoader, BufferPointer, FixedSizeAllocator, FixedSizeAllocatorInfo, IndexBufferWriter,
};
pub use table_index_list::{IndexEntry, IndexSlot, TableIndexList};
pub use unbound_index::{UnboundIndex, UnboundIndexStorage};

/// The constraint an index enforces (DuckDB's `IndexConstraintType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IndexConstraintType {
    /// A plain index.
    None = 0,
    Unique = 1,
    Primary = 2,
    /// The index of the referencing columns of a FOREIGN KEY.
    Foreign = 3,
}

impl IndexConstraintType {
    /// The name used in generated index names (`EnumUtil::ToString`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Unique => "UNIQUE",
            Self::Primary => "PRIMARY",
            Self::Foreign => "FOREIGN",
        }
    }
}

/// How an append treats keys that already exist (DuckDB's `IndexAppendMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndexAppendMode {
    /// Duplicates in a unique index are a constraint violation.
    #[default]
    Default,
    /// Duplicates in a unique index are skipped.
    IgnoreDuplicates,
    /// Duplicates are inserted even into a unique index (WAL replay, checkpoint deltas).
    InsertDuplicates,
}

/// What a constraint lookup verifies (DuckDB's `VerifyExistenceType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyExistenceType {
    /// Appending to a PRIMARY KEY/UNIQUE table: the keys must not exist.
    Append,
    /// Appending to a FOREIGN KEY table: the keys must exist in the referenced table.
    AppendFk,
    /// Deleting from a referenced table: the keys must not be referenced.
    DeleteFk,
}

/// How deleted rows leave the indexes at commit (DuckDB's `IndexRemovalType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexRemovalType {
    /// Remove from the main index and remember the rows for older transactions.
    MainIndex,
    /// Remove from the main index only (no other transaction is active).
    MainIndexOnly,
    /// Undo [`IndexRemovalType::MainIndex`].
    RevertMainIndex,
    /// Undo [`IndexRemovalType::MainIndexOnly`].
    RevertMainIndexOnly,
    /// Forget rows remembered for older transactions (cleanup).
    DeletedRowsInUse,
}

/// A pointer into the memory of a fixed-size allocator: buffer id (bits 0-31), segment
/// offset (bits 32-55) and one byte of metadata (bits 56-63), DuckDB's `IndexPointer`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct IndexPointer(u64);

impl IndexPointer {
    const SHIFT_OFFSET: u32 = 32;
    const SHIFT_METADATA: u32 = 56;
    const AND_OFFSET: u64 = 0x0000_0000_00FF_FFFF;
    const AND_BUFFER_ID: u64 = 0x0000_0000_FFFF_FFFF;
    const AND_METADATA: u64 = 0xFF00_0000_0000_0000;

    pub const fn new(buffer_id: u32, offset: u32) -> Self {
        Self(((offset as u64) << Self::SHIFT_OFFSET) + buffer_id as u64)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn has_metadata(self) -> bool {
        self.0 & Self::AND_METADATA != 0
    }

    pub const fn metadata(self) -> u8 {
        (self.0 >> Self::SHIFT_METADATA) as u8
    }

    pub fn set_metadata(&mut self, metadata: u8) {
        self.0 &= !Self::AND_METADATA;
        self.0 |= u64::from(metadata) << Self::SHIFT_METADATA;
    }

    pub const fn offset(self) -> u64 {
        (self.0 >> Self::SHIFT_OFFSET) & Self::AND_OFFSET
    }

    pub const fn buffer_id(self) -> u64 {
        self.0 & Self::AND_BUFFER_ID
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    /// Shifts the buffer id (used when merging allocators).
    pub fn increase_buffer_id(&mut self, summand: u64) {
        self.0 += summand;
    }
}

impl std::fmt::Debug for IndexPointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IndexPointer(meta={:#x}, buffer={}, offset={})",
            self.metadata(),
            self.buffer_id(),
            self.offset()
        )
    }
}
