//! Chimp128 floating point compression (DuckDB's `compression/chimp/*`). DuckDB 1.5 no
//! longer writes Chimp segments, but reads them from old database files.
//!
//! Values are XOR-ed with a reference among the previous 128 values and the significant
//! bits are written to an MSB-first bit stream that runs through the whole segment. Per
//! group of [`CHIMP_SEQUENCE_SIZE`] values the metadata (growing downwards from the end of
//! the segment) holds the group's data offset, its leading-zero blocks (8 x 3 bits per 3
//! bytes), its 2-bit flags and its packed (index, leading zeros, significant bits) data.

use std::marker::PhantomData;

use roastedduck_common::Result;
use roastedduck_types::{PhysicalType, Vector};

use super::primitive::{Primitive, corrupt, load, write_values};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// Values per group (`ChimpPrimitives::CHIMP_SEQUENCE_SIZE`).
pub(crate) const CHIMP_SEQUENCE_SIZE: usize = 1024;
/// `ChimpConstants::BUFFER_SIZE`: the ring buffer of previous values.
const BUFFER_SIZE: usize = 128;
/// `ChimpConstants::Decompression::LEADING_REPRESENTATION`.
const LEADING_REPRESENTATION: [u8; 8] = [0, 8, 12, 16, 18, 20, 22, 24];

/// The flag of a value (`ChimpConstants::Flags`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flag {
    ValueIdentical,
    TrailingExceedsThreshold,
    LeadingZeroEquality,
    LeadingZeroLoad,
}

/// An MSB-first bit reader (DuckDB's `BitReader`).
#[derive(Debug, Default)]
pub(crate) struct BitReader {
    /// Offset of the stream in the segment.
    start: usize,
    /// Bits read so far.
    index: usize,
}

impl BitReader {
    /// `ReadValue(size)`: the next `size <= 64` bits as an integer.
    pub(crate) fn read(&mut self, bytes: &[u8], size: u32) -> Result<u64> {
        let mut value = 0u64;
        for _ in 0..size {
            let position = self.start + self.index / 8;
            let byte = *bytes
                .get(position)
                .ok_or_else(|| corrupt("Chimp bit stream out of range - corrupt database file"))?;
            let bit = (byte >> (7 - (self.index % 8))) & 1;
            value = (value << 1) | u64::from(bit);
            self.index += 1;
        }
        Ok(value)
    }
}

/// A float type stored by Chimp/Patas as its bits (`ChimpType` / `FloatingToExact`).
pub(crate) trait ChimpFloat: Primitive {
    const BITS: u32;
    fn from_bits_u64(bits: u64) -> Self;
}

impl ChimpFloat for f32 {
    const BITS: u32 = 32;
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits as u32)
    }
}

impl ChimpFloat for f64 {
    const BITS: u32 = 64;
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits)
    }
}

/// The scan state of a Chimp segment (DuckDB's `ChimpScanState` / `ChimpGroupState`).
#[derive(Debug)]
pub(crate) struct ChimpScanState<T> {
    data: PersistentData,
    count: usize,
    metadata: usize,
    total_value_count: usize,
    input: BitReader,
    values: Vec<T>,
    index: usize,
    _marker: PhantomData<T>,
}

impl<T: ChimpFloat> ChimpScanState<T> {
    fn left_in_group(&self) -> usize {
        CHIMP_SEQUENCE_SIZE - self.total_value_count % CHIMP_SEQUENCE_SIZE
    }

    /// `LoadGroup`: reads the group's metadata and decodes its values.
    fn load_group(&mut self) -> Result<()> {
        let bytes = self.data.bytes();
        let underflow = || corrupt("Chimp metadata out of range - corrupt database file");
        self.metadata = self.metadata.checked_sub(4).ok_or_else(underflow)?;
        self.metadata = self.metadata.checked_sub(1).ok_or_else(underflow)?;
        let leading_zero_block_count = usize::from(load::<u8>(bytes, self.metadata)?);
        self.metadata = self
            .metadata
            .checked_sub(3 * leading_zero_block_count)
            .ok_or_else(underflow)?;
        let leading_zero_blocks = self.metadata;
        let group_size = (self.count - self.total_value_count).min(CHIMP_SEQUENCE_SIZE);
        let flag_count = group_size - 1;
        let flag_bytes = flag_count.div_ceil(4);
        self.metadata = self.metadata.checked_sub(flag_bytes).ok_or_else(underflow)?;
        // `LoadFlags`: 2 bits per flag, most significant first; the first value has none.
        let mut flags = Vec::with_capacity(group_size);
        flags.push(Flag::ValueIdentical);
        for index in 0..flag_count {
            let byte = load::<u8>(bytes, self.metadata + index / 4)?;
            let value = (byte >> (6 - 2 * (index % 4))) & 3;
            flags.push(match value {
                0 => Flag::ValueIdentical,
                1 => Flag::TrailingExceedsThreshold,
                2 => Flag::LeadingZeroEquality,
                _ => Flag::LeadingZeroLoad,
            });
        }
        // `LoadLeadingZeros`: 8 values of 3 bits per 3 bytes, least significant first.
        let mut leading_zeros = Vec::with_capacity(leading_zero_block_count * 8);
        for index in 0..leading_zero_block_count * 8 {
            let block = leading_zero_blocks + (index / 8) * 3;
            let mut raw = [0u8; 4];
            for (offset, byte) in raw.iter_mut().enumerate().take(3) {
                *byte = load::<u8>(bytes, block + offset)?;
            }
            let value = (u32::from_le_bytes(raw) >> (3 * (index % 8))) & 7;
            leading_zeros.push(LEADING_REPRESENTATION[value as usize]);
        }
        // `LoadPackedData` (aligned on a two-byte boundary).
        let packed_count = flags[1..]
            .iter()
            .filter(|&&flag| flag == Flag::TrailingExceedsThreshold)
            .count();
        self.metadata = self
            .metadata
            .checked_sub(packed_count * 2)
            .ok_or_else(underflow)?;
        if self.metadata & 1 == 1 {
            self.metadata -= 1;
        }
        let mut packed = Vec::with_capacity(packed_count);
        let significant_mask = if T::BITS == 64 { 63 } else { 31 };
        for index in 0..packed_count {
            let value: u16 = load(bytes, self.metadata + index * 2)?;
            let reference = usize::from((value >> 9) & 127);
            let leading = LEADING_REPRESENTATION[usize::from((value >> 6) & 7)];
            let mut significant = u32::from(value & significant_mask);
            if significant == 0 {
                significant = 64;
            }
            packed.push((reference, leading, significant));
        }
        // `LoadValues` (`Chimp128Decompression`).
        let bits = T::BITS;
        let mut ring = [0u64; BUFFER_SIZE];
        let mut ring_index = 0usize;
        let mut reference_value = 0u64;
        let mut leading = u8::MAX;
        let (mut leading_index, mut packed_index) = (0, 0);
        self.values.clear();
        for (row, &flag) in flags.iter().enumerate() {
            let result = if row == 0 {
                let value = self.input.read(bytes, bits)?;
                ring[0] = value;
                value
            } else {
                let value = match flag {
                    Flag::ValueIdentical => {
                        let index = self.input.read(bytes, 7)? as usize;
                        ring[index]
                    }
                    Flag::TrailingExceedsThreshold => {
                        let &(reference, leading_zero, significant) =
                            packed.get(packed_index).ok_or_else(|| {
                                corrupt("Chimp packed data out of range - corrupt database file")
                            })?;
                        packed_index += 1;
                        leading = leading_zero;
                        let trailing = bits.wrapping_sub(significant).wrapping_sub(u32::from(leading));
                        let value = self.input.read(bytes, significant.min(64))?;
                        value.checked_shl(trailing).unwrap_or(0) ^ ring[reference]
                    }
                    Flag::LeadingZeroEquality => {
                        self.input.read(bytes, bits.saturating_sub(u32::from(leading)))? ^ reference_value
                    }
                    Flag::LeadingZeroLoad => {
                        leading = *leading_zeros.get(leading_index).ok_or_else(|| {
                            corrupt("Chimp leading zeros out of range - corrupt database file")
                        })?;
                        leading_index += 1;
                        self.input.read(bytes, bits.saturating_sub(u32::from(leading)))? ^ reference_value
                    }
                };
                let value = if bits == 32 { value & 0xFFFF_FFFF } else { value };
                ring_index += 1;
                ring[ring_index % BUFFER_SIZE] = value;
                value
            };
            reference_value = result;
            self.values.push(T::from_bits_u64(result));
        }
        self.index = 0;
        Ok(())
    }

    /// `ScanGroup`.
    fn scan_group(&mut self, count: usize, out: Option<(&mut Vector, usize)>) -> Result<()> {
        if self.total_value_count.is_multiple_of(CHIMP_SEQUENCE_SIZE) && self.total_value_count < self.count {
            self.load_group()?;
        }
        if let Some((result, offset)) = out {
            let values = self
                .values
                .get(self.index..self.index + count)
                .ok_or_else(|| corrupt("Chimp group shorter than its count - corrupt database file"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: ChimpFloat> CodecScan for ChimpScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let metadata = load::<u32>(data.bytes(), 0)? as usize;
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            input: BitReader { start: 4, index: 0 },
            values: Vec::with_capacity(CHIMP_SEQUENCE_SIZE),
            index: 0,
            _marker: PhantomData,
        })
    }

    /// `ChimpScanState::Skip`: the bit stream is continuous, so every group is decoded.
    fn skip(&mut self, _segment: &ColumnSegment, mut skip_count: usize) -> Result<()> {
        while skip_count > 0 {
            let to_skip = skip_count.min(self.left_in_group());
            self.scan_group(to_skip, None)?;
            skip_count -= to_skip;
        }
        Ok(())
    }

    /// `ChimpScanPartial`.
    fn scan_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let to_scan = (count - scanned).min(self.left_in_group());
            self.scan_group(to_scan, Some((&mut *result, result_offset + scanned)))?;
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `ChimpCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<ChimpScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<ChimpScanState<f64>>::boxed(segment),
        _ => return None,
    })
}
