//! Bit packing (DuckDB's `bitpacking.cpp` and `bitpacking_hugeint.cpp`).
//!
//! Values are compressed in groups of [`BITPACKING_METADATA_GROUP_SIZE`] with one of four
//! modes: `CONSTANT`, `CONSTANT_DELTA`, `DELTA_FOR` (delta encoding with a frame of
//! reference) and `FOR` (frame of reference). Segment layout: an 8-byte header with the offset of the
//! end of the metadata, the group data growing upwards, and per group one `u32` of
//! metadata (`mode << 24 | data offset`) growing downwards from the end of the segment.

use std::marker::PhantomData;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, PhysicalType, Vector};

use super::bitpacking_primitives::{
    BITPACKING_HEADER_SIZE, GROUP_SIZE, minimum_bit_width_signed, minimum_bit_width_unsigned,
    pack_buffer_u64, pack_buffer_u128, required_size, unpack_group_for, unpack_group_u64, unpack_group_u128,
};
use super::primitive::{
    Primitive, align8, corrupt, load, result_bytes, unified_values, update_numeric_stats,
};
use super::{Analyzer, CodecScan, CompressionInfo, CompressionType, Compressor, SegmentScan, Sequential};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
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

    /// Decodes the whole group at byte `start` of `data` with the frame of reference
    /// added, as `T` values into `out` (`GROUP_SIZE` values' bytes); `false` (nothing
    /// written) if there is no fused kernel for the width, `out` is not aligned for `T`,
    /// or the group is out of range.
    fn unpack_group_for<T: BitpackingValue<Wide = Self>>(
        data: &[u8],
        start: usize,
        width: u8,
        frame: Self,
        out: &mut [u8],
    ) -> bool {
        let _ = (data, start, width, frame, out);
        false
    }
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

    #[inline]
    fn unpack_group_for<T: BitpackingValue<Wide = Self>>(
        data: &[u8],
        start: usize,
        width: u8,
        frame: u64,
        out: &mut [u8],
    ) -> bool {
        let (Some(src), Some(slots)) = (data.get(start..), typed_slots::<T>(out)) else {
            return false;
        };
        let Ok(slots) = <&mut [T; GROUP_SIZE]>::try_from(slots) else {
            return false;
        };
        unpack_group_for(src, width, frame, slots, T::from_wide)
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

    /// `BitpackingScanPartial`: decodes straight into the result vector, one algorithm
    /// group at a time.
    fn scan_partial(
        &mut self,
        _segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let target = result_bytes::<T>(result, result_offset, count)?;
        let mut scanned = 0;
        while scanned < count {
            if self.group_offset == BITPACKING_METADATA_GROUP_SIZE {
                self.load_next_group()?;
            }
            let remaining = count - scanned;
            let out = &mut target[scanned * T::SIZE..];
            let to_scan = match self.group.mode {
                BitpackingMode::Constant => {
                    let to_scan = remaining.min(BITPACKING_METADATA_GROUP_SIZE - self.group_offset);
                    let value = T::from_wide(self.group.constant);
                    for slot in out.chunks_exact_mut(T::SIZE).take(to_scan) {
                        value.store(slot);
                    }
                    to_scan
                }
                BitpackingMode::ConstantDelta => {
                    let to_scan = remaining.min(BITPACKING_METADATA_GROUP_SIZE - self.group_offset);
                    for (index, slot) in out.chunks_exact_mut(T::SIZE).take(to_scan).enumerate() {
                        let multiplier = T::Wide::from_usize(self.group_offset + index);
                        let value = self
                            .group
                            .constant
                            .wrapping_mul(multiplier)
                            .wrapping_add(self.group.frame_of_reference);
                        T::from_wide(value).store(slot);
                    }
                    to_scan
                }
                BitpackingMode::For | BitpackingMode::DeltaFor => {
                    let to_scan = remaining.min(GROUP_SIZE - self.group_offset % GROUP_SIZE);
                    if to_scan == GROUP_SIZE
                        && T::Wide::unpack_group_for::<T>(
                            self.data.bytes(),
                            self.group.data + self.group_offset * self.group.width as usize / 8,
                            self.group.width,
                            self.group.frame_of_reference,
                            &mut out[..GROUP_SIZE * T::SIZE],
                        )
                    {
                        // A whole group decoded straight into the result
                        // (`BitpackingScanPartial` unpacking into the result vector).
                        if self.group.mode == BitpackingMode::DeltaFor
                            && let Some(typed) = typed_slots::<T>(&mut out[..GROUP_SIZE * T::SIZE])
                        {
                            // `DeltaDecode` in place: only the low `T` bits of the running
                            // sum are kept, and those do not depend on the bits narrowed
                            // away.
                            let mut previous = self.group.delta_offset;
                            for slot in typed {
                                previous = slot.to_wide().wrapping_add(previous);
                                *slot = T::from_wide(previous);
                            }
                            self.group.delta_offset = previous;
                        }
                        self.group_offset += to_scan;
                        scanned += to_scan;
                        continue;
                    }
                    self.unpack_algorithm_group()?;
                    let offset_in_group = self.group_offset % GROUP_SIZE;
                    let frame = self.group.frame_of_reference;
                    let decoded = &self.decompression_buffer[offset_in_group..offset_in_group + to_scan];
                    if self.group.mode == BitpackingMode::DeltaFor
                        && let Some(typed) = typed_slots::<T>(&mut out[..to_scan * T::SIZE])
                    {
                        let mut previous = self.group.delta_offset;
                        for (slot, &delta) in typed.iter_mut().zip(decoded) {
                            previous = delta.wrapping_add(frame).wrapping_add(previous);
                            *slot = T::from_wide(previous);
                        }
                        self.group.delta_offset = previous;
                    } else if self.group.mode == BitpackingMode::DeltaFor {
                        let slots = out.chunks_exact_mut(T::SIZE);
                        // `ApplyFrameOfReference` + `DeltaDecode`.
                        let mut previous = self.group.delta_offset;
                        for (slot, &delta) in slots.zip(decoded) {
                            previous = delta.wrapping_add(frame).wrapping_add(previous);
                            T::from_wide(previous).store(slot);
                        }
                        self.group.delta_offset = previous;
                    } else if let Some(typed) = typed_slots::<T>(&mut out[..to_scan * T::SIZE]) {
                        // Values written as `T` (the vector's native layout).
                        for (slot, &value) in typed.iter_mut().zip(decoded) {
                            *slot = T::from_wide(value.wrapping_add(frame));
                        }
                    } else {
                        for (slot, &value) in out.chunks_exact_mut(T::SIZE).zip(decoded) {
                            T::from_wide(value.wrapping_add(frame)).store(slot);
                        }
                    }
                    to_scan
                }
                BitpackingMode::Invalid | BitpackingMode::Auto => {
                    return Err(corrupt("Invalid bitpacking mode"));
                }
            };
            self.group_offset += to_scan;
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `bytes` as `T` slots when they are aligned for `T` and `T`'s in-memory layout is the
/// stored (little-endian) one.
fn typed_slots<T: BitpackingValue>(bytes: &mut [u8]) -> Option<&mut [T]> {
    if cfg!(target_endian = "big") || bytes.as_ptr().align_offset(align_of::<T>()) != 0 {
        return None;
    }
    // SAFETY: the bytes are aligned for `T`, `T` is a primitive integer for which every bit
    // pattern is valid, and the length is a multiple of `T::SIZE` (`size_of::<T>()`).
    Some(unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<T>(), bytes.len() / T::SIZE) })
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

/// The integer arithmetic of the bit-packing analysis (`T` and `MakeSigned<T>` with
/// DuckDB's overflow-checked `TrySubtractOperator`).
pub(crate) trait PackInt: BitpackingValue + Ord {
    /// `MakeSigned<T>::type`.
    type Signed: Copy + Ord + std::fmt::Debug;
    const SIGNED: bool;
    const BITS: u32;
    const MIN: Self;
    const MAX: Self;
    const SIGNED_MIN: Self::Signed;
    const SIGNED_MAX: Self::Signed;
    const SIGNED_ZERO: Self::Signed;
    fn checked_sub(self, other: Self) -> Option<Self>;
    /// `static_cast<T_S>(value)`.
    fn to_signed(self) -> Self::Signed;
    /// `static_cast<T>(value)`.
    fn from_signed(value: Self::Signed) -> Self;
    fn signed_checked_sub(a: Self::Signed, b: Self::Signed) -> Option<Self::Signed>;
    fn signed_wrapping_sub(a: Self::Signed, b: Self::Signed) -> Self::Signed;
    /// The value's bits, zero extended.
    fn unsigned_bits(self) -> u128;
    /// The value (signed types only).
    fn as_i128(self) -> i128;
}

macro_rules! pack_int {
    ($($ty:ty => $signed:ty, $is_signed:expr);* $(;)?) => {$(
        impl PackInt for $ty {
            type Signed = $signed;
            const SIGNED: bool = $is_signed;
            const BITS: u32 = <$ty>::BITS;
            const MIN: Self = <$ty>::MIN;
            const MAX: Self = <$ty>::MAX;
            const SIGNED_MIN: $signed = <$signed>::MIN;
            const SIGNED_MAX: $signed = <$signed>::MAX;
            const SIGNED_ZERO: $signed = 0;
            #[inline]
            fn checked_sub(self, other: Self) -> Option<Self> {
                <$ty>::checked_sub(self, other)
            }
            #[inline]
            fn to_signed(self) -> $signed {
                self as $signed
            }
            #[inline]
            fn from_signed(value: $signed) -> Self {
                value as Self
            }
            #[inline]
            fn signed_checked_sub(a: $signed, b: $signed) -> Option<$signed> {
                a.checked_sub(b)
            }
            #[inline]
            fn signed_wrapping_sub(a: $signed, b: $signed) -> $signed {
                a.wrapping_sub(b)
            }
            #[inline]
            fn unsigned_bits(self) -> u128 {
                (self as u128) & (u128::MAX >> (128 - <$ty>::BITS))
            }
            #[inline]
            fn as_i128(self) -> i128 {
                self as i128
            }
        }
    )*};
}

pack_int! {
    i8 => i8, true; i16 => i16, true; i32 => i32, true; i64 => i64, true; i128 => i128, true;
    u8 => i8, false; u16 => i16, false; u32 => i32, false; u64 => i64, false; u128 => i128, false;
}

/// `MinimumBitWidth(value)` with `T`'s signedness.
fn minimum_bit_width<T: PackInt>(value: T) -> u8 {
    if T::SIGNED {
        minimum_bit_width_signed(value.as_i128(), value.as_i128(), T::BITS)
    } else {
        minimum_bit_width_unsigned(value.unsigned_bits(), T::BITS)
    }
}

/// `MinimumBitWidth<T, false>(value)`.
fn minimum_bit_width_unsigned_t<T: PackInt>(value: T) -> u8 {
    minimum_bit_width_unsigned(value.unsigned_bits(), T::BITS)
}

/// `PackBuffer<T, false>` of `values` into `dst`.
fn pack_values<T: PackInt>(values: &[T], width: u8, dst: &mut [u8]) {
    if T::BITS > 64 {
        let wide: Vec<u128> = values.iter().map(|value| value.unsigned_bits()).collect();
        pack_buffer_u128(&wide, width, dst);
    } else {
        let wide: Vec<u64> = values.iter().map(|value| value.unsigned_bits() as u64).collect();
        pack_buffer_u64(&wide, width, dst);
    }
}

/// A finished metadata group, handed to the writer (`BitpackingWriter`).
#[derive(Debug)]
enum GroupOutput<'a, T: PackInt> {
    Constant {
        constant: T,
        count: usize,
    },
    ConstantDelta {
        constant: T::Signed,
        frame_of_reference: T,
        count: usize,
    },
    DeltaFor {
        values: &'a [T],
        width: u8,
        frame_of_reference: T,
        delta_offset: T::Signed,
        count: usize,
    },
    For {
        values: &'a [T],
        width: u8,
        frame_of_reference: T,
        count: usize,
    },
}

/// The group being collected and the statistics of it (`BitpackingState`).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors DuckDB's BitpackingState")]
struct BitpackingState<T: PackInt> {
    /// The group's values; slots of NULL rows keep stale values like DuckDB's buffer.
    buffer: Vec<T>,
    delta_buffer: Vec<T::Signed>,
    index: usize,
    total_size: u64,
    minimum: T,
    maximum: T,
    min_max_diff: T,
    minimum_delta: T::Signed,
    maximum_delta: T::Signed,
    min_max_delta_diff: T::Signed,
    delta_offset: T::Signed,
    all_valid: bool,
    all_invalid: bool,
    has_valid: bool,
    has_invalid: bool,
    can_do_delta: bool,
    can_do_for: bool,
    /// `force_bitpacking_mode` (always `AUTO`).
    mode: BitpackingMode,
}

impl<T: PackInt> BitpackingState<T> {
    fn new() -> Self {
        let mut state = Self {
            buffer: vec![T::default(); BITPACKING_METADATA_GROUP_SIZE],
            delta_buffer: vec![T::SIGNED_ZERO; BITPACKING_METADATA_GROUP_SIZE],
            index: 0,
            total_size: 0,
            minimum: T::MAX,
            maximum: T::MIN,
            min_max_diff: T::default(),
            minimum_delta: T::SIGNED_MAX,
            maximum_delta: T::SIGNED_MIN,
            min_max_delta_diff: T::SIGNED_ZERO,
            delta_offset: T::SIGNED_ZERO,
            all_valid: true,
            all_invalid: true,
            has_valid: false,
            has_invalid: false,
            can_do_delta: false,
            can_do_for: false,
            mode: BitpackingMode::Auto,
        };
        state.reset();
        state
    }

    /// `Reset`.
    fn reset(&mut self) {
        self.minimum = T::MAX;
        self.minimum_delta = T::SIGNED_MAX;
        self.maximum = T::MIN;
        self.maximum_delta = T::SIGNED_MIN;
        self.delta_offset = T::SIGNED_ZERO;
        self.all_valid = true;
        self.all_invalid = true;
        self.has_valid = false;
        self.has_invalid = false;
        self.can_do_delta = false;
        self.can_do_for = false;
        self.index = 0;
        self.min_max_diff = T::default();
        self.min_max_delta_diff = T::SIGNED_ZERO;
    }

    /// `CalculateFORStats`.
    fn calculate_for_stats(&mut self) {
        if let Some(diff) = self.maximum.checked_sub(self.minimum) {
            self.min_max_diff = diff;
            self.can_do_for = true;
        } else {
            self.can_do_for = false;
        }
    }

    /// `CalculateDeltaStats`.
    fn calculate_delta_stats(&mut self) {
        if self.maximum > T::from_signed(T::SIGNED_MAX) || self.index < 2 || !self.all_valid {
            return;
        }
        let can_do_all = !T::SIGNED
            || (T::signed_checked_sub(self.minimum.to_signed(), self.maximum.to_signed()).is_some()
                && T::signed_checked_sub(self.maximum.to_signed(), self.minimum.to_signed()).is_some());
        // The value before the first one is 0 (`compression_buffer[-1]`).
        let mut previous = T::default().to_signed();
        for i in 0..self.index {
            let current = self.buffer[i].to_signed();
            self.delta_buffer[i] = if can_do_all {
                T::signed_wrapping_sub(current, previous)
            } else {
                let Some(delta) = T::signed_checked_sub(current, previous) else {
                    return;
                };
                delta
            };
            previous = current;
        }
        self.can_do_delta = true;
        for i in 1..self.index {
            self.maximum_delta = self.maximum_delta.max(self.delta_buffer[i]);
            self.minimum_delta = self.minimum_delta.min(self.delta_buffer[i]);
        }
        self.delta_buffer[0] = self.minimum_delta;
        match T::signed_checked_sub(self.maximum_delta, self.minimum_delta) {
            Some(diff) => self.min_max_delta_diff = diff,
            None => self.can_do_delta = false,
        }
        if self.can_do_delta {
            match T::signed_checked_sub(self.buffer[0].to_signed(), self.minimum_delta) {
                Some(offset) => self.delta_offset = offset,
                None => self.can_do_delta = false,
            }
        }
    }

    /// `Flush`: picks the group's mode and hands it to `write`; `false` if the group cannot
    /// be bit packed.
    fn flush(&mut self, mut write: impl FnMut(GroupOutput<'_, T>, &Self) -> Result<()>) -> Result<bool> {
        if self.index == 0 {
            return Ok(true);
        }
        let count = self.index;
        let type_size = T::SIZE as u64;
        if (self.all_invalid || self.maximum == self.minimum)
            && matches!(self.mode, BitpackingMode::Auto | BitpackingMode::Constant)
        {
            write(
                GroupOutput::Constant {
                    constant: self.maximum,
                    count,
                },
                self,
            )?;
            self.total_size += type_size + 4;
            return Ok(true);
        }
        self.calculate_for_stats();
        self.calculate_delta_stats();
        if self.can_do_delta {
            if self.maximum_delta == self.minimum_delta
                && !matches!(self.mode, BitpackingMode::For | BitpackingMode::DeltaFor)
            {
                let frame_of_reference = self.buffer[0];
                write(
                    GroupOutput::ConstantDelta {
                        constant: self.maximum_delta,
                        frame_of_reference,
                        count,
                    },
                    self,
                )?;
                self.total_size += 2 * type_size + 4;
                return Ok(true);
            }
            let delta_width = minimum_bit_width_unsigned_t(T::from_signed(self.min_max_delta_diff));
            let regular_width = minimum_bit_width(self.min_max_diff);
            let prefer_for = self.can_do_for && delta_width >= regular_width;
            if !prefer_for && self.mode != BitpackingMode::For {
                // `SubtractFrameOfReference` on the deltas.
                let minimum_delta = self.minimum_delta;
                let deltas: Vec<T> = self.delta_buffer[..count]
                    .iter()
                    .map(|&delta| T::from_signed(T::signed_wrapping_sub(delta, minimum_delta)))
                    .collect();
                write(
                    GroupOutput::DeltaFor {
                        values: &deltas,
                        width: delta_width,
                        frame_of_reference: T::from_signed(minimum_delta),
                        delta_offset: self.delta_offset,
                        count,
                    },
                    self,
                )?;
                self.total_size += type_size + 8 + type_size + required_size(count, delta_width) as u64;
                return Ok(true);
            }
        }
        if self.can_do_for {
            let width = minimum_bit_width_unsigned_t(self.min_max_diff);
            let minimum = self.minimum;
            // `SubtractFrameOfReference` (unsigned wrapping) over every slot.
            for value in &mut self.buffer[..count] {
                *value = T::from_signed(T::signed_wrapping_sub(value.to_signed(), minimum.to_signed()));
            }
            let values = std::mem::take(&mut self.buffer);
            let result = write(
                GroupOutput::For {
                    values: &values[..count],
                    width,
                    frame_of_reference: minimum,
                    count,
                },
                self,
            );
            self.buffer = values;
            result?;
            self.total_size += required_size(count, width) as u64 + type_size + 8;
            return Ok(true);
        }
        Ok(false)
    }

    /// `Update`: adds a value; a full group is flushed.
    fn update(
        &mut self,
        value: T,
        valid: bool,
        write: impl FnMut(GroupOutput<'_, T>, &Self) -> Result<()>,
    ) -> Result<bool> {
        self.has_valid |= valid;
        self.has_invalid |= !valid;
        self.all_valid &= valid;
        self.all_invalid &= !valid;
        if valid {
            self.buffer[self.index] = value;
            self.minimum = self.minimum.min(value);
            self.maximum = self.maximum.max(value);
        }
        self.index += 1;
        if self.index == BITPACKING_METADATA_GROUP_SIZE {
            let success = self.flush(write)?;
            self.reset();
            return Ok(success);
        }
        Ok(true)
    }
}

/// `BitpackingAnalyzeState`.
#[derive(Debug)]
struct BitpackingAnalyzer<T: PackInt> {
    state: BitpackingState<T>,
    ty: LogicalType,
    info: CompressionInfo,
}

impl<T: PackInt> Analyzer for BitpackingAnalyzer<T> {
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        // Groups of 2048 values may not fit a (small) block: conservatively rule it out.
        let type_size = match input.logical_type().physical_type() {
            PhysicalType::Bool => 1,
            other => other.size(),
        };
        if type_size * BITPACKING_METADATA_GROUP_SIZE * 2 > self.info.block_size {
            return Ok(false);
        }
        for (value, valid) in unified_values::<T>(input, count) {
            if !self.state.update(value, valid, |_, _| Ok(()))? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn final_analyze(&mut self) -> Result<Option<u64>> {
        if !self.state.flush(|_, _| Ok(()))? {
            return Ok(None);
        }
        Ok(Some(self.state.total_size))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let physical = self.ty.physical_type();
        let mut compressor = BitpackingCompressor::<T> {
            state: BitpackingState::new(),
            write_statistics: physical != PhysicalType::List,
            physical,
            ty: self.ty,
            block_size: self.info.block_size,
            buffer: Vec::new(),
            data_offset: 0,
            metadata_offset: 0,
            count: 0,
            stats: None,
        };
        compressor.create_empty_segment();
        Ok(Box::new(compressor))
    }
}

/// `BitpackingCompressionState`: group data grows from the header upwards, the group
/// metadata from the end of the block downwards; a flush moves the metadata next to the
/// data.
#[derive(Debug)]
struct BitpackingCompressor<T: PackInt> {
    state: BitpackingState<T>,
    ty: LogicalType,
    physical: PhysicalType,
    write_statistics: bool,
    block_size: usize,
    buffer: Vec<u8>,
    /// Next free byte for group data (`data_ptr`).
    data_offset: usize,
    /// Start of the metadata written so far (`metadata_ptr`).
    metadata_offset: usize,
    count: u64,
    stats: Option<BaseStatistics>,
}

impl<T: PackInt> BitpackingCompressor<T> {
    /// `CreateEmptySegment`.
    fn create_empty_segment(&mut self) {
        self.buffer = vec![0; self.block_size];
        self.data_offset = BITPACKING_HEADER_SIZE;
        self.metadata_offset = self.block_size;
        self.count = 0;
        self.stats = Some(BaseStatistics::create_empty(self.ty.clone()));
    }

    /// `CanStore`.
    fn can_store(&self, data_bytes: usize, meta_bytes: usize) -> bool {
        let required_data = align8(data_bytes);
        let required_meta = self.block_size - (self.metadata_offset - self.data_offset) + meta_bytes;
        required_data + required_meta <= self.block_size - BITPACKING_HEADER_SIZE
    }

    /// `FlushSegment`.
    fn flush_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let unaligned = self.data_offset;
        let metadata_start = align8(unaligned);
        let metadata_size = self.block_size - self.metadata_offset;
        let total = metadata_start + metadata_size;
        if !self.can_store(0, 0) {
            return Err(Error::internal("Error in bitpacking size calculation"));
        }
        self.buffer[unaligned..metadata_start].fill(0);
        self.buffer
            .copy_within(self.metadata_offset..self.block_size, metadata_start);
        self.buffer[..8].copy_from_slice(&((metadata_start + metadata_size) as u64).to_le_bytes());
        let stats = self
            .stats
            .take()
            .ok_or_else(|| Error::internal("bitpacking segment without statistics"))?;
        sink.flush(FinishedSegment {
            data: self.buffer[..total].to_vec(),
            count: self.count,
            stats,
            compression: CompressionType::BitPacking,
            overflow_blocks: Vec::new(),
        })
    }

    /// `ReserveSpace`: starts a new segment when the group does not fit.
    fn reserve_space(&mut self, sink: &mut SegmentSink<'_>, data_bytes: usize) -> Result<()> {
        if !self.can_store(data_bytes, 4) {
            self.flush_segment(sink)?;
            self.create_empty_segment();
        }
        Ok(())
    }

    /// `WriteMetaData`.
    fn write_metadata(&mut self, mode: BitpackingMode) {
        self.metadata_offset -= 4;
        let encoded = encode_meta(mode, self.data_offset as u32);
        self.buffer[self.metadata_offset..self.metadata_offset + 4].copy_from_slice(&encoded.to_le_bytes());
    }

    /// `WriteData`.
    fn write_data<V: Primitive>(&mut self, value: V) {
        value.store(&mut self.buffer[self.data_offset..]);
        self.data_offset += V::SIZE;
    }

    /// Writes a packed group.
    fn write_packed(&mut self, values: &[T], width: u8, count: usize) {
        let size = required_size(count, width);
        pack_values(
            values,
            width,
            &mut self.buffer[self.data_offset..self.data_offset + size],
        );
        self.data_offset += size;
    }

    /// The writer of a flushed group (`BitpackingWriter`), including `UpdateStats`.
    fn write_group(
        &mut self,
        sink: &mut SegmentSink<'_>,
        output: &GroupOutput<'_, T>,
        state: &BitpackingState<T>,
    ) -> Result<()> {
        let type_size = T::SIZE;
        let count = match *output {
            GroupOutput::Constant { constant, count } => {
                self.reserve_space(sink, type_size)?;
                self.write_metadata(BitpackingMode::Constant);
                self.write_data(constant);
                count
            }
            GroupOutput::ConstantDelta {
                constant,
                frame_of_reference,
                count,
            } => {
                self.reserve_space(sink, 2 * type_size)?;
                self.write_metadata(BitpackingMode::ConstantDelta);
                self.write_data(frame_of_reference);
                self.write_data(T::from_signed(constant));
                count
            }
            GroupOutput::DeltaFor {
                values,
                width,
                frame_of_reference,
                delta_offset,
                count,
            } => {
                self.reserve_space(sink, required_size(count, width) + 3 * type_size)?;
                self.write_metadata(BitpackingMode::DeltaFor);
                self.write_data(frame_of_reference);
                self.write_width(width);
                self.write_data(T::from_signed(delta_offset));
                self.write_packed(values, width, count);
                count
            }
            GroupOutput::For {
                values,
                width,
                frame_of_reference,
                count,
            } => {
                self.reserve_space(sink, required_size(count, width) + 2 * type_size)?;
                self.write_metadata(BitpackingMode::For);
                self.write_data(frame_of_reference);
                self.write_width(width);
                self.write_packed(values, width, count);
                count
            }
        };
        self.count += count as u64;
        if self.write_statistics
            && let Some(stats) = &mut self.stats
        {
            if state.has_valid {
                stats.set_has_no_null_fast();
            }
            if state.has_invalid {
                stats.set_has_null_fast();
            }
            if !state.all_invalid {
                update_numeric_stats(stats, self.physical, state.maximum);
                update_numeric_stats(stats, self.physical, state.minimum);
            }
        }
        Ok(())
    }

    /// `WriteData(static_cast<T>(width))`: the width in a `T` slot.
    fn write_width(&mut self, width: u8) {
        let mut slot = [0u8; 16];
        slot[0] = width;
        self.buffer[self.data_offset..self.data_offset + T::SIZE].copy_from_slice(&slot[..T::SIZE]);
        self.data_offset += T::SIZE;
    }
}

impl<T: PackInt> Compressor for BitpackingCompressor<T> {
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        let mut state = std::mem::replace(&mut self.state, BitpackingState::new());
        let result = unified_values::<T>(input, count).try_for_each(|(value, valid)| {
            state
                .update(value, valid, |output, state| {
                    self.write_group(sink, &output, state)
                })
                .map(|_| ())
        });
        self.state = state;
        result
    }

    /// `BitpackingCompressionState::Finalize`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let mut state = std::mem::replace(&mut self.state, BitpackingState::new());
        let result = state.flush(|output, state| self.write_group(sink, &output, state));
        self.state = state;
        result?;
        self.flush_segment(sink)
    }
}

fn analyzer<T: PackInt>(ty: &LogicalType, info: &CompressionInfo) -> Box<dyn Analyzer> {
    Box::new(BitpackingAnalyzer::<T> {
        state: BitpackingState::new(),
        ty: ty.clone(),
        info: *info,
    })
}

/// `BitpackingFun::GetFunction(type).init_analyze`.
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
        PhysicalType::UInt8 => analyzer::<u8>(ty, info),
        PhysicalType::UInt16 => analyzer::<u16>(ty, info),
        PhysicalType::UInt32 => analyzer::<u32>(ty, info),
        PhysicalType::UInt64 | PhysicalType::List => analyzer::<u64>(ty, info),
        PhysicalType::Int128 => analyzer::<i128>(ty, info),
        PhysicalType::UInt128 => analyzer::<u128>(ty, info),
        _ => return None,
    })
}
