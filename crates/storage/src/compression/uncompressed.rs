//! Uncompressed segments (DuckDB's `FixedSizeUncompressed`, `ValidityUncompressed` and
//! `UncompressedStringStorage`). The byte layouts are DuckDB's on-disk layouts:
//!
//! - fixed-size values: `count` values back to back (LIST columns store the cumulative
//!   end offset of each list as a `u64`);
//! - validity: 64-bit little-endian words, bit set = valid, initialised to all valid;
//! - strings: a header `(dictionary size: u32, dictionary end: u32)`, then one `i32`
//!   per row with the cumulative dictionary size after the row (negative for a marker of
//!   a big string stored in an overflow block), and the string bytes growing backwards
//!   from the dictionary end.

use std::sync::Arc;

use rustdb_buffer::{MAXIMUM_BLOCK, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{LogicalTypeId, PhysicalType, StringT, ValidityMask, Vector};

use crate::statistics::BaseStatistics;
use crate::table::column_segment::{ColumnSegment, Layout, OverflowBlock, SegmentBuffer};

/// Size of the string dictionary header (`DICTIONARY_HEADER_SIZE`).
pub(crate) const DICTIONARY_HEADER_SIZE: usize = 8;
/// Size of a big string marker: block id and offset (`BIG_STRING_MARKER_SIZE`).
pub(crate) const BIG_STRING_MARKER_SIZE: usize = 12;
/// Bytes of validity per standard vector (`ValidityMask::STANDARD_MASK_SIZE`).
pub(crate) const STANDARD_MASK_SIZE: usize = STANDARD_VECTOR_SIZE / 8;

/// Strings at least this long are stored in overflow blocks
/// (`StringUncompressed::GetStringBlockLimit`).
pub(crate) fn string_block_limit(block_size: usize) -> usize {
    (block_size / 4 / 8 * 8).min(4096)
}

/// Writes the initial contents of a fresh transient segment (`init_segment`).
pub(crate) fn init_segment(segment: &ColumnSegment) -> Result<()> {
    let buffer = segment.pin()?;
    match segment.layout() {
        Layout::Validity => {
            let fill = vec![0xFF; segment.segment_size()];
            // SAFETY: the segment is being constructed and is not shared yet.
            unsafe { buffer.write(0, &fill) }
        }
        Layout::Strings => {
            let end = u32::try_from(segment.segment_size())
                .map_err(|_| Error::internal("string segment larger than 4 GiB"))?;
            let mut header = [0u8; DICTIONARY_HEADER_SIZE];
            header[4..].copy_from_slice(&end.to_le_bytes());
            // SAFETY: the segment is being constructed and is not shared yet.
            unsafe { buffer.write(0, &header) }
        }
        Layout::Fixed(_) => Ok(()),
    }
}

/// `NullValue<T>()`: the bytes DuckDB writes for NULL rows of a fixed-size type.
fn null_bytes(physical: PhysicalType, width: usize) -> Vec<u8> {
    match physical {
        PhysicalType::Int8 => i8::MIN.to_le_bytes().to_vec(),
        PhysicalType::Int16 => i16::MIN.to_le_bytes().to_vec(),
        PhysicalType::Int32 => i32::MIN.to_le_bytes().to_vec(),
        PhysicalType::Int64 => i64::MIN.to_le_bytes().to_vec(),
        // hugeint_t{lower: 0, upper: INT64_MIN}
        PhysicalType::Int128 => i128::MIN.to_le_bytes().to_vec(),
        PhysicalType::Float => f32::NAN.to_le_bytes().to_vec(),
        PhysicalType::Double => f64::NAN.to_le_bytes().to_vec(),
        PhysicalType::Interval => {
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&i32::MIN.to_le_bytes());
            bytes.extend_from_slice(&i32::MIN.to_le_bytes());
            bytes.extend_from_slice(&i64::MIN.to_le_bytes());
            bytes
        }
        _ => vec![0; width],
    }
}

/// Appends rows `offset..offset + count` of `vector` to a transient segment and returns
/// how many fit.
pub(crate) fn append(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    stats: &mut BaseStatistics,
    vector: &Vector,
    offset: usize,
    count: usize,
) -> Result<usize> {
    match segment.layout() {
        Layout::Fixed(width) => append_fixed(segment, buffer, stats, vector, offset, count, width),
        Layout::Validity => append_validity(segment, buffer, stats, vector, offset, count),
        Layout::Strings => append_strings(segment, buffer, stats, vector, offset, count),
    }
}

fn append_fixed(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    stats: &mut BaseStatistics,
    vector: &Vector,
    offset: usize,
    count: usize,
    width: usize,
) -> Result<usize> {
    let base = segment.count() as usize;
    let max_tuples = segment.segment_size() / width;
    let copy_count = count.min(max_tuples.saturating_sub(base));
    if copy_count == 0 {
        return Ok(0);
    }
    let physical = vector.logical_type().physical_type();
    let source = vector.data_bytes();
    let sel = vector.sel();
    let validity = vector.validity();
    let mut target = vec![0u8; copy_count * width];
    let is_list = segment.logical_type().physical_type() == PhysicalType::List;
    let null_value = null_bytes(physical, width);
    let mut any_valid = false;
    let mut any_null = false;
    for (index, chunk) in target.chunks_exact_mut(width).enumerate() {
        let slot = sel.slot(offset + index);
        if is_list {
            // List offsets are appended from a UBIGINT vector by the list column.
            chunk.copy_from_slice(&source[slot * width..(slot + 1) * width]);
            continue;
        }
        if validity.is_valid(slot) {
            any_valid = true;
            chunk.copy_from_slice(&source[slot * width..(slot + 1) * width]);
        } else {
            any_null = true;
            chunk.copy_from_slice(&null_value);
        }
    }
    if !is_list {
        if any_valid {
            stats.set_has_no_null_fast();
        }
        if any_null {
            stats.set_has_null_fast();
        }
        update_numeric_stats(stats, vector, offset, copy_count);
    }
    // SAFETY: rows at and after the published count are not visible to readers yet.
    unsafe { buffer.write(base * width, &target)? };
    segment.set_count((base + copy_count) as u64);
    Ok(copy_count)
}

/// Widens the numeric statistics by the valid rows `offset..offset + count`.
fn update_numeric_stats(stats: &mut BaseStatistics, vector: &Vector, offset: usize, count: usize) {
    let Some(numeric) = stats.numeric_mut() else {
        return;
    };
    macro_rules! update {
        ($ty:ty) => {{
            let data = vector.unified::<$ty>();
            numeric.update((offset..offset + count).filter_map(|row| data.get(row)));
        }};
    }
    match vector.logical_type().physical_type() {
        PhysicalType::Bool => update!(bool),
        PhysicalType::Int8 => update!(i8),
        PhysicalType::Int16 => update!(i16),
        PhysicalType::Int32 => update!(i32),
        PhysicalType::Int64 => update!(i64),
        PhysicalType::Int128 => update!(i128),
        PhysicalType::UInt8 => update!(u8),
        PhysicalType::UInt16 => update!(u16),
        PhysicalType::UInt32 => update!(u32),
        PhysicalType::UInt64 => update!(u64),
        PhysicalType::UInt128 => update!(u128),
        PhysicalType::Float => update!(f32),
        PhysicalType::Double => update!(f64),
        _ => {}
    }
}

fn append_validity(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    stats: &mut BaseStatistics,
    vector: &Vector,
    offset: usize,
    count: usize,
) -> Result<usize> {
    let base = segment.count() as usize;
    let max_tuples = segment.segment_size() / STANDARD_MASK_SIZE * STANDARD_VECTOR_SIZE;
    let append_count = count.min(max_tuples.saturating_sub(base));
    let validity = vector.validity();
    if validity.is_all_valid() {
        if append_count > 0 {
            stats.set_has_no_null_fast();
        }
        segment.set_count((base + append_count) as u64);
        return Ok(append_count);
    }
    let sel = vector.sel();
    // Rewrite the words covering the new rows; bits below `base` keep their values.
    let first_word = base / 64;
    let last_word = (base + append_count).div_ceil(64);
    // SAFETY: the words are only read by this writer: rows below `base` in the first
    // word are immutable and preserved by reading them back before the rewrite, and
    // readers never look at bits at or after the published count.
    let current = unsafe { buffer.bytes(first_word * 8, (last_word - first_word) * 8)? }.to_vec();
    let mut words: Vec<u64> = current
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    for index in 0..append_count {
        let row = base + index;
        if validity.is_valid(sel.slot(offset + index)) {
            stats.set_has_no_null_fast();
        } else {
            stats.set_has_null_fast();
            words[row / 64 - first_word] &= !(1u64 << (row % 64));
        }
    }
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    // SAFETY: see above; the first word's bits below `base` are written back unchanged.
    unsafe { buffer.write(first_word * 8, &bytes)? };
    segment.set_count((base + append_count) as u64);
    Ok(append_count)
}

fn append_strings(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    stats: &mut BaseStatistics,
    vector: &Vector,
    offset: usize,
    count: usize,
) -> Result<usize> {
    let base = segment.count() as usize;
    let segment_size = segment.segment_size();
    let block_limit = string_block_limit(segment.block_size());
    let check_unicode = stats.logical_type().id() == LogicalTypeId::Varchar;
    // SAFETY: the header and the offsets of published rows are only written by this
    // appender; the dictionary end does not change while appending.
    let (mut dictionary_size, dictionary_end) =
        unsafe { (buffer.read_u32(0)? as usize, buffer.read_u32(4)? as usize) };
    let data = vector.unified::<StringT>();
    let mut offsets = Vec::with_capacity(count);
    let mut previous = if base > 0 {
        // SAFETY: a published row's offset is immutable.
        let bytes = unsafe { buffer.bytes(DICTIONARY_HEADER_SIZE + (base - 1) * 4, 4)? };
        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    } else {
        0
    };
    let mut appended = 0;
    for index in 0..count {
        let used = DICTIONARY_HEADER_SIZE + (base + index) * 4 + dictionary_size;
        let mut remaining = segment_size.saturating_sub(used);
        if remaining < 4 {
            break;
        }
        remaining -= 4;
        let Some(value) = data.get(offset + index) else {
            stats.set_has_null_fast();
            offsets.push(previous);
            appended += 1;
            continue;
        };
        let bytes = vector.string_value(&value);
        let big = bytes.len() >= block_limit;
        let required = if big { BIG_STRING_MARKER_SIZE } else { bytes.len() };
        if required > remaining {
            break;
        }
        stats.set_has_no_null_fast();
        if let Some(string) = stats.string_mut() {
            string.update(bytes, check_unicode);
        }
        dictionary_size += required;
        let position = dictionary_end - dictionary_size;
        if big {
            let (block_id, block_offset) = write_overflow_string(segment, bytes)?;
            let mut marker = [0u8; BIG_STRING_MARKER_SIZE];
            marker[..8].copy_from_slice(&block_id.to_le_bytes());
            marker[8..].copy_from_slice(&block_offset.to_le_bytes());
            // SAFETY: the dictionary grows downwards into unpublished space.
            unsafe { buffer.write(position, &marker)? };
            previous = -(dictionary_size as i32);
        } else {
            // SAFETY: as above.
            unsafe { buffer.write(position, bytes)? };
            previous = dictionary_size as i32;
        }
        offsets.push(previous);
        appended += 1;
    }
    let offset_bytes: Vec<u8> = offsets.iter().flat_map(|value| value.to_le_bytes()).collect();
    // SAFETY: offsets of unpublished rows and the dictionary size are only read by this
    // appender.
    unsafe {
        buffer.write(DICTIONARY_HEADER_SIZE + base * 4, &offset_bytes)?;
        buffer.write(0, &(dictionary_size as u32).to_le_bytes())?;
    }
    segment.set_count((base + appended) as u64);
    Ok(appended)
}

/// Stores a big string in an in-memory overflow block as `[length: u32][bytes]` and
/// returns its location (`WriteStringMemory`).
fn write_overflow_string(segment: &ColumnSegment, bytes: &[u8]) -> Result<(i64, i32)> {
    let total = bytes.len() + 4;
    let mut state = segment.string_state();
    let needs_block = state
        .memory_blocks
        .last()
        .is_none_or(|block| block.used + total >= block.size);
    if needs_block {
        let size = total.max(segment.block_size());
        let pin = segment
            .buffer_manager()
            .allocate(MemoryTag::OverflowStrings, size, false)?;
        state.memory_blocks.push(OverflowBlock {
            block: Arc::clone(pin.block()),
            size,
            used: 0,
        });
    }
    let Some(block) = state.memory_blocks.last_mut() else {
        return Err(Error::internal("overflow block missing"));
    };
    let pin = SegmentBuffer::new(segment.buffer_manager().pin(&block.block)?, 0);
    let mut record = Vec::with_capacity(total);
    record.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    record.extend_from_slice(bytes);
    // SAFETY: the region past `used` of an overflow block is unpublished.
    unsafe { pin.write(block.used, &record)? };
    let location = (block.block.block_id(), block.used as i32);
    block.used += total;
    Ok(location)
}

/// Reads a big string written by [`write_overflow_string`]; returns a pin of its block
/// and its byte range.
fn read_overflow_string(
    segment: &ColumnSegment,
    block_id: i64,
    offset: i32,
) -> Result<(SegmentBuffer, usize, usize)> {
    if block_id < MAXIMUM_BLOCK {
        return read_disk_overflow_string(segment, block_id, offset);
    }
    let handle = {
        let state = segment.string_state();
        let block = state
            .memory_blocks
            .iter()
            .find(|block| block.block.block_id() == block_id)
            .ok_or_else(|| Error::internal(format!("overflow string block {block_id} not found")))?;
        Arc::clone(&block.block)
    };
    let pin = SegmentBuffer::new(segment.buffer_manager().pin(&handle)?, 0);
    let offset = offset as usize;
    // SAFETY: published overflow strings are immutable.
    let length = unsafe { pin.read_u32(offset)? } as usize;
    Ok((pin, offset + 4, length))
}

/// The bytes of a big string stored at `block_id`/`offset`, in memory or on disk.
pub(crate) fn read_overflow_string_bytes(
    segment: &ColumnSegment,
    block_id: i64,
    offset: i32,
) -> Result<Vec<u8>> {
    let (pin, start, length) = read_overflow_string(segment, block_id, offset)?;
    // SAFETY: published overflow strings are immutable.
    Ok(unsafe { pin.bytes(start, length)? }.to_vec())
}

/// Reads a big string from on-disk overflow blocks (`ReadOverflowString` for persistent
/// blocks): `[length: u32][bytes]`, continuing in the block whose id is stored in the
/// last 8 bytes of a full block. The string is copied into a fresh buffer.
fn read_disk_overflow_string(
    segment: &ColumnSegment,
    block_id: i64,
    offset: i32,
) -> Result<(SegmentBuffer, usize, usize)> {
    let block_manager = segment
        .persistent_block_manager()
        .ok_or_else(|| Error::internal("on-disk overflow string of a transient segment"))?;
    let buffer_manager = segment.buffer_manager();
    let string_space = segment.block_size() - 8;
    let mut handle = buffer_manager.register_block(block_manager, block_id);
    let mut pin = SegmentBuffer::new(buffer_manager.pin(&handle)?, 0);
    let mut offset = usize::try_from(offset).map_err(|_| Error::internal("negative overflow offset"))?;
    // SAFETY: on-disk blocks are immutable.
    let length = unsafe { pin.read_u32(offset)? } as usize;
    offset += 4;
    let target = buffer_manager.allocate(MemoryTag::OverflowStrings, length.max(1), false)?;
    let target = SegmentBuffer::new(target, 0);
    let mut written = 0;
    while written < length {
        let to_copy = (length - written).min(string_space.saturating_sub(offset));
        if to_copy > 0 {
            // SAFETY: on-disk blocks are immutable; the target buffer is not shared yet.
            unsafe {
                let bytes = pin.bytes(offset, to_copy)?.to_vec();
                target.write(written, &bytes)?;
            }
            written += to_copy;
            offset += to_copy;
        }
        if written < length {
            // SAFETY: as above.
            let next = unsafe { pin.bytes(string_space, 8)? };
            let next = i64::from_le_bytes(next.try_into().unwrap_or_default());
            handle = buffer_manager.register_block(block_manager, next);
            pin = SegmentBuffer::new(buffer_manager.pin(&handle)?, 0);
            offset = 0;
        }
    }
    Ok((target, 0, length))
}

/// Scans rows `start..start + count` into `result` at `result_offset`.
pub(crate) fn scan(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    start: usize,
    count: usize,
    result: &mut Vector,
    result_offset: usize,
) -> Result<()> {
    if count == 0 {
        return Ok(());
    }
    match segment.layout() {
        Layout::Fixed(width) => {
            // SAFETY: rows below the published count are immutable.
            let source = unsafe { buffer.bytes(start * width, count * width)? };
            let target = result.data_bytes_mut();
            let target_start = result_offset * width;
            target
                .get_mut(target_start..target_start + count * width)
                .ok_or_else(|| Error::internal("scan target vector too small"))?
                .copy_from_slice(source);
            Ok(())
        }
        Layout::Validity => scan_validity(buffer, start, count, result, result_offset),
        Layout::Strings => scan_strings(segment, buffer, start, count, result, result_offset),
    }
}

/// Copies `count` validity bits starting at bit `start` of the segment into the mask of
/// `result` starting at `result_offset` (`ValidityUncompressed::UnalignedScan`).
fn scan_validity(
    buffer: &SegmentBuffer,
    start: usize,
    count: usize,
    result: &mut Vector,
    result_offset: usize,
) -> Result<()> {
    let first_word = start / 64;
    let last_word = (start + count).div_ceil(64);
    // SAFETY: words covering published rows; bits beyond the count are ignored below.
    let bytes = unsafe { buffer.bytes(first_word * 8, (last_word - first_word) * 8)? };
    let source: Vec<u64> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    if source.iter().all(|&word| word == u64::MAX) {
        return Ok(());
    }
    let bit = |row: usize| source[(start + row) / 64 - first_word] >> ((start + row) % 64) & 1 == 1;
    let capacity = result.capacity().max(result_offset + count);
    let mut words = match result.validity().words() {
        Some(words) => words.to_vec(),
        None => Vec::new(),
    };
    words.resize(capacity.div_ceil(64), u64::MAX);
    for row in 0..count {
        if !bit(row) {
            let target = result_offset + row;
            words[target / 64] &= !(1u64 << (target % 64));
        }
    }
    result.set_validity(ValidityMask::from_words(words));
    Ok(())
}

fn scan_strings(
    segment: &ColumnSegment,
    buffer: &SegmentBuffer,
    start: usize,
    count: usize,
    result: &mut Vector,
    result_offset: usize,
) -> Result<()> {
    // SAFETY: the dictionary end never changes after a segment is published.
    let dictionary_end = unsafe { buffer.read_u32(4)? } as usize;
    let first = start.saturating_sub(1);
    let offset_count = start + count - first;
    // SAFETY: offsets of published rows are immutable.
    let offset_bytes = unsafe { buffer.bytes(DICTIONARY_HEADER_SIZE + first * 4, offset_count * 4)? };
    let offsets: Vec<i32> = offset_bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| i32::from_le_bytes(*chunk))
        .collect();
    let mut previous = if start > 0 { offsets[0] } else { 0 };
    let skip = usize::from(start > 0);
    let mut overflow_pins = Vec::new();
    let mut strings = Vec::with_capacity(count);
    for &current in &offsets[skip..skip + count] {
        let length = (current.unsigned_abs() - previous.unsigned_abs()) as usize;
        previous = current;
        if current >= 0 {
            let position = dictionary_end - current as usize;
            // SAFETY: dictionary bytes of published rows are immutable; the pin of
            // `buffer` is kept alive in the result vector below.
            let bytes = unsafe { buffer.bytes(position, length)? };
            // SAFETY: the bytes live in the pinned block kept alive by `result`.
            strings.push(unsafe { StringT::from_raw(bytes) });
        } else if length == 0 {
            strings.push(StringT::default());
        } else {
            let position = dictionary_end - current.unsigned_abs() as usize;
            // SAFETY: published markers are immutable.
            let marker = unsafe { buffer.bytes(position, BIG_STRING_MARKER_SIZE)? };
            let block_id = i64::from_le_bytes(marker[..8].try_into().unwrap_or_default());
            let block_offset = i32::from_le_bytes(marker[8..].try_into().unwrap_or_default());
            let (pin, string_start, string_length) = read_overflow_string(segment, block_id, block_offset)?;
            // SAFETY: published overflow strings are immutable and `pin` is kept alive
            // in the result vector below.
            let bytes = unsafe { pin.bytes(string_start, string_length)? };
            // SAFETY: as above.
            strings.push(unsafe { StringT::from_raw(bytes) });
            overflow_pins.push(pin);
        }
    }
    let values = result.values_mut::<StringT>();
    values
        .get_mut(result_offset..result_offset + count)
        .ok_or_else(|| Error::internal("scan target vector too small"))?
        .copy_from_slice(&strings);
    buffer.keep_alive_in(result);
    for pin in &overflow_pins {
        pin.keep_alive_in(result);
    }
    Ok(())
}

/// Truncates the segment's contents to `new_count` rows (`RevertAppend`).
pub(crate) fn revert_append(segment: &ColumnSegment, new_count: u64) -> Result<()> {
    let new_count = new_count as usize;
    let count = segment.count() as usize;
    if new_count >= count {
        return Ok(());
    }
    let buffer = segment.pin()?;
    match segment.layout() {
        Layout::Fixed(_) => Ok(()),
        Layout::Validity => {
            // Reset the bits of the removed rows to valid.
            let first_word = new_count / 64;
            let size = segment.segment_size();
            // SAFETY: the removed rows were never visible to readers (reverted appends are
            // uncommitted) and the append lock excludes other writers.
            let current = unsafe { buffer.bytes(first_word * 8, 8)? };
            let mut word = u64::from_le_bytes(current.try_into().unwrap_or([0xFF; 8]));
            word |= !((1u64 << (new_count % 64)) - 1);
            // SAFETY: as above.
            unsafe {
                buffer.write(first_word * 8, &word.to_le_bytes())?;
                let rest = size.saturating_sub((first_word + 1) * 8);
                buffer.write((first_word + 1) * 8, &vec![0xFF; rest])?;
            }
            Ok(())
        }
        Layout::Strings => {
            let dictionary_size = if new_count == 0 {
                0
            } else {
                // SAFETY: as above.
                let bytes = unsafe { buffer.bytes(DICTIONARY_HEADER_SIZE + (new_count - 1) * 4, 4)? };
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).unsigned_abs()
            };
            // SAFETY: as above.
            unsafe { buffer.write(0, &dictionary_size.to_le_bytes()) }
        }
    }
}
