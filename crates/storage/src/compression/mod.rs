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

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, PhysicalType, Vector};

use crate::checkpoint::column_checkpointer::SegmentSink;
use crate::data_table::TableIoManager;
use crate::table::ColumnSegment;
use crate::table_filter::TableFilter;

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

    /// Whether no row of the segment can pass `filter`, which rejects NULLs, as far as the
    /// codec can tell cheaply (DuckDB's RLE `Filter` evaluates the filter on the run values
    /// once and skips the segment when no run matches). `false` means "unknown".
    fn never_matches(&mut self, segment: &ColumnSegment, filter: &TableFilter) -> Result<bool> {
        let _ = (segment, filter);
        Ok(false)
    }

    /// Scans rows `start..start + count` into row 0 of `result` like
    /// [`SegmentScan::scan`], decoding only the rows of `sel` (ascending, relative to
    /// `start`); the others may hold anything (DuckDB's `CompressionFunction::select`).
    /// Returns `false`, doing nothing, if the codec cannot do better than a full scan.
    fn select(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        sel: &[u32],
    ) -> Result<bool> {
        let _ = (segment, start, count, result, sel);
        Ok(false)
    }

    /// Rows `start..start + count` (a whole vector) as a dictionary vector over the
    /// segment's dictionary, if the codec has one (DuckDB's `ScanToDictionaryVector`).
    fn scan_dictionary(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
    ) -> Result<Option<Vector>> {
        let _ = (segment, start, count);
        Ok(None)
    }

    /// Scans rows `start..start + count` like [`SegmentScan::scan`] (into row 0 of
    /// `result`) while keeping only the rows of `sel` whose stored value passes `filter`
    /// (DuckDB's `CompressionFunction::filter`); rows dropped from `sel` need not be
    /// written, and without `needs_values` (a column scanned for its filter only) no row
    /// needs to be. Returns `false`, doing nothing, if the codec cannot do this better
    /// than a scan followed by the filter. `filter` rejects NULLs; the caller drops NULL
    /// rows.
    #[allow(clippy::too_many_arguments, reason = "DuckDB's filter function signature")]
    fn filter(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
        needs_values: bool,
    ) -> Result<bool> {
        let _ = (segment, start, count, result, sel, filter, needs_values);
        Ok(false)
    }
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
    /// [`SegmentScan::never_matches`]; does not move the state.
    fn never_matches(&mut self, segment: &ColumnSegment, filter: &TableFilter) -> Result<bool> {
        let _ = (segment, filter);
        Ok(false)
    }

    /// Whether the codec implements [`CodecScan::filter_partial`].
    const FILTERS: bool = false;

    /// [`SegmentScan::filter`] over the next `count` rows, with `sel` holding every row
    /// `0..count`.
    fn filter_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
    ) -> Result<()> {
        let _ = (segment, count, result, sel, filter);
        Err(Error::internal("codec without a filter"))
    }
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

    fn never_matches(&mut self, segment: &ColumnSegment, filter: &TableFilter) -> Result<bool> {
        self.state.never_matches(segment, filter)
    }

    fn filter(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
        _needs_values: bool,
    ) -> Result<bool> {
        // Only for a selection of every row (the first filter of a vector).
        if !S::FILTERS || sel.len() != count {
            return Ok(false);
        }
        if start < self.position {
            self.state = S::init(segment)?;
            self.position = 0;
        }
        if start > self.position {
            self.state.skip(segment, start - self.position)?;
        }
        self.state.filter_partial(segment, count, result, sel, filter)?;
        self.position = start + count;
        Ok(true)
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

impl CompressionType {
    /// The name `EnumUtil` uses (`force_compression` values, case-insensitive).
    pub const fn enum_name(self) -> &'static str {
        match self {
            Self::Auto => "AUTO",
            Self::Uncompressed => "UNCOMPRESSED",
            Self::Constant => "CONSTANT",
            Self::Rle => "RLE",
            Self::Dictionary => "DICTIONARY",
            Self::PforDelta => "PFOR",
            Self::BitPacking => "BITPACKING",
            Self::Fsst => "FSST",
            Self::Chimp => "CHIMP",
            Self::Patas => "PATAS",
            Self::Alp => "ALP",
            Self::AlpRd => "ALPRD",
            Self::Zstd => "ZSTD",
            Self::Roaring => "ROARING",
            Self::Empty => "EMPTY",
            Self::DictFsst => "DICT_FSST",
        }
    }

    /// `EnumUtil::FromString<CompressionType>`.
    pub fn from_enum_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|compression| compression.enum_name().eq_ignore_ascii_case(name))
    }
}

/// How checkpoints compress segments: the database-wide settings DuckDB reads in
/// `ColumnDataCheckpointer` and the codecs (`force_compression`,
/// `zstd_min_string_length`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionConfig {
    /// `force_compression`: the codec to use where it applies (`Auto` lets every codec
    /// compete).
    pub force_compression: CompressionType,
    /// `zstd_min_string_length`: below this average string length ZSTD is penalised.
    pub zstd_min_string_length: u64,
    /// `disabled_compression_methods`: a bit per disabled compression type
    /// (`CompressionFunctionSet::is_disabled`).
    pub disabled: u32,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            force_compression: CompressionType::Auto,
            zstd_min_string_length: 4096,
            disabled: 0,
        }
    }
}

impl CompressionConfig {
    /// Whether `compression` is disabled (`disabled_compression_methods`).
    pub const fn is_disabled(&self, compression: CompressionType) -> bool {
        self.disabled & (1 << compression as u32) != 0
    }

    /// Disables exactly `methods` (`SetDisabledCompressionMethods`).
    pub fn set_disabled(&mut self, methods: &[CompressionType]) {
        self.disabled = methods
            .iter()
            .fold(0, |bits, &compression| bits | (1 << compression as u32));
    }

    /// The disabled methods (`GetDisabledCompressionMethods`).
    pub fn disabled_methods(&self) -> Vec<CompressionType> {
        CompressionType::ALL
            .into_iter()
            .filter(|&compression| self.is_disabled(compression))
            .collect()
    }
}

/// What a codec knows about the database it writes to (DuckDB's `CompressionInfo` plus the
/// storage version and settings the codecs consult).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompressionInfo {
    /// The usable size of a block.
    pub block_size: usize,
    /// The serialization version written (`StorageManager::GetStorageVersion`).
    pub storage_version: u64,
    pub config: CompressionConfig,
}

impl CompressionInfo {
    /// `GetCompactionFlushLimit`: segments filled beyond this are not compacted.
    pub(crate) fn compaction_flush_limit(&self) -> usize {
        self.block_size / 5 * 4
    }
}

/// The analyze step of a codec (`init_analyze` / `analyze` / `final_analyze`).
pub(crate) trait Analyzer {
    /// Accounts for `count` rows of `input`; `false` rules the codec out.
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool>;
    /// The estimated size of the compressed data (`None`: the codec cannot be used).
    fn final_analyze(&mut self) -> Result<Option<u64>>;
    /// `init_compression`: the compression state, built from the analysis.
    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>>;
}

/// The compression step of a codec (`compress` / `compress_finalize`).
pub(crate) trait Compressor {
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()>;
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()>;
}

/// `EmitCompressionFunction` + `TypeIsSupported`: the codecs that compete for a column of
/// `physical` type, in DuckDB's registry order (ties go to the earlier one).
pub(crate) fn candidate_functions(physical: PhysicalType) -> Vec<CompressionType> {
    use CompressionType as C;
    use PhysicalType as P;
    let integer = matches!(
        physical,
        P::Bool
            | P::Int8
            | P::Int16
            | P::Int32
            | P::Int64
            | P::Int128
            | P::UInt8
            | P::UInt16
            | P::UInt32
            | P::UInt64
            | P::UInt128
            | P::List
    );
    let float = matches!(physical, P::Float | P::Double);
    let string = physical == P::Varchar;
    let supported = |compression: C| match compression {
        C::Uncompressed => integer || float || string || matches!(physical, P::Interval | P::Bit),
        C::Rle => integer || float,
        C::BitPacking => integer,
        C::Dictionary | C::Fsst | C::Zstd | C::DictFsst => string,
        C::Chimp | C::Patas | C::Alp | C::AlpRd => float,
        C::Roaring => matches!(physical, P::Bit | P::Bool),
        C::Auto | C::Constant | C::PforDelta | C::Empty => false,
    };
    [
        C::Uncompressed,
        C::Rle,
        C::BitPacking,
        C::Dictionary,
        C::Chimp,
        C::Patas,
        C::Alp,
        C::AlpRd,
        C::Fsst,
        C::Zstd,
        C::Roaring,
        C::DictFsst,
    ]
    .into_iter()
    .filter(|&compression| supported(compression))
    .collect()
}

/// Whether a compression type can be written (DuckDB's `CompressionAvailabilityResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionAvailability {
    Available,
    /// Only decompression is supported.
    Deprecated,
    /// The storage version is too old for it.
    NotAvailableYet,
}

/// `CompressionTypeIsAvailable`: whether `compression` can be written with storage version
/// `storage_version` (`None`: unknown, so available unless always deprecated).
pub fn compression_type_is_available(
    compression: CompressionType,
    storage_version: Option<u64>,
) -> CompressionAvailability {
    use CompressionType as C;
    // (type, minimum storage version, maximum storage version); no minimum: deprecated.
    let (minimum, maximum): (Option<u64>, Option<u64>) = match compression {
        C::Patas | C::Chimp => (None, Some(0)),
        C::Dictionary | C::Fsst => (Some(0), Some(4)),
        C::DictFsst => (Some(5), None),
        _ => return CompressionAvailability::Available,
    };
    let Some(minimum) = minimum else {
        return CompressionAvailability::Deprecated;
    };
    let Some(current) = storage_version else {
        return CompressionAvailability::Available;
    };
    if minimum > current {
        return CompressionAvailability::NotAvailableYet;
    }
    if maximum.is_some_and(|maximum| maximum < current) {
        return CompressionAvailability::Deprecated;
    }
    CompressionAvailability::Available
}

/// `DBConfig::TryGetCompressionFunction`: whether `compression` has a function for
/// `physical` (the constant and empty-validity functions included).
pub fn has_compression_function(compression: CompressionType, physical: PhysicalType) -> bool {
    match compression {
        // `ConstantFun::TypeIsSupported` / `EmptyValidityCompressionFun::TypeIsSupported`.
        CompressionType::Constant => matches!(
            physical,
            PhysicalType::Bit
                | PhysicalType::Bool
                | PhysicalType::Int8
                | PhysicalType::Int16
                | PhysicalType::Int32
                | PhysicalType::Int64
                | PhysicalType::UInt8
                | PhysicalType::UInt16
                | PhysicalType::UInt32
                | PhysicalType::UInt64
                | PhysicalType::Int128
                | PhysicalType::UInt128
                | PhysicalType::Float
                | PhysicalType::Double
        ),
        CompressionType::Empty => physical == PhysicalType::Bit,
        other => candidate_functions(physical).contains(&other),
    }
}

/// Whether a codec stores the column's validity itself (`CompressionValidity::
/// NO_VALIDITY_REQUIRED`), so the validity column is written as an empty segment.
pub(crate) fn covers_validity(compression: CompressionType) -> bool {
    compression == CompressionType::DictFsst
}

/// `init_analyze` of `compression` for a column of type `ty` (`physical`); `None` when the
/// codec does not apply (unsupported type or storage version, deprecated codec).
pub(crate) fn init_analyze(
    compression: CompressionType,
    ty: &LogicalType,
    physical: PhysicalType,
    info: &CompressionInfo,
    io: &Arc<TableIoManager>,
) -> Result<Option<Box<dyn Analyzer>>> {
    Ok(match compression {
        CompressionType::Uncompressed => Some(uncompressed::init_analyze(ty, info, io)?),
        CompressionType::Rle => rle::init_analyze(ty, physical, info),
        CompressionType::BitPacking => bitpacking::init_analyze(ty, physical, info),
        CompressionType::Roaring => roaring::init_analyze(ty, physical, info),
        CompressionType::Alp => alp::init_analyze(ty, physical, info),
        CompressionType::AlpRd => alprd::init_analyze(ty, physical, info),
        CompressionType::Dictionary if physical == PhysicalType::Varchar => {
            dictionary::init_analyze(ty, info)
        }
        CompressionType::Fsst if physical == PhysicalType::Varchar => fsst::init_analyze(ty, info),
        CompressionType::DictFsst if physical == PhysicalType::Varchar => dict_fsst::init_analyze(ty, info),
        CompressionType::Zstd if physical == PhysicalType::Varchar => zstd::init_analyze(ty, info),
        _ => None,
    })
}
