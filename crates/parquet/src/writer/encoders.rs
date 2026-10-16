//! Value encoders of the writer (DuckDB's `RleBpEncoder`, `DbpEncoder`, `DlbaEncoder` and
//! `BssEncoder`), producing exactly DuckDB's byte streams.

use crate::decode::int_to_zigzag;

/// The values per bit-packing group (`BITPACKING_ALGORITHM_GROUP_SIZE`).
const GROUP_SIZE: usize = 32;

/// `ParquetDecodeUtils::VarintEncode`.
pub fn varint_encode(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Packs `values` (a multiple of 32 of them) with `width` bits each, least significant
/// bit first (`BitpackingPrimitives::PackBuffer`), appending `values.len() * width / 8`
/// bytes.
pub fn bit_pack(values: &[u64], width: u8, out: &mut Vec<u8>) {
    debug_assert!(values.len() % GROUP_SIZE == 0);
    let width = usize::from(width);
    if width == 0 {
        return;
    }
    let start = out.len();
    out.resize(start + values.len() * width / 8, 0);
    let packed = &mut out[start..];
    let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    let mut bit = 0usize;
    for &value in values {
        let mut value = value & mask;
        let mut remaining = width;
        let mut position = bit;
        while remaining > 0 {
            let byte = position / 8;
            let shift = position % 8;
            let take = (8 - shift).min(remaining);
            packed[byte] |= ((value & ((1u64 << take) - 1)) << shift) as u8;
            value = if take == 64 { 0 } else { value >> take };
            remaining -= take;
            position += take;
        }
        bit += width;
    }
}

/// `BitpackingPrimitives::MinimumBitWidth` of unsigned values, with DuckDB's rounding of
/// widths that save less than a byte per value up to the type width (`GetEffectiveWidth`).
pub fn minimum_bit_width(values: &[u64]) -> u8 {
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return 0;
    }
    let width = 64 - max.leading_zeros() as u8;
    if width + 8 > 64 { 64 } else { width }
}

/// The RLE / bit-packing hybrid encoder (`RleBpEncoder`).
#[derive(Debug)]
pub struct RleBpEncoder {
    bit_width: u8,
    byte_width: usize,
    rle_value: u32,
    rle_count: u64,
    bp_block: [u64; Self::BP_BLOCK_SIZE],
    bp_block_count: usize,
}

impl RleBpEncoder {
    const MINIMUM_RLE_COUNT: u64 = 4;
    const BP_BLOCK_SIZE: usize = 256;

    pub fn new(bit_width: u8) -> Self {
        Self {
            bit_width,
            byte_width: usize::from(bit_width).div_ceil(8),
            rle_value: 0,
            rle_count: 0,
            bp_block: [0; Self::BP_BLOCK_SIZE],
            bp_block_count: 0,
        }
    }

    /// `BeginWrite`.
    pub fn begin_write(&mut self) {
        self.rle_count = 0;
        self.bp_block_count = 0;
    }

    /// `WriteValue`.
    pub fn write_value(&mut self, out: &mut Vec<u8>, value: u32) {
        if self.bp_block_count != 0 {
            self.bp_block[self.bp_block_count] = u64::from(value);
            self.bp_block_count += 1;
            if self.bp_block_count == Self::BP_BLOCK_SIZE {
                self.write_run(out);
            }
            return;
        }
        if self.rle_count == 0 {
            self.rle_value = value;
            self.rle_count = 1;
            return;
        }
        if self.rle_value == value {
            self.rle_count += 1;
            return;
        }
        if self.rle_count >= Self::MINIMUM_RLE_COUNT {
            self.write_run(out);
            self.rle_value = value;
            self.rle_count = 1;
            return;
        }
        // Not enough repeats: switch to a bit-packed run.
        for _ in 0..self.rle_count {
            self.bp_block[self.bp_block_count] = u64::from(self.rle_value);
            self.bp_block_count += 1;
        }
        self.bp_block[self.bp_block_count] = u64::from(value);
        self.bp_block_count += 1;
        self.rle_count = 0;
    }

    /// `WriteMany`: `count` copies of `value`.
    pub fn write_many(&mut self, out: &mut Vec<u8>, value: u32, mut count: u64) {
        if self.rle_count != 0 && count > 0 {
            self.write_value(out, value);
            count -= 1;
        }
        while self.bp_block_count != 0 && count > 0 {
            self.write_value(out, value);
            count -= 1;
        }
        self.rle_value = value;
        self.rle_count += count;
    }

    /// `FinishWrite`.
    pub fn finish_write(&mut self, out: &mut Vec<u8>) {
        self.write_run(out);
    }

    fn write_run(&mut self, out: &mut Vec<u8>) {
        if self.rle_count != 0 {
            varint_encode(self.rle_count << 1, out);
            let bytes = self.rle_value.to_le_bytes();
            out.extend_from_slice(&bytes[..self.byte_width.min(4)]);
            self.rle_count = 0;
        } else if self.bp_block_count != 0 {
            varint_encode(((Self::BP_BLOCK_SIZE / 8) << 1 | 1) as u64, out);
            // Unused slots of a partial block keep earlier values, like DuckDB's buffer.
            bit_pack(&self.bp_block, self.bit_width, out);
            self.bp_block_count = 0;
        }
    }
}

/// The DELTA_BINARY_PACKED encoder (`DbpEncoder`): blocks of 2048 values in 8 miniblocks.
#[derive(Debug)]
pub struct DbpEncoder {
    total_value_count: u64,
    count: u64,
    previous_value: i64,
    min_delta: i64,
    data: Vec<u64>,
}

impl DbpEncoder {
    const BLOCK_SIZE_IN_VALUES: usize = 2048;
    const MINIBLOCKS_IN_A_BLOCK: usize = 8;
    const VALUES_IN_A_MINIBLOCK: usize = Self::BLOCK_SIZE_IN_VALUES / Self::MINIBLOCKS_IN_A_BLOCK;

    pub fn new(total_value_count: u64) -> Self {
        Self {
            total_value_count,
            count: 0,
            previous_value: 0,
            min_delta: i64::MAX,
            data: Vec::with_capacity(Self::BLOCK_SIZE_IN_VALUES),
        }
    }

    /// `BeginWrite`: the header and the first value.
    pub fn begin_write(&mut self, out: &mut Vec<u8>, first_value: i64) {
        varint_encode(Self::BLOCK_SIZE_IN_VALUES as u64, out);
        varint_encode(Self::MINIBLOCKS_IN_A_BLOCK as u64, out);
        varint_encode(self.total_value_count, out);
        varint_encode(int_to_zigzag(first_value), out);
        if self.total_value_count != 0 {
            self.count += 1;
        }
        self.previous_value = first_value;
        self.min_delta = i64::MAX;
        self.data.clear();
    }

    /// `WriteValue`.
    pub fn write_value(&mut self, out: &mut Vec<u8>, value: i64) {
        let delta = (value as u64).wrapping_sub(self.previous_value as u64) as i64;
        self.previous_value = value;
        self.min_delta = self.min_delta.min(delta);
        self.data.push(delta as u64);
        if self.data.len() == Self::BLOCK_SIZE_IN_VALUES {
            self.write_block(out);
        }
    }

    /// `FinishWrite`.
    pub fn finish_write(&mut self, out: &mut Vec<u8>) {
        if !self.data.is_empty() {
            self.write_block(out);
        }
    }

    fn write_block(&mut self, out: &mut Vec<u8>) {
        let block_count = self.data.len();
        let miniblocks = block_count.div_ceil(Self::VALUES_IN_A_MINIBLOCK);
        let mut values = vec![0u64; miniblocks * Self::VALUES_IN_A_MINIBLOCK];
        for (slot, &delta) in values.iter_mut().zip(&self.data) {
            *slot = delta.wrapping_sub(self.min_delta as u64);
        }
        let mut widths = [0u8; Self::MINIBLOCKS_IN_A_BLOCK];
        for (index, width) in widths.iter_mut().enumerate().take(miniblocks) {
            let start = index * Self::VALUES_IN_A_MINIBLOCK;
            *width = minimum_bit_width(&values[start..start + Self::VALUES_IN_A_MINIBLOCK]);
        }
        varint_encode(int_to_zigzag(self.min_delta), out);
        out.extend_from_slice(&widths);
        for (index, &width) in widths.iter().enumerate().take(miniblocks) {
            let start = index * Self::VALUES_IN_A_MINIBLOCK;
            bit_pack(&values[start..start + Self::VALUES_IN_A_MINIBLOCK], width, out);
        }
        self.count += block_count as u64;
        self.min_delta = i64::MAX;
        self.data.clear();
    }
}

/// The DELTA_LENGTH_BYTE_ARRAY encoder (`DlbaEncoder`): DELTA_BINARY_PACKED lengths,
/// then the concatenated bytes.
#[derive(Debug)]
pub struct DlbaEncoder {
    lengths: DbpEncoder,
    bytes: Vec<u8>,
}

impl DlbaEncoder {
    pub fn new(total_value_count: u64, total_string_size: usize) -> Self {
        Self {
            lengths: DbpEncoder::new(total_value_count),
            bytes: Vec::with_capacity(total_string_size),
        }
    }

    pub fn begin_write(&mut self, out: &mut Vec<u8>, first: &[u8]) {
        self.lengths.begin_write(out, first.len() as i64);
        self.bytes.extend_from_slice(first);
    }

    pub fn write_value(&mut self, out: &mut Vec<u8>, value: &[u8]) {
        self.lengths.write_value(out, value.len() as i64);
        self.bytes.extend_from_slice(value);
    }

    pub fn finish_write(&mut self, out: &mut Vec<u8>) {
        self.lengths.finish_write(out);
        out.extend_from_slice(&self.bytes);
    }
}

/// The BYTE_STREAM_SPLIT encoder (`BssEncoder`).
#[derive(Debug)]
pub struct BssEncoder {
    total_value_count: usize,
    width: usize,
    count: usize,
    buffer: Vec<u8>,
}

impl BssEncoder {
    pub fn new(total_value_count: usize, width: usize) -> Self {
        Self {
            total_value_count,
            width,
            count: 0,
            buffer: Vec::new(),
        }
    }

    pub fn begin_write(&mut self) {
        self.buffer = vec![0; self.total_value_count * self.width];
    }

    /// Adds a value given as its little-endian bytes.
    pub fn write_value(&mut self, bytes: &[u8]) {
        for (i, &byte) in bytes.iter().enumerate().take(self.width) {
            if let Some(slot) = self.buffer.get_mut(i * self.total_value_count + self.count) {
                *slot = byte;
            }
        }
        self.count += 1;
    }

    pub fn finish_write(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.buffer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::decode::{BlockCursor, BssDecoder, DbpDecoder, RleBpDecoder, unpack_bits};

    fn cursor(data: Vec<u8>) -> BlockCursor {
        let len = data.len();
        BlockCursor::new(Arc::new(data), 0..len)
    }

    #[test]
    fn bit_packing_round_trips() {
        let values: Vec<u64> = (0..64).map(|i| (i * 37) % 128).collect();
        let mut out = Vec::new();
        bit_pack(&values, 7, &mut out);
        assert_eq!(out.len(), 64 * 7 / 8);
        let mut decoded = vec![0u64; 64];
        unpack_bits(&out, 0, &mut decoded, 7);
        assert_eq!(decoded, values);
    }

    #[test]
    fn rle_bp_round_trips() {
        let values: Vec<u32> = [vec![3; 10], (0..20).map(|i| i % 5).collect(), vec![1; 3]].concat();
        let mut encoder = RleBpEncoder::new(3);
        let mut out = Vec::new();
        encoder.begin_write();
        for &value in &values {
            encoder.write_value(&mut out, value);
        }
        encoder.finish_write(&mut out);
        let mut decoder = RleBpDecoder::new(cursor(out), 3).unwrap();
        let mut decoded = vec![0u32; values.len()];
        decoder.get_batch(&mut decoded).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn dbp_round_trips() {
        let values: Vec<i64> = (0..5000).map(|i| i * i % 1000 - 300).collect();
        let mut encoder = DbpEncoder::new(values.len() as u64);
        let mut out = Vec::new();
        encoder.begin_write(&mut out, values[0]);
        for &value in &values[1..] {
            encoder.write_value(&mut out, value);
        }
        encoder.finish_write(&mut out);
        let mut decoder = DbpDecoder::new(cursor(out)).unwrap();
        let mut decoded = vec![0i64; values.len()];
        decoder.get_batch(&mut decoded).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn bss_round_trips() {
        let mut encoder = BssEncoder::new(3, 4);
        encoder.begin_write();
        for value in [1.5f32, -2.0, 3.25] {
            encoder.write_value(&value.to_le_bytes());
        }
        let mut out = Vec::new();
        encoder.finish_write(&mut out);
        let mut decoder = BssDecoder::new(cursor(out));
        let mut decoded = Vec::new();
        decoder.get_batch(4, 3, &mut decoded).unwrap();
        assert_eq!(&decoded[4..8], &(-2.0f32).to_le_bytes());
    }
}
