//! Roaring bitmap compression of validity masks and booleans (DuckDB's
//! `compression/roaring/*`).
//!
//! The rows of a segment are split into containers of [`ROARING_CONTAINER_SIZE`] bits,
//! each stored as a run container (runs of NULLs), an array container (the positions of
//! the NULLs or of the valid rows) or a bitset. Runs and arrays with many entries are
//! "compressed": they store one count per 256-row segment of the container and one byte
//! per position. Segment layout: an 8-byte offset of the container metadata, the container
//! data, and the metadata (2-bit container types, 7-bit run counts, 8-bit array sizes).

use rustdb_common::Result;
use rustdb_types::{PhysicalType, Vector};

use super::SegmentScan;
use super::bitpacking_primitives::{required_size, round_up_to_group, unpack_buffer_u64};
use super::primitive::{bit, corrupt, load, set_invalid_where, write_values};
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
#[allow(dead_code, reason = "used by the checkpoint writer")]
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
