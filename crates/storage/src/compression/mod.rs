//! Segment encodings (DuckDB's `CompressionFunction`s in `storage/compression`).
//!
//! Every [`ColumnSegment`](crate::table::ColumnSegment) is encoded with one
//! [`CompressionType`]. Transient (in-memory, appendable) segments are always
//! uncompressed; checkpoints pick an encoding per segment.

pub(crate) mod constant;
pub(crate) mod uncompressed;

use std::fmt;

/// The encoding of a column segment (DuckDB's `CompressionType`; the discriminants are
/// stored on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum CompressionType {
    Auto = 0,
    Uncompressed = 1,
    /// Every value equals the segment's statistics (no block).
    Constant = 2,
    Rle = 3,
    Dictionary = 4,
    PforDelta = 5,
    BitPacking = 6,
    Fsst = 7,
    Chimp = 8,
    Patas = 9,
    Alp = 10,
    AlpRd = 11,
    Zstd = 12,
    Roaring = 13,
    /// A validity segment without NULLs (no block).
    Empty = 14,
    DictFsst = 15,
}

impl CompressionType {
    /// Every compression type, in discriminant order.
    pub const ALL: [Self; 16] = [
        Self::Auto,
        Self::Uncompressed,
        Self::Constant,
        Self::Rle,
        Self::Dictionary,
        Self::PforDelta,
        Self::BitPacking,
        Self::Fsst,
        Self::Chimp,
        Self::Patas,
        Self::Alp,
        Self::AlpRd,
        Self::Zstd,
        Self::Roaring,
        Self::Empty,
        Self::DictFsst,
    ];

    /// The type with discriminant `value`.
    pub fn from_u8(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }

    /// The name DuckDB shows in `pragma_storage_info` (`CompressionTypeToString`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Uncompressed => "Uncompressed",
            Self::Constant => "Constant",
            Self::Rle => "RLE",
            Self::Dictionary => "Dictionary",
            Self::PforDelta => "PFOR",
            Self::BitPacking => "BitPacking",
            Self::Fsst => "FSST",
            Self::Chimp => "Chimp",
            Self::Patas => "Patas",
            Self::Alp => "ALP",
            Self::AlpRd => "ALPRD",
            Self::Zstd => "ZSTD",
            Self::Roaring => "Roaring",
            Self::Empty => "Empty Validity",
            Self::DictFsst => "DICT_FSST",
        }
    }
}

impl fmt::Display for CompressionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
