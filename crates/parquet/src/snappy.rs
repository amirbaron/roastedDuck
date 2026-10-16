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
    let (mut ip, mut op) = decompress_fast(src, dst, header)?;
    while ip < src.len() {
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

/// Input bytes the fast loop needs after a tag: the longest tag (5 bytes) or a tag with
/// a 16-byte literal.
const FAST_INPUT_SLACK: usize = 17;
/// Output bytes the fast loop needs: the longest copy (64 bytes) with a 16-byte overrun.
const FAST_OUTPUT_SLACK: usize = 80;

/// The main loop of [`decompress`] while both buffers have room for the longest element
/// plus an overrun (`SnappyDecompressor::DecompressAllTags`): no per-byte bounds checks.
/// Returns the input and output positions where the careful loop takes over.
fn decompress_fast(src: &[u8], dst: &mut [u8], mut ip: usize) -> Result<(usize, usize), SnappyError> {
    let mut op = 0;
    let src_ptr = src.as_ptr();
    let dst_ptr = dst.as_mut_ptr();
    while ip + FAST_INPUT_SLACK <= src.len() && op + FAST_OUTPUT_SLACK <= dst.len() {
        // SAFETY: `ip + 17 <= src.len()`, so the tag and the up to 16 bytes after it are
        // in bounds.
        let tag = unsafe { *src_ptr.add(ip) };
        let (length, offset) = match tag & 3 {
            0 => {
                let literal = usize::from(tag >> 2) + 1;
                if literal <= 16 {
                    // SAFETY: 16 bytes after the tag are readable (checked above) and
                    // `op + 80 <= dst.len()`; the buffers do not overlap.
                    unsafe { std::ptr::copy_nonoverlapping(src_ptr.add(ip + 1), dst_ptr.add(op), 16) };
                    ip += 1 + literal;
                    op += literal;
                    continue;
                }
                if literal > 60 || ip + 1 + literal > src.len() || op + literal > dst.len() {
                    // Long literals take the careful path.
                    break;
                }
                // SAFETY: both ranges were checked just above; the buffers do not overlap.
                unsafe { std::ptr::copy_nonoverlapping(src_ptr.add(ip + 1), dst_ptr.add(op), literal) };
                ip += 1 + literal;
                op += literal;
                continue;
            }
            // SAFETY (all three arms): the tag is followed by at least 16 readable bytes.
            1 => {
                let byte = unsafe { *src_ptr.add(ip + 1) };
                ip += 2;
                (
                    usize::from((tag >> 2) & 7) + 4,
                    (usize::from(tag >> 5) << 8) | usize::from(byte),
                )
            }
            2 => {
                let bytes = unsafe { std::ptr::read_unaligned(src_ptr.add(ip + 1).cast::<[u8; 2]>()) };
                ip += 3;
                (usize::from(tag >> 2) + 1, usize::from(u16::from_le_bytes(bytes)))
            }
            _ => {
                let bytes = unsafe { std::ptr::read_unaligned(src_ptr.add(ip + 1).cast::<[u8; 4]>()) };
                ip += 5;
                (usize::from(tag >> 2) + 1, u32::from_le_bytes(bytes) as usize)
            }
        };
        if offset == 0 || offset > op {
            return Err(SnappyError::Corrupt);
        }
        // `length <= 64` and `op + 80 <= dst.len()`: there is room for copying up to 16
        // bytes past the end of the match.
        let from = op - offset;
        if offset >= 16 {
            let mut copied = 0;
            while copied < length {
                // SAFETY: the chunk ends before `op + length + 16 <= dst.len()`, and the
                // source chunk ends at or before the destination chunk starts (the offset is
                // at least 16), so they do not overlap.
                unsafe {
                    std::ptr::copy_nonoverlapping(dst_ptr.add(from + copied), dst_ptr.add(op + copied), 16);
                }
                copied += 16;
            }
        } else if offset >= 8 {
            let mut copied = 0;
            while copied < length {
                // SAFETY: as above with 8-byte chunks and an offset of at least 8.
                unsafe {
                    std::ptr::copy_nonoverlapping(dst_ptr.add(from + copied), dst_ptr.add(op + copied), 8);
                }
                copied += 8;
            }
        } else {
            for index in 0..length {
                // SAFETY: `from + index < op + index < op + length <= dst.len()`.
                unsafe { *dst_ptr.add(op + index) = *dst_ptr.add(from + index) };
            }
        }
        op += length;
    }
    Ok((ip, op))
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

    /// Throughput on md5-like strings (`cargo test --release -- --ignored snappy_speed`).
    #[test]
    #[ignore = "benchmark"]
    fn snappy_speed() {
        let mut page = Vec::new();
        for value in bytes(9, 200_000 * 32, 16).chunks(32) {
            page.extend_from_slice(&32u32.to_le_bytes());
            page.extend(value.iter().map(|digit| b"0123456789abcdef"[*digit as usize]));
        }
        let compressed = snap::raw::Encoder::new().compress_vec(&page).unwrap();
        let mut output = vec![0; page.len()];
        for name in ["ours", "snap", "ours", "snap"] {
            let start = std::time::Instant::now();
            for _ in 0..20 {
                if name == "ours" {
                    decompress(&compressed, &mut output).unwrap();
                } else {
                    snap::raw::Decoder::new()
                        .decompress(&compressed, &mut output)
                        .unwrap();
                }
            }
            let seconds = start.elapsed().as_secs_f64();
            println!("{name}: {:.0} MB/s", 20.0 * page.len() as f64 / seconds / 1e6);
        }
        assert_eq!(output, page);
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
