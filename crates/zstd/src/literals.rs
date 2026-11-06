//! The literals section of a block (`zstd_compress_literals.c`).

use crate::huf::{self, Repeat};
use crate::params::Strategy;

/// The Huffman table of a block and whether the next may reuse it (`ZSTD_hufCTables_t`).
#[derive(Clone, Debug)]
pub(crate) struct HufTables {
    pub table: huf::CTable,
    pub repeat: Repeat,
}

impl Default for HufTables {
    fn default() -> Self {
        Self {
            table: huf::CTable::default(),
            repeat: Repeat::None,
        }
    }
}

/// `set_basic`, `set_rle`, `set_compressed`, `set_repeat` (`symbolEncodingType_e`).
pub(crate) const SET_BASIC: u32 = 0;
pub(crate) const SET_RLE: u32 = 1;
pub(crate) const SET_COMPRESSED: u32 = 2;
pub(crate) const SET_REPEAT: u32 = 3;

/// `ZSTD_minGain`.
pub(crate) fn min_gain(src_size: usize, strategy: Strategy) -> usize {
    let min_log = if strategy >= Strategy::BtUltra {
        strategy as u32 - 1
    } else {
        6
    };
    (src_size >> min_log) + 2
}

/// `ZSTD_noCompressLiterals`.
fn no_compress(out: &mut Vec<u8>, src: &[u8]) -> usize {
    write_raw_or_rle_header(out, SET_BASIC, src.len());
    out.extend_from_slice(src);
    out.len()
}

fn write_raw_or_rle_header(out: &mut Vec<u8>, kind: u32, size: usize) {
    let size = size as u32;
    match 1 + usize::from(size > 31) + usize::from(size > 4095) {
        1 => out.push((kind + (size << 3)) as u8),
        2 => out.extend_from_slice(&((kind + (1 << 2) + (size << 4)) as u16).to_le_bytes()),
        _ => out.extend_from_slice(&(kind + (3 << 2) + (size << 4)).to_le_bytes()[..3]),
    }
}

/// `ZSTD_minLiteralsToCompress`.
fn min_literals_to_compress(strategy: Strategy, repeat: Repeat) -> usize {
    let shift = (9 - strategy as u32).min(3);
    if repeat == Repeat::Valid { 6 } else { 8 << shift }
}

/// `ZSTD_compressLiterals`: appends the literals section; returns its size. `next` starts
/// as a copy of `prev` and receives a new table when one is built.
pub(crate) fn compress_literals(
    out: &mut Vec<u8>,
    src: &[u8],
    prev: &HufTables,
    next: &mut HufTables,
    strategy: Strategy,
    disable_compression: bool,
    suspect_uncompressible: bool,
) -> usize {
    let start = out.len();
    let src_size = src.len();
    let lh_size = 3 + usize::from(src_size >= 1 << 10) + usize::from(src_size >= 16 << 10);
    let mut single_stream = src_size < 256;
    next.clone_from(prev);
    if disable_compression || src_size < min_literals_to_compress(strategy, prev.repeat) {
        return no_compress(out, src) - start;
    }
    let mut repeat = prev.repeat;
    let flags = if strategy < Strategy::Lazy && src_size <= 1024 {
        huf::FLAG_PREFER_REPEAT
    } else {
        0
    } | if strategy >= Strategy::BtUltra {
        huf::FLAG_OPTIMAL_DEPTH
    } else {
        0
    } | if suspect_uncompressible {
        huf::FLAG_SUSPECT_UNCOMPRESSIBLE
    } else {
        0
    };
    if repeat == Repeat::Valid && lh_size == 3 {
        single_stream = true;
    }
    out.resize(start + lh_size, 0);
    let compressed = huf::compress(out, src, !single_stream, &mut next.table, &mut repeat, flags);
    let mut kind = SET_COMPRESSED;
    if repeat != Repeat::None {
        kind = SET_REPEAT;
    }
    let min_gain = min_gain(src_size, strategy);
    let compressed_size = match compressed {
        Some(size) if size != 0 && size < src_size - min_gain => size,
        _ => {
            out.truncate(start);
            next.clone_from(prev);
            return no_compress(out, src) - start;
        }
    };
    if compressed_size == 1 && (src_size >= 8 || src.iter().all(|&byte| byte == src[0])) {
        out.truncate(start);
        next.clone_from(prev);
        write_raw_or_rle_header(out, SET_RLE, src_size);
        out.push(src[0]);
        return out.len() - start;
    }
    if kind == SET_COMPRESSED {
        next.repeat = Repeat::Check;
    }
    let size = src_size as u32;
    let compressed = compressed_size as u32;
    let header = &mut out[start..start + lh_size];
    match lh_size {
        3 => {
            let lhc = kind + (u32::from(!single_stream) << 2) + (size << 4) + (compressed << 14);
            header.copy_from_slice(&lhc.to_le_bytes()[..3]);
        }
        4 => {
            let lhc = kind + (2 << 2) + (size << 4) + (compressed << 18);
            header.copy_from_slice(&lhc.to_le_bytes());
        }
        _ => {
            let lhc = kind + (3 << 2) + (size << 4) + (compressed << 22);
            header[..4].copy_from_slice(&lhc.to_le_bytes());
            header[4] = (compressed >> 10) as u8;
        }
    }
    lh_size + compressed_size
}
