//! ALP for "real doubles" (DuckDB's `compression/alprd/*`).
//!
//! Each value's bits are cut in a left part (dictionary encoded with up to 8 entries per
//! segment, other left parts stored as exceptions) and a right part (bit packed). Values
//! are stored in vectors of [`ALP_VECTOR_SIZE`]; since storage version 7 a vector may be
//! stored uncompressed (exception count [`UNCOMPRESSED_MODE_SENTINEL`]). Segment layout: a
//! `u32` offset of the end of the metadata, the right and left bit widths, the dictionary
//! size and dictionary, the vectors, and one `u32` vector offset per vector growing
//! downwards.

use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_types::{PhysicalType, Vector};

use super::alp::ALP_VECTOR_SIZE;
use super::bitpacking_primitives::{required_size, unpack_buffer_u64};
use super::primitive::{Primitive, corrupt, load, slice, write_values};
use super::{CodecScan, SegmentScan, Sequential};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// `AlpRDConstants::MAX_DICTIONARY_SIZE`.
pub(crate) const MAX_DICTIONARY_SIZE: usize = 8;
/// `AlpRDConstants::HEADER_SIZE`: metadata pointer, bit widths and dictionary size.
pub(crate) const HEADER_SIZE: usize = 4 + 1 + 1 + 1;
/// The exception count marking an uncompressed vector (`UNCOMPRESSED_MODE_SENTINEL`).
pub(crate) const UNCOMPRESSED_MODE_SENTINEL: u16 = u16::MAX;

/// A floating point type stored as its bit pattern (`FloatingToExact`).
pub(crate) trait RdFloat: Primitive {
    /// Bits of the exact (integer) type.
    const BITS: u32;
    fn from_bits_u64(bits: u64) -> Self;
}

impl RdFloat for f32 {
    const BITS: u32 = 32;
    #[inline]
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits as u32)
    }
}

impl RdFloat for f64 {
    const BITS: u32 = 64;
    #[inline]
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits)
    }
}

/// The scan state of an ALPRD segment (DuckDB's `AlpRDScanState`).
#[derive(Debug)]
pub(crate) struct AlpRdScanState<T> {
    data: PersistentData,
    count: usize,
    metadata: usize,
    total_value_count: usize,
    right_bit_width: u8,
    left_bit_width: u8,
    dictionary: [u16; MAX_DICTIONARY_SIZE],
    decoded: Vec<T>,
    index: usize,
    left: Vec<u64>,
    right: Vec<u64>,
    _marker: PhantomData<T>,
}

impl<T: RdFloat> AlpRdScanState<T> {
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

    /// `LoadVector` + `AlpRDDecompression::Decompress`.
    fn load_vector(&mut self, block_size: usize) -> Result<()> {
        self.index = 0;
        self.metadata = self.metadata.wrapping_sub(4);
        let bytes = self.data.bytes();
        let data_offset = load::<u32>(bytes, self.metadata)? as usize;
        if data_offset >= block_size {
            return Err(corrupt(format!(
                "Corrupted ALPRD segment: stored data_byte_offset ({data_offset}) exceeds the segments block size \
                 ({block_size})"
            )));
        }
        let vector_size = ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
        let mut pointer = data_offset;
        let exceptions_count = load::<u16>(bytes, pointer)?;
        pointer += 2;
        self.decoded.clear();
        if exceptions_count == UNCOMPRESSED_MODE_SENTINEL {
            let copy_size = vector_size * T::SIZE;
            if pointer + copy_size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALPRD segment: stored vector_size is invalid, to-copy bytes ({copy_size}) would exceed \
                     bytes remaining in the block ({})",
                    block_size as i64 - pointer as i64
                )));
            }
            let raw = slice(bytes, pointer, copy_size)?;
            self.decoded.extend(raw.chunks_exact(T::SIZE).map(T::load));
            return Ok(());
        }
        let exceptions_count = usize::from(exceptions_count);
        let left_size = required_size(vector_size, self.left_bit_width);
        let right_size = required_size(vector_size, self.right_bit_width);
        let padded = vector_size.next_multiple_of(32);
        self.left.clear();
        self.left.resize(padded, 0);
        self.right.clear();
        self.right.resize(padded, 0);
        // The payload checks of `LoadVector`, against the sizes of DuckDB's vector state
        // buffers and the block (the right parts reuse the left parts' message).
        let mut read_bytes = 0;
        let mut check = |size: usize, max: usize, what: &str| {
            if size > max || data_offset + read_bytes + size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALPRD segment: {what} payload too large"
                )));
            }
            read_bytes += size;
            Ok(())
        };
        check(left_size, ALP_VECTOR_SIZE * 8, "left_encoded")?;
        check(right_size, ALP_VECTOR_SIZE * 8, "left_encoded")?;
        if exceptions_count > 0 {
            check(exceptions_count * 2, ALP_VECTOR_SIZE * 2, "exceptions")?;
            check(exceptions_count * 2, ALP_VECTOR_SIZE * 2, "exceptions_positions")?;
        }
        let left = slice(bytes, pointer, left_size)?;
        unpack_buffer_u64(left, vector_size, self.left_bit_width, &mut self.left);
        pointer += left_size;
        let right = slice(bytes, pointer, right_size)?;
        unpack_buffer_u64(right, vector_size, self.right_bit_width, &mut self.right);
        pointer += right_size;
        let right_bit_width = u32::from(self.right_bit_width);
        let mask = if T::BITS == 64 {
            u64::MAX
        } else {
            (1u64 << T::BITS) - 1
        };
        let combine = |left: u16, right: u64| {
            T::from_bits_u64((u64::from(left).checked_shl(right_bit_width).unwrap_or(0) | right) & mask)
        };
        for row in 0..vector_size {
            let left = self
                .dictionary
                .get(self.left[row] as usize)
                .copied()
                .unwrap_or_default();
            self.decoded.push(combine(left, self.right[row]));
        }
        if exceptions_count > 0 {
            let exceptions = slice(bytes, pointer, exceptions_count * 2)?;
            let positions = slice(bytes, pointer + exceptions_count * 2, exceptions_count * 2)?;
            for (exception, position) in exceptions
                .as_chunks::<2>()
                .0
                .iter()
                .zip(positions.as_chunks::<2>().0)
            {
                let left = u16::from_le_bytes([exception[0], exception[1]]);
                let position = usize::from(u16::from_le_bytes([position[0], position[1]]));
                if position >= vector_size {
                    return Err(corrupt(format!(
                        "Corrupted ALPRD segment: exception position ({position}) exceeds vector_size ({vector_size})"
                    )));
                }
                self.decoded[position] = combine(left, self.right[position]);
            }
        }
        Ok(())
    }

    /// `ScanVector`.
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
                .ok_or_else(|| corrupt("Corrupted ALPRD segment: vector shorter than its count"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: RdFloat> CodecScan for AlpRdScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let block_size = segment.block_size();
        let metadata = load::<u32>(bytes, 0)? as usize;
        if segment.block_offset() + metadata > block_size {
            return Err(corrupt(
                "Corrupted ALPRD segment: metadata_offset value is corrupted",
            ));
        }
        if segment.block_offset() + 4 + HEADER_SIZE > block_size {
            return Err(corrupt(
                "Corrupted ALPRD segment: reading header bytes would exceed block space",
            ));
        }
        let right_bit_width: u8 = load(bytes, 4)?;
        let left_bit_width: u8 = load(bytes, 5)?;
        let dictionary_size = usize::from(load::<u8>(bytes, 6)?);
        if dictionary_size > MAX_DICTIONARY_SIZE {
            return Err(corrupt(
                "Corrupt database file: ALPRD dictionary size exceeds maximum",
            ));
        }
        if 4 + HEADER_SIZE + dictionary_size * 2 > metadata {
            return Err(corrupt(
                "Corrupted ALPRD segment: actual_dictionary_size is corrupted",
            ));
        }
        let mut dictionary = [0u16; MAX_DICTIONARY_SIZE];
        for (index, entry) in dictionary.iter_mut().enumerate().take(dictionary_size) {
            *entry = load(bytes, HEADER_SIZE + index * 2)?;
        }
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            right_bit_width,
            left_bit_width,
            dictionary,
            decoded: Vec::with_capacity(ALP_VECTOR_SIZE),
            index: 0,
            left: Vec::new(),
            right: Vec::new(),
            _marker: PhantomData,
        })
    }

    /// `AlpRDScanState::Skip`.
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

    /// `AlpRDScanPartial`.
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

/// `AlpRDCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<AlpRdScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<AlpRdScanState<f64>>::boxed(segment),
        _ => return None,
    })
}
