//! Entropy-cost block splitting and partition repcode reconciliation (`zstd_compress.c`).

use crate::block::{BlockCompressor, BlockState, entropy_compress, is_rle};
use crate::fse;
use crate::hist;
use crate::huf::{self, Repeat};
use crate::params::{CParams, Strategy};
use crate::seq::{LL_BITS, LongLength, ML_BITS, SeqStore};
use crate::sequences::{self, FseRepeat};
use crate::window::Mem;

fn literal_cost(src: &[u8], prev: &crate::literals::HufTables, strategy: Strategy) -> Option<usize> {
    if src.len() <= if prev.repeat == Repeat::Valid { 6 } else { 63 } {
        return Some(src.len());
    }
    let mut count = [0u32; 256];
    let mut max = 255;
    let largest = hist::count(&mut count, &mut max, src) as usize;
    if largest == src.len() {
        return Some(1);
    }
    if largest <= (src.len() >> 7) + 4 {
        return Some(src.len());
    }
    let mut repeat = prev.repeat;
    if repeat == Repeat::Check && !huf::validate_ctable(&prev.table, &count, max) {
        repeat = Repeat::None;
    }
    let flags = if strategy >= Strategy::BtUltra {
        huf::FLAG_OPTIMAL_DEPTH
    } else {
        0
    };
    let log = huf::optimal_table_log(huf::HUF_TABLELOG_DEFAULT, src.len(), max, &count, flags);
    let mut table = huf::CTable::default();
    let bits = huf::build_ctable(&mut table, &count, max, log);
    let new_size = huf::estimate_compressed_size(&table, &count, max);
    let mut header = Vec::new();
    let header_size = huf::write_ctable(&mut header, &table, max, bits)?;
    let data_size = if repeat != Repeat::None {
        let old_size = huf::estimate_compressed_size(&prev.table, &count, max);
        if old_size < src.len() && (old_size <= header_size + new_size || header_size + 12 >= src.len()) {
            old_size
        } else if new_size + header_size >= src.len() {
            return Some(src.len());
        } else {
            new_size + header_size
        }
    } else if new_size + header_size >= src.len() {
        return Some(src.len());
    } else {
        new_size + header_size
    };
    Some(
        data_size
            + usize::from(src.len() >= 256) * 6
            + 3
            + usize::from(src.len() >= 1024)
            + usize::from(src.len() >= 16_384),
    )
}

fn symbol_cost(
    kind: u32,
    codes: &[u8],
    table: &fse::CTable,
    extra: Option<&[u8]>,
    norm: &[i16],
    log: u32,
) -> usize {
    let mut count = [0u32; 53];
    let mut max = 0;
    for &code in codes {
        count[usize::from(code)] += 1;
        max = max.max(u32::from(code));
    }
    let cost = match kind {
        0 => Some(sequences::cross_entropy_cost(norm, log, &count, max)),
        1 => Some(0),
        _ => sequences::fse_bit_cost(table, &count, max),
    };
    let Some(mut cost) = cost else {
        return codes.len() * 10;
    };
    for &code in codes {
        cost += extra.map_or(usize::from(code), |bits| usize::from(bits[usize::from(code)]));
    }
    cost >> 3
}

fn estimate(seq: &SeqStore, prev: &BlockState, params: CParams) -> Option<usize> {
    let literals = literal_cost(&seq.literals, &prev.huf, params.strategy)?;
    let mut next = prev.fse.clone();
    let mut header = Vec::new();
    let types = sequences::build_statistics(&mut header, seq, &prev.fse, &mut next, params.strategy)?;
    let of = symbol_cost(
        types.of,
        &seq.of_code,
        &next.offcode,
        None,
        &sequences::OF_DEFAULT_NORM,
        sequences::OF_DEFAULT_NORM_LOG,
    );
    let ll = symbol_cost(
        types.ll,
        &seq.ll_code,
        &next.litlength,
        Some(&LL_BITS),
        &sequences::LL_DEFAULT_NORM,
        sequences::LL_DEFAULT_NORM_LOG,
    );
    let ml = symbol_cost(
        types.ml,
        &seq.ml_code,
        &next.matchlength,
        Some(&ML_BITS),
        &sequences::ML_DEFAULT_NORM,
        sequences::ML_DEFAULT_NORM_LOG,
    );
    let n = seq.sequences.len();
    Some(literals + of + ll + ml + header.len() + 2 + usize::from(n >= 128) + usize::from(n >= 0x7f00) + 3)
}

fn lengths(seq: &SeqStore, index: usize) -> (usize, usize) {
    let def = seq.sequences[index];
    let long = index == seq.long_length_pos;
    (
        usize::from(def.lit_length)
            + if long && seq.long_length == LongLength::Literal {
                65_536
            } else {
                0
            },
        usize::from(def.ml_base)
            + 3
            + if long && seq.long_length == LongLength::Match {
                65_536
            } else {
                0
            },
    )
}

fn chunk(seq: &SeqStore, start: usize, end: usize) -> SeqStore {
    let literal_start: usize = (0..start).map(|i| lengths(seq, i).0).sum();
    let literal_end = if end == seq.sequences.len() {
        seq.literals.len()
    } else {
        literal_start + (start..end).map(|i| lengths(seq, i).0).sum::<usize>()
    };
    let mut result = SeqStore::new();
    result.sequences.extend_from_slice(&seq.sequences[start..end]);
    result
        .literals
        .extend_from_slice(&seq.literals[literal_start..literal_end]);
    // A C++ view may retain an end-boundary long marker whose code write lands
    // in the next view. Owning slices retain only the marker within this view.
    if seq.long_length != LongLength::None && (start..end).contains(&seq.long_length_pos) {
        result.long_length = seq.long_length;
        result.long_length_pos = seq.long_length_pos - start;
    }
    result.compute_codes();
    result
}

fn derive(
    parts: &mut Vec<usize>,
    seq: &SeqStore,
    prev: &BlockState,
    params: CParams,
    start: usize,
    end: usize,
) {
    if end - start < 300 || parts.len() >= 196 {
        return;
    }
    let middle = usize::midpoint(start, end);
    let full = estimate(&chunk(seq, start, end), prev, params);
    let left = estimate(&chunk(seq, start, middle), prev, params);
    let right = estimate(&chunk(seq, middle, end), prev, params);
    if let (Some(full), Some(left), Some(right)) = (full, left, right)
        && left + right < full
    {
        derive(parts, seq, prev, params, start, middle);
        parts.push(middle);
        derive(parts, seq, prev, params, middle, end);
    }
}

fn raw_offset(rep: [u32; 3], off: u32, ll0: bool) -> u32 {
    if off > 3 {
        off - 3
    } else {
        let code = off - 1 + u32::from(ll0);
        if code == 3 { rep[0] - 1 } else { rep[code as usize] }
    }
}

fn resolve(seq: &mut SeqStore, decoder: &mut [u32; 3], coder: &mut [u32; 3]) {
    for i in 0..seq.sequences.len() {
        let ll0 = lengths(seq, i).0 == 0;
        let off = seq.sequences[i].off_base;
        if off <= 3 && raw_offset(*decoder, off, ll0) != raw_offset(*coder, off, ll0) {
            seq.sequences[i].off_base = raw_offset(*coder, off, ll0) + 3;
        }
        *decoder = crate::opt::new_rep(*decoder, seq.sequences[i].off_base, ll0);
        *coder = crate::opt::new_rep(*coder, off, ll0);
    }
}

#[allow(clippy::too_many_arguments, reason = "the frozen partition encoder's state")]
fn encode(
    out: &mut Vec<u8>,
    seq: &mut SeqStore,
    prev: &mut BlockState,
    next: &mut BlockState,
    params: CParams,
    src: &[u8],
    last: bool,
    first: bool,
) -> bool {
    let header = out.len();
    out.extend_from_slice(&[0; 3]);
    let body_start = out.len();
    let mut size = entropy_compress(out, seq, prev, next, &params, src.len());
    if !first && size < 25 && is_rle(src) {
        out.truncate(body_start);
        out.push(src[0]);
        size = 1;
    }
    let (kind, body) = if size == 0 {
        out.extend_from_slice(src);
        (0, src.len())
    } else if size == 1 {
        (1, src.len())
    } else {
        std::mem::swap(prev, next);
        (2, size)
    };
    let bits = u32::from(last) + (kind << 1) + ((body as u32) << 3);
    out[header..header + 3].copy_from_slice(&bits.to_le_bytes()[..3]);
    if prev.fse.offcode_repeat == FseRepeat::Valid {
        prev.fse.offcode_repeat = FseRepeat::Check;
    }
    size > 1
}

pub(crate) fn compress(
    block: &mut BlockCompressor,
    out: &mut Vec<u8>,
    mem: Mem<'_>,
    start: usize,
    size: usize,
    last: bool,
) {
    if size < 7 {
        let header = u32::from(last) + ((size as u32) << 3);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(&mem.cur[start..start + size]);
        return;
    }
    block.build_seq_store(mem, start, size);
    block.seq.compute_codes();
    let params = block.ms.params;
    let mut partitions = Vec::new();
    derive(
        &mut partitions,
        &block.seq,
        &block.prev,
        params,
        0,
        block.seq.sequences.len(),
    );
    if partitions.is_empty() {
        encode(
            out,
            &mut block.seq,
            &mut block.prev,
            &mut block.next,
            params,
            &mem.cur[start..start + size],
            last,
            block.is_first_block,
        );
        return;
    }
    partitions.push(block.seq.sequences.len());
    let mut decoder = block.prev.rep;
    let mut coder = decoder;
    let mut seq_start = 0;
    let mut src_pos = start;
    for (ordinal, &seq_end) in partitions.iter().enumerate() {
        let mut part = chunk(&block.seq, seq_start, seq_end);
        let consumed: usize = (0..part.sequences.len())
            .map(|i| {
                let (a, b) = lengths(&part, i);
                a + b
            })
            .sum();
        let final_part = ordinal + 1 == partitions.len();
        let bytes = if final_part {
            start + size - src_pos
        } else {
            consumed
        };
        let original_decoder = decoder;
        resolve(&mut part, &mut decoder, &mut coder);
        block.next.rep = decoder;
        let compressed = encode(
            out,
            &mut part,
            &mut block.prev,
            &mut block.next,
            params,
            &mem.cur[src_pos..src_pos + bytes],
            last && final_part,
            block.is_first_block,
        );
        if !compressed {
            decoder = original_decoder;
        }
        block.prev.rep = decoder;
        src_pos += bytes;
        seq_start = seq_end;
    }
}
