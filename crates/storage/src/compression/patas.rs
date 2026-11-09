//! Patas floating point compression (DuckDB's `compression/patas/*`). DuckDB 1.5 no longer
//! writes Patas segments, but reads them from old database files.
//!
//! Values are XOR-ed with a reference among the previous 128 values; the significant bytes
//! of the XOR are stored byte aligned. Per group of [`PATAS_GROUP_SIZE`] values the
//! metadata (growing downwards from the end of the segment) holds the group's data offset
//! and one packed (index difference, significant bytes, trailing zeros) `u16` per value.

use std::marker::PhantomData;

use roastedduck_common::Result;
use roastedduck_types::{PhysicalType, Vector};

use super::chimp::ChimpFloat;
use super::primitive::{corrupt, load, slice, write_values};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// Values per group (`PatasPrimitives::PATAS_GROUP_SIZE`).
pub(crate) const PATAS_GROUP_SIZE: usize = 1024;

/// The scan state of a Patas segment (DuckDB's `PatasScanState` / `PatasGroupState`).
#[derive(Debug)]
pub(crate) struct PatasScanState<T> {
    data: PersistentData,
    count: usize,
    metadata: usize,
    total_value_count: usize,
    values: Vec<T>,
    index: usize,
    _marker: PhantomData<T>,
}

impl<T: ChimpFloat> PatasScanState<T> {
    fn left_in_group(&self) -> usize {
        PATAS_GROUP_SIZE - self.total_value_count % PATAS_GROUP_SIZE
    }

    fn group_finished(&self) -> bool {
        self.total_value_count.is_multiple_of(PATAS_GROUP_SIZE)
    }

    /// `SkipGroup`.
    fn skip_group(&mut self) {
        let group_size = PATAS_GROUP_SIZE.min(self.count - self.total_value_count);
        self.metadata = self.metadata.wrapping_sub(4 + 2 * group_size);
        self.total_value_count += group_size;
    }

    /// `LoadGroup` + `PatasGroupState::LoadValues`.
    fn load_group(&mut self, block_offset: usize, block_size: usize) -> Result<()> {
        let bytes = self.data.bytes();
        let underflow = || corrupt("Patas metadata out of range - corrupt database file");
        self.metadata = self.metadata.checked_sub(4).ok_or_else(underflow)?;
        let data_offset = load::<u32>(bytes, self.metadata)? as usize;
        if block_offset + data_offset >= block_size {
            return Err(corrupt(
                "Corrupted Patas segment: data_byte_offset would reach outside of the blocks memory",
            ));
        }
        let group_size = PATAS_GROUP_SIZE.min(self.count - self.total_value_count);
        self.metadata = self.metadata.checked_sub(2 * group_size).ok_or_else(underflow)?;
        let type_size = (T::BITS / 8) as usize;
        let mut position = data_offset;
        let mut values: Vec<u64> = Vec::with_capacity(group_size);
        for row in 0..group_size {
            let packed: u16 = load(bytes, self.metadata + row * 2)?;
            // `PackedDataUtils::Unpack` reinterpreted as `PatasUnpackedValueStats`.
            let index_diff = usize::from((packed >> 9) & 127);
            let significant_bytes = usize::from((packed >> 6) & 7);
            let trailing_zeros = u32::from(packed & if T::BITS == 64 { 63 } else { 31 });
            if index_diff > row {
                return Err(corrupt("Corrupted Patas segment: invalid backward reference"));
            }
            if significant_bytes > type_size || trailing_zeros >= T::BITS {
                return Err(corrupt("Corrupted Patas segment: invalid packed value metadata"));
            }
            // `ByteReader::ReadValue(bytes, trailing_zero)`: 0 bytes means a full value
            // unless the value is equal to its reference.
            let read_bytes = match significant_bytes {
                0 if trailing_zeros < 8 => type_size,
                0 => 0,
                other => other,
            };
            let mut raw = [0u8; 8];
            raw[..read_bytes].copy_from_slice(slice(bytes, position, read_bytes)?);
            position += read_bytes;
            let value = u64::from_le_bytes(raw);
            let previous = if row == 0 { 0 } else { values[row - index_diff] };
            let mut decoded = value.checked_shl(trailing_zeros).unwrap_or(0) ^ previous;
            if T::BITS == 32 {
                decoded &= 0xFFFF_FFFF;
            }
            values.push(decoded);
        }
        self.values.clear();
        self.values.extend(values.into_iter().map(T::from_bits_u64));
        self.index = 0;
        Ok(())
    }

    /// `ScanGroup`.
    fn scan_group(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        out: Option<(&mut Vector, usize)>,
    ) -> Result<()> {
        if self.group_finished() && self.total_value_count < self.count {
            self.load_group(segment.block_offset(), segment.block_size())?;
        }
        if let Some((result, offset)) = out {
            let values = self
                .values
                .get(self.index..self.index + count)
                .ok_or_else(|| corrupt("Patas group shorter than its count - corrupt database file"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: ChimpFloat> CodecScan for PatasScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let metadata = load::<u32>(data.bytes(), 0)? as usize;
        if segment.block_offset() + metadata > segment.block_size() {
            return Err(corrupt(
                "Corrupted Patas segment: metadata_offset reaches outside of the blocks memory",
            ));
        }
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            values: Vec::with_capacity(PATAS_GROUP_SIZE),
            index: 0,
            _marker: PhantomData,
        })
    }

    /// `PatasScanState::Skip`.
    fn skip(&mut self, segment: &ColumnSegment, mut skip_count: usize) -> Result<()> {
        if self.total_value_count != 0 && !self.group_finished() {
            let to_skip = skip_count.min(self.left_in_group());
            self.scan_group(segment, to_skip, None)?;
            skip_count -= to_skip;
        }
        for _ in 0..skip_count / PATAS_GROUP_SIZE {
            self.skip_group();
        }
        skip_count %= PATAS_GROUP_SIZE;
        if skip_count > 0 {
            self.scan_group(segment, skip_count, None)?;
        }
        Ok(())
    }

    /// `PatasScanPartial`.
    fn scan_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let to_scan = (count - scanned).min(self.left_in_group());
            self.scan_group(segment, to_scan, Some((&mut *result, result_offset + scanned)))?;
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `PatasCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<PatasScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<PatasScanState<f64>>::boxed(segment),
        _ => return None,
    })
}
