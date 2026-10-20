//! LZ4 block compression exactly like the reference implementation DuckDB bundles
//! (`LZ4_compress_default`, lz4 1.9.4: `LZ4_compress_generic` without dictionary and
//! acceleration 1), so `LZ4_RAW` pages are byte-identical to DuckDB's. Decompression
//! uses `lz4_flex`.

/// The shortest match (`MINMATCH`).
const MIN_MATCH: usize = 4;
/// Matches end this far before the input end (`LASTLITERALS`).
const LAST_LITERALS: usize = 5;
/// The last match starts this far before the end (`MFLIMIT`).
const MF_LIMIT: usize = 12;
/// Inputs shorter than this are all literals (`LZ4_minLength`).
const MIN_LENGTH: usize = MF_LIMIT + 1;
/// Inputs below this use 16-bit positions (`LZ4_64Klimit`).
const LIMIT_64K: usize = 64 * 1024 + (MF_LIMIT - 1);
/// `LZ4_MEMORY_USAGE - 2` (`LZ4_HASHLOG`).
const HASH_LOG: u32 = 12;
/// `LZ4_skipTrigger`.
const SKIP_TRIGGER: u32 = 6;
/// `LZ4_DISTANCE_MAX`.
const DISTANCE_MAX: usize = 65535;
const ML_BITS: u32 = 4;
const ML_MASK: usize = (1 << ML_BITS) - 1;
const RUN_MASK: usize = (1 << (8 - ML_BITS)) - 1;

#[inline]
fn read32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

#[inline]
fn read64(data: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// The position table: 16-bit positions in 2^13 slots for small inputs (`byU16`),
/// 32-bit positions in 2^12 slots otherwise (`byU32`).
enum Table {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl Table {
    /// `LZ4_hashPosition`: `LZ4_hash4` for 16-bit tables, `LZ4_hash5` (of 8 bytes) for
    /// 32-bit ones (64-bit little-endian platforms).
    #[inline]
    fn hash(&self, data: &[u8], at: usize) -> usize {
        match self {
            Self::U16(_) => (read32(data, at).wrapping_mul(2_654_435_761) >> (32 - (HASH_LOG + 1))) as usize,
            Self::U32(_) => {
                const PRIME_5_BYTES: u64 = 889_523_592_379;
                ((read64(data, at) << 24).wrapping_mul(PRIME_5_BYTES) >> (64 - HASH_LOG)) as usize
            }
        }
    }

    #[inline]
    fn get(&self, slot: usize) -> usize {
        match self {
            Self::U16(table) => usize::from(table[slot]),
            Self::U32(table) => table[slot] as usize,
        }
    }

    #[inline]
    fn put(&mut self, slot: usize, position: usize) {
        match self {
            Self::U16(table) => table[slot] = position as u16,
            Self::U32(table) => table[slot] = position as u32,
        }
    }

    /// Whether a match at `match_index` is too far back from `current` (only checked
    /// with 32-bit positions).
    #[inline]
    fn too_far(&self, match_index: usize, current: usize) -> bool {
        matches!(self, Self::U32(_)) && match_index + DISTANCE_MAX < current
    }
}

/// `LZ4_count`: the length of the common prefix of `data[a..]` and `data[b..]`, up to
/// `limit` for `a`.
fn count(data: &[u8], mut a: usize, mut b: usize, limit: usize) -> usize {
    let start = a;
    while a + 8 <= limit {
        let diff = read64(data, a) ^ read64(data, b);
        if diff != 0 {
            return a - start + (diff.trailing_zeros() >> 3) as usize;
        }
        a += 8;
        b += 8;
    }
    while a < limit && data[a] == data[b] {
        a += 1;
        b += 1;
    }
    a - start
}

/// Appends a length in LZ4's 255-continuation encoding (after the token's 15).
fn push_length(out: &mut Vec<u8>, mut length: usize) {
    while length >= 255 {
        out.push(255);
        length -= 255;
    }
    out.push(length as u8);
}

/// Appends the LZ4 block of `src` to `out` (`LZ4_compress_default`).
pub fn compress(src: &[u8], out: &mut Vec<u8>) {
    let input_size = src.len();
    let mut anchor = 0usize;
    if input_size >= MIN_LENGTH {
        compress_sequences(src, out, &mut anchor);
    }
    // `_last_literals`.
    let last_run = input_size - anchor;
    if last_run >= RUN_MASK {
        out.push((RUN_MASK << ML_BITS) as u8);
        push_length(out, last_run - RUN_MASK);
    } else {
        out.push((last_run << ML_BITS) as u8);
    }
    out.extend_from_slice(&src[anchor..]);
}

/// The main loop of `LZ4_compress_generic_validated`: the sequences before the last
/// literals; leaves `anchor` at the first byte not yet emitted.
fn compress_sequences(src: &[u8], out: &mut Vec<u8>, anchor: &mut usize) {
    let input_size = src.len();
    let mut table = if input_size < LIMIT_64K {
        Table::U16(vec![0; 1 << (HASH_LOG + 1)])
    } else {
        Table::U32(vec![0; 1 << HASH_LOG])
    };
    let mflimit_plus_one = input_size - MF_LIMIT + 1;
    let match_limit = input_size - LAST_LITERALS;
    let mut ip = 0usize;
    // First byte.
    let slot = table.hash(src, ip);
    table.put(slot, ip);
    ip += 1;
    let mut forward_hash = table.hash(src, ip);
    loop {
        // Find a match.
        let mut matched;
        {
            let mut forward_ip = ip;
            let mut step = 1usize;
            let mut search_match_nb: u32 = 1 << SKIP_TRIGGER;
            loop {
                let hash = forward_hash;
                let current = forward_ip;
                let match_index = table.get(hash);
                ip = forward_ip;
                forward_ip += step;
                step = (search_match_nb >> SKIP_TRIGGER) as usize;
                search_match_nb += 1;
                if forward_ip > mflimit_plus_one {
                    return;
                }
                matched = match_index;
                forward_hash = table.hash(src, forward_ip);
                table.put(hash, current);
                if table.too_far(match_index, current) {
                    continue;
                }
                if read32(src, matched) == read32(src, ip) {
                    break;
                }
            }
        }
        // Catch up.
        while ip > *anchor && matched > 0 && src[ip - 1] == src[matched - 1] {
            ip -= 1;
            matched -= 1;
        }
        // Encode the literals.
        let literal_length = ip - *anchor;
        let mut token = out.len();
        if literal_length >= RUN_MASK {
            out.push((RUN_MASK << ML_BITS) as u8);
            push_length(out, literal_length - RUN_MASK);
        } else {
            out.push((literal_length << ML_BITS) as u8);
        }
        out.extend_from_slice(&src[*anchor..ip]);
        loop {
            // `_next_match`: the offset and the match length.
            out.extend_from_slice(&((ip - matched) as u16).to_le_bytes());
            let match_code = count(src, ip + MIN_MATCH, matched + MIN_MATCH, match_limit);
            ip += match_code + MIN_MATCH;
            if match_code >= ML_MASK {
                out[token] += ML_MASK as u8;
                push_length(out, match_code - ML_MASK);
            } else {
                out[token] += match_code as u8;
            }
            *anchor = ip;
            if ip >= mflimit_plus_one {
                return;
            }
            // Fill the table.
            let slot = table.hash(src, ip - 2);
            table.put(slot, ip - 2);
            // Test the next position.
            let hash = table.hash(src, ip);
            let current = ip;
            let match_index = table.get(hash);
            table.put(hash, current);
            if !table.too_far(match_index, current) && read32(src, match_index) == read32(src, ip) {
                matched = match_index;
                token = out.len();
                out.push(0);
                continue;
            }
            break;
        }
        // Prepare the next loop.
        ip += 1;
        forward_hash = table.hash(src, ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_lz4_flex() {
        let mut state = 7u64;
        for len in [0usize, 1, 12, 13, 14, 100, 1000, 65_546, 65_547, 70_000, 300_000] {
            let data: Vec<u8> = (0..len)
                .map(|i| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    if i % 97 < 40 {
                        (i % 13) as u8
                    } else {
                        (state % 7) as u8
                    }
                })
                .collect();
            let mut compressed = Vec::new();
            compress(&data, &mut compressed);
            let decompressed = lz4_flex::block::decompress(&compressed, data.len()).unwrap();
            assert_eq!(decompressed, data, "length {len}");
        }
    }
}
