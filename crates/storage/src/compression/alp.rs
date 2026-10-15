//! Adaptive Lossless floating-Point compression (DuckDB's `compression/alp/*`).
//!
//! Values are compressed in vectors of [`ALP_VECTOR_SIZE`]: each value is multiplied by
//! `10^exponent / 10^factor` and rounded to an integer, the integers are stored with a
//! frame of reference and bit packing, and values that do not round-trip are stored as
//! exceptions. Since storage version 7 a vector may also be stored uncompressed (exponent
//! [`UNCOMPRESSED_MODE_SENTINEL`]). Segment layout: a `u32` offset of the end of the
//! metadata, the vectors, and one `u32` vector offset per vector growing downwards.

use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_types::{PhysicalType, Vector};

use super::bitpacking_primitives::{required_size, unpack_buffer_u64};
use super::primitive::{Primitive, corrupt, load, slice, write_values};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// Values per ALP vector (`AlpConstants::ALP_VECTOR_SIZE`).
pub(crate) const ALP_VECTOR_SIZE: usize = 1024;
/// `AlpConstants::HEADER_SIZE`.
#[allow(dead_code, reason = "used by the checkpoint writer")]
pub(crate) const HEADER_SIZE: usize = 4;
/// The exponent marking an uncompressed vector (`UNCOMPRESSED_MODE_SENTINEL`).
pub(crate) const UNCOMPRESSED_MODE_SENTINEL: u8 = u8::MAX;
/// `AlpConstants::FACT_ARR`.
pub(crate) const FACT_ARR: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// A floating point type ALP can compress (`AlpTypedConstants<T>` and the typed parts of
/// `AlpCompression` / `AlpDecompression`).
#[allow(dead_code, reason = "used by the checkpoint writer")]
pub(crate) trait AlpFloat: Primitive + PartialOrd {
    /// The integer with the same bits (`FloatingToExact`).
    type Exact: Primitive;
    /// `MAGIC_NUMBER` (2^22 + 2^23 for floats, 2^51 + 2^52 for doubles).
    const MAGIC_NUMBER: Self;
    /// `MAX_EXPONENT`.
    const MAX_EXPONENT: u8;
    fn exp(index: usize) -> Self;
    fn frac(index: usize) -> Self;
    fn from_i64(value: i64) -> Self;
    /// A truncating cast to `i64` (DuckDB's `LossyNumericCast<int64_t>`).
    fn to_i64(self) -> i64;
    fn from_exact(exact: Self::Exact) -> Self;
    fn to_exact(self) -> Self::Exact;
    fn mul(self, other: Self) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn is_finite(self) -> bool;
    fn is_negative_zero(self) -> bool;
    fn to_f64(self) -> f64;
}

/// `AlpTypedConstants<float>::EXP_ARR`.
#[allow(dead_code, reason = "used by the checkpoint writer")]
const EXP_ARR_F32: [f32; 11] = [
    1.0,
    10.0,
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    10_000_000_000.0,
];
/// `AlpTypedConstants<float>::FRAC_ARR`.
const FRAC_ARR_F32: [f32; 11] = [
    1.0,
    0.1,
    0.01,
    0.001,
    0.0001,
    0.00001,
    0.000_001,
    0.000_000_1,
    0.000_000_01,
    0.000_000_001,
    0.000_000_000_1,
];
/// `AlpTypedConstants<double>::EXP_ARR`.
#[allow(dead_code, reason = "used by the checkpoint writer")]
const EXP_ARR_F64: [f64; 24] = [
    1.0,
    10.0,
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    10_000_000_000.0,
    100_000_000_000.0,
    1_000_000_000_000.0,
    10_000_000_000_000.0,
    100_000_000_000_000.0,
    1_000_000_000_000_000.0,
    10_000_000_000_000_000.0,
    100_000_000_000_000_000.0,
    1_000_000_000_000_000_000.0,
    10_000_000_000_000_000_000.0,
    100_000_000_000_000_000_000.0,
    1_000_000_000_000_000_000_000.0,
    10_000_000_000_000_000_000_000.0,
    100_000_000_000_000_000_000_000.0,
];
/// `AlpTypedConstants<double>::FRAC_ARR`.
const FRAC_ARR_F64: [f64; 21] = [
    1.0,
    0.1,
    0.01,
    0.001,
    0.0001,
    0.00001,
    0.000_001,
    0.000_000_1,
    0.000_000_01,
    0.000_000_001,
    0.000_000_000_1,
    0.000_000_000_01,
    0.000_000_000_001,
    0.000_000_000_000_1,
    0.000_000_000_000_01,
    0.000_000_000_000_001,
    0.000_000_000_000_000_1,
    0.000_000_000_000_000_01,
    0.000_000_000_000_000_001,
    0.000_000_000_000_000_000_1,
    0.000_000_000_000_000_000_01,
];

impl AlpFloat for f32 {
    type Exact = u32;
    const MAGIC_NUMBER: Self = 12_582_912.0;
    const MAX_EXPONENT: u8 = 10;
    #[inline]
    fn exp(index: usize) -> Self {
        EXP_ARR_F32.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn frac(index: usize) -> Self {
        FRAC_ARR_F32.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn from_i64(value: i64) -> Self {
        value as Self
    }
    #[inline]
    fn to_i64(self) -> i64 {
        self as i64
    }
    #[inline]
    fn from_exact(exact: u32) -> Self {
        Self::from_bits(exact)
    }
    #[inline]
    fn to_exact(self) -> u32 {
        self.to_bits()
    }
    #[inline]
    fn mul(self, other: Self) -> Self {
        self * other
    }
    #[inline]
    fn add(self, other: Self) -> Self {
        self + other
    }
    #[inline]
    fn sub(self, other: Self) -> Self {
        self - other
    }
    #[inline]
    fn is_finite(self) -> bool {
        Self::is_finite(self)
    }
    #[inline]
    fn is_negative_zero(self) -> bool {
        self == 0.0 && self.is_sign_negative()
    }
    #[inline]
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
}

impl AlpFloat for f64 {
    type Exact = u64;
    const MAGIC_NUMBER: Self = 6_755_399_441_055_744.0;
    const MAX_EXPONENT: u8 = 18;
    #[inline]
    fn exp(index: usize) -> Self {
        EXP_ARR_F64.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn frac(index: usize) -> Self {
        FRAC_ARR_F64.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn from_i64(value: i64) -> Self {
        value as Self
    }
    #[inline]
    fn to_i64(self) -> i64 {
        self as i64
    }
    #[inline]
    fn from_exact(exact: u64) -> Self {
        Self::from_bits(exact)
    }
    #[inline]
    fn to_exact(self) -> u64 {
        self.to_bits()
    }
    #[inline]
    fn mul(self, other: Self) -> Self {
        self * other
    }
    #[inline]
    fn add(self, other: Self) -> Self {
        self + other
    }
    #[inline]
    fn sub(self, other: Self) -> Self {
        self - other
    }
    #[inline]
    fn is_finite(self) -> bool {
        Self::is_finite(self)
    }
    #[inline]
    fn is_negative_zero(self) -> bool {
        self == 0.0 && self.is_sign_negative()
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self
    }
}

/// `AlpCompression::DecodeValue`.
#[inline]
pub(crate) fn decode_value<T: AlpFloat>(encoded: i64, exponent: u8, factor: u8) -> T {
    let fact = FACT_ARR.get(usize::from(factor)).copied().unwrap_or(1);
    T::from_i64(encoded)
        .mul(T::from_i64(fact))
        .mul(T::frac(usize::from(exponent)))
}

/// The scan state of an ALP segment (DuckDB's `AlpScanState`).
#[derive(Debug)]
pub(crate) struct AlpScanState<T> {
    data: PersistentData,
    count: usize,
    /// Offset of the next vector's metadata entry (`metadata_ptr`).
    metadata: usize,
    total_value_count: usize,
    /// The decoded current vector and the read position in it.
    decoded: Vec<T>,
    index: usize,
    unpacked: Vec<u64>,
    _marker: PhantomData<T>,
}

impl<T: AlpFloat> AlpScanState<T> {
    fn left_in_vector(&self) -> usize {
        ALP_VECTOR_SIZE - self.total_value_count % ALP_VECTOR_SIZE
    }

    fn vector_finished(&self) -> bool {
        self.total_value_count.is_multiple_of(ALP_VECTOR_SIZE)
    }

    /// `SkipVector`.
    fn skip_vector(&mut self) {
        self.metadata = self.metadata.wrapping_sub(4);
        self.total_value_count += ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
    }

    /// `LoadVector`: decodes the next vector into `self.decoded`.
    fn load_vector(&mut self, block_size: usize) -> Result<()> {
        self.index = 0;
        self.metadata = self.metadata.wrapping_sub(4);
        let bytes = self.data.bytes();
        let data_offset = load::<u32>(bytes, self.metadata)? as usize;
        if data_offset >= block_size {
            return Err(corrupt(format!(
                "Corrupted ALP segment: stored data_byte_offset ({data_offset}) exceeds the segments block size \
                 ({block_size})"
            )));
        }
        let vector_size = ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
        let mut pointer = data_offset;
        let exponent: u8 = load(bytes, pointer)?;
        pointer += 1;
        self.decoded.clear();
        if exponent == UNCOMPRESSED_MODE_SENTINEL {
            let copy_size = vector_size * T::SIZE;
            if pointer + copy_size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: stored vector_size is invalid, to-copy bytes ({copy_size}) would exceed \
                     bytes remaining in the block ({})",
                    block_size as i64 - pointer as i64
                )));
            }
            let raw = slice(bytes, pointer, copy_size)?;
            self.decoded.extend(raw.chunks_exact(T::SIZE).map(T::load));
            return Ok(());
        }
        let factor: u8 = load(bytes, pointer)?;
        let exceptions_count = usize::from(load::<u16>(bytes, pointer + 1)?);
        let frame_of_reference: u64 = load(bytes, pointer + 3)?;
        let bit_width: u8 = load(bytes, pointer + 11)?;
        pointer += 12;
        if exceptions_count > vector_size {
            return Err(corrupt(format!(
                "Corrupted ALP segment: exceptions_count ({exceptions_count}) exceeds vector_size ({vector_size})"
            )));
        }
        if factor > exponent {
            return Err(corrupt(format!(
                "Corrupted ALP segment: v_factor ({factor}) exceeds v_exponent ({exponent})"
            )));
        }
        if bit_width > 64 {
            return Err(corrupt(format!(
                "Corrupted ALP segment: Invalid bit_width encountered: {bit_width}"
            )));
        }
        // The payload checks of `LoadVector`, against the sizes of DuckDB's vector state
        // buffers and the block.
        let mut read_bytes = 0;
        let mut check = |size: usize, max: usize, what: &str| {
            if size > max || data_offset + read_bytes + size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: {what} payload too large"
                )));
            }
            read_bytes += size;
            Ok(())
        };
        let packed_size = if bit_width > 0 {
            required_size(vector_size, bit_width)
        } else {
            0
        };
        check(packed_size, ALP_VECTOR_SIZE * 8, "encoded")?;
        let exceptions_size = exceptions_count * T::SIZE;
        let positions_size = exceptions_count * 2;
        if exceptions_count > 0 {
            check(exceptions_size, ALP_VECTOR_SIZE * T::SIZE, "exceptions")?;
            check(positions_size, ALP_VECTOR_SIZE * 2, "exceptions_positions")?;
        }
        let positions = slice(bytes, pointer + packed_size + exceptions_size, positions_size)?;
        for position in positions.as_chunks::<2>().0 {
            let position = u16::from_le_bytes([position[0], position[1]]);
            if usize::from(position) >= vector_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: exception position ({position}) exceeds vector_size ({vector_size})"
                )));
            }
        }
        // `AlpDecompression::Decompress`: unpack, add the frame of reference, decode, patch.
        self.unpacked.clear();
        self.unpacked.resize(vector_size.next_multiple_of(32), 0);
        if bit_width > 0 {
            unpack_buffer_u64(
                slice(bytes, pointer, packed_size)?,
                vector_size,
                bit_width,
                &mut self.unpacked,
            );
        }
        pointer += packed_size;
        self.decoded
            .extend(self.unpacked[..vector_size].iter().map(|&value| {
                decode_value::<T>(value.wrapping_add(frame_of_reference) as i64, exponent, factor)
            }));
        let exceptions = slice(bytes, pointer, exceptions_size)?;
        for (exception, position) in exceptions.chunks_exact(T::SIZE).zip(positions.as_chunks::<2>().0) {
            let position = usize::from(u16::from_le_bytes([position[0], position[1]]));
            self.decoded[position] = T::load(exception);
        }
        Ok(())
    }

    /// `ScanVector`: reads (or skips) `count` values up to the vector boundary.
    fn scan_vector(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        out: Option<(&mut Vector, usize)>,
    ) -> Result<()> {
        if self.vector_finished() && self.total_value_count < self.count {
            self.load_vector(segment.block_size())?;
        }
        if let Some((result, offset)) = out {
            let values = self
                .decoded
                .get(self.index..self.index + count)
                .ok_or_else(|| corrupt("Corrupted ALP segment: vector shorter than its count"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: AlpFloat> CodecScan for AlpScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let metadata = load::<u32>(data.bytes(), 0)? as usize;
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            decoded: Vec::with_capacity(ALP_VECTOR_SIZE),
            index: 0,
            unpacked: Vec::with_capacity(ALP_VECTOR_SIZE),
            _marker: PhantomData,
        })
    }

    /// `AlpScanState::Skip`.
    fn skip(&mut self, segment: &ColumnSegment, mut skip_count: usize) -> Result<()> {
        if self.total_value_count != 0 && !self.vector_finished() {
            let to_skip = skip_count.min(self.left_in_vector());
            self.scan_vector(segment, to_skip, None)?;
            skip_count -= to_skip;
        }
        for _ in 0..skip_count / ALP_VECTOR_SIZE {
            self.skip_vector();
        }
        skip_count %= ALP_VECTOR_SIZE;
        if skip_count > 0 {
            self.scan_vector(segment, skip_count, None)?;
        }
        Ok(())
    }

    /// `AlpScanPartial`.
    fn scan_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let to_scan = (count - scanned).min(self.left_in_vector());
            self.scan_vector(segment, to_scan, Some((&mut *result, result_offset + scanned)))?;
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `AlpCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<AlpScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<AlpScanState<f64>>::boxed(segment),
        _ => return None,
    })
}
