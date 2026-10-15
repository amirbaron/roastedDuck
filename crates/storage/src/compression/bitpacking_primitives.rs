//! Bit packing of groups of 32 integers (DuckDB's `BitpackingPrimitives` on top of
//! `duckdb_fastpforlib` and `HugeIntPacker`).
//!
//! Whatever the value type, fastpforlib's layout is the same: the group's values, masked to
//! `width` bits, are concatenated least significant bit first into a little-endian bit
//! stream of `32 * width / 8` bytes. Packing and unpacking therefore work on raw `u64` /
//! `u128` bit patterns and do not need per-type kernels.

/// Values per packed group (`BITPACKING_ALGORITHM_GROUP_SIZE`).
pub(crate) const GROUP_SIZE: usize = 32;
/// The header of a bit-packed segment: the offset of its metadata (`BITPACKING_HEADER_SIZE`).
pub(crate) const BITPACKING_HEADER_SIZE: usize = 8;

/// `RoundUpToAlgorithmGroupSize`.
#[inline]
pub(crate) const fn round_up_to_group(count: usize) -> usize {
    count.next_multiple_of(GROUP_SIZE)
}

/// Bytes needed to pack `count` values of `width` bits (`GetRequiredSize`).
#[inline]
pub(crate) const fn required_size(count: usize, width: u8) -> usize {
    round_up_to_group(count) * width as usize / 8
}

/// The group's packed bytes as little-endian 64-bit words, zero padded (one spare word so
/// values straddling the last word can be read without bounds checks).
#[inline]
fn group_words(src: &[u8], width: u8, words: &mut [u64; 66]) -> usize {
    let len = (GROUP_SIZE * width as usize / 8).min(src.len());
    let full = len / 8;
    for (word, chunk) in words.iter_mut().zip(src[..full * 8].as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let mut raw = [0u8; 8];
    raw[..len - full * 8].copy_from_slice(&src[full * 8..len]);
    words[full] = u64::from_le_bytes(raw);
    for word in &mut words[full + 1..] {
        *word = 0;
    }
    full
}

/// Bits `bit..bit + width` (`width <= 64`) of a word stream.
#[inline]
fn extract(words: &[u64; 66], bit: usize, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    let index = bit / 64;
    let shift = (bit % 64) as u32;
    let mut value = words[index] >> shift;
    if shift + width > 64 {
        value |= words[index + 1] << (64 - shift);
    }
    if width == 64 {
        value
    } else {
        value & ((1u64 << width) - 1)
    }
}

/// Unpacks one group of 32 values of `width <= 64` bits (`UnPackGroup` without sign
/// extension).
#[inline]
pub(crate) fn unpack_group_u64(src: &[u8], width: u8, out: &mut [u64; GROUP_SIZE]) {
    if width == 0 {
        *out = [0; GROUP_SIZE];
        return;
    }
    let mut words = [0u64; 66];
    group_words(src, width, &mut words);
    let width = u32::from(width);
    for (index, value) in out.iter_mut().enumerate() {
        *value = extract(&words, index * width as usize, width);
    }
}

/// Unpacks one group of 32 values of `width <= 128` bits (`HugeIntPacker::Unpack`).
pub(crate) fn unpack_group_u128(src: &[u8], width: u8, out: &mut [u128; GROUP_SIZE]) {
    if width == 0 {
        *out = [0; GROUP_SIZE];
        return;
    }
    // 32 values of up to 128 bits: 64 words.
    let mut words = [0u64; 66];
    group_words(src, width, &mut words);
    let width = u32::from(width);
    for (index, value) in out.iter_mut().enumerate() {
        let bit = index * width as usize;
        let low = extract(&words, bit, width.min(64));
        let high = if width > 64 {
            extract(&words, bit + 64, width - 64)
        } else {
            0
        };
        *value = u128::from(low) | (u128::from(high) << 64);
    }
}

/// Unpacks `count` values (rounded up to whole groups) of `width <= 64` bits into `out`,
/// which must hold `round_up_to_group(count)` values (`UnPackBuffer`).
pub(crate) fn unpack_buffer_u64(src: &[u8], count: usize, width: u8, out: &mut [u64]) {
    let group_bytes = GROUP_SIZE * width as usize / 8;
    for (index, target) in out[..round_up_to_group(count)]
        .as_chunks_mut::<GROUP_SIZE>()
        .0
        .iter_mut()
        .enumerate()
    {
        let start = (index * group_bytes).min(src.len());
        unpack_group_u64(&src[start..], width, target);
    }
}

/// Like [`unpack_buffer_u64`], narrowing every value to `u32` (`UnPackBuffer<uint32_t>`).
pub(crate) fn unpack_buffer_u32(src: &[u8], count: usize, width: u8, out: &mut [u32]) {
    let group_bytes = GROUP_SIZE * width as usize / 8;
    let mut group = [0u64; GROUP_SIZE];
    for (index, target) in out[..round_up_to_group(count)]
        .as_chunks_mut::<GROUP_SIZE>()
        .0
        .iter_mut()
        .enumerate()
    {
        let start = (index * group_bytes).min(src.len());
        unpack_group_u64(&src[start..], width, &mut group);
        for (target, value) in target.iter_mut().zip(group) {
            *target = value as u32;
        }
    }
}

/// Writes words to the group's `32 * width / 8` bytes.
fn store_words(words: &[u64; 66], width: u8, dst: &mut [u8]) {
    let len = GROUP_SIZE * width as usize / 8;
    for (index, byte) in dst[..len].iter_mut().enumerate() {
        *byte = (words[index / 8] >> ((index % 8) * 8)) as u8;
    }
}

/// ORs `width <= 64` bits of `value` in at `bit`.
#[inline]
fn insert(words: &mut [u64; 66], bit: usize, width: u32, value: u64) {
    if width == 0 {
        return;
    }
    let value = if width == 64 {
        value
    } else {
        value & ((1u64 << width) - 1)
    };
    let index = bit / 64;
    let shift = (bit % 64) as u32;
    words[index] |= value << shift;
    if shift + width > 64 {
        words[index + 1] |= value >> (64 - shift);
    }
}

/// Packs one group of 32 values of `width <= 64` bits into `32 * width / 8` bytes of `dst`
/// (`PackGroup`).
pub(crate) fn pack_group_u64(values: &[u64; GROUP_SIZE], width: u8, dst: &mut [u8]) {
    if width == 0 {
        return;
    }
    let mut words = [0u64; 66];
    let width32 = u32::from(width);
    for (index, &value) in values.iter().enumerate() {
        insert(&mut words, index * width as usize, width32, value);
    }
    store_words(&words, width, dst);
}

/// Packs one group of 32 values of `width <= 128` bits (`HugeIntPacker::Pack`).
pub(crate) fn pack_group_u128(values: &[u128; GROUP_SIZE], width: u8, dst: &mut [u8]) {
    if width == 0 {
        return;
    }
    let mut words = [0u64; 66];
    let width32 = u32::from(width);
    for (index, &value) in values.iter().enumerate() {
        let bit = index * width as usize;
        insert(&mut words, bit, width32.min(64), value as u64);
        if width32 > 64 {
            insert(&mut words, bit + 64, width32 - 64, (value >> 64) as u64);
        }
    }
    store_words(&words, width, dst);
}

/// Packs `values` (any count; the last group is zero padded) into `dst`, which must hold
/// `required_size(values.len(), width)` bytes (`PackBuffer<T, false>`).
pub(crate) fn pack_buffer_u64(values: &[u64], width: u8, dst: &mut [u8]) {
    let group_bytes = GROUP_SIZE * width as usize / 8;
    for (index, chunk) in values.chunks(GROUP_SIZE).enumerate() {
        let mut group = [0u64; GROUP_SIZE];
        group[..chunk.len()].copy_from_slice(chunk);
        pack_group_u64(&group, width, &mut dst[index * group_bytes..]);
    }
}

/// Packs `values` like [`pack_buffer_u64`] with values of up to 128 bits.
pub(crate) fn pack_buffer_u128(values: &[u128], width: u8, dst: &mut [u8]) {
    let group_bytes = GROUP_SIZE * width as usize / 8;
    for (index, chunk) in values.chunks(GROUP_SIZE).enumerate() {
        let mut group = [0u128; GROUP_SIZE];
        group[..chunk.len()].copy_from_slice(chunk);
        pack_group_u128(&group, width, &mut dst[index * group_bytes..]);
    }
}

/// The number of bits needed for the unsigned `value` of a `type_bits`-bit type
/// (`MinimumBitWidth<T, false>` including `GetEffectiveWidth`).
pub(crate) fn minimum_bit_width_unsigned(value: u128, type_bits: u32) -> u8 {
    if value == 0 {
        return 0;
    }
    effective_width(128 - value.leading_zeros(), type_bits)
}

/// The number of bits needed for a signed value range of a `type_bits`-bit type
/// (`MinimumBitWidth<T, true>(min, max)` including `GetEffectiveWidth`): one sign bit plus
/// the magnitude bits of `max(-min, max)`.
pub(crate) fn minimum_bit_width_signed(min: i128, max: i128, type_bits: u32) -> u8 {
    let type_min = if type_bits >= 128 {
        i128::MIN
    } else {
        -(1i128 << (type_bits - 1))
    };
    if min == type_min {
        return type_bits as u8;
    }
    let value = (-min).max(max);
    if value == 0 {
        return 0;
    }
    effective_width(1 + 128 - value.leading_zeros(), type_bits)
}

/// `GetEffectiveWidth`: widths that save less than a byte per value use the full type.
fn effective_width(width: u32, type_bits: u32) -> u8 {
    if width + type_bits / 8 > type_bits {
        type_bits as u8
    } else {
        width as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_round_trip_at_every_width() {
        for width in 0..=64u8 {
            let mask = if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            let values: [u64; GROUP_SIZE] =
                std::array::from_fn(|i| (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) & mask);
            let mut packed = vec![0u8; required_size(GROUP_SIZE, width)];
            pack_group_u64(&values, width, &mut packed);
            let mut unpacked = [0u64; GROUP_SIZE];
            unpack_group_u64(&packed, width, &mut unpacked);
            assert_eq!(values, unpacked, "width {width}");
        }
        for width in [0u8, 1, 63, 64, 65, 100, 127, 128] {
            let mask = if width == 128 {
                u128::MAX
            } else {
                (1u128 << width) - 1
            };
            let values: [u128; GROUP_SIZE] = std::array::from_fn(|i| {
                (i as u128).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C834) & mask
            });
            let mut packed = vec![0u8; required_size(GROUP_SIZE, width)];
            pack_group_u128(&values, width, &mut packed);
            let mut unpacked = [0u128; GROUP_SIZE];
            unpack_group_u128(&packed, width, &mut unpacked);
            assert_eq!(values, unpacked, "width {width}");
        }
    }

    #[test]
    fn layout_is_least_significant_bit_first() {
        // fastpforlib packs value i at bit i * width of a little-endian stream.
        let mut values = [0u64; GROUP_SIZE];
        values[0] = 0b101;
        values[1] = 0b011;
        let mut packed = vec![0u8; required_size(GROUP_SIZE, 3)];
        pack_group_u64(&values, 3, &mut packed);
        assert_eq!(packed[0], 0b0001_1101);
    }

    #[test]
    fn minimum_bit_widths_follow_duckdb() {
        assert_eq!(minimum_bit_width_unsigned(0, 32), 0);
        assert_eq!(minimum_bit_width_unsigned(1, 32), 1);
        assert_eq!(minimum_bit_width_unsigned(255, 32), 8);
        // 29 bits + 4 bytes > 32 bits: the full width is used.
        assert_eq!(minimum_bit_width_unsigned(1 << 28, 32), 32);
        assert_eq!(minimum_bit_width_signed(0, 0, 32), 0);
        assert_eq!(minimum_bit_width_signed(0, 1, 32), 2);
        assert_eq!(minimum_bit_width_signed(-3, 2, 32), 3);
        assert_eq!(minimum_bit_width_signed(i128::from(i32::MIN), 0, 32), 32);
    }
}
