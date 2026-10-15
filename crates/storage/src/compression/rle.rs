//! Run-length encoding (DuckDB's `rle.cpp`).
//!
//! Segment layout: an 8-byte header with the offset of the run lengths, the run values
//! (`T`) and, at that (8-byte aligned) offset, one `u16` length per run.

use rustdb_common::Result;
use rustdb_types::{PhysicalType, Vector};

use super::primitive::{Primitive, corrupt, fill_values, load};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// The run length type (`rle_count_t`).
pub(crate) type RleCount = u16;

/// The size of the segment header holding the offset of the run lengths
/// (`RLEConstants::RLE_HEADER_SIZE`).
pub(crate) const RLE_HEADER_SIZE: usize = 8;

/// The scan state of an RLE segment (DuckDB's `RLEScanState`).
#[derive(Debug)]
pub(crate) struct RleScanState<T> {
    data: PersistentData,
    /// The run being read.
    entry_pos: usize,
    /// How many rows of the run were read.
    position_in_entry: usize,
    /// The offset of the run lengths (`rle_count_offset`).
    rle_count_offset: usize,
    /// The number of run lengths that fit between the offset and the end of the block.
    max_entry_pos: usize,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Primitive> RleScanState<T> {
    /// The length of run `entry`.
    #[inline]
    fn run_length(&self, entry: usize) -> Result<usize> {
        let length: RleCount = load(
            self.data.bytes(),
            self.rle_count_offset + entry * size_of::<RleCount>(),
        )?;
        Ok(usize::from(length))
    }

    /// The value of run `entry`.
    #[inline]
    fn run_value(&self, entry: usize) -> Result<T> {
        load(self.data.bytes(), RLE_HEADER_SIZE + entry * T::SIZE)
    }

    /// `ForwardToNextRun`.
    fn forward_to_next_run(&mut self) -> Result<()> {
        self.entry_pos += 1;
        if self.entry_pos > self.max_entry_pos {
            return Err(corrupt(
                "Corrupted RLE segment: index_pointer[entry_pos] would reach outside of the blocks memory",
            ));
        }
        self.position_in_entry = 0;
        Ok(())
    }

    /// `SkipInternal`.
    fn skip_internal(&mut self, mut skip_count: usize) -> Result<()> {
        while skip_count > 0 {
            let run_end = self.run_length(self.entry_pos)?;
            let skip_amount = skip_count.min(run_end.saturating_sub(self.position_in_entry));
            skip_count -= skip_amount;
            self.position_in_entry += skip_amount;
            if self.position_in_entry >= run_end {
                self.forward_to_next_run()?;
            }
        }
        Ok(())
    }
}

impl<T: Primitive> CodecScan for RleScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let offset: u64 = load(data.bytes(), 0)?;
        let rle_count_offset = u32::try_from(offset).map_err(|_| corrupt("Corrupted RLE segment"))? as usize;
        let block_size = segment.block_size();
        let block_offset = segment.block_offset();
        if rle_count_offset < RLE_HEADER_SIZE {
            return Err(corrupt(
                "Corrupted RLE segment: rle_count_offset is smaller than the header size. The index_pointer would \
                 point into the region reserved for the header data",
            ));
        }
        if block_offset + rle_count_offset > block_size {
            return Err(corrupt(
                "Corrupted RLE segment: rle_count_offset exceeds the block size. The index_pointer would start \
                 outside of the segment",
            ));
        }
        let max_entry_pos = (block_size - block_offset - rle_count_offset) / size_of::<RleCount>();
        if rle_count_offset > (RLE_HEADER_SIZE + max_entry_pos * T::SIZE).next_multiple_of(8) {
            return Err(corrupt(
                "Corrupted RLE segment: rle_count_offset has more values than fit in the segment. \
                 index_pointer[entry_pos] would reach outside of the segment",
            ));
        }
        Ok(Self {
            data,
            entry_pos: 0,
            position_in_entry: 0,
            rle_count_offset,
            max_entry_pos,
            _marker: std::marker::PhantomData,
        })
    }

    fn skip(&mut self, _segment: &ColumnSegment, count: usize) -> Result<()> {
        self.skip_internal(count)
    }

    /// `RLEScanPartialInternal` (always producing a flat vector).
    fn scan_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        mut result_offset: usize,
    ) -> Result<()> {
        let result_end = result_offset + count;
        while result_offset < result_end {
            let run_end = self.run_length(self.entry_pos)?;
            let run_count = run_end.saturating_sub(self.position_in_entry);
            let remaining = result_end - result_offset;
            let element = self.run_value(self.entry_pos)?;
            if run_count > remaining {
                fill_values(result, result_offset, remaining, element)?;
                self.position_in_entry += remaining;
                break;
            }
            fill_values(result, result_offset, run_count, element)?;
            result_offset += run_count;
            self.forward_to_next_run()?;
        }
        Ok(())
    }
}

/// `RLEFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Bool | PhysicalType::Int8 => Sequential::<RleScanState<i8>>::boxed(segment),
        PhysicalType::Int16 => Sequential::<RleScanState<i16>>::boxed(segment),
        PhysicalType::Int32 => Sequential::<RleScanState<i32>>::boxed(segment),
        PhysicalType::Int64 => Sequential::<RleScanState<i64>>::boxed(segment),
        PhysicalType::Int128 => Sequential::<RleScanState<i128>>::boxed(segment),
        PhysicalType::UInt8 => Sequential::<RleScanState<u8>>::boxed(segment),
        PhysicalType::UInt16 => Sequential::<RleScanState<u16>>::boxed(segment),
        PhysicalType::UInt32 => Sequential::<RleScanState<u32>>::boxed(segment),
        PhysicalType::UInt64 | PhysicalType::List => Sequential::<RleScanState<u64>>::boxed(segment),
        PhysicalType::UInt128 => Sequential::<RleScanState<u128>>::boxed(segment),
        PhysicalType::Float => Sequential::<RleScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<RleScanState<f64>>::boxed(segment),
        _ => return None,
    })
}
