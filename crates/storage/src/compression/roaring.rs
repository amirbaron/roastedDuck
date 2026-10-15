//! Roaring bitmap compression of validity masks and booleans (DuckDB's
//! `compression/roaring/*`).
//!
//! The rows of a segment are split into containers of [`ROARING_CONTAINER_SIZE`] bits,
//! each stored as a run container (runs of NULLs), an array container (the positions of
//! the NULLs or of the valid rows) or a bitset. Runs and arrays with many entries are
//! "compressed": they store one count per 256-row segment of the container and one byte
//! per position. Segment layout: an 8-byte offset of the container metadata, the container
//! data, and the metadata (2-bit container types, 7-bit run counts, 8-bit array sizes).

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, PhysicalType, Vector};

use super::bitpacking_primitives::{pack_buffer_u64, required_size, round_up_to_group, unpack_buffer_u64};
use super::primitive::{align8, bit, corrupt, load, set_invalid_where, update_numeric_stats, write_values};
use super::{Analyzer, CompressionInfo, CompressionType, Compressor, SegmentScan};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// The rows of a run/array segment (`COMPRESSED_SEGMENT_SIZE`).
pub(crate) const COMPRESSED_SEGMENT_SIZE: usize = 256;
/// Rows per container (`ROARING_CONTAINER_SIZE`).
pub(crate) const ROARING_CONTAINER_SIZE: usize = 2048;
/// `UNCOMPRESSED_SIZE`: the bytes of a bitset container.
pub(crate) const UNCOMPRESSED_SIZE: usize = ROARING_CONTAINER_SIZE / 8;
/// `COMPRESSED_SEGMENT_COUNT`.
pub(crate) const COMPRESSED_SEGMENT_COUNT: usize = ROARING_CONTAINER_SIZE / COMPRESSED_SEGMENT_SIZE;
/// `MAX_RUN_IDX`.
pub(crate) const MAX_RUN_IDX: usize = (UNCOMPRESSED_SIZE - COMPRESSED_SEGMENT_COUNT) / 2;
/// `MAX_ARRAY_IDX`.
pub(crate) const MAX_ARRAY_IDX: usize = UNCOMPRESSED_SIZE - COMPRESSED_SEGMENT_COUNT;
/// The array size marking a bitset container (`BITSET_CONTAINER_SENTINEL_VALUE`).
pub(crate) const BITSET_CONTAINER_SENTINEL_VALUE: usize = MAX_ARRAY_IDX + 1;
/// Arrays of at least this many entries are compressed (`COMPRESSED_ARRAY_THRESHOLD`).
pub(crate) const COMPRESSED_ARRAY_THRESHOLD: usize = 8;
/// Runs of at least this many entries are compressed (`COMPRESSED_RUN_THRESHOLD`).
pub(crate) const COMPRESSED_RUN_THRESHOLD: usize = 4;
/// `CONTAINER_TYPE_BITWIDTH`.
pub(crate) const CONTAINER_TYPE_BITWIDTH: u8 = 2;
/// `RUN_CONTAINER_SIZE_BITWIDTH`.
pub(crate) const RUN_CONTAINER_SIZE_BITWIDTH: u8 = 7;

/// The kind of a container (`ContainerType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainerType {
    Run,
    Array,
    Bitset,
}

/// `ContainerMetadata`: the kind of a container, whether it lists NULLs (`nulls`, always
/// for runs) or valid rows, and its run count / array size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContainerMetadata {
    pub container_type: ContainerType,
    pub nulls: bool,
    pub count: u16,
}

impl ContainerMetadata {
    /// `GetDataSizeInBytes`.
    pub(crate) fn data_size(self, container_size: usize) -> usize {
        match self.container_type {
            ContainerType::Bitset => container_size / 64 * 8,
            ContainerType::Run => {
                let runs = usize::from(self.count);
                if runs >= COMPRESSED_RUN_THRESHOLD {
                    COMPRESSED_SEGMENT_COUNT + runs * 2
                } else {
                    runs * 4
                }
            }
            ContainerType::Array => {
                let cardinality = usize::from(self.count);
                if cardinality >= COMPRESSED_ARRAY_THRESHOLD {
                    COMPRESSED_SEGMENT_COUNT + cardinality
                } else {
                    cardinality * 2
                }
            }
        }
    }
}

/// `ContainerMetadataCollection::Deserialize` + `ContainerMetadataCollectionScanner`.
fn read_metadata(bytes: &[u8], mut offset: usize, container_count: usize) -> Result<Vec<ContainerMetadata>> {
    let mut types = vec![0u64; round_up_to_group(container_count)];
    let types_size = required_size(types.len(), CONTAINER_TYPE_BITWIDTH);
    unpack_buffer_u64(
        bytes.get(offset..).unwrap_or(&[]),
        container_count,
        CONTAINER_TYPE_BITWIDTH,
        &mut types,
    );
    offset += types_size;
    let runs = types[..container_count]
        .iter()
        .filter(|&&t| (t >> 1) & 1 == 1)
        .count();
    let mut run_counts = vec![0u64; round_up_to_group(runs)];
    if runs > 0 {
        unpack_buffer_u64(
            bytes.get(offset..).unwrap_or(&[]),
            runs,
            RUN_CONTAINER_SIZE_BITWIDTH,
            &mut run_counts,
        );
        offset += required_size(runs, RUN_CONTAINER_SIZE_BITWIDTH);
    }
    let arrays = container_count - runs;
    let cardinalities = bytes
        .get(offset..offset + arrays)
        .ok_or_else(|| corrupt("invalid metadata offset in RoaringScanState constructor"))?;
    let (mut run_index, mut array_index) = (0, 0);
    let mut result = Vec::with_capacity(container_count);
    for &container_type in &types[..container_count] {
        let inverted = container_type & 1 == 1;
        if (container_type >> 1) & 1 == 1 {
            let count = run_counts[run_index] as u16;
            run_index += 1;
            result.push(ContainerMetadata {
                container_type: ContainerType::Run,
                nulls: true,
                count,
            });
        } else {
            let count = usize::from(cardinalities[array_index]);
            array_index += 1;
            result.push(if count == BITSET_CONTAINER_SENTINEL_VALUE {
                ContainerMetadata {
                    container_type: ContainerType::Bitset,
                    nulls: true,
                    count: count as u16,
                }
            } else {
                ContainerMetadata {
                    container_type: ContainerType::Array,
                    nulls: inverted,
                    count: count as u16,
                }
            });
        }
    }
    Ok(result)
}

/// `ContainerSegmentScan`: yields the base row of the 256-row segment each successive
/// compressed entry lies in.
struct SegmentBases<'a> {
    counts: &'a [u8],
    index: usize,
    count: u8,
}

impl SegmentBases<'_> {
    fn next_base(&mut self) -> usize {
        while self.index < COMPRESSED_SEGMENT_COUNT && self.count >= self.counts[self.index] {
            self.count = 0;
            self.index += 1;
        }
        self.count = self.count.wrapping_add(1);
        self.index * COMPRESSED_SEGMENT_SIZE
    }
}

/// The scan state of a Roaring segment (DuckDB's `RoaringScanState`): each container is
/// decoded into a validity bitmap when first scanned.
#[derive(Debug)]
pub(crate) struct RoaringScanState {
    data: PersistentData,
    is_bool: bool,
    count: usize,
    containers: Vec<ContainerMetadata>,
    data_start: Vec<usize>,
    /// The index and bitmap (bit set = valid / true) of the decoded container.
    current: Option<(usize, [u64; ROARING_CONTAINER_SIZE / 64])>,
}

impl RoaringScanState {
    pub(crate) fn new(segment: &ColumnSegment, is_bool: bool) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let metadata_offset: u64 = load(bytes, 0)?;
        let metadata_offset = usize::try_from(metadata_offset)
            .ok()
            .filter(|&offset| offset < segment.block_size())
            .ok_or_else(|| corrupt("invalid metadata offset in RoaringScanState constructor"))?;
        let count = segment.count() as usize;
        let container_count = count.div_ceil(ROARING_CONTAINER_SIZE);
        let containers = read_metadata(bytes, 8 + metadata_offset, container_count)?;
        let mut data_start = Vec::with_capacity(container_count);
        let mut position: usize = 0;
        for metadata in &containers {
            match metadata.container_type {
                ContainerType::Bitset => position = position.next_multiple_of(8),
                ContainerType::Array if usize::from(metadata.count) < COMPRESSED_ARRAY_THRESHOLD => {
                    position = position.next_multiple_of(2);
                }
                ContainerType::Run if usize::from(metadata.count) < COMPRESSED_RUN_THRESHOLD => {
                    position = position.next_multiple_of(4);
                }
                _ => {}
            }
            data_start.push(8 + position);
            position += metadata.data_size(ROARING_CONTAINER_SIZE);
        }
        Ok(Self {
            data,
            is_bool,
            count,
            containers,
            data_start,
            current: None,
        })
    }

    /// Decodes container `index` into a bitmap (`LoadContainer` + the container scans).
    fn decode(&self, index: usize) -> Result<[u64; ROARING_CONTAINER_SIZE / 64]> {
        let bytes = self.data.bytes();
        let metadata = self.containers[index];
        let start = self.data_start[index];
        let container_size = (self.count - index * ROARING_CONTAINER_SIZE).min(ROARING_CONTAINER_SIZE);
        let out_of_range = || corrupt("Roaring container out of range - corrupt database file");
        let mut bits = [u64::MAX; ROARING_CONTAINER_SIZE / 64];
        let set = |bits: &mut [u64; 32], row: usize, valid: bool| {
            if row < ROARING_CONTAINER_SIZE {
                if valid {
                    bits[row / 64] |= 1 << (row % 64);
                } else {
                    bits[row / 64] &= !(1 << (row % 64));
                }
            }
        };
        let count = usize::from(metadata.count);
        match metadata.container_type {
            ContainerType::Bitset => {
                for (word_index, word) in bits.iter_mut().enumerate().take(container_size.div_ceil(64)) {
                    *word = load::<u64>(bytes, start + word_index * 8)?;
                }
            }
            ContainerType::Run => {
                let mut runs = Vec::with_capacity(count);
                if count >= COMPRESSED_RUN_THRESHOLD {
                    let counts = bytes
                        .get(start..start + COMPRESSED_SEGMENT_COUNT)
                        .ok_or_else(out_of_range)?;
                    let data = bytes
                        .get(start + COMPRESSED_SEGMENT_COUNT..start + COMPRESSED_SEGMENT_COUNT + 2 * count)
                        .ok_or_else(out_of_range)?;
                    let mut bases = SegmentBases {
                        counts,
                        index: 0,
                        count: 0,
                    };
                    for run in data.as_chunks::<2>().0 {
                        let run_start = bases.next_base() + usize::from(run[0]);
                        let run_end = bases.next_base() + usize::from(run[1]);
                        runs.push((run_start, run_end));
                    }
                } else {
                    for run in 0..count {
                        let run_start = usize::from(load::<u16>(bytes, start + run * 4)?);
                        let length = usize::from(load::<u16>(bytes, start + run * 4 + 2)?);
                        runs.push((run_start, run_start + 1 + length));
                    }
                }
                for (run_start, run_end) in runs {
                    for row in run_start..run_end.min(ROARING_CONTAINER_SIZE) {
                        set(&mut bits, row, false);
                    }
                }
            }
            ContainerType::Array => {
                if !metadata.nulls {
                    bits = [0; ROARING_CONTAINER_SIZE / 64];
                }
                if count >= COMPRESSED_ARRAY_THRESHOLD {
                    let counts = bytes
                        .get(start..start + COMPRESSED_SEGMENT_COUNT)
                        .ok_or_else(out_of_range)?;
                    let data = bytes
                        .get(start + COMPRESSED_SEGMENT_COUNT..start + COMPRESSED_SEGMENT_COUNT + count)
                        .ok_or_else(out_of_range)?;
                    let mut bases = SegmentBases {
                        counts,
                        index: 0,
                        count: 0,
                    };
                    for &value in data {
                        let row = bases.next_base() + usize::from(value);
                        set(&mut bits, row, !metadata.nulls);
                    }
                } else {
                    for entry in 0..count {
                        let row = usize::from(load::<u16>(bytes, start + entry * 2)?);
                        set(&mut bits, row, !metadata.nulls);
                    }
                }
            }
        }
        Ok(bits)
    }
}

impl SegmentScan for RoaringScanState {
    /// `RoaringScanPartial` / `RoaringScanPartialBoolean`.
    fn scan(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let row = start + scanned;
            let container = row / ROARING_CONTAINER_SIZE;
            let offset = row % ROARING_CONTAINER_SIZE;
            if self.current.is_none_or(|(index, _)| index != container) {
                if container >= self.containers.len() {
                    return Err(corrupt("Roaring scan out of range - corrupt database file"));
                }
                self.current = Some((container, self.decode(container)?));
            }
            let Some((_, bits)) = &self.current else {
                return Err(corrupt("Roaring container missing"));
            };
            let to_scan = (count - scanned).min(ROARING_CONTAINER_SIZE - offset);
            if self.is_bool {
                let values: Vec<u8> = (0..to_scan).map(|i| u8::from(bit(bits, offset + i))).collect();
                write_values(result, result_offset + scanned, &values)?;
            } else {
                set_invalid_where(result, result_offset + scanned, to_scan, |i| {
                    bit(bits, offset + i)
                });
            }
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `RoaringCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    let is_bool = match physical {
        PhysicalType::Bit => false,
        PhysicalType::Bool => true,
        _ => return None,
    };
    Some(RoaringScanState::new(segment, is_bool).map(|state| Box::new(state) as Box<dyn SegmentScan>))
}

impl ContainerMetadata {
    /// `ContainerMetadata::CreateMetadata`: the cheapest container for `count` values with
    /// `array_null` NULLs, `array_non_null` valid values and `runs` runs of NULLs.
    pub(crate) fn create(count: u16, array_null: u16, array_non_null: u16, runs: u16) -> Self {
        let bitset = Self {
            container_type: ContainerType::Bitset,
            nulls: true,
            count,
        };
        let can_use_null_array = usize::from(array_null) < MAX_ARRAY_IDX;
        let can_use_non_null_array = usize::from(array_non_null) < MAX_ARRAY_IDX;
        let can_use_run = usize::from(runs) < MAX_RUN_IDX;
        if !can_use_null_array && !can_use_non_null_array && !can_use_run {
            return bitset;
        }
        let array_cost = |cardinality: u16| -> u16 {
            if usize::from(cardinality) < COMPRESSED_ARRAY_THRESHOLD {
                cardinality.wrapping_mul(2)
            } else {
                (COMPRESSED_SEGMENT_COUNT as u16).wrapping_add(cardinality)
            }
        };
        let lowest_array_cost = array_cost(array_null).min(array_cost(array_non_null));
        let lowest_run_cost = if usize::from(runs) < COMPRESSED_RUN_THRESHOLD {
            runs.wrapping_mul(4)
        } else {
            (COMPRESSED_SEGMENT_COUNT as u16).wrapping_add(runs.wrapping_mul(2))
        };
        let bitset_cost = count.div_ceil(64) * 8;
        if lowest_array_cost.min(lowest_run_cost) > bitset_cost {
            return bitset;
        }
        if lowest_array_cost <= lowest_run_cost {
            if array_null <= array_non_null {
                Self {
                    container_type: ContainerType::Array,
                    nulls: true,
                    count: array_null,
                }
            } else {
                Self {
                    container_type: ContainerType::Array,
                    nulls: false,
                    count: array_non_null,
                }
            }
        } else {
            Self {
                container_type: ContainerType::Run,
                nulls: true,
                count: runs,
            }
        }
    }
}

/// The container metadata written so far (`ContainerMetadataCollection`): container types,
/// run counts and array sizes, and how many of them belong to the current segment.
#[derive(Debug, Default)]
struct MetadataCollection {
    container_type: Vec<u8>,
    number_of_runs: Vec<u8>,
    cardinality: Vec<u8>,
    count_in_segment: usize,
    runs_in_segment: usize,
    arrays_in_segment: usize,
}

impl MetadataCollection {
    /// `AddMetadata`.
    fn add(&mut self, metadata: ContainerMetadata) {
        let (is_run, inverted) = match metadata.container_type {
            ContainerType::Run => (true, metadata.nulls),
            ContainerType::Bitset => (false, false),
            ContainerType::Array => (false, metadata.nulls),
        };
        self.container_type
            .push(u8::from(is_run) << 1 | u8::from(inverted));
        match metadata.container_type {
            ContainerType::Run => {
                self.number_of_runs.push(metadata.count as u8);
                self.runs_in_segment += 1;
            }
            ContainerType::Bitset => {
                self.cardinality.push(BITSET_CONTAINER_SENTINEL_VALUE as u8);
                self.arrays_in_segment += 1;
            }
            ContainerType::Array => {
                self.cardinality.push(metadata.count as u8);
                self.arrays_in_segment += 1;
            }
        }
        self.count_in_segment += 1;
    }

    /// `GetMetadataSize`.
    fn metadata_size(container_count: usize, run_containers: usize, array_containers: usize) -> usize {
        required_size(container_count, CONTAINER_TYPE_BITWIDTH)
            + required_size(run_containers, RUN_CONTAINER_SIZE_BITWIDTH)
            + array_containers
    }

    /// `GetMetadataSizeForSegment`.
    fn segment_metadata_size(&self) -> usize {
        Self::metadata_size(
            self.runs_in_segment + self.arrays_in_segment,
            self.runs_in_segment,
            self.arrays_in_segment,
        )
    }

    /// `FlushSegment`.
    fn flush_segment(&mut self) {
        self.runs_in_segment = 0;
        self.count_in_segment = 0;
        self.arrays_in_segment = 0;
    }

    /// `Serialize`: the metadata of the current segment; returns its size.
    fn serialize(&self, dest: &mut [u8]) -> usize {
        let types_size = required_size(self.count_in_segment, CONTAINER_TYPE_BITWIDTH);
        let runs_size = required_size(self.runs_in_segment, RUN_CONTAINER_SIZE_BITWIDTH);
        let types: Vec<u64> = self.container_type[self.container_type.len() - self.count_in_segment..]
            .iter()
            .map(|&t| u64::from(t))
            .collect();
        pack_buffer_u64(&types, CONTAINER_TYPE_BITWIDTH, &mut dest[..types_size]);
        let mut offset = types_size;
        if !self.number_of_runs.is_empty() {
            let runs: Vec<u64> = self.number_of_runs[self.number_of_runs.len() - self.runs_in_segment..]
                .iter()
                .map(|&r| u64::from(r))
                .collect();
            pack_buffer_u64(
                &runs,
                RUN_CONTAINER_SIZE_BITWIDTH,
                &mut dest[offset..offset + runs_size],
            );
            offset += runs_size;
        }
        if !self.cardinality.is_empty() {
            let arrays = &self.cardinality[self.cardinality.len() - self.arrays_in_segment..];
            dest[offset..offset + arrays.len()].copy_from_slice(arrays);
        }
        types_size + runs_size + self.arrays_in_segment
    }
}

/// The bits a vector contributes: its validity for validity columns, the values for
/// boolean columns (`BitPackBooleans`: NULL rows repeat the previous bit). `stats` receives
/// the boolean statistics.
fn vector_bits(
    input: &Vector,
    count: usize,
    is_bool: bool,
    stats: Option<(&mut BaseStatistics, PhysicalType)>,
    bits: &mut Vec<bool>,
) {
    bits.clear();
    let validity = input.validity();
    let sel = input.sel();
    if !is_bool {
        bits.extend((0..count).map(|row| validity.is_valid(sel.slot(row))));
        return;
    }
    let data = input.data_bytes();
    let mut stats = stats;
    let mut last = false;
    for row in 0..count {
        let slot = sel.slot(row);
        let valid = validity.is_valid(slot);
        if valid {
            last = data.get(slot).is_some_and(|&byte| byte != 0);
        }
        if let Some((stats, physical)) = &mut stats {
            if valid {
                update_numeric_stats(stats, *physical, i8::from(last));
                stats.set_has_no_null_fast();
            } else {
                stats.set_has_null_fast();
            }
        }
        bits.push(last);
    }
}

/// `RoaringAnalyzeState`: counts zeros, ones and runs of zeros per container to choose its
/// encoding, and estimates the segments.
#[derive(Debug)]
struct RoaringAnalyzer {
    ty: LogicalType,
    is_bool: bool,
    info: CompressionInfo,
    one_count: u16,
    zero_count: u16,
    run_count: u16,
    last_bit_set: bool,
    count: u16,
    data_size: usize,
    metadata_size: usize,
    segment_count: usize,
    current_count: usize,
    total_count: usize,
    total_size: usize,
    metadata_collection: MetadataCollection,
    container_metadata: Vec<ContainerMetadata>,
    bits: Vec<bool>,
}

impl RoaringAnalyzer {
    /// `HandleBit`.
    fn handle_bit(&mut self, bit_set: bool) {
        if !bit_set && (self.count == 0 || self.last_bit_set) {
            self.run_count += 1;
        }
        self.one_count += u16::from(bit_set);
        self.zero_count += u16::from(!bit_set);
        self.last_bit_set = bit_set;
        self.count += 1;
    }

    /// `FlushContainer`.
    fn flush_container(&mut self) {
        if self.count == 0 {
            return;
        }
        let metadata = ContainerMetadata::create(self.count, self.zero_count, self.one_count, self.run_count);
        let mut runs = self.metadata_collection.runs_in_segment;
        let mut arrays = self.metadata_collection.arrays_in_segment;
        if metadata.container_type == ContainerType::Run {
            runs += 1;
        } else {
            arrays += 1;
        }
        self.metadata_size = MetadataCollection::metadata_size(runs + arrays, runs, arrays);
        self.data_size += metadata.data_size(usize::from(self.count));
        if self.metadata_size + self.data_size > self.info.block_size {
            self.flush_segment();
        }
        self.container_metadata.push(metadata);
        self.metadata_collection.add(metadata);
        self.current_count += usize::from(self.count);
        self.one_count = 0;
        self.zero_count = 0;
        self.run_count = 0;
        self.last_bit_set = false;
        self.count = 0;
    }

    /// `FlushSegment`.
    fn flush_segment(&mut self) {
        if self.current_count == 0 {
            return;
        }
        self.metadata_collection.flush_segment();
        self.total_size += self.data_size + self.metadata_size;
        self.data_size = 0;
        self.metadata_size = 0;
        self.current_count = 0;
        self.segment_count += 1;
    }
}

impl Analyzer for RoaringAnalyzer {
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        let mut bits = std::mem::take(&mut self.bits);
        vector_bits(input, count, self.is_bool, None, &mut bits);
        for &bit_set in &bits {
            self.handle_bit(bit_set);
            if usize::from(self.count) == ROARING_CONTAINER_SIZE {
                self.flush_container();
            }
        }
        self.bits = bits;
        self.total_count += count;
        Ok(true)
    }

    /// `RoaringFinalAnalyze`.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        self.flush_container();
        self.flush_segment();
        Ok(Some((self.total_size as f64 * 2.0) as u64))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let physical = self.ty.physical_type();
        let mut compressor = RoaringCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            physical,
            is_bool: self.is_bool,
            block_size: self.info.block_size,
            total_count: self.total_count,
            containers: self.container_metadata,
            buffer: Vec::new(),
            data_offset: 0,
            segment_count: 0,
            metadata_collection: MetadataCollection::default(),
            container: None,
            bits: Vec::with_capacity(ROARING_CONTAINER_SIZE),
            written: 0,
            input_bits: Vec::new(),
            pending: None,
        };
        compressor.create_empty_segment();
        compressor.initialize_container()?;
        Ok(Box::new(compressor))
    }
}

/// `RoaringCompressState`: containers are encoded with the metadata chosen by the analysis;
/// container data grows from the 8-byte header, the metadata follows it when a segment is
/// flushed.
#[derive(Debug)]
struct RoaringCompressor {
    ty: LogicalType,
    physical: PhysicalType,
    is_bool: bool,
    block_size: usize,
    /// Rows in the column (`analyze_state.total_count`).
    total_count: usize,
    containers: Vec<ContainerMetadata>,
    buffer: Vec<u8>,
    /// Next free byte for container data (`data_ptr`).
    data_offset: usize,
    segment_count: u64,
    stats: BaseStatistics,
    metadata_collection: MetadataCollection,
    /// The metadata and data offset of the container being filled.
    container: Option<(ContainerMetadata, usize)>,
    /// The bits of the container being filled.
    bits: Vec<bool>,
    /// Rows of finished containers (`total_count` of the C++ state).
    written: usize,
    input_bits: Vec<bool>,
    /// A segment finished while reserving a container, to be handed to the sink.
    pending: Option<FinishedSegment>,
}

impl RoaringCompressor {
    /// `CreateEmptySegment`.
    fn create_empty_segment(&mut self) {
        self.buffer = vec![0; self.block_size];
        self.data_offset = 8;
        self.segment_count = 0;
        self.stats = BaseStatistics::create_empty(self.ty.clone());
    }

    /// `CanStore`.
    fn can_store(&self, container_size: usize, metadata: ContainerMetadata) -> bool {
        let mut runs = self.metadata_collection.runs_in_segment;
        let mut arrays = self.metadata_collection.arrays_in_segment;
        let mut new_data_space = 0;
        if metadata.container_type == ContainerType::Bitset {
            new_data_space += align8(self.data_offset) - self.data_offset;
        }
        new_data_space += metadata.data_size(container_size);
        if metadata.container_type == ContainerType::Run {
            runs += 1;
        } else {
            arrays += 1;
        }
        let new_metadata_space = MetadataCollection::metadata_size(runs + arrays, runs, arrays);
        let used = self.data_offset - 8;
        align8(used + new_data_space) + new_metadata_space <= self.block_size - 8
    }

    /// `InitializeContainer`: reserves the next container's space, flushing the segment if it
    /// does not fit.
    fn initialize_container(&mut self) -> Result<()> {
        if self.written == self.total_count {
            return Ok(());
        }
        let index = self.written / ROARING_CONTAINER_SIZE;
        let metadata = *self
            .containers
            .get(index)
            .ok_or_else(|| Error::internal("roaring container without metadata"))?;
        // DuckDB sizes every container like the first one.
        let container_size = self.total_count.min(ROARING_CONTAINER_SIZE).next_multiple_of(64);
        if !self.can_store(container_size, metadata) {
            self.pending = self.flush_segment_internal()?;
            self.create_empty_segment();
        }
        let count = usize::from(metadata.count);
        match metadata.container_type {
            ContainerType::Bitset => {
                self.data_offset = align8(self.data_offset);
                let end = self.data_offset + container_size / 8;
                self.buffer[self.data_offset..end].fill(0xFF);
            }
            ContainerType::Run if count < COMPRESSED_RUN_THRESHOLD => {
                self.data_offset = self.data_offset.next_multiple_of(4);
            }
            ContainerType::Array if count < COMPRESSED_ARRAY_THRESHOLD => {
                self.data_offset = self.data_offset.next_multiple_of(2);
            }
            _ => {
                self.buffer[self.data_offset..self.data_offset + COMPRESSED_SEGMENT_COUNT].fill(0);
            }
        }
        self.container = Some((metadata, self.data_offset));
        self.data_offset += metadata.data_size(container_size);
        self.metadata_collection.add(metadata);
        Ok(())
    }

    /// `FlushContainer`: encodes the container's bits at its reserved place.
    fn flush_container(&mut self) -> Result<()> {
        if self.bits.is_empty() {
            return Ok(());
        }
        let (metadata, start) = self
            .container
            .ok_or_else(|| Error::internal("roaring container was not initialized"))?;
        let appended = self.bits.len();
        let nulls = self.bits.iter().filter(|&&bit| !bit).count();
        match metadata.container_type {
            ContainerType::Bitset => {
                // `AppendBitset`: the reserved words are all ones; NULLs clear bits.
                for (row, _) in self.bits.iter().enumerate().filter(|(_, bit)| !**bit) {
                    self.buffer[start + row / 8] &= !(1 << (row % 8));
                }
            }
            ContainerType::Run => self.write_runs(start, usize::from(metadata.count)),
            ContainerType::Array => self.write_array(start, usize::from(metadata.count), metadata.nulls),
        }
        let has_nulls = nulls != 0;
        let has_non_nulls = nulls != appended;
        let bitset = metadata.container_type == ContainerType::Bitset;
        if has_nulls || bitset {
            self.stats.set_has_null_fast();
        }
        if has_non_nulls || bitset {
            self.stats.set_has_no_null_fast();
        }
        self.written += appended;
        self.segment_count += appended as u64;
        self.bits.clear();
        self.container = None;
        Ok(())
    }

    /// The runs of NULLs of the container, as (start, end) with an exclusive end.
    fn null_runs(&self) -> Vec<(usize, usize)> {
        let mut runs = Vec::new();
        let mut start = None;
        for (row, &bit) in self.bits.iter().enumerate() {
            match (bit, start) {
                (false, None) => start = Some(row),
                (true, Some(run_start)) => {
                    runs.push((run_start, row));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(run_start) = start {
            runs.push((run_start, self.bits.len()));
        }
        runs
    }

    /// `AppendRun` + `Finalize` of a run container.
    fn write_runs(&mut self, start: usize, run_count: usize) {
        let appended = self.bits.len();
        let runs = self.null_runs();
        if run_count >= COMPRESSED_RUN_THRESHOLD {
            let (counts, data) = self.buffer[start..].split_at_mut(COMPRESSED_SEGMENT_COUNT);
            for (index, &(run_start, run_end)) in runs.iter().enumerate().take(MAX_RUN_IDX) {
                data[index * 2] = (run_start % COMPRESSED_SEGMENT_SIZE) as u8;
                counts[run_start / COMPRESSED_SEGMENT_SIZE] += 1;
                data[index * 2 + 1] = (run_end % COMPRESSED_SEGMENT_SIZE) as u8;
                // A run ended by the end of a full container is not counted.
                if run_end != ROARING_CONTAINER_SIZE {
                    counts[run_end / COMPRESSED_SEGMENT_SIZE] += 1;
                }
            }
        } else {
            for (index, &(run_start, run_end)) in runs.iter().enumerate() {
                // A run ended by the end of the container gets one more row (DuckDB's
                // `Finalize` does not subtract one).
                let length = if run_end == appended {
                    run_end - run_start
                } else {
                    run_end - run_start - 1
                };
                let offset = start + index * 4;
                self.buffer[offset..offset + 2].copy_from_slice(&(run_start as u16).to_le_bytes());
                self.buffer[offset + 2..offset + 4].copy_from_slice(&(length as u16).to_le_bytes());
            }
        }
    }

    /// `AppendToArray`: the positions of the NULLs (`nulls`) or of the valid rows.
    fn write_array(&mut self, start: usize, cardinality: usize, nulls: bool) {
        let positions: Vec<usize> = self
            .bits
            .iter()
            .enumerate()
            .filter(|(_, bit)| **bit != nulls)
            .map(|(row, _)| row)
            .collect();
        if cardinality >= COMPRESSED_ARRAY_THRESHOLD {
            let (counts, data) = self.buffer[start..].split_at_mut(COMPRESSED_SEGMENT_COUNT);
            for (index, &row) in positions.iter().enumerate().take(MAX_ARRAY_IDX) {
                data[index] = (row % COMPRESSED_SEGMENT_SIZE) as u8;
                counts[row / COMPRESSED_SEGMENT_SIZE] += 1;
            }
        } else {
            for (index, &row) in positions.iter().enumerate() {
                let offset = start + index * 2;
                self.buffer[offset..offset + 2].copy_from_slice(&(row as u16).to_le_bytes());
            }
        }
    }

    /// `FlushSegment` to `sink`.
    fn flush_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        match self.flush_segment_internal()? {
            Some(segment) => sink.flush(segment),
            None => Ok(()),
        }
    }

    /// `FlushSegment`: appends the segment's container metadata after its (8-byte aligned)
    /// data.
    fn flush_segment_internal(&mut self) -> Result<Option<FinishedSegment>> {
        if self.segment_count == 0 {
            return Ok(None);
        }
        let data_size = align8(self.data_offset - 8);
        self.buffer[self.data_offset..8 + data_size].fill(0);
        let metadata_size = self.metadata_collection.segment_metadata_size();
        let written = self
            .metadata_collection
            .serialize(&mut self.buffer[8 + data_size..]);
        if written != metadata_size {
            return Err(Error::internal(
                "mismatch in metadata size during RoaringCompressState::FlushSegment",
            ));
        }
        self.metadata_collection.flush_segment();
        self.buffer[..8].copy_from_slice(&(data_size as u64).to_le_bytes());
        let total = 8 + data_size + metadata_size;
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        Ok(Some(FinishedSegment {
            data: self.buffer[..total].to_vec(),
            count: self.segment_count,
            stats,
            compression: CompressionType::Roaring,
            overflow_blocks: Vec::new(),
        }))
    }
}

impl Compressor for RoaringCompressor {
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        let mut bits = std::mem::take(&mut self.input_bits);
        let physical = self.physical;
        let stats = self.is_bool.then_some((&mut self.stats, physical));
        vector_bits(input, count, self.is_bool, stats, &mut bits);
        for &bit_set in &bits {
            self.bits.push(bit_set);
            if self.bits.len() == ROARING_CONTAINER_SIZE {
                // `NextContainer`.
                self.flush_container()?;
                self.initialize_container_with(sink)?;
            }
        }
        self.input_bits = bits;
        Ok(())
    }

    /// `RoaringCompressState::Finalize`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        self.flush_container()?;
        self.flush_segment(sink)
    }
}

impl RoaringCompressor {
    /// `InitializeContainer` with a sink to flush full segments to.
    fn initialize_container_with(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        self.initialize_container()?;
        if let Some(segment) = self.pending.take() {
            sink.flush(segment)?;
        }
        Ok(())
    }
}

/// `RoaringCompressionFun::GetFunction(type).init_analyze`: Roaring needs storage version 4
/// (version 7 for booleans).
pub(crate) fn init_analyze(
    ty: &LogicalType,
    physical: PhysicalType,
    info: &CompressionInfo,
) -> Option<Box<dyn Analyzer>> {
    let is_bool = match physical {
        PhysicalType::Bit => false,
        PhysicalType::Bool => true,
        _ => return None,
    };
    if info.storage_version < 4 || (is_bool && info.storage_version < 7) {
        return None;
    }
    Some(Box::new(RoaringAnalyzer {
        ty: ty.clone(),
        is_bool,
        info: *info,
        one_count: 0,
        zero_count: 0,
        run_count: 0,
        last_bit_set: false,
        count: 0,
        data_size: 0,
        metadata_size: 0,
        segment_count: 0,
        current_count: 0,
        total_count: 0,
        total_size: 0,
        metadata_collection: MetadataCollection::default(),
        container_metadata: Vec::new(),
        bits: Vec::new(),
    }))
}
