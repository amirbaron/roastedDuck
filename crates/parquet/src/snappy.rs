//! Snappy decompression (the raw format of Google's `snappy::RawUncompress`, which
//! DuckDB's reader calls), tuned like the C++ implementation: short literals and copies
//! move 16 bytes at a time when the buffers have room, overwriting bytes that later
//! elements fill in.

/// Why a snappy block could not be decompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnappyError {
    /// The block is malformed (truncated, bad offset, too much output, ...).
    Corrupt,
    /// The block's uncompressed length is not the expected one.
    LengthMismatch,
}

/// The uncompressed length stored at the start of a block, and the length of its varint.
fn read_header(src: &[u8]) -> Result<(usize, usize), SnappyError> {
    let mut result: u64 = 0;
    for (index, &byte) in src.iter().enumerate().take(5) {
        result |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            let len = u32::try_from(result).map_err(|_| SnappyError::Corrupt)?;
            return Ok((len as usize, index + 1));
        }
    }
    Err(SnappyError::Corrupt)
}

/// The uncompressed length of a block (`snappy::GetUncompressedLength`).
pub fn decompress_len(src: &[u8]) -> Result<usize, SnappyError> {
    read_header(src).map(|(len, _)| len)
}

/// Reads a little-endian integer of `bytes.len()` (at most 4) bytes.
#[inline]
fn read_le(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rev()
        .fold(0, |value, &byte| (value << 8) | usize::from(byte))
}

/// Decompresses the block `src` into `dst`, which must be exactly its uncompressed size.
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Result<(), SnappyError> {
    let (len, header) = read_header(src)?;
    if len != dst.len() {
        return Err(SnappyError::LengthMismatch);
    }
    let end = dst.len();
    let mut ip = header;
    let mut op = 0;
    while ip < src.len() {
        if ip + 2 * SLOP + 2 < src.len() && op + 2 * SLOP < end {
            // Back to the fast loop after an element it leaves to this one
            // (`DecompressAllTags` calling `DecompressBranchless` again).
            (ip, op) = decompress_branchless(src, dst, ip, op);
            if ip >= src.len() {
                break;
            }
        }
        let tag = src[ip];
        ip += 1;
        if tag.trailing_zeros() >= 2 {
            // A literal.
            let mut literal = usize::from(tag >> 2) + 1;
            if literal <= 16 && ip + 16 <= src.len() && op + 16 <= end {
                dst[op..op + 16].copy_from_slice(&src[ip..ip + 16]);
                ip += literal;
                op += literal;
                continue;
            }
            if literal > 60 {
                let extra = literal - 60;
                let bytes = src.get(ip..ip + extra).ok_or(SnappyError::Corrupt)?;
                literal = read_le(bytes) + 1;
                ip += extra;
            }
            if literal > src.len() - ip || literal > end - op {
                return Err(SnappyError::Corrupt);
            }
            dst[op..op + literal].copy_from_slice(&src[ip..ip + literal]);
            ip += literal;
            op += literal;
            continue;
        }
        let (length, offset) = match tag & 3 {
            1 => {
                let byte = *src.get(ip).ok_or(SnappyError::Corrupt)?;
                ip += 1;
                (
                    usize::from((tag >> 2) & 7) + 4,
                    (usize::from(tag >> 5) << 8) | usize::from(byte),
                )
            }
            2 => {
                let bytes = src.get(ip..ip + 2).ok_or(SnappyError::Corrupt)?;
                ip += 2;
                (usize::from(tag >> 2) + 1, read_le(bytes))
            }
            _ => {
                let bytes = src.get(ip..ip + 4).ok_or(SnappyError::Corrupt)?;
                ip += 4;
                (usize::from(tag >> 2) + 1, read_le(bytes))
            }
        };
        if offset == 0 || offset > op || length > end - op {
            return Err(SnappyError::Corrupt);
        }
        copy_match(dst, op, offset, length);
        op += length;
    }
    if op == end {
        Ok(())
    } else {
        Err(SnappyError::Corrupt)
    }
}

/// The bytes the branchless loop may read or write past an element (`kSlopBytes`).
const SLOP: usize = 64;

/// `kLengthMinusOffset`: per tag, the element's length minus its copy offset, with the
/// offset bits the tag holds (copy-1) and a fake offset of 256 for literals; `0xFF` for
/// the elements the branchless loop leaves to the careful one (long literals, copy-4).
const LENGTH_MINUS_OFFSET: [i16; 256] = {
    let mut table = [0i16; 256];
    let mut tag = 0;
    while tag < 256 {
        let data = (tag >> 2) as i16;
        table[tag] = match tag & 3 {
            0 if data >= 60 => 0xFF,
            0 => data + 1 - 256,
            1 => (data & 7) + 4 - (data >> 3) * 256,
            2 => data + 1,
            _ => 0xFF,
        };
        tag += 1;
    }
    table
};

/// The bits of the two bytes after a tag that hold its copy offset, by tag type, 16
/// bits each (`kExtractMasksCombined`): none for literals and copy-4.
const OFFSET_MASKS_PACKED: u64 = 0x0000_FFFF_00FF_0000;

/// Copies `len` (at most 64) bytes from `src` to `dst` in 32-byte steps, writing up to
/// 64 bytes (`MemCopy64`).
///
/// # Safety
/// 64 bytes must be readable at `src` and writable at `dst`; the regions may overlap.
#[inline]
unsafe fn copy64(dst: *mut u8, src: *const u8, len: usize) {
    // SAFETY: the caller guarantees 64 readable and writable bytes; `copy` allows
    // overlap.
    unsafe {
        std::ptr::copy(src, dst, 32);
        if len > 32 {
            std::ptr::copy(src.add(32), dst.add(32), 32);
        }
    }
}

/// The main loop of [`decompress`] (snappy's `DecompressBranchless`): literals and
/// non-overlapping copies are both "copy `len` bytes from a pointer" (the input or the
/// output), done one element late and 32 bytes at a time, so the loop barely branches.
/// Long literals, copy-4 and invalid copies stop it; returns the input and output
/// positions where the careful loop takes over.
fn decompress_branchless(src: &[u8], dst: &mut [u8], start: usize, op_start: usize) -> (usize, usize) {
    let mut ip = start;
    let mut op = op_start;
    if start + 2 * SLOP + 2 >= src.len() || op + 2 * SLOP >= dst.len() {
        return (ip, op);
    }
    let ip_limit = src.len() - 2 * SLOP - 1;
    let op_limit = dst.len() - 2 * SLOP;
    let s = src.as_ptr();
    let d = dst.as_mut_ptr();
    let safe_source = [0u8; SLOP];
    let mut deferred_src: *const u8 = safe_source.as_ptr();
    let mut deferred_len = 0usize;
    ip += 1;
    // SAFETY: `ip - 1 < start + 2 * SLOP + 2 < src.len()`.
    let mut tag = usize::from(unsafe { *s.add(ip - 1) });
    // SAFETY (whole loop): at the top of an iteration `ip < ip_limit`, so the two
    // elements read at most `2 * (SLOP - 2) + 2 * 3` bytes past it (the longest short
    // literal is 60 bytes, and 64 are read), within `src`. At the top `op +
    // deferred_len < op_limit`, and an iteration grows `op` by at most 128 bytes,
    // writing at most 64 bytes past it, within `dst`. Copy sources are at or before
    // the output written so far (`delta >= 0`, the offset at least the length) or in
    // the input.
    'outer: loop {
        for _ in 0..2 {
            let old_ip = ip;
            let length_minus_offset = LENGTH_MINUS_OFFSET[tag] as isize;
            let tag_type = tag & 3;
            // The bytes to the next tag, selected without a branch.
            let advance = if tag_type == 0 { (tag >> 2) + 1 } else { tag_type };
            // SAFETY: within the bounds of the loop invariant above.
            tag = usize::from(unsafe { *s.add(ip + advance) });
            ip += advance + 1;
            // SAFETY: within the bounds of the loop invariant above.
            let next =
                u16::from_le_bytes(unsafe { std::ptr::read_unaligned(s.add(old_ip).cast::<[u8; 2]>()) })
                    as isize;
            let len = (length_minus_offset & 0xFF) as usize;
            // `ExtractOffset`: the masks packed in a constant, avoiding a load.
            let extracted = next & ((OFFSET_MASKS_PACKED >> (tag_type * 16)) & 0xFFFF) as isize;
            let len_minus_offset = length_minus_offset - extracted;
            if length_minus_offset > extracted {
                if len & 0x80 != 0 {
                    ip = old_ip;
                    break 'outer;
                }
                // A copy-1 or copy-2 overlapping its own output.
                // SAFETY: within the bounds of the loop invariant above.
                unsafe { copy64(d.add(op), deferred_src, deferred_len) };
                op += deferred_len;
                deferred_src = safe_source.as_ptr();
                deferred_len = 0;
                let offset = len as isize - len_minus_offset;
                if offset <= 0 || offset as usize > op {
                    ip = old_ip;
                    break 'outer;
                }
                let offset = offset as usize;
                if offset >= 8 {
                    let mut copied = 0;
                    while copied < len {
                        // SAFETY: within the bounds of the loop invariant above.
                        unsafe {
                            std::ptr::copy_nonoverlapping(d.add(op + copied - offset), d.add(op + copied), 8);
                        }
                        copied += 8;
                    }
                } else {
                    for index in 0..len {
                        // SAFETY: within the bounds of the loop invariant above.
                        unsafe { *d.add(op + index) = *d.add(op + index - offset) };
                    }
                }
                op += len;
                continue;
            }
            let delta = (op + deferred_len) as isize + len_minus_offset - len as isize;
            if delta < 0 {
                // Only literals at the very start (their fake offset reaches back).
                if tag_type != 0 {
                    ip = old_ip;
                    break 'outer;
                }
                // SAFETY: within the bounds of the loop invariant above.
                unsafe { copy64(d.add(op), deferred_src, deferred_len) };
                op += deferred_len;
                // SAFETY: within the bounds of the loop invariant above.
                deferred_src = unsafe { s.add(old_ip) };
                deferred_len = len;
                continue;
            }
            let from: *const u8 = if tag_type == 0 {
                // SAFETY: within the bounds of the loop invariant above.
                unsafe { s.add(old_ip) }
            } else {
                // SAFETY: within the bounds of the loop invariant above.
                unsafe { d.add(delta as usize) }
            };
            // SAFETY: within the bounds of the loop invariant above.
            unsafe { copy64(d.add(op), deferred_src, deferred_len) };
            op += deferred_len;
            deferred_src = from;
            deferred_len = len;
        }
        if ip >= ip_limit || op + deferred_len >= op_limit {
            break;
        }
    }
    // `ip` is just past the next tag.
    ip -= 1;
    if deferred_len > 0 {
        // SAFETY: as in the loop, 64 bytes are writable at `op`.
        unsafe { copy64(d.add(op), deferred_src, deferred_len) };
        op += deferred_len;
    }
    (ip, op)
}

/// Copies `length` bytes starting `offset` bytes back to `op` (the regions may overlap:
/// the pattern of the last `offset` bytes repeats). The caller checked that
/// `0 < offset <= op` and `op + length <= dst.len()`.
#[inline]
fn copy_match(dst: &mut [u8], op: usize, offset: usize, length: usize) {
    let from = op - offset;
    if op + length + 16 <= dst.len() {
        // Room to overrun: copy in steps that never read bytes not yet written.
        if offset >= 16 {
            let mut copied = 0;
            while copied < length {
                dst.copy_within(from + copied..from + copied + 16, op + copied);
                copied += 16;
            }
            return;
        }
        if offset >= 8 {
            let mut copied = 0;
            while copied < length {
                dst.copy_within(from + copied..from + copied + 8, op + copied);
                copied += 8;
            }
            return;
        }
    }
    if offset >= length {
        dst.copy_within(from..from + length, op);
        return;
    }
    for index in 0..length {
        dst[op + index] = dst[from + index];
    }
}

/// The size of the blocks compressed independently (`kBlockSize`).
const BLOCK_SIZE: usize = 1 << 16;
/// The largest hash table (`kMaxHashTableBits`).
const MAX_HASH_TABLE_BITS: u32 = 15;
/// The smallest hash table (`kMinHashTableSize`).
const MIN_HASH_TABLE_SIZE: usize = 1 << 8;
/// The input bytes the main loop keeps in reserve (`kInputMarginBytes`).
const INPUT_MARGIN_BYTES: usize = 15;

/// Compresses `src` into `out` (appending) exactly like snappy's `RawCompress` at its
/// default level (the C++ snappy DuckDB bundles, built without CRC32 instructions):
/// the varint length, then each 64 KiB block compressed on its own.
pub fn compress(src: &[u8], out: &mut Vec<u8>) {
    let mut length = src.len() as u64;
    loop {
        let byte = (length & 0x7f) as u8;
        length >>= 7;
        if length == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    let mut table = Vec::new();
    for fragment in src.chunks(BLOCK_SIZE) {
        let table_size = calculate_table_size(fragment.len());
        table.clear();
        table.resize(table_size, 0u16);
        compress_fragment(fragment, out, &mut table);
    }
}

/// `CalculateTableSize`: the next power of two of the fragment size, within bounds.
fn calculate_table_size(input_size: usize) -> usize {
    if input_size > 1 << MAX_HASH_TABLE_BITS {
        return 1 << MAX_HASH_TABLE_BITS;
    }
    if input_size < MIN_HASH_TABLE_SIZE {
        return MIN_HASH_TABLE_SIZE;
    }
    input_size.next_power_of_two()
}

#[inline]
fn load32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

#[inline]
fn load64(data: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// `TableEntry`: the slot of four bytes in a table of `mask / 2 + 1` entries.
#[inline]
fn table_slot(bytes: u32, mask: u32) -> usize {
    const MAGIC: u32 = 0x1e35_a7bd;
    let hash = MAGIC.wrapping_mul(bytes) >> (31 - MAX_HASH_TABLE_BITS);
    ((hash & mask) >> 1) as usize
}

/// `EmitLiteral`.
fn emit_literal(out: &mut Vec<u8>, literal: &[u8]) {
    let n = literal.len() - 1;
    if n < 60 {
        out.push((n << 2) as u8);
    } else {
        let count = (n.ilog2() >> 3) as usize + 1;
        out.push(((59 + count) << 2) as u8);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..count]);
    }
    out.extend_from_slice(literal);
}

/// `EmitCopyAtMost64`.
fn emit_copy_at_most_64(out: &mut Vec<u8>, offset: usize, len: usize) {
    if len < 12 {
        if offset < 2048 {
            out.push((((len - 4) << 2) | 1 | ((offset >> 8) << 5)) as u8);
            out.push(offset as u8);
        } else {
            out.push((((len - 1) << 2) | 2) as u8);
            out.extend_from_slice(&(offset as u16).to_le_bytes());
        }
    } else {
        out.push((((len - 1) << 2) | 2) as u8);
        out.extend_from_slice(&(offset as u16).to_le_bytes());
    }
}

/// `EmitCopy`: copies longer than 64 bytes as several, keeping at least 4 for the last.
fn emit_copy(out: &mut Vec<u8>, offset: usize, mut len: usize) {
    if len >= 12 {
        while len >= 68 {
            emit_copy_at_most_64(out, offset, 64);
            len -= 64;
        }
        if len > 64 {
            emit_copy_at_most_64(out, offset, 60);
            len -= 60;
        }
    }
    emit_copy_at_most_64(out, offset, len);
}

/// The number of equal bytes at `a` and `b` (`b` before `a`), up to the end of `data`
/// (`FindMatchLength`).
fn match_length(data: &[u8], earlier: usize, at: usize) -> usize {
    let limit = data.len();
    let mut matched = 0;
    while at + matched + 8 <= limit {
        let x = load64(data, earlier + matched) ^ load64(data, at + matched);
        if x != 0 {
            return matched + (x.trailing_zeros() >> 3) as usize;
        }
        matched += 8;
    }
    while at + matched < limit && data[earlier + matched] == data[at + matched] {
        matched += 1;
    }
    matched
}

/// `CompressFragment`: one block, its copies only within it.
fn compress_fragment(input: &[u8], out: &mut Vec<u8>, table: &mut [u16]) {
    let input_size = input.len();
    let mask = 2 * (table.len() as u32 - 1);
    let mut ip = 0usize;
    let ip_end = input_size;
    if input_size >= INPUT_MARGIN_BYTES {
        let ip_limit = input_size - INPUT_MARGIN_BYTES;
        'main: loop {
            // Bytes in [next_emit, ip) are emitted as a literal.
            let next_emit = ip;
            ip += 1;
            let mut skip: u32 = 32;
            let mut candidate;
            let mut found = None;
            if ip_limit >= ip + 16 {
                let delta = ip;
                for i in 0..16 {
                    let dword = load32(input, ip + i);
                    let slot = table_slot(dword, mask);
                    candidate = usize::from(table[slot]);
                    table[slot] = (delta + i) as u16;
                    if load32(input, candidate) == dword {
                        found = Some((i, candidate));
                        break;
                    }
                }
                if found.is_none() {
                    ip += 16;
                    skip += 16;
                }
            }
            if let Some((i, matched_candidate)) = found {
                // A literal of the `i + 1` bytes before the match.
                out.push((i << 2) as u8);
                out.extend_from_slice(&input[next_emit..=next_emit + i]);
                ip += i;
                candidate = matched_candidate;
            } else {
                loop {
                    let data = load32(input, ip);
                    let slot = table_slot(data, mask);
                    let bytes_between_hash_lookups = (skip >> 5) as usize;
                    skip += skip >> 5;
                    let next_ip = ip + bytes_between_hash_lookups;
                    if next_ip > ip_limit {
                        ip = next_emit;
                        break 'main;
                    }
                    candidate = usize::from(table[slot]);
                    table[slot] = ip as u16;
                    if data == load32(input, candidate) {
                        break;
                    }
                    ip = next_ip;
                }
                emit_literal(out, &input[next_emit..ip]);
            }
            // Copies, as long as the bytes after one match again.
            loop {
                let base = ip;
                let matched = 4 + match_length(input, candidate + 4, ip + 4);
                ip += matched;
                emit_copy(out, base - candidate, matched);
                if ip >= ip_limit {
                    break 'main;
                }
                table[table_slot(load32(input, ip - 1), mask)] = (ip - 1) as u16;
                let slot = table_slot(load32(input, ip), mask);
                candidate = usize::from(table[slot]);
                table[slot] = ip as u16;
                if load32(input, ip) != load32(input, candidate) {
                    break;
                }
            }
        }
    }
    if ip < ip_end {
        emit_literal(out, &input[ip..ip_end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(data: &[u8]) {
        let compressed = snap::raw::Encoder::new().compress_vec(data).unwrap();
        assert_eq!(decompress_len(&compressed).unwrap(), data.len());
        let mut output = vec![0xaa; data.len()];
        decompress(&compressed, &mut output).unwrap();
        assert_eq!(output, data);
    }

    /// A deterministic pseudo-random byte generator (xorshift).
    fn bytes(seed: u64, len: usize, alphabet: u8) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % u64::from(alphabet)) as u8
            })
            .collect()
    }

    #[test]
    fn decompresses_what_snap_compresses() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(&vec![7; 100_000]);
        round_trip(&bytes(1, 100_000, 255));
        round_trip(&bytes(2, 100_000, 4));
        round_trip(&bytes(3, 70_000, 16));
        let text: Vec<u8> = (0..20_000)
            .flat_map(|i: u32| format!("str_{} {:x};", i % 777, i.wrapping_mul(2_654_435_761)).into_bytes())
            .collect();
        round_trip(&text);
        for len in 0..300 {
            round_trip(&bytes(len as u64 + 10, len, 3));
        }
    }

    /// Throughput (`cargo test --release -- --ignored snappy_speed`) on md5-like strings,
    /// or on the pages dumped in the file `SNAPPY_PAGES` (u32 compressed and
    /// uncompressed lengths, then the bytes, per page).
    #[test]
    #[ignore = "benchmark"]
    fn snappy_speed() {
        let pages: Vec<(Vec<u8>, usize)> = if let Ok(path) = std::env::var("SNAPPY_PAGES") {
            let dump = std::fs::read(path).unwrap();
            let mut pages = Vec::new();
            let mut position = 0;
            while position < dump.len() {
                let word = |at: usize| u32::from_le_bytes(dump[at..at + 4].try_into().unwrap()) as usize;
                let (compressed, uncompressed) = (word(position), word(position + 4));
                pages.push((
                    dump[position + 8..position + 8 + compressed].to_vec(),
                    uncompressed,
                ));
                position += 8 + compressed;
            }
            pages
        } else {
            let mut page = Vec::new();
            for value in bytes(9, 200_000 * 32, 16).chunks(32) {
                page.extend_from_slice(&32u32.to_le_bytes());
                page.extend(value.iter().map(|digit| b"0123456789abcdef"[*digit as usize]));
            }
            let len = page.len();
            vec![(snap::raw::Encoder::new().compress_vec(&page).unwrap(), len)]
        };
        let total: usize = pages.iter().map(|(_, len)| len).sum();
        let largest = pages.iter().map(|(_, len)| *len).max().unwrap_or(0);
        let mut output = vec![0; largest];
        let mut expected = vec![0; largest];
        for name in ["ours", "snap", "ours", "snap"] {
            let start = std::time::Instant::now();
            for (compressed, len) in &pages {
                if name == "ours" {
                    decompress(compressed, &mut output[..*len]).unwrap();
                } else {
                    snap::raw::Decoder::new()
                        .decompress(compressed, &mut expected[..*len])
                        .unwrap();
                }
            }
            let seconds = start.elapsed().as_secs_f64();
            println!("{name}: {:.0} MB/s", total as f64 / seconds / 1e6);
        }
        assert_eq!(output, expected);
    }

    #[test]
    fn decompresses_mixed_streams() {
        // Long literals, runs, near and far copies interleaved, so the fast loop leaves
        // and re-enters many times.
        for seed in 0..200u64 {
            let mut data = Vec::new();
            let mut state = seed * 7919 + 1;
            while data.len() < 20_000 + (seed as usize % 7) * 3_000 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let len = (state % 300) as usize + 1;
                match state % 5 {
                    0 => data.extend(bytes(state, len, 255)),
                    1 => data.extend(std::iter::repeat_n((state >> 8) as u8, len)),
                    2 if data.len() > 70_000 / 10 => {
                        let from = (state as usize >> 4) % (data.len() - 64);
                        let chunk: Vec<u8> = data[from..from + 64.min(len)].to_vec();
                        data.extend(chunk);
                    }
                    3 => data.extend(bytes(state, len, 4)),
                    _ => data.extend(format!("value_{}_", state % 1000).into_bytes()),
                }
            }
            round_trip(&data);
        }
    }

    /// Writes `compress` of the file `SNAPPY_COMPRESS_IN` to `SNAPPY_COMPRESS_OUT`, to
    /// compare with the C++ snappy (`cargo test -- --ignored snappy_compress_file`).
    #[test]
    #[ignore = "comparison tool"]
    fn snappy_compress_file() {
        let input = std::fs::read(std::env::var("SNAPPY_COMPRESS_IN").unwrap()).unwrap();
        let mut output = Vec::new();
        compress(&input, &mut output);
        std::fs::write(std::env::var("SNAPPY_COMPRESS_OUT").unwrap(), &output).unwrap();
    }

    #[test]
    fn compresses_what_it_decompresses() {
        for seed in 0..50u64 {
            let mut data = bytes(seed, 70_000 + seed as usize * 1000, (seed % 200) as u8 + 2);
            data.extend(std::iter::repeat_n(7u8, 1000));
            data.extend(bytes(seed + 1, 100, 255));
            for len in [0, 1, 14, 15, 16, 17, 100, 65_536, 65_537, data.len()] {
                let input = &data[..len.min(data.len())];
                let mut compressed = Vec::new();
                compress(input, &mut compressed);
                let mut output = vec![0; input.len()];
                decompress(&compressed, &mut output).unwrap();
                assert_eq!(output, input);
            }
        }
    }

    #[test]
    fn rejects_corrupt_blocks_without_panicking() {
        let data = bytes(5, 5000, 6);
        let compressed = snap::raw::Encoder::new().compress_vec(&data).unwrap();
        let mut output = vec![0; data.len()];
        assert_eq!(
            decompress(&compressed, &mut output[..10]),
            Err(SnappyError::LengthMismatch)
        );
        assert!(decompress(&compressed[..compressed.len() / 2], &mut output).is_err());
        for seed in 0..2000 {
            let mut corrupt = compressed.clone();
            let noise = bytes(seed, 4, 255);
            let position = (seed as usize * 7919) % corrupt.len();
            corrupt[position] ^= noise[0] | 1;
            let _ = decompress(&corrupt, &mut output);
            let garbage = bytes(seed + 77, 64, 255);
            let _ = decompress(&garbage, &mut output);
        }
    }
}
