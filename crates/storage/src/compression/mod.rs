//! Segment encodings (DuckDB's `CompressionFunction`s in `storage/compression`).
//!
//! Every [`ColumnSegment`](crate::table::ColumnSegment) is encoded with one
//! [`CompressionType`]. Transient (in-memory, appendable) segments are always
//! uncompressed; checkpoints pick an encoding per segment.
//!
//! Reading a compressed segment goes through a [`SegmentScan`]: the codec's scan state
//! (DuckDB's `SegmentScanState` subclass) created by [`init_scan`]. Codecs that can only be
//! read front to back implement [`CodecScan`] and are wrapped in a [`Sequential`] adapter,
//! which skips forward (or restarts) to the requested row the way DuckDB's
//! `ColumnSegment::Skip` and `FetchRow` do.

pub(crate) mod alp;
pub(crate) mod alprd;
pub(crate) mod bitpacking;
pub(crate) mod bitpacking_primitives;
pub(crate) mod chimp;
pub(crate) mod constant;
pub(crate) mod dict_fsst;
pub(crate) mod dictionary;
pub(crate) mod fsst;
pub(crate) mod libfsst;
pub(crate) mod patas;
pub(crate) mod primitive;
pub(crate) mod rle;
pub(crate) mod roaring;
pub(crate) mod uncompressed;
pub(crate) mod zstd;

use std::fmt;

use rustdb_common::{Error, Result};
use rustdb_types::{PhysicalType, Vector};

use crate::table::ColumnSegment;

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

/// The scan state of a compressed segment (DuckDB's codec-specific `SegmentScanState`
/// together with its `scan_partial` function).
pub trait SegmentScan: fmt::Debug + Send + Sync {
    /// Scans rows `start..start + count` of `segment` into rows
    /// `result_offset..result_offset + count` of the flat vector `result`
    /// (`ScanPartial` at `GetPositionInSegment() == start`).
    fn scan(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()>;
}

/// A codec whose scan state only moves forward (DuckDB's `InitScan`, `Skip` and
/// `ScanPartial` of codecs that decode their data sequentially).
pub(crate) trait CodecScan: fmt::Debug + Send + Sync + Sized {
    /// `InitScan`: a state positioned at the first row of `segment`.
    fn init(segment: &ColumnSegment) -> Result<Self>;
    /// `Skip`: moves the state `count` rows forward.
    fn skip(&mut self, segment: &ColumnSegment, count: usize) -> Result<()>;
    /// `ScanPartial`: decodes the next `count` rows into `result` at `result_offset`.
    fn scan_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()>;
}

/// Adapts a [`CodecScan`] to random positions: rows before the current position restart
/// the scan, rows after it are skipped to.
#[derive(Debug)]
pub(crate) struct Sequential<S> {
    state: S,
    position: usize,
}

impl<S: CodecScan + 'static> Sequential<S> {
    pub(crate) fn boxed(segment: &ColumnSegment) -> Result<Box<dyn SegmentScan>> {
        Ok(Box::new(Self {
            state: S::init(segment)?,
            position: 0,
        }))
    }
}

impl<S: CodecScan> SegmentScan for Sequential<S> {
    fn scan(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        if start < self.position {
            self.state = S::init(segment)?;
            self.position = 0;
        }
        if start > self.position {
            self.state.skip(segment, start - self.position)?;
        }
        self.state.scan_partial(segment, count, result, result_offset)?;
        self.position = start + count;
        Ok(())
    }
}

/// The physical type a segment of `segment`'s type stores (validity segments: `Bit`).
fn physical_type(segment: &ColumnSegment) -> PhysicalType {
    segment.logical_type().physical_type()
}

/// `CompressionFunction::init_scan` of a compressed (non-constant, non-uncompressed)
/// segment.
pub(crate) fn init_scan(segment: &ColumnSegment) -> Result<Box<dyn SegmentScan>> {
    let physical = physical_type(segment);
    let unsupported = || {
        Error::internal(format!(
            "unsupported physical type {} for {} segments",
            physical.name(),
            segment.compression()
        ))
    };
    match segment.compression() {
        CompressionType::Rle => rle::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::BitPacking => bitpacking::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::Alp => alp::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::AlpRd => alprd::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::Chimp => chimp::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::Patas => patas::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::Zstd if physical == PhysicalType::Varchar => {
            Ok(Box::new(zstd::ZstdScanState::new(segment)?))
        }
        CompressionType::Roaring => roaring::init_scan(segment, physical).ok_or_else(unsupported)?,
        CompressionType::Dictionary if physical == PhysicalType::Varchar => {
            Ok(Box::new(dictionary::DictionaryScanState::new(segment)?))
        }
        CompressionType::Fsst if physical == PhysicalType::Varchar => {
            Ok(Box::new(fsst::FsstScanState::new(segment)?))
        }
        CompressionType::DictFsst if physical == PhysicalType::Varchar => {
            Ok(Box::new(dict_fsst::DictFsstScanState::new(segment)?))
        }
        other => Err(Error::not_implemented(format!(
            "reading {other} compressed segments"
        ))),
    }
}

/// `CompressionFunction::fetch_row`: reads row `row` of a compressed segment into
/// `result[result_index]`.
pub(crate) fn fetch_row(
    segment: &ColumnSegment,
    row: usize,
    result: &mut Vector,
    result_index: usize,
) -> Result<()> {
    let mut state = init_scan(segment)?;
    state.scan(segment, row, 1, result, result_index)
}
