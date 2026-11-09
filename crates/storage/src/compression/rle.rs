//! Run-length encoding (DuckDB's `rle.cpp`).
//!
//! Segment layout: an 8-byte header with the offset of the run lengths, the run values
//! (`T`) and, at that (8-byte aligned) offset, one `u16` length per run.

use roastedduck_common::Result;
use roastedduck_types::{LogicalType, PhysicalType, Vector};

use super::primitive::{
    Primitive, align8, align8_floor, corrupt, load, result_bytes, unified_values, update_numeric_stats,
    write_values,
};
use super::{Analyzer, CodecScan, CompressionInfo, CompressionType, Compressor, SegmentScan, Sequential};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;
use crate::table_filter::TableFilter;

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
    /// For the filter last evaluated (by address): which runs pass it, and how many
    /// (DuckDB's `matching_runs` / `matching_run_count`).
    matching_runs: Option<MatchingRuns>,
    _marker: std::marker::PhantomData<T>,
}

/// Which runs of a segment pass a filter.
#[derive(Debug)]
struct MatchingRuns {
    /// The filter's address.
    filter: usize,
    runs: Vec<bool>,
    count: usize,
}

impl<T: Primitive> RleScanState<T> {
    /// Evaluates `filter` on every run value once per segment.
    fn matching_runs(&mut self, segment: &ColumnSegment, filter: &TableFilter) -> Result<&MatchingRuns> {
        let key = std::ptr::from_ref(filter) as usize;
        if self.matching_runs.as_ref().is_none_or(|m| m.filter != key) {
            // Like DuckDB, the values up to the (aligned) run length offset: padding values
            // can only make a run look like a match, never hide one.
            let total = (self.rle_count_offset - RLE_HEADER_SIZE) / T::SIZE;
            let mut values = Vector::flat(segment.logical_type().clone(), total.max(1));
            let stored = self
                .data
                .bytes()
                .get(RLE_HEADER_SIZE..RLE_HEADER_SIZE + total * T::SIZE)
                .ok_or_else(|| corrupt("Corrupted RLE segment"))?;
            result_bytes::<T>(&mut values, 0, total)?.copy_from_slice(stored);
            let mut sel: Vec<u32> = (0..total as u32).collect();
            filter.select(&values, total, &mut sel)?;
            let mut runs = vec![false; total];
            for &run in &sel {
                runs[run as usize] = true;
            }
            self.matching_runs = Some(MatchingRuns {
                filter: key,
                runs,
                count: sel.len(),
            });
        }
        self.matching_runs
            .as_ref()
            .ok_or_else(|| corrupt("RLE filter state missing"))
    }

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

    /// `SkipInternal`: walks the run lengths, which are read from the pinned block
    /// directly rather than one checked load per run.
    fn skip_internal(&mut self, mut skip_count: usize) -> Result<()> {
        if skip_count == 0 {
            return Ok(());
        }
        let lengths = self
            .data
            .bytes()
            .get(self.rle_count_offset..)
            .ok_or_else(|| corrupt("Corrupted RLE segment"))?;
        let (lengths, _) = lengths.as_chunks::<{ size_of::<RleCount>() }>();
        let mut entry = self.entry_pos;
        let mut position = self.position_in_entry;
        loop {
            let run_end = usize::from(RleCount::from_le_bytes(
                *lengths
                    .get(entry)
                    .ok_or_else(|| corrupt("Corrupted RLE segment"))?,
            ));
            let remaining = run_end.saturating_sub(position);
            if skip_count < remaining {
                position += skip_count;
                break;
            }
            // The run is exhausted: `ForwardToNextRun`.
            skip_count -= remaining;
            entry += 1;
            position = 0;
            if entry > self.max_entry_pos {
                return Err(corrupt(
                    "Corrupted RLE segment: index_pointer[entry_pos] would reach outside of the blocks memory",
                ));
            }
            if skip_count == 0 {
                break;
            }
        }
        self.entry_pos = entry;
        self.position_in_entry = position;
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
            matching_runs: None,
            _marker: std::marker::PhantomData,
        })
    }

    fn never_matches(&mut self, segment: &ColumnSegment, filter: &TableFilter) -> Result<bool> {
        Ok(self.matching_runs(segment, filter)?.count == 0)
    }

    const FILTERS: bool = true;

    /// `RLEFilter`'s fast path: only the rows of matching runs are written and selected.
    fn filter_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
    ) -> Result<()> {
        self.matching_runs(segment, filter)?;
        let matching = std::mem::take(&mut self.matching_runs);
        let runs = matching.as_ref().map_or(&[][..], |m| m.runs.as_slice());
        sel.clear();
        let target = result_bytes::<T>(result, 0, count)?;
        let mut row = 0;
        while row < count {
            let run_end = self.run_length(self.entry_pos)?;
            let run_count = run_end.saturating_sub(self.position_in_entry);
            let take = run_count.min(count - row);
            if runs.get(self.entry_pos).copied().unwrap_or(true) {
                let value = self.run_value(self.entry_pos)?;
                for slot in target[row * T::SIZE..(row + take) * T::SIZE].chunks_exact_mut(T::SIZE) {
                    value.store(slot);
                }
                sel.extend(row as u32..(row + take) as u32);
            }
            row += take;
            if take == run_count {
                self.forward_to_next_run()?;
            } else {
                self.position_in_entry += take;
            }
        }
        self.matching_runs = matching;
        Ok(())
    }

    fn skip(&mut self, _segment: &ColumnSegment, count: usize) -> Result<()> {
        self.skip_internal(count)
    }

    /// `CanEmitConstantVector` + `RLEScanConstant`: a whole vector inside one run is a
    /// constant vector.
    fn scan_constant(&mut self, segment: &ColumnSegment, count: usize) -> Result<Option<Vector>> {
        if count != roastedduck_common::STANDARD_VECTOR_SIZE {
            return Ok(None);
        }
        let run_length = self.run_length(self.entry_pos)?;
        if run_length.saturating_sub(self.position_in_entry) < count {
            return Ok(None);
        }
        let mut result = Vector::flat(segment.logical_type().clone(), 1);
        write_values(&mut result, 0, &[self.run_value(self.entry_pos)?])?;
        result.make_constant();
        self.position_in_entry += count;
        if self.position_in_entry >= run_length {
            self.forward_to_next_run()?;
        }
        Ok(Some(result))
    }

    /// `RLEScanPartialInternal` (always producing a flat vector).
    fn scan_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        // The target is taken once: runs are often short.
        let mut target = result_bytes::<T>(result, result_offset, count)?;
        while !target.is_empty() {
            let run_end = self.run_length(self.entry_pos)?;
            let run_count = run_end.saturating_sub(self.position_in_entry);
            let remaining = target.len() / T::SIZE;
            let element = self.run_value(self.entry_pos)?;
            let take = run_count.min(remaining);
            let (run, rest) = target.split_at_mut(take * T::SIZE);
            for slot in run.chunks_exact_mut(T::SIZE) {
                element.store(slot);
            }
            target = rest;
            if run_count > remaining {
                self.position_in_entry += remaining;
                break;
            }
            self.forward_to_next_run()?;
        }
        Ok(())
    }

    const SELECTS: bool = true;

    /// `RLESelect`: writes the run value of the rows of `sel` only.
    fn select_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        sel: &[u32],
    ) -> Result<()> {
        let target = result_bytes::<T>(result, 0, count)?;
        let mut row = 0;
        let mut selected = sel.iter().map(|&row| row as usize).peekable();
        while row < count {
            let run_end = self.run_length(self.entry_pos)?;
            let run_count = run_end.saturating_sub(self.position_in_entry);
            let remaining = count - row;
            let end = row + run_count.min(remaining);
            if selected.peek().is_some_and(|&next| next < end) {
                let element = self.run_value(self.entry_pos)?;
                while let Some(next) = selected.next_if(|&next| next < end) {
                    element.store(&mut target[next * T::SIZE..(next + 1) * T::SIZE]);
                }
            }
            row = end;
            if run_count > remaining {
                self.position_in_entry += remaining;
                break;
            }
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

/// The run being built (`RLEState`).
#[derive(Debug)]
struct RleState<T> {
    seen_count: u64,
    last_value: T,
    last_seen_count: RleCount,
    /// No valid value seen yet (never reset).
    all_null: bool,
}

impl<T: Primitive> RleState<T> {
    fn new() -> Self {
        Self {
            seen_count: 0,
            last_value: T::NULL_VALUE,
            last_seen_count: 0,
            all_null: true,
        }
    }

    /// `RLEState::Update`: `flush(value, count, is_null)` is called for every finished run.
    fn update(
        &mut self,
        value: T,
        valid: bool,
        mut flush: impl FnMut(T, RleCount, bool) -> Result<()>,
    ) -> Result<()> {
        if valid {
            if self.all_null {
                // The first valid value; earlier NULLs belong to its run.
                self.last_value = value;
                self.seen_count += 1;
                self.last_seen_count += 1;
                self.all_null = false;
            } else if self.last_value == value {
                self.last_seen_count += 1;
            } else {
                if self.last_seen_count > 0 {
                    flush(self.last_value, self.last_seen_count, self.all_null)?;
                    self.seen_count += 1;
                }
                self.last_value = value;
                self.last_seen_count = 1;
            }
        } else {
            self.last_seen_count += 1;
        }
        if self.last_seen_count == RleCount::MAX {
            flush(self.last_value, self.last_seen_count, self.all_null)?;
            self.last_seen_count = 0;
            self.seen_count += 1;
        }
        Ok(())
    }
}

/// `RLEAnalyzeState`.
#[derive(Debug)]
struct RleAnalyzer<T> {
    state: RleState<T>,
    ty: LogicalType,
    info: CompressionInfo,
}

impl<T: Primitive> Analyzer for RleAnalyzer<T> {
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        for (value, valid) in unified_values::<T>(input, count) {
            self.state.update(value, valid, |_, _, _| Ok(()))?;
        }
        Ok(true)
    }

    /// `RLEFinalAnalyze`.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        Ok(Some(
            (size_of::<RleCount>() + T::SIZE) as u64 * self.state.seen_count,
        ))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let entry_size = T::SIZE + size_of::<RleCount>();
        let max_rle_count = align8_floor((self.info.block_size - RLE_HEADER_SIZE) / entry_size);
        let physical = self.ty.physical_type();
        Ok(Box::new(RleCompressor::<T> {
            state: RleState::new(),
            write_statistics: physical != PhysicalType::List,
            physical,
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            buffer: vec![0; self.info.block_size],
            count: 0,
            entry_count: 0,
            max_rle_count,
        }))
    }
}

/// `RLECompressState`: runs are written to a block-sized buffer, the values from the
/// header on and the counts from `max_rle_count` values further; a flush moves the counts
/// next to the values.
#[derive(Debug)]
struct RleCompressor<T> {
    state: RleState<T>,
    ty: LogicalType,
    physical: PhysicalType,
    write_statistics: bool,
    buffer: Vec<u8>,
    count: u64,
    stats: BaseStatistics,
    entry_count: usize,
    max_rle_count: usize,
}

impl<T: Primitive> RleCompressor<T> {
    /// `WriteValue`.
    fn write_value(
        &mut self,
        sink: &mut SegmentSink<'_>,
        value: T,
        count: RleCount,
        is_null: bool,
    ) -> Result<()> {
        let value_offset = RLE_HEADER_SIZE + self.entry_count * T::SIZE;
        value.store(&mut self.buffer[value_offset..]);
        let count_offset =
            RLE_HEADER_SIZE + self.max_rle_count * T::SIZE + self.entry_count * size_of::<RleCount>();
        self.buffer[count_offset..count_offset + 2].copy_from_slice(&count.to_le_bytes());
        self.entry_count += 1;
        if self.write_statistics {
            if is_null {
                self.stats.set_has_null_fast();
            } else {
                self.stats.set_has_no_null_fast();
                update_numeric_stats(&mut self.stats, self.physical, value);
            }
        }
        self.count += u64::from(count);
        if self.entry_count == self.max_rle_count {
            self.flush_segment(sink)?;
            self.entry_count = 0;
        }
        Ok(())
    }

    /// `FlushSegment`: compacts the counts next to the values and hands the segment over.
    fn flush_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let counts_size = size_of::<RleCount>() * self.entry_count;
        let original_offset = RLE_HEADER_SIZE + self.max_rle_count * T::SIZE;
        let minimal_offset = RLE_HEADER_SIZE + T::SIZE * self.entry_count;
        let aligned_offset = align8(minimal_offset);
        let total = aligned_offset + counts_size;
        self.buffer[minimal_offset..aligned_offset].fill(0);
        self.buffer
            .copy_within(original_offset..original_offset + counts_size, aligned_offset);
        self.buffer[..8].copy_from_slice(&(aligned_offset as u64).to_le_bytes());
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        sink.flush(FinishedSegment {
            data: self.buffer[..total].to_vec(),
            count: std::mem::take(&mut self.count),
            stats,
            compression: CompressionType::Rle,
            overflow_blocks: Vec::new(),
        })
    }
}

impl<T: Primitive> Compressor for RleCompressor<T> {
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        let mut state = std::mem::replace(&mut self.state, RleState::new());
        let result = unified_values::<T>(input, count).try_for_each(|(value, valid)| {
            state.update(value, valid, |v, c, n| self.write_value(sink, v, c, n))
        });
        self.state = state;
        result
    }

    /// `RLECompressState::Finalize`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let (value, count, is_null) = (
            self.state.last_value,
            self.state.last_seen_count,
            self.state.all_null,
        );
        self.write_value(sink, value, count, is_null)?;
        self.flush_segment(sink)
    }
}

fn analyzer<T: Primitive>(ty: &LogicalType, info: &CompressionInfo) -> Box<dyn Analyzer> {
    Box::new(RleAnalyzer::<T> {
        state: RleState::new(),
        ty: ty.clone(),
        info: *info,
    })
}

/// `RLEFun::GetFunction(type).init_analyze`.
pub(crate) fn init_analyze(
    ty: &LogicalType,
    physical: PhysicalType,
    info: &CompressionInfo,
) -> Option<Box<dyn Analyzer>> {
    Some(match physical {
        PhysicalType::Bool | PhysicalType::Int8 => analyzer::<i8>(ty, info),
        PhysicalType::Int16 => analyzer::<i16>(ty, info),
        PhysicalType::Int32 => analyzer::<i32>(ty, info),
        PhysicalType::Int64 => analyzer::<i64>(ty, info),
        PhysicalType::Int128 => analyzer::<i128>(ty, info),
        PhysicalType::UInt8 => analyzer::<u8>(ty, info),
        PhysicalType::UInt16 => analyzer::<u16>(ty, info),
        PhysicalType::UInt32 => analyzer::<u32>(ty, info),
        PhysicalType::UInt64 | PhysicalType::List => analyzer::<u64>(ty, info),
        PhysicalType::UInt128 => analyzer::<u128>(ty, info),
        PhysicalType::Float => analyzer::<f32>(ty, info),
        PhysicalType::Double => analyzer::<f64>(ty, info),
        _ => return None,
    })
}
