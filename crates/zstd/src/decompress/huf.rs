//! Huffman decoding of literals (`huf_decompress.c`): single-symbol (X1) and
//! double-symbol (X2) tables, one or four streams, with zstd's fast four-stream loops.

use super::bits::{BitReader, Status, highbit32, read64};
use super::entropy::{HUF_SYMBOLVALUE_MAX, HUF_TABLELOG_MAX, HufStats, read_stats};
use crate::error::{Error, Result};

/// `HUF_DECODER_FAST_TABLELOG`: the table log of the fast loops.
const FAST_TABLE_LOG: u32 = 11;
/// `ZSTD_HUFFDTABLE_CAPACITY_LOG`.
const CAPACITY_LOG: u32 = 12;
const TABLE_SIZE: usize = 1 << CAPACITY_LOG;

/// A Huffman decoding table (`HUF_DTable` with its `DTableDesc`), kept between blocks for
/// `set_repeat` literals.
#[derive(Clone)]
pub(crate) struct HufTable {
    /// Double-symbol entries (`tableType` 1) or single-symbol ones.
    double: bool,
    table_log: u32,
    /// `HUF_DEltX1`: the bit count in the low byte, the symbol in the high byte.
    x1: Box<[u16; TABLE_SIZE]>,
    /// `HUF_DEltX2`: the sequence in the low 16 bits, then the bit count and the length.
    x2: Box<[u32; TABLE_SIZE]>,
    stats: Box<HufStats>,
}

impl std::fmt::Debug for HufTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HufTable")
            .field("double", &self.double)
            .field("table_log", &self.table_log)
            .finish_non_exhaustive()
    }
}

impl Default for HufTable {
    fn default() -> Self {
        Self {
            double: false,
            table_log: 0,
            x1: Box::new([0; TABLE_SIZE]),
            x2: Box::new([0; TABLE_SIZE]),
            stats: Box::default(),
        }
    }
}

#[inline(always)]
fn read16(buf: &[u8], pos: usize) -> usize {
    usize::from(u16::from_le_bytes([buf[pos], buf[pos + 1]]))
}

/// `HUF_rescaleStats`: raises the table log to `target` (when below it).
fn rescale_stats(stats: &mut HufStats, target: u32) -> u32 {
    let table_log = stats.table_log;
    if table_log > target {
        return table_log;
    }
    if table_log < target {
        let scale = target - table_log;
        for weight in &mut stats.weights[..stats.nb_symbols as usize] {
            if *weight != 0 {
                *weight += scale as u8;
            }
        }
        let rank = &mut stats.rank_stats;
        for s in (scale + 1..=target).rev() {
            rank[s as usize] = rank[(s - scale) as usize];
        }
        for s in 1..=scale {
            rank[s as usize] = 0;
        }
    }
    target
}

/// `HUF_buildDEltX2U32`.
#[inline(always)]
fn delt_x2(symbol: u32, nb_bits: u32, base_seq: u32, level: u32) -> u32 {
    let seq = if level == 1 {
        symbol
    } else {
        base_seq + (symbol << 8)
    };
    seq + (nb_bits << 16) + (level << 24)
}

/// `HUF_fillDTableX2ForWeight`: the entries of `symbols`, each `nb_bits` long.
fn fill_x2_for_weight(
    dt: &mut [u32],
    symbols: &[u8],
    nb_bits: u32,
    table_log: u32,
    base_seq: u32,
    level: u32,
) {
    let length = 1usize << ((table_log - nb_bits) & 0x1F);
    for (chunk, &symbol) in dt.chunks_mut(length).zip(symbols) {
        chunk.fill(delt_x2(u32::from(symbol), nb_bits, base_seq, level));
    }
}

/// `HUF_fillDTableX2Level2`: the second symbols after `base_seq`, which took
/// `consumed_bits`.
#[allow(clippy::too_many_arguments, reason = "zstd's signature")]
fn fill_x2_level2(
    dt: &mut [u32],
    target_log: u32,
    consumed_bits: u32,
    rank_val: &[u32],
    min_weight: usize,
    max_weight1: usize,
    sorted: &[u8],
    rank_start: &[u32],
    nb_bits_baseline: u32,
    base_seq: u32,
) {
    if min_weight > 1 {
        // Positions whose second symbol would not fit hold the first symbol alone.
        let skip = rank_val[min_weight] as usize;
        dt[..skip].fill(delt_x2(base_seq, consumed_bits, 0, 1));
    }
    for w in min_weight..max_weight1 {
        let (begin, end) = (rank_start[w] as usize, rank_start[w + 1] as usize);
        let nb_bits = nb_bits_baseline - w as u32;
        fill_x2_for_weight(
            &mut dt[rank_val[w] as usize..],
            &sorted[begin..end],
            nb_bits + consumed_bits,
            target_log,
            base_seq,
            2,
        );
    }
}

impl HufTable {
    /// `HUF_readDTableX1_wksp`: reads the table at the start of `src`; returns its size.
    fn read_x1(&mut self, src: &[u8]) -> Result<usize> {
        let stats = &mut *self.stats;
        let i_size = read_stats(stats, src)?;
        // `maxTableLog + 1`, at most the fast loops' table log.
        let table_log = rescale_stats(stats, (CAPACITY_LOG + 1).min(FAST_TABLE_LOG));
        if table_log > CAPACITY_LOG + 1 {
            return Err(Error::TableLogTooLarge);
        }
        self.double = false;
        self.table_log = table_log;
        let nb_symbols = stats.nb_symbols as usize;
        let rank_val = &stats.rank_stats;
        let mut rank_start = [0u32; HUF_TABLELOG_MAX as usize + 1];
        let mut next = 0u32;
        for n in 0..=table_log as usize {
            rank_start[n] = next;
            next += rank_val[n];
        }
        let mut symbols = [0u8; HUF_SYMBOLVALUE_MAX + 1];
        for (n, &weight) in stats.weights[..nb_symbols].iter().enumerate() {
            let w = weight as usize;
            symbols[rank_start[w] as usize] = n as u8;
            rank_start[w] += 1;
        }
        let dt = &mut *self.x1;
        let mut symbol = rank_val[0] as usize;
        let mut u_start = 0usize;
        for (w, &count) in rank_val.iter().enumerate().take(table_log as usize + 1).skip(1) {
            let count = count as usize;
            let length = (1usize << w) >> 1;
            let nb_bits = (table_log + 1 - w as u32) as u16;
            for &s in &symbols[symbol..symbol + count] {
                dt[u_start..u_start + length].fill(nb_bits | (u16::from(s) << 8));
                u_start += length;
            }
            symbol += count;
        }
        Ok(i_size)
    }

    /// `HUF_readDTableX2_wksp`.
    fn read_x2(&mut self, src: &[u8]) -> Result<usize> {
        let stats = &mut *self.stats;
        let i_size = read_stats(stats, src)?;
        let table_log = stats.table_log;
        let mut max_table_log = CAPACITY_LOG;
        if table_log > max_table_log {
            return Err(Error::TableLogTooLarge);
        }
        if table_log <= FAST_TABLE_LOG && max_table_log > FAST_TABLE_LOG {
            max_table_log = FAST_TABLE_LOG;
        }
        let rank_stats = &stats.rank_stats;
        let mut max_w = table_log as usize;
        while rank_stats[max_w] == 0 {
            max_w -= 1;
        }
        // `rankStart0`: where each weight starts in the sorted symbols (weight 0 last).
        let mut rank_start0 = [0u32; HUF_TABLELOG_MAX as usize + 3];
        let mut next = 0u32;
        for w in 1..=max_w {
            rank_start0[w] = next;
            next += rank_stats[w];
        }
        rank_start0[max_w + 1] = next;
        let mut cursor = rank_start0;
        cursor[0] = next;
        let mut sorted = [0u8; HUF_SYMBOLVALUE_MAX + 1];
        for (s, &weight) in stats.weights[..stats.nb_symbols as usize].iter().enumerate() {
            let w = weight as usize;
            sorted[cursor[w] as usize] = s as u8;
            cursor[w] += 1;
        }
        let mut rank_val = [[0u32; HUF_TABLELOG_MAX as usize + 1]; HUF_TABLELOG_MAX as usize];
        let rescale = max_table_log as i32 - table_log as i32 - 1;
        let mut next_rank_val = 0u32;
        for w in 1..=max_w {
            rank_val[0][w] = next_rank_val;
            next_rank_val += rank_stats[w] << (w as i32 + rescale);
        }
        let min_bits = table_log + 1 - max_w as u32;
        for consumed in min_bits..=max_table_log - min_bits {
            let first = rank_val[0];
            for (value, &start) in rank_val[consumed as usize][1..=max_w]
                .iter_mut()
                .zip(&first[1..=max_w])
            {
                *value = start >> consumed;
            }
        }
        // `HUF_fillDTableX2`.
        let dt = &mut *self.x2;
        let target_log = max_table_log;
        let nb_bits_baseline = table_log + 1;
        let scale_log = nb_bits_baseline as i32 - target_log as i32;
        let min_bits = nb_bits_baseline - max_w as u32;
        let w_end = max_w + 1;
        for w in 1..w_end {
            let (begin, end) = (rank_start0[w] as usize, rank_start0[w + 1] as usize);
            let nb_bits = nb_bits_baseline - w as u32;
            if target_log - nb_bits >= min_bits {
                // Room for a second symbol.
                let mut start = rank_val[0][w] as usize;
                let length = 1usize << ((target_log - nb_bits) & 0x1F);
                let min_weight = (nb_bits as i32 + scale_log).max(1) as usize;
                for &symbol in &sorted[begin..end] {
                    fill_x2_level2(
                        &mut dt[start..],
                        target_log,
                        nb_bits,
                        &rank_val[nb_bits as usize],
                        min_weight,
                        w_end,
                        &sorted,
                        &rank_start0,
                        nb_bits_baseline,
                        u32::from(symbol),
                    );
                    start += length;
                }
            } else {
                fill_x2_for_weight(
                    &mut dt[rank_val[0][w] as usize..],
                    &sorted[begin..end],
                    nb_bits,
                    target_log,
                    0,
                    1,
                );
            }
        }
        self.double = true;
        self.table_log = max_table_log;
        Ok(i_size)
    }

    /// `HUF_decompress1X1_DCtx_wksp`: a new single-symbol table and one stream.
    pub(crate) fn decompress_1x1(&mut self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        let h_size = self.read_x1(src)?;
        if h_size >= src.len() {
            return Err(Error::SrcSizeWrong);
        }
        self.decompress_1x_using_table(dst, &src[h_size..])
    }

    /// `HUF_decompress4X_hufOnly_wksp`: a new table of the faster kind and four streams.
    pub(crate) fn decompress_4x_huf_only(&mut self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        if dst.is_empty() {
            return Err(Error::DstSizeTooSmall);
        }
        if src.is_empty() {
            return Err(Error::CorruptionDetected);
        }
        let h_size = if select_decoder(dst.len(), src.len()) {
            self.read_x2(src)?
        } else {
            self.read_x1(src)?
        };
        if h_size >= src.len() {
            return Err(Error::SrcSizeWrong);
        }
        self.decompress_4x_using_table(dst, &src[h_size..])
    }

    /// `HUF_decompress1X_usingDTable`.
    pub(crate) fn decompress_1x_using_table(&self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        let mut bits = BitReader::new(src)?;
        if self.double {
            decode_stream_x2(dst, &mut bits, &self.x2[..], self.table_log);
        } else {
            decode_stream_x1(dst, &mut bits, &self.x1[..], self.table_log);
        }
        if !bits.finished() {
            return Err(Error::CorruptionDetected);
        }
        Ok(())
    }

    /// `HUF_decompress4X_usingDTable`: the fast loop when it applies, else the generic one.
    pub(crate) fn decompress_4x_using_table(&self, dst: &mut [u8], src: &[u8]) -> Result<()> {
        if let Some(mut args) = FastArgs::new(dst.len(), src, self.table_log)? {
            if self.double {
                fast_loop_x2(&mut args, dst, src, &self.x2[..]);
            } else {
                fast_loop_x1(&mut args, dst, src, &self.x1[..]);
            }
            return args.finish(dst, src, |out, bits| {
                if self.double {
                    decode_stream_x2(out, bits, &self.x2[..], FAST_TABLE_LOG)
                } else {
                    decode_stream_x1(out, bits, &self.x1[..], FAST_TABLE_LOG)
                }
            });
        }
        if self.double {
            decompress_4x_generic::<true>(dst, src, &self.x1[..], &self.x2[..], self.table_log)
        } else {
            decompress_4x_generic::<false>(dst, src, &self.x1[..], &self.x2[..], self.table_log)
        }
    }
}

/// `HUF_selectDecoder`: whether the double-symbol decoder is likely faster.
fn select_decoder(dst_size: usize, c_src_size: usize) -> bool {
    /// `algoTime`: table and per-256-bytes decode times of X1 and X2 by compression ratio.
    const ALGO_TIME: [[(u32, u32); 2]; 16] = [
        [(0, 0), (1, 1)],
        [(0, 0), (1, 1)],
        [(150, 216), (381, 119)],
        [(170, 205), (514, 112)],
        [(177, 199), (539, 110)],
        [(197, 194), (644, 107)],
        [(221, 192), (735, 107)],
        [(256, 189), (881, 106)],
        [(359, 188), (1167, 109)],
        [(582, 187), (1570, 114)],
        [(688, 187), (1712, 122)],
        [(825, 186), (1965, 136)],
        [(976, 185), (2131, 150)],
        [(1180, 186), (2070, 175)],
        [(1377, 185), (1731, 202)],
        [(1412, 185), (1695, 202)],
    ];
    let q = if c_src_size >= dst_size {
        15
    } else {
        c_src_size * 16 / dst_size
    };
    let d256 = (dst_size >> 8) as u32;
    let (t0, d0) = ALGO_TIME[q][0];
    let (t1, d1) = ALGO_TIME[q][1];
    let time0 = t0 + d0 * d256;
    let mut time1 = t1 + d1 * d256;
    time1 += time1 >> 5;
    time1 < time0
}

/// `HUF_decodeStreamX1`: decodes all of `out` from `bits`.
#[inline(always)]
fn decode_stream_x1(out: &mut [u8], bits: &mut BitReader<'_>, dt: &[u16], dt_log: u32) -> usize {
    let decode = |bits: &mut BitReader<'_>| -> u8 {
        let entry = dt[bits.look_fast(dt_log) as usize];
        bits.skip(u32::from(entry & 0xFF));
        (entry >> 8) as u8
    };
    let end = out.len();
    let mut p = 0usize;
    if end > 3 {
        while bits.reload() == Status::Unfinished && p < end - 3 {
            out[p] = decode(bits);
            out[p + 1] = decode(bits);
            out[p + 2] = decode(bits);
            out[p + 3] = decode(bits);
            p += 4;
        }
    } else {
        bits.reload();
    }
    while p < end {
        out[p] = decode(bits);
        p += 1;
    }
    end
}

/// `HUF_decodeSymbolX2`: writes 2 bytes at `p`; returns how many were decoded.
#[inline(always)]
fn decode_symbol_x2(out: &mut [u8], p: usize, bits: &mut BitReader<'_>, dt: &[u32], dt_log: u32) -> usize {
    let entry = dt[bits.look_fast(dt_log) as usize];
    out[p..p + 2].copy_from_slice(&(entry as u16).to_le_bytes());
    bits.skip((entry >> 16) & 0xFF);
    (entry >> 24) as usize
}

/// `HUF_decodeStreamX2`.
#[inline(always)]
fn decode_stream_x2(out: &mut [u8], bits: &mut BitReader<'_>, dt: &[u32], dt_log: u32) -> usize {
    let end = out.len();
    let mut p = 0usize;
    if end >= 8 {
        if dt_log <= 11 {
            while bits.reload() == Status::Unfinished && p + 9 < end {
                for _ in 0..5 {
                    p += decode_symbol_x2(out, p, bits, dt, dt_log);
                }
            }
        } else {
            while bits.reload() == Status::Unfinished && p + 7 < end {
                for _ in 0..4 {
                    p += decode_symbol_x2(out, p, bits, dt, dt_log);
                }
            }
        }
    } else {
        bits.reload();
    }
    if end - p >= 2 {
        while bits.reload() == Status::Unfinished && p + 2 <= end {
            p += decode_symbol_x2(out, p, bits, dt, dt_log);
        }
        while p + 2 <= end {
            p += decode_symbol_x2(out, p, bits, dt, dt_log);
        }
    }
    if p < end {
        // `HUF_decodeLastSymbolX2`.
        let entry = dt[bits.look_fast(dt_log) as usize];
        out[p] = entry as u8;
        let nb_bits = (entry >> 16) & 0xFF;
        if entry >> 24 == 1 {
            bits.skip(nb_bits);
        } else if bits.consumed < 64 {
            bits.skip(nb_bits);
            bits.consumed = bits.consumed.min(64);
        }
        p += 1;
    }
    p
}

/// `HUF_decompress4X1_usingDTable_internal_body` and its X2 twin: four streams, each
/// filling a quarter of `dst`, without the fast loop.
fn decompress_4x_generic<const DOUBLE: bool>(
    dst: &mut [u8],
    src: &[u8],
    x1: &[u16],
    x2: &[u32],
    dt_log: u32,
) -> Result<()> {
    if src.len() < 10 || dst.len() < 6 {
        return Err(Error::CorruptionDetected);
    }
    let (length1, length2, length3) = (read16(src, 0), read16(src, 2), read16(src, 4));
    let Some(length4) = src.len().checked_sub(length1 + length2 + length3 + 6) else {
        return Err(Error::CorruptionDetected);
    };
    let istart1 = 6;
    let istart2 = istart1 + length1;
    let istart3 = istart2 + length2;
    let istart4 = istart3 + length3;
    let dst_size = dst.len();
    let segment = dst_size.div_ceil(4);
    let op_start2 = segment;
    let op_start3 = 2 * segment;
    let op_start4 = 3 * segment;
    if op_start4 > dst_size {
        return Err(Error::CorruptionDetected);
    }
    let mut streams = [
        BitReader::new(&src[istart1..istart2])?,
        BitReader::new(&src[istart2..istart3])?,
        BitReader::new(&src[istart3..istart4])?,
        BitReader::new(&src[istart4..istart4 + length4])?,
    ];
    let starts = [0, op_start2, op_start3, op_start4];
    let mut op = starts;
    if DOUBLE {
        let olimit = dst_size.saturating_sub(7);
        if dst_size - op[3] >= 8 {
            let mut end_signal = true;
            while end_signal && op[3] < olimit {
                for _ in 0..4 {
                    for (o, bits) in op.iter_mut().zip(streams.iter_mut()) {
                        *o += decode_symbol_x2(dst, *o, bits, x2, dt_log);
                    }
                }
                end_signal = streams
                    .iter_mut()
                    .fold(true, |all, bits| (bits.reload_fast() == Status::Unfinished) & all);
            }
        }
    } else {
        let olimit = dst_size.saturating_sub(3);
        if dst_size - op[3] >= 8 {
            let mut end_signal = true;
            while end_signal && op[3] < olimit {
                for _ in 0..4 {
                    for (o, bits) in op.iter_mut().zip(streams.iter_mut()) {
                        let entry = x1[bits.look_fast(dt_log) as usize];
                        bits.skip(u32::from(entry & 0xFF));
                        dst[*o] = (entry >> 8) as u8;
                        *o += 1;
                    }
                }
                end_signal = streams
                    .iter_mut()
                    .fold(true, |all, bits| (bits.reload_fast() == Status::Unfinished) & all);
            }
        }
    }
    if op[0] > op_start2 || op[1] > op_start3 || op[2] > op_start4 {
        return Err(Error::CorruptionDetected);
    }
    let ends = [op_start2, op_start3, op_start4, dst_size];
    for i in 0..4 {
        let out = &mut dst[op[i]..ends[i]];
        if DOUBLE {
            decode_stream_x2(out, &mut streams[i], x2, dt_log);
        } else {
            decode_stream_x1(out, &mut streams[i], x1, dt_log);
        }
    }
    if !streams.iter().all(BitReader::finished) {
        return Err(Error::CorruptionDetected);
    }
    Ok(())
}

/// `HUF_DecompressFastArgs`: the state of the four streams in the fast loops. Input
/// positions are those of the 8 bytes loaded into `bits`, which read from the top bit
/// down and hold a marker bit below the last valid one.
struct FastArgs {
    ip: [usize; 4],
    op: [usize; 4],
    bits: [u64; 4],
    iend: [usize; 4],
}

impl FastArgs {
    /// `HUF_DecompressFastArgs_init`: `None` when the generic loop must decode.
    fn new(dst_size: usize, src: &[u8], table_log: u32) -> Result<Option<Self>> {
        if dst_size == 0 {
            return Ok(None);
        }
        if src.len() < 10 {
            return Err(Error::CorruptionDetected);
        }
        if table_log != FAST_TABLE_LOG {
            return Ok(None);
        }
        let (length1, length2, length3) = (read16(src, 0), read16(src, 2), read16(src, 4));
        let length4 = src.len().wrapping_sub(length1 + length2 + length3 + 6);
        let iend = [
            6,
            6 + length1,
            6 + length1 + length2,
            6 + length1 + length2 + length3,
        ];
        if length1 < 8 || length2 < 8 || length3 < 8 || length4 < 8 {
            return Ok(None);
        }
        if length4 > src.len() {
            return Err(Error::CorruptionDetected);
        }
        let ip = [iend[1] - 8, iend[2] - 8, iend[3] - 8, src.len() - 8];
        let segment = dst_size.div_ceil(4);
        let op = [0, segment, 2 * segment, 3 * segment];
        if op[3] >= dst_size {
            return Ok(None);
        }
        // `HUF_initFastDStream`.
        let init = |pos: usize| {
            let last = src[pos + 7];
            let consumed = if last == 0 {
                0
            } else {
                8 - highbit32(u32::from(last))
            };
            (read64(src, pos) | 1) << consumed
        };
        let bits = [init(ip[0]), init(ip[1]), init(ip[2]), init(ip[3])];
        Ok(Some(Self { ip, op, bits, iend }))
    }

    /// Finishes each stream with the generic decoder after the fast loop
    /// (`HUF_initRemainingDStream`), checking each fills its quarter exactly.
    fn finish(
        &self,
        dst: &mut [u8],
        src: &[u8],
        decode: impl Fn(&mut [u8], &mut BitReader<'_>) -> usize,
    ) -> Result<()> {
        let dst_size = dst.len();
        let segment = dst_size.div_ceil(4);
        let mut segment_end = 0usize;
        for i in 0..4 {
            segment_end = if segment <= dst_size - segment_end {
                segment_end + segment
            } else {
                dst_size
            };
            if self.op[i] > segment_end {
                return Err(Error::CorruptionDetected);
            }
            if (self.ip[i] as isize) < self.iend[i] as isize - 8 {
                return Err(Error::CorruptionDetected);
            }
            let mut bits = BitReader::at(src, self.ip[i], self.bits[i].trailing_zeros());
            let written = decode(&mut dst[self.op[i]..segment_end], &mut bits);
            if self.op[i] + written != segment_end {
                return Err(Error::CorruptionDetected);
            }
        }
        Ok(())
    }
}

/// The safe number of fast-loop iterations for the input: each consumes at most 7 bytes
/// of every stream, and every stream must read at or above the previous one.
#[inline(always)]
fn input_iterations(ip: &[usize; 4]) -> Option<usize> {
    if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
        return None;
    }
    Some(ip[0] / 7)
}

/// Moves the stream to the next unread byte and reloads it (`HUF_4X1_RELOAD_STREAM`).
#[inline(always)]
fn reload_fast_stream(bits: &mut u64, ip: &mut usize, src: *const u8) {
    let ctz = bits.trailing_zeros();
    *ip -= (ctz >> 3) as usize;
    // SAFETY: the loop's iteration bound keeps `ip` at or above 0 and it never rises
    // above its start, `src.len() - 8` or lower.
    *bits = (u64::from_le(unsafe { src.add(*ip).cast::<u64>().read_unaligned() }) | 1) << (ctz & 7);
}

/// `HUF_decompress4X1_usingDTable_internal_fast_c_loop`: 5 symbols per stream per
/// iteration, bounds checked once per batch of iterations.
#[inline(never)]
fn fast_loop_x1(args: &mut FastArgs, dst: &mut [u8], src: &[u8], dt: &[u16]) {
    debug_assert!(dt.len() >= 1 << FAST_TABLE_LOG);
    let oend = dst.len();
    let out = dst.as_mut_ptr();
    let input = src.as_ptr();
    let (mut bits, mut ip, mut op) = (args.bits, args.ip, args.op);
    while let Some(iiters) = input_iterations(&ip) {
        let oiters = (oend - op[3]) / 5;
        let olimit = op[3] + oiters.min(iiters) * 5;
        if op[3] == olimit {
            break;
        }
        while op[3] < olimit {
            for k in 0..5 {
                for s in 0..4 {
                    // SAFETY: an 11-bit index of a table of 2048 entries or more; the
                    // streams advance in lockstep, the last bounded by `olimit <= oend`.
                    unsafe {
                        let entry = *dt.get_unchecked((bits[s] >> 53) as usize);
                        bits[s] <<= entry & 0x3F;
                        *out.add(op[s] + k) = (entry >> 8) as u8;
                    }
                }
            }
            for s in 0..4 {
                op[s] += 5;
                reload_fast_stream(&mut bits[s], &mut ip[s], input);
            }
        }
    }
    args.bits = bits;
    args.ip = ip;
    args.op = op;
}

/// `HUF_decompress4X2_usingDTable_internal_fast_c_loop`: 5 lookups of up to 2 symbols per
/// stream per iteration.
#[inline(never)]
fn fast_loop_x2(args: &mut FastArgs, dst: &mut [u8], src: &[u8], dt: &[u32]) {
    debug_assert!(dt.len() >= 1 << FAST_TABLE_LOG);
    let out = dst.as_mut_ptr();
    let input = src.as_ptr();
    let (mut bits, mut ip, mut op) = (args.bits, args.ip, args.op);
    let oend = [op[1], op[2], op[3], dst.len()];
    while let Some(mut iters) = input_iterations(&ip) {
        for s in 0..4 {
            iters = iters.min((oend[s] - op[s]) / 10);
        }
        let olimit = op[3] + iters * 5;
        if op[3] == olimit {
            break;
        }
        while op[3] < olimit {
            for _ in 0..5 {
                for s in 0..4 {
                    // SAFETY: an 11-bit index of a table of 2048 entries or more; each
                    // iteration writes at most 10 bytes per stream, within `oend[s]`.
                    unsafe {
                        let entry = *dt.get_unchecked((bits[s] >> 53) as usize);
                        out.add(op[s])
                            .cast::<[u8; 2]>()
                            .write_unaligned((entry as u16).to_le_bytes());
                        bits[s] <<= (entry >> 16) & 0x3F;
                        op[s] += (entry >> 24) as usize;
                    }
                }
            }
            for s in 0..4 {
                reload_fast_stream(&mut bits[s], &mut ip[s], input);
            }
        }
    }
    args.bits = bits;
    args.ip = ip;
    args.op = op;
}
