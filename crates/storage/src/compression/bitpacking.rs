//! Bit packing (DuckDB's `bitpacking.cpp` and `bitpacking_hugeint.cpp`).
//!
//! Values are compressed in groups of [`BITPACKING_METADATA_GROUP_SIZE`] with one of four
//! modes: `CONSTANT`, `CONSTANT_DELTA`, `DELTA_FOR` (delta encoding with a frame of
//! reference) and `FOR` (frame of reference). Segment layout: an 8-byte header with the offset of the
//! end of the metadata, the group data growing upwards, and per group one `u32` of
//! metadata (`mode << 24 | data offset`) growing downwards from the end of the segment.

use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_types::{PhysicalType, Vector};

use super::bitpacking_primitives::{GROUP_SIZE, unpack_group_u64, unpack_group_u128};
use super::primitive::{Primitive, corrupt, fill_values, load, write_values};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// Values per metadata group (`BITPACKING_METADATA_GROUP_SIZE`).
pub(crate) const BITPACKING_METADATA_GROUP_SIZE: usize = 2048;

/// The encoding of a metadata group (DuckDB's `BitpackingMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BitpackingMode {
    Invalid = 0,
    Auto = 1,
    Constant = 2,
    ConstantDelta = 3,
    DeltaFor = 4,
    For = 5,
}

impl BitpackingMode {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Invalid,
            1 => Self::Auto,
            2 => Self::Constant,
            3 => Self::ConstantDelta,
            4 => Self::DeltaFor,
            5 => Self::For,
            _ => return None,
        })
    }
}

/// `EncodeMeta`.
#[allow(dead_code, reason = "used by the checkpoint writer")]
pub(crate) const fn encode_meta(mode: BitpackingMode, offset: u32) -> u32 {
    offset | ((mode as u32) << 24)
}

/// An unsigned integer wide enough to hold the bits of a bit-packed type; decoding
/// arithmetic is done in it with wrapping operations and truncated to the type at the
/// end, which equals DuckDB's wrapping arithmetic in the type itself.
pub(crate) trait Wide: Copy + Default + Send + Sync + std::fmt::Debug + 'static {
    fn wrapping_add(self, other: Self) -> Self;
    fn wrapping_mul(self, other: Self) -> Self;
    fn from_usize(value: usize) -> Self;
    /// `UnPackBlock`: one group of 32 values of `width` bits.
    fn unpack_group(src: &[u8], width: u8, out: &mut [Self; GROUP_SIZE]);
}

impl Wide for u64 {
    #[inline]
    fn wrapping_add(self, other: Self) -> Self {
        self.wrapping_add(other)
    }
    #[inline]
    fn wrapping_mul(self, other: Self) -> Self {
        self.wrapping_mul(other)
    }
    #[inline]
    fn from_usize(value: usize) -> Self {
        value as Self
    }
    #[inline]
    fn unpack_group(src: &[u8], width: u8, out: &mut [Self; GROUP_SIZE]) {
        unpack_group_u64(src, width, out);
    }
}

impl Wide for u128 {
    #[inline]
    fn wrapping_add(self, other: Self) -> Self {
        self.wrapping_add(other)
    }
    #[inline]
    fn wrapping_mul(self, other: Self) -> Self {
        self.wrapping_mul(other)
    }
    #[inline]
    fn from_usize(value: usize) -> Self {
        value as Self
    }
    #[inline]
    fn unpack_group(src: &[u8], width: u8, out: &mut [Self; GROUP_SIZE]) {
        unpack_group_u128(src, width, out);
    }
}

/// An integer type that can be bit packed.
pub(crate) trait BitpackingValue: Primitive {
    type Wide: Wide;
    /// The bits of the value (any extension; only the low `SIZE * 8` bits matter).
    fn to_wide(self) -> Self::Wide;
    /// The value whose bits are the low `SIZE * 8` bits of `wide`.
    fn from_wide(wide: Self::Wide) -> Self;
}

macro_rules! bitpacking_value {
    ($wide:ty => $($ty:ty),*) => {$(
        impl BitpackingValue for $ty {
            type Wide = $wide;
            #[inline]
            fn to_wide(self) -> $wide {
                self as $wide
            }
            #[inline]
            fn from_wide(wide: $wide) -> Self {
                wide as Self
            }
        }
    )*};
}

bitpacking_value!(u64 => i8, i16, i32, i64, u8, u16, u32, u64);
bitpacking_value!(u128 => i128, u128);

/// The metadata of the group being read (`bitpacking_metadata_t` and the values stored at
/// the start of the group's data).
#[derive(Clone, Copy, Debug)]
struct Group<W> {
    mode: BitpackingMode,
    /// Offset of the packed values within the segment.
    data: usize,
    width: u8,
    frame_of_reference: W,
    constant: W,
    delta_offset: W,
}

/// The scan state of a bit-packed segment (DuckDB's `BitpackingScanState`).
#[derive(Debug)]
pub(crate) struct BitpackingScanState<T: BitpackingValue> {
    data: PersistentData,
    /// Offset of the metadata of the next group (`bitpacking_metadata_ptr`).
    next_metadata: Option<usize>,
    group: Group<T::Wide>,
    /// Rows of the current group already read (`current_group_offset`).
    group_offset: usize,
    decompression_buffer: [T::Wide; GROUP_SIZE],
    values: Vec<T>,
    _marker: PhantomData<T>,
}

impl<T: BitpackingValue> BitpackingScanState<T> {
    /// `LoadNextGroup`.
    fn load_next_group(&mut self) -> Result<()> {
        let bytes = self.data.bytes();
        let position = self
            .next_metadata
            .ok_or_else(|| corrupt("Bitpacking metadata is out of range - corrupt database file"))?;
        let encoded: u32 = load(bytes, position)?;
        self.next_metadata = position.checked_sub(size_of::<u32>());
        let mode = BitpackingMode::from_u8((encoded >> 24) as u8)
            .ok_or_else(|| corrupt("Invalid bitpacking mode"))?;
        let mut pointer = (encoded & 0x00FF_FFFF) as usize;
        self.group_offset = 0;
        self.group.mode = mode;
        match mode {
            BitpackingMode::Constant => {
                self.group.constant = load::<T>(bytes, pointer)?.to_wide();
                pointer += T::SIZE;
            }
            BitpackingMode::For | BitpackingMode::ConstantDelta | BitpackingMode::DeltaFor => {
                self.group.frame_of_reference = load::<T>(bytes, pointer)?.to_wide();
                pointer += T::SIZE;
            }
            BitpackingMode::Invalid | BitpackingMode::Auto => {
                return Err(corrupt("Invalid bitpacking mode"));
            }
        }
        match mode {
            BitpackingMode::ConstantDelta => {
                self.group.constant = load::<T>(bytes, pointer)?.to_wide();
                pointer += T::SIZE;
            }
            BitpackingMode::For | BitpackingMode::DeltaFor => {
                // The width is stored in a `T` slot; its low byte is the width.
                self.group.width = load::<u8>(bytes, pointer)?;
                pointer += T::SIZE.max(1);
            }
            _ => {}
        }
        if mode == BitpackingMode::DeltaFor {
            self.group.delta_offset = load::<T>(bytes, pointer)?.to_wide();
            pointer += T::SIZE;
        }
        self.group.data = pointer;
        Ok(())
    }

    /// Unpacks the algorithm group containing row `group_offset` of the current group into
    /// the decompression buffer.
    fn unpack_algorithm_group(&mut self) -> Result<()> {
        let offset_in_group = self.group_offset % GROUP_SIZE;
        let start = self.group.data + (self.group_offset - offset_in_group) * self.group.width as usize / 8;
        let src = self
            .data
            .bytes()
            .get(start..)
            .ok_or_else(|| corrupt("Bitpacking group data is out of range - corrupt database file"))?;
        T::Wide::unpack_group(src, self.group.width, &mut self.decompression_buffer);
        Ok(())
    }

    /// Decodes the next `count` values of a `FOR` or `DELTA_FOR` group, which must lie within
    /// one algorithm group, into `self.values` (cleared first); for `DELTA_FOR` the delta offset
    /// moves along.
    fn decode_packed(&mut self, count: usize) -> Result<()> {
        self.unpack_algorithm_group()?;
        let offset_in_group = self.group_offset % GROUP_SIZE;
        let frame = self.group.frame_of_reference;
        self.values.clear();
        if self.group.mode == BitpackingMode::DeltaFor {
            // `ApplyFrameOfReference` + `DeltaDecode`.
            let mut previous = self.group.delta_offset;
            for &delta in &self.decompression_buffer[offset_in_group..offset_in_group + count] {
                previous = delta.wrapping_add(frame).wrapping_add(previous);
                self.values.push(T::from_wide(previous));
            }
            self.group.delta_offset = previous;
        } else {
            self.values.extend(
                self.decompression_buffer[offset_in_group..offset_in_group + count]
                    .iter()
                    .map(|&value| T::from_wide(value.wrapping_add(frame))),
            );
        }
        self.group_offset += count;
        Ok(())
    }
}

impl<T: BitpackingValue> CodecScan for BitpackingScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let metadata_offset: u64 = load(data.bytes(), 0)?;
        let first = usize::try_from(metadata_offset)
            .ok()
            .and_then(|offset| offset.checked_sub(size_of::<u32>()))
            .filter(|&offset| segment.block_offset() + offset < segment.block_size())
            .ok_or_else(|| {
                corrupt(format!(
                    "Bitpacking offset is out of range at block \"{}\" - corrupt database file",
                    segment.block_id()
                ))
            })?;
        let mut state = Self {
            data,
            next_metadata: Some(first),
            group: Group {
                mode: BitpackingMode::Invalid,
                data: 0,
                width: 0,
                frame_of_reference: T::Wide::default(),
                constant: T::Wide::default(),
                delta_offset: T::Wide::default(),
            },
            group_offset: 0,
            decompression_buffer: [T::Wide::default(); GROUP_SIZE],
            values: Vec::with_capacity(GROUP_SIZE),
            _marker: PhantomData,
        };
        state.load_next_group()?;
        Ok(state)
    }

    /// `BitpackingScanState::Skip`.
    fn skip(&mut self, _segment: &ColumnSegment, skip_count: usize) -> Result<()> {
        let mut skipped = 0;
        let initial_group_offset = self.group_offset;
        let groups_to_skip = (skip_count + self.group_offset) / BITPACKING_METADATA_GROUP_SIZE;
        if groups_to_skip > 0 {
            self.next_metadata = self
                .next_metadata
                .and_then(|offset| offset.checked_sub((groups_to_skip - 1) * size_of::<u32>()));
            self.load_next_group()?;
            skipped += BITPACKING_METADATA_GROUP_SIZE - initial_group_offset;
            skipped += (groups_to_skip - 1) * BITPACKING_METADATA_GROUP_SIZE;
        }
        let mut remaining = skip_count - skipped;
        if self.group.mode == BitpackingMode::DeltaFor {
            // The deltas up to the target row are needed to continue from it.
            while remaining > 0 {
                let to_skip = remaining.min(GROUP_SIZE - self.group_offset % GROUP_SIZE);
                self.decode_packed(to_skip)?;
                remaining -= to_skip;
            }
        } else {
            self.group_offset += remaining;
        }
        Ok(())
    }

    /// `BitpackingScanPartial`.
    fn scan_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            if self.group_offset == BITPACKING_METADATA_GROUP_SIZE {
                self.load_next_group()?;
            }
            let remaining = count - scanned;
            match self.group.mode {
                BitpackingMode::Constant => {
                    let to_scan = remaining.min(BITPACKING_METADATA_GROUP_SIZE - self.group_offset);
                    let value = T::from_wide(self.group.constant);
                    fill_values(result, result_offset + scanned, to_scan, value)?;
                    scanned += to_scan;
                    self.group_offset += to_scan;
                }
                BitpackingMode::ConstantDelta => {
                    let to_scan = remaining.min(BITPACKING_METADATA_GROUP_SIZE - self.group_offset);
                    self.values.clear();
                    for index in 0..to_scan {
                        let multiplier = T::Wide::from_usize(self.group_offset + index);
                        let value = self
                            .group
                            .constant
                            .wrapping_mul(multiplier)
                            .wrapping_add(self.group.frame_of_reference);
                        self.values.push(T::from_wide(value));
                    }
                    write_values(result, result_offset + scanned, &self.values)?;
                    scanned += to_scan;
                    self.group_offset += to_scan;
                }
                BitpackingMode::For | BitpackingMode::DeltaFor => {
                    let to_scan = remaining.min(GROUP_SIZE - self.group_offset % GROUP_SIZE);
                    self.decode_packed(to_scan)?;
                    write_values(result, result_offset + scanned, &self.values)?;
                    scanned += to_scan;
                }
                BitpackingMode::Invalid | BitpackingMode::Auto => {
                    return Err(corrupt("Invalid bitpacking mode"));
                }
            }
        }
        Ok(())
    }
}

/// `BitpackingFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Bool | PhysicalType::Int8 => Sequential::<BitpackingScanState<i8>>::boxed(segment),
        PhysicalType::Int16 => Sequential::<BitpackingScanState<i16>>::boxed(segment),
        PhysicalType::Int32 => Sequential::<BitpackingScanState<i32>>::boxed(segment),
        PhysicalType::Int64 => Sequential::<BitpackingScanState<i64>>::boxed(segment),
        PhysicalType::UInt8 => Sequential::<BitpackingScanState<u8>>::boxed(segment),
        PhysicalType::UInt16 => Sequential::<BitpackingScanState<u16>>::boxed(segment),
        PhysicalType::UInt32 => Sequential::<BitpackingScanState<u32>>::boxed(segment),
        PhysicalType::UInt64 | PhysicalType::List => Sequential::<BitpackingScanState<u64>>::boxed(segment),
        PhysicalType::Int128 => Sequential::<BitpackingScanState<i128>>::boxed(segment),
        PhysicalType::UInt128 => Sequential::<BitpackingScanState<u128>>::boxed(segment),
        _ => return None,
    })
}
