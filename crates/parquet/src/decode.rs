//! Low-level decoding (DuckDB's `ByteBuffer`, `ParquetDecodeUtils`, `RleBpDecoder`,
//! `DbpDecoder` and `BssDecoder`): a bounds-checked byte cursor, varints, bit unpacking,
//! the RLE/bit-packing hybrid, `DELTA_BINARY_PACKED` and `BYTE_STREAM_SPLIT`.

use roastedduck_common::{Error, ErrorKind, Result};

/// A `std::runtime_error` of the C++ reader, reported as an `Invalid Error`.
pub(crate) fn runtime_error(message: impl Into<roastedduck_common::ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Invalid, message.into())
}

/// `ByteBuffer::available` failing.
pub(crate) fn out_of_buffer() -> Error {
    runtime_error("Out of buffer")
}

/// A forward cursor over bytes (DuckDB's `ByteBuffer`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ByteBuffer<'a> {
    data: &'a [u8],
}

impl<'a> ByteBuffer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// The remaining bytes.
    #[inline]
    pub fn remaining(&self) -> &'a [u8] {
        self.data
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `ByteBuffer::check_available`.
    #[inline]
    pub fn check_available(&self, len: usize) -> bool {
        len <= self.data.len()
    }

    /// `ByteBuffer::available`.
    #[inline]
    pub fn available(&self, len: usize) -> Result<()> {
        if self.check_available(len) {
            Ok(())
        } else {
            Err(out_of_buffer())
        }
    }

    /// `ByteBuffer::inc`.
    #[inline]
    pub fn inc(&mut self, len: usize) -> Result<()> {
        self.available(len)?;
        self.data = &self.data[len..];
        Ok(())
    }

    /// Takes the next `len` bytes.
    #[inline]
    pub fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        self.available(len)?;
        let (head, tail) = self.data.split_at(len);
        self.data = tail;
        Ok(head)
    }

    #[inline]
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    #[inline]
    pub fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `ParquetDecodeUtils::VarintDecode<uint64_t>`.
    pub fn read_varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            if shift < 64 {
                result |= u64::from(byte & 0x7f) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 64 {
                return Err(runtime_error("Varint-decoding found too large number"));
            }
        }
    }

    /// `VarintDecode<uint32_t>`.
    pub fn read_varint_u32(&mut self) -> Result<u32> {
        let mut result: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            if shift < 32 {
                result |= u32::from(byte & 0x7f) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 32 {
                return Err(runtime_error("Varint-decoding found too large number"));
            }
        }
    }
}

/// A decompressed page; vectors whose strings point into it keep it alive.
pub type PageBlock = std::sync::Arc<Vec<u8>>;

/// A position inside a shared page block, for decoders that outlive one call.
#[derive(Clone, Debug, Default)]
pub struct BlockCursor {
    block: PageBlock,
    position: usize,
    end: usize,
}

impl BlockCursor {
    /// The bytes `range` of `block`.
    pub fn new(block: PageBlock, range: std::ops::Range<usize>) -> Self {
        let end = range.end.min(block.len());
        Self {
            block,
            position: range.start.min(end),
            end,
        }
    }

    /// The unread bytes.
    #[inline]
    pub fn remaining(&self) -> &[u8] {
        &self.block[self.position..self.end]
    }

    /// The offset of the next unread byte in the block.
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn block(&self) -> &PageBlock {
        &self.block
    }

    /// A buffer over the unread bytes; [`Self::consume`] the bytes it read afterwards.
    #[inline]
    pub fn buffer(&self) -> ByteBuffer<'_> {
        ByteBuffer::new(self.remaining())
    }

    /// Marks `len` more bytes as read.
    #[inline]
    pub fn consume(&mut self, len: usize) {
        self.position = (self.position + len).min(self.end);
    }
}

/// `ParquetDecodeUtils::ZigzagToInt`.
#[inline]
pub fn zigzag_to_int(value: u64) -> i64 {
    (value >> 1) as i64 ^ -((value & 1) as i64)
}

/// `ParquetDecodeUtils::IntToZigzag`.
#[inline]
pub fn int_to_zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// The number of bits needed to store `value` (`RleBpDecoder::ComputeBitWidth`).
pub fn compute_bit_width(value: u64) -> u8 {
    (64 - value.leading_zeros()) as u8
}

/// The widest bit-packed value (`BITPACK_MASKS_SIZE - 1`).
const MAX_BIT_WIDTH: u8 = 64;

/// An integer type bit-packed values are unpacked into.
pub trait UnpackTarget: Copy + Default {
    const BITS: u8;
    fn from_u64(value: u64) -> Self;
}

macro_rules! unpack_target {
    ($($ty:ty),*) => {$(
        impl UnpackTarget for $ty {
            const BITS: u8 = <$ty>::BITS as u8;
            #[inline]
            fn from_u64(value: u64) -> Self {
                value as $ty
            }
        }
    )*};
}
unpack_target!(u8, u16, u32, u64);

/// `ParquetDecodeUtils::CheckWidth` and the target-width check of `BitUnpack`.
fn check_unpack_width<T: UnpackTarget>(width: u8) -> Result<()> {
    if width > MAX_BIT_WIDTH {
        return Err(Error::invalid_input(format!(
            "The width ({width}) of the bitpacked data exceeds the supported max width (65), the file might be corrupted."
        )));
    }
    if width > T::BITS {
        return Err(Error::new(
            ErrorKind::Io,
            format!(
                "The width ({width}) of the bitpacked data exceeds the maximum width ({}) for the target type, the file might be corrupted.",
                T::BITS
            ),
        ));
    }
    Ok(())
}

/// Reads up to 8 little-endian bytes at `offset`, zero-padding past the end of `data`.
#[inline]
fn load_u64(data: &[u8], offset: usize) -> u64 {
    if let Some(bytes) = data.get(offset..offset + 8) {
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        return u64::from_le_bytes(array);
    }
    let mut array = [0; 8];
    if offset < data.len() {
        let available = data.len() - offset;
        array[..available].copy_from_slice(&data[offset..]);
    }
    u64::from_le_bytes(array)
}

/// Unpacks `dst.len()` values of `width` bits starting at bit `bit_offset` of `data`
/// (least significant bit first).
#[inline]
pub fn unpack_bits<T: UnpackTarget>(data: &[u8], bit_offset: usize, dst: &mut [T], width: u8) {
    if width == 0 {
        dst.fill(T::default());
        return;
    }
    let width = usize::from(width);
    // With 8 readable bytes past the last value, every value is one unaligned load.
    if width <= 32 && (bit_offset + dst.len() * width) / 8 + 8 <= data.len() {
        unpack_bits_padded(data, bit_offset, dst, width);
        return;
    }
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let mut bit = bit_offset;
    if width <= 56 {
        for value in dst {
            let word = load_u64(data, bit / 8);
            *value = T::from_u64((word >> (bit % 8)) & mask);
            bit += width;
        }
    } else {
        for value in dst {
            let shift = bit % 8;
            let low = load_u64(data, bit / 8) >> shift;
            let high = if shift == 0 {
                0
            } else {
                u64::from(data.get(bit / 8 + 8).copied().unwrap_or(0)) << (64 - shift)
            };
            *value = T::from_u64((low | high) & mask);
            bit += width;
        }
    }
}

/// [`unpack_bits`] for `width <= 32` when `data` holds 8 bytes past the last value: one
/// loop per width, so shifts and masks are constants.
fn unpack_bits_padded<T: UnpackTarget>(data: &[u8], bit_offset: usize, dst: &mut [T], width: usize) {
    macro_rules! dispatch {
        ($($width:literal)*) => {
            match width {
                $($width => unpack_bits_width::<T, $width>(data, bit_offset, dst),)*
                _ => debug_assert!(false, "unpack width {width} out of range"),
            }
        };
    }
    dispatch!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32);
}

#[inline]
fn unpack_bits_width<T: UnpackTarget, const WIDTH: usize>(data: &[u8], bit_offset: usize, dst: &mut [T]) {
    let mask = (1u64 << WIDTH) - 1;
    let last_byte = (bit_offset + dst.len() * WIDTH) / 8;
    if last_byte + 8 > data.len() {
        return;
    }
    let base = data.as_ptr();
    let mut bit = bit_offset;
    for value in dst {
        // SAFETY: `bit / 8 <= last_byte` and `last_byte + 8 <= data.len()` (checked
        // above), so the 8 bytes read are in bounds.
        let word = u64::from_le(unsafe { base.add(bit / 8).cast::<u64>().read_unaligned() });
        *value = T::from_u64((word >> (bit % 8)) & mask);
        bit += WIDTH;
    }
}

/// The RLE/bit-packing hybrid decoder (`RleBpDecoder`) of definition/repetition levels,
/// dictionary indices and RLE booleans.
#[derive(Debug, Clone)]
pub struct RleBpDecoder {
    cursor: BlockCursor,
    bit_width: u8,
    byte_encoded_len: usize,
    max_value: u64,
    current_value: u64,
    repeat_count: u32,
    literal_count: u32,
    /// Bit position inside the current literal run (relative to the cursor).
    bit_position: usize,
}

impl RleBpDecoder {
    pub fn new(cursor: BlockCursor, bit_width: u8) -> Result<Self> {
        if bit_width >= 64 {
            return Err(runtime_error("Decode bit width too large"));
        }
        Ok(Self {
            cursor,
            bit_width,
            byte_encoded_len: usize::from(bit_width).div_ceil(8),
            max_value: (1u64 << bit_width) - 1,
            current_value: 0,
            repeat_count: 0,
            literal_count: 0,
            bit_position: 0,
        })
    }

    /// Reads the header of the next run (`NextCounts`).
    fn next_counts(&mut self) -> Result<()> {
        if self.bit_position != 0 {
            self.cursor.buffer().available(1)?;
            self.cursor.consume(1);
            self.bit_position = 0;
        }
        let mut buffer = self.cursor.buffer();
        let before = buffer.len();
        let indicator = buffer.read_varint_u32()?;
        if indicator & 1 == 1 {
            self.literal_count = (indicator >> 1).wrapping_mul(8);
        } else {
            self.repeat_count = indicator >> 1;
            let bytes = buffer.take(self.byte_encoded_len)?;
            self.current_value = bytes
                .iter()
                .enumerate()
                .fold(0, |value, (i, &byte)| value | (u64::from(byte) << (i * 8)));
            if self.repeat_count > 0 && self.current_value > self.max_value {
                return Err(runtime_error(
                    "Payload value bigger than allowed. Corrupted file?",
                ));
            }
        }
        let consumed = before - buffer.len();
        self.cursor.consume(consumed);
        Ok(())
    }

    /// Whether the next `batch_size` values are all `value` (`HasRepeatedBatch`).
    pub fn has_repeated_batch(&mut self, batch_size: u32, value: u64) -> Result<bool> {
        if self.repeat_count == 0 && self.literal_count == 0 {
            self.next_counts()?;
        }
        Ok(self.repeat_count >= batch_size && self.current_value == value)
    }

    /// Consumes a batch checked by [`Self::has_repeated_batch`].
    pub fn skip_repeated_batch(&mut self, batch_size: u32) {
        self.repeat_count -= batch_size;
    }

    /// Bit-unpacks `count` literal values, checking like `BitUnpack`.
    fn unpack_literals<T: UnpackTarget>(&mut self, out: Option<&mut [T]>, count: usize) -> Result<()> {
        check_unpack_width::<T>(self.bit_width)?;
        let width = usize::from(self.bit_width);
        let bits = self.bit_position + count * width;
        let remaining = self.cursor.remaining();
        // `src.available(count * width / BITPACK_DLEN)`: only whole bytes are checked.
        if count * width / 8 > remaining.len() {
            return Err(out_of_buffer());
        }
        if let Some(out) = out {
            unpack_bits(remaining, self.bit_position, out, self.bit_width);
        }
        // Keep the sub-byte position; whole bytes are consumed.
        let whole = (bits / 8).min(remaining.len());
        self.cursor.consume(whole);
        self.bit_position = bits - whole * 8;
        Ok(())
    }

    /// Decodes the next `out.len()` values (`GetBatch`).
    pub fn get_batch<T: UnpackTarget>(&mut self, out: &mut [T]) -> Result<()> {
        let mut read = 0;
        while read < out.len() {
            if self.repeat_count > 0 {
                let batch = (out.len() - read).min(self.repeat_count as usize);
                out[read..read + batch].fill(T::from_u64(self.current_value));
                self.repeat_count -= batch as u32;
                read += batch;
            } else if self.literal_count > 0 {
                let batch = (out.len() - read).min(self.literal_count as usize);
                self.unpack_literals(Some(&mut out[read..read + batch]), batch)?;
                self.literal_count -= batch as u32;
                read += batch;
            } else {
                self.next_counts()?;
            }
        }
        Ok(())
    }

    /// Skips `count` values (`Skip`).
    pub fn skip(&mut self, count: usize) -> Result<()> {
        let mut skipped = 0;
        while skipped < count {
            if self.repeat_count > 0 {
                let batch = (count - skipped).min(self.repeat_count as usize);
                self.repeat_count -= batch as u32;
                skipped += batch;
            } else if self.literal_count > 0 {
                let batch = (count - skipped).min(self.literal_count as usize);
                self.unpack_literals::<u64>(None, batch)?;
                self.literal_count -= batch as u32;
                skipped += batch;
            } else {
                self.next_counts()?;
            }
        }
        Ok(())
    }
}

/// The number of values per bit-packing group (`BITPACKING_ALGORITHM_GROUP_SIZE`).
const GROUP_SIZE: usize = 32;

/// The `DELTA_BINARY_PACKED` decoder (`DbpDecoder`).
#[derive(Debug)]
pub struct DbpDecoder {
    cursor: BlockCursor,
    miniblocks_per_block: usize,
    values_per_miniblock: usize,
    total_values: u64,
    previous_value: i64,
    is_first_value: bool,
    read_values: u64,
    min_delta: i64,
    miniblock_index: usize,
    /// The bit widths of the miniblocks of the current block.
    bit_widths: Vec<u8>,
    miniblock_offset: usize,
    unpacked: [u64; GROUP_SIZE],
    unpacked_offset: usize,
}

impl DbpDecoder {
    pub fn new(mut cursor: BlockCursor) -> Result<Self> {
        let mut buffer = cursor.buffer();
        let before = buffer.len();
        let block_size = buffer.read_varint()?;
        let miniblocks_per_block = buffer.read_varint()?;
        if miniblocks_per_block == 0 {
            return Err(Error::invalid_input(
                "Parquet file has invalid number of miniblocks per block for DELTA_BINARY_PACKED",
            ));
        }
        let values_per_miniblock = block_size / miniblocks_per_block;
        let total_values = buffer.read_varint()?;
        let previous_value = zigzag_to_int(buffer.read_varint()?);
        let consumed = before - buffer.len();
        cursor.consume(consumed);
        if !(block_size % miniblocks_per_block == 0 && values_per_miniblock % GROUP_SIZE as u64 == 0) {
            return Err(Error::invalid_input(
                "Parquet file has invalid block sizes for DELTA_BINARY_PACKED",
            ));
        }
        let miniblocks_per_block = usize::try_from(miniblocks_per_block).map_err(|_| out_of_buffer())?;
        let values_per_miniblock = usize::try_from(values_per_miniblock).map_err(|_| out_of_buffer())?;
        Ok(Self {
            cursor,
            miniblocks_per_block,
            values_per_miniblock,
            total_values,
            previous_value,
            is_first_value: true,
            read_values: 0,
            min_delta: i64::MAX,
            miniblock_index: miniblocks_per_block - 1,
            bit_widths: Vec::new(),
            miniblock_offset: values_per_miniblock,
            unpacked: [0; GROUP_SIZE],
            unpacked_offset: GROUP_SIZE,
        })
    }

    /// The number of values in the stream (`TotalValues`).
    pub fn total_values(&self) -> u64 {
        self.total_values
    }

    /// The block offset after the values decoded so far (`BufferPtr`).
    pub fn position(&self) -> usize {
        self.cursor.position()
    }

    /// Decodes the next `out.len()` values (`GetBatch`); values wrap like the C++ casts.
    pub fn get_batch<T: DbpTarget>(&mut self, out: &mut [T]) -> Result<()> {
        if self.read_values + out.len() as u64 > self.total_values {
            return Err(runtime_error("DBP decode did not find enough values"));
        }
        self.read_values += out.len() as u64;
        self.decode(Some(out), 0)
    }

    /// Skips `count` values (`Skip`).
    pub fn skip<T: DbpTarget>(&mut self, count: usize) -> Result<()> {
        if self.read_values + count as u64 > self.total_values {
            return Err(runtime_error("DBP decode did not find enough values"));
        }
        self.read_values += count as u64;
        self.decode::<T>(None, count)
    }

    /// Consumes the rest of the current miniblock (`Finalize`).
    pub fn finalize(&mut self) -> Result<()> {
        if self.miniblock_offset == self.values_per_miniblock {
            return Ok(());
        }
        let remaining = self.values_per_miniblock - self.miniblock_offset;
        self.decode::<i64>(None, remaining)
    }

    /// `GetBatchInternal`: `out` is `None` when skipping `skip_count` values.
    fn decode<T: DbpTarget>(&mut self, mut out: Option<&mut [T]>, skip_count: usize) -> Result<()> {
        let batch_size = out.as_ref().map_or(skip_count, |out| out.len());
        if batch_size == 0 {
            return Ok(());
        }
        let mut offset = 0;
        if self.is_first_value {
            if let Some(out) = out.as_deref_mut() {
                out[0] = T::from_i64(self.previous_value);
            }
            offset += 1;
            self.is_first_value = false;
        }
        while offset < batch_size {
            let next = (batch_size - offset).min(GROUP_SIZE - self.unpacked_offset);
            if next != 0 {
                for i in 0..next {
                    let unpacked = self.unpacked[self.unpacked_offset + i];
                    let value = T::wrap(
                        (self.previous_value as u64)
                            .wrapping_add(self.min_delta as u64)
                            .wrapping_add(unpacked),
                    );
                    if let Some(out) = out.as_deref_mut() {
                        out[offset + i] = value;
                    }
                    self.previous_value = value.to_i64();
                }
                offset += next;
                self.unpacked_offset += next;
                continue;
            }
            if self.miniblock_offset == self.values_per_miniblock {
                self.miniblock_offset = 0;
                self.miniblock_index += 1;
                if self.miniblock_index == self.miniblocks_per_block {
                    let mut buffer = self.cursor.buffer();
                    let before = buffer.len();
                    self.min_delta = zigzag_to_int(buffer.read_varint()?);
                    let widths = buffer.take(self.miniblocks_per_block)?;
                    self.bit_widths.clear();
                    self.bit_widths.extend_from_slice(widths);
                    let consumed = before - buffer.len();
                    self.cursor.consume(consumed);
                    self.miniblock_index = 0;
                }
            }
            let width = self.bit_widths.get(self.miniblock_index).copied().unwrap_or(0);
            check_unpack_width::<u64>(width)?;
            let bytes = GROUP_SIZE * usize::from(width) / 8;
            let packed = self.cursor.buffer().take(bytes)?;
            unpack_bits(packed, 0, &mut self.unpacked, width);
            self.cursor.consume(bytes);
            self.unpacked_offset = 0;
            self.miniblock_offset += GROUP_SIZE;
        }
        Ok(())
    }
}

/// An integer type `DELTA_BINARY_PACKED` values are decoded into.
pub trait DbpTarget: Copy + Default {
    fn from_i64(value: i64) -> Self;
    /// The value truncated to this type (the C++ `static_cast<T>`).
    fn wrap(value: u64) -> Self;
    /// The value sign-extended back (`static_cast<int64_t>`).
    fn to_i64(self) -> i64;
}

macro_rules! dbp_target {
    ($($ty:ty),*) => {$(
        impl DbpTarget for $ty {
            #[inline]
            fn from_i64(value: i64) -> Self {
                value as $ty
            }
            #[inline]
            fn wrap(value: u64) -> Self {
                value as $ty
            }
            #[inline]
            fn to_i64(self) -> i64 {
                self as i64
            }
        }
    )*};
}
dbp_target!(i32, i64, u32, u64);

/// The `BYTE_STREAM_SPLIT` decoder (`BssDecoder`): byte `k` of value `i` is at
/// `k * count + i`.
#[derive(Debug)]
pub struct BssDecoder {
    cursor: BlockCursor,
    value_offset: usize,
}

impl BssDecoder {
    pub fn new(cursor: BlockCursor) -> Self {
        Self {
            cursor,
            value_offset: 0,
        }
    }

    fn check(&self, width: usize, count: usize) -> Result<usize> {
        let data = self.cursor.remaining();
        if !data.len().is_multiple_of(width) {
            return Err(runtime_error(format!(
                "Data buffer size for the BYTE_STREAM_SPLIT encoding ({}) should be a multiple of the type size ({width})",
                data.len()
            )));
        }
        if (self.value_offset + count) * width > data.len() {
            return Err(out_of_buffer());
        }
        Ok(data.len() / width)
    }

    /// Decodes `count` values of `width` bytes into `out` (`GetBatch`).
    pub fn get_batch(&mut self, width: usize, count: usize, out: &mut Vec<u8>) -> Result<()> {
        let stream_len = self.check(width, count)?;
        out.clear();
        out.resize(width * count, 0);
        let data = self.cursor.remaining();
        for byte in 0..width {
            let stream = &data[byte * stream_len + self.value_offset..][..count];
            for (i, &value) in stream.iter().enumerate() {
                out[i * width + byte] = value;
            }
        }
        self.value_offset += count;
        Ok(())
    }

    /// Skips `count` values (`Skip`).
    pub fn skip(&mut self, width: usize, count: usize) -> Result<()> {
        self.check(width, count)?;
        self.value_offset += count;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(data: &[u8]) -> BlockCursor {
        BlockCursor::new(std::sync::Arc::new(data.to_vec()), 0..data.len())
    }

    #[test]
    fn unpacks_bits_lsb_first() {
        // Values 0..8 with width 3: the example from the Parquet specification.
        let packed = [0b1000_1000, 0b1100_0110, 0b1111_1010];
        let mut out = [0u8; 8];
        unpack_bits(&packed, 0, &mut out, 3);
        assert_eq!(out, [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn rle_and_bit_packed_runs() {
        // An RLE run of five 1s followed by a bit-packed run of 8 values (width 3).
        let data = [0b0000_1010, 1, 0b0000_0011, 0b1000_1000, 0b1100_0110, 0b1111_1010];
        let mut decoder = RleBpDecoder::new(cursor(&data), 3).unwrap();
        let mut out = [0u32; 13];
        decoder.get_batch(&mut out).unwrap();
        assert_eq!(out, [1, 1, 1, 1, 1, 0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn rejects_wide_bit_widths_for_the_target() {
        let data = [0b0000_0011, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut decoder = RleBpDecoder::new(cursor(&data), 63).unwrap();
        let mut out = [0u32; 8];
        let error = decoder.get_batch(&mut out).unwrap_err();
        assert!(error.to_string().contains("exceeds the maximum width (32)"));
    }

    #[test]
    fn delta_binary_packed() {
        // From the specification: 1, 2, 3, 4, 5 with block size 128, 4 miniblocks.
        let mut data = vec![128, 1, 4, 5, 2]; // block size, miniblocks, count, first (zigzag 1)
        data.push(2); // min delta 1 (zigzag)
        data.extend([0, 0, 0, 0]); // bit widths
        let mut decoder = DbpDecoder::new(cursor(&data)).unwrap();
        let mut out = [0i32; 5];
        decoder.get_batch(&mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn byte_stream_split() {
        let data = [1, 2, 10, 20];
        let mut decoder = BssDecoder::new(cursor(&data));
        let mut out = Vec::new();
        decoder.get_batch(2, 2, &mut out).unwrap();
        assert_eq!(out, [1, 10, 2, 20]);
    }
}
