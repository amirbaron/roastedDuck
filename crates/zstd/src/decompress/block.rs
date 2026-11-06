//! Compressed blocks (`zstd_decompress_block.c`): the literals section, the sequence
//! tables and the sequences, executed into the output.

use std::ptr;
use std::sync::LazyLock;

use super::bits::{BitReader, highbit32};
use super::entropy::{read_ncount, table_step};
use super::huf::HufTable;
use crate::error::{Error, Result};
use crate::literals::{SET_BASIC, SET_COMPRESSED, SET_REPEAT, SET_RLE};
use crate::params::BLOCK_SIZE_MAX;
use crate::seq::{LL_BITS, ML_BITS, REP_NUM, REP_START};
use crate::sequences::{
    LL_DEFAULT_NORM, LL_DEFAULT_NORM_LOG, LL_FSE_LOG, MAX_LL, MAX_ML, MAX_OFF, ML_DEFAULT_NORM,
    ML_DEFAULT_NORM_LOG, ML_FSE_LOG, OF_DEFAULT_NORM, OF_DEFAULT_NORM_LOG, OFF_FSE_LOG,
};

/// `WILDCOPY_OVERLENGTH`: copies may write this far past their end.
pub(crate) const WILDCOPY_OVERLENGTH: usize = 32;
/// `WILDCOPY_VECLEN`.
const WILDCOPY_VECLEN: usize = 16;
/// `MIN_CBLOCK_SIZE`.
const MIN_CBLOCK_SIZE: usize = 2;
/// `MIN_LITERALS_FOR_4_STREAMS`.
const MIN_LITERALS_FOR_4_STREAMS: usize = 6;
/// `LONGNBSEQ`.
const LONG_NB_SEQ: usize = 0x7F00;
/// `STREAM_ACCUMULATOR_MIN_64`.
const STREAM_ACCUMULATOR_MIN: u32 = 57;

/// `LL_base`.
const LL_BASE: [u32; MAX_LL as usize + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 0x80,
    0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
];
/// `OF_base`.
const OF_BASE: [u32; MAX_OFF as usize + 1] = [
    0,
    1,
    1,
    5,
    0xD,
    0x1D,
    0x3D,
    0x7D,
    0xFD,
    0x1FD,
    0x3FD,
    0x7FD,
    0xFFD,
    0x1FFD,
    0x3FFD,
    0x7FFD,
    0xFFFD,
    0x1_FFFD,
    0x3_FFFD,
    0x7_FFFD,
    0xF_FFFD,
    0x1F_FFFD,
    0x3F_FFFD,
    0x7F_FFFD,
    0xFF_FFFD,
    0x1FF_FFFD,
    0x3FF_FFFD,
    0x7FF_FFFD,
    0xFFF_FFFD,
    0x1FFF_FFFD,
    0x3FFF_FFFD,
    0x7FFF_FFFD,
];
/// `OF_bits`.
const OF_BITS: [u8; MAX_OFF as usize + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31,
];
/// `ML_base`.
const ML_BASE: [u32; MAX_ML as usize + 1] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203, 0x403, 0x803, 0x1003,
    0x2003, 0x4003, 0x8003, 0x1_0003,
];

/// `ZSTD_seqSymbol`: one state of a sequence decoding table, packed into one word that the
/// decoder loads at once (`nextState`, `nbAdditionalBits`, `nbBits`, `baseValue` from the
/// low bits up).
#[derive(Clone, Copy, Debug, Default)]
struct SeqSymbol(u64);

impl SeqSymbol {
    const fn new(next_state: u16, nb_additional_bits: u8, nb_bits: u8, base_value: u32) -> Self {
        Self(
            next_state as u64
                | (nb_additional_bits as u64) << 16
                | (nb_bits as u64) << 24
                | (base_value as u64) << 32,
        )
    }

    #[inline(always)]
    fn next_state(self) -> usize {
        (self.0 & 0xFFFF) as usize
    }

    #[inline(always)]
    fn nb_additional_bits(self) -> u32 {
        ((self.0 >> 16) & 0xFF) as u32
    }

    #[inline(always)]
    fn nb_bits(self) -> u32 {
        ((self.0 >> 24) & 0xFF) as u32
    }

    #[inline(always)]
    fn base_value(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

/// A sequence decoding table and its log (the `ZSTD_seqSymbol_header`).
#[derive(Clone, Debug)]
struct SeqTable {
    table_log: u32,
    entries: Vec<SeqSymbol>,
}

impl SeqTable {
    fn new(max_log: u32) -> Self {
        Self {
            table_log: 0,
            entries: vec![SeqSymbol::default(); 1 << max_log],
        }
    }

    /// `ZSTD_buildSeqTable_rle`.
    fn build_rle(&mut self, base_value: u32, nb_additional_bits: u8) {
        self.table_log = 0;
        self.entries[0] = SeqSymbol::new(0, nb_additional_bits, 0, base_value);
    }

    /// `ZSTD_buildFSETable`.
    fn build(
        &mut self,
        norm: &[i16],
        max_symbol: u32,
        base_value: &[u32],
        nb_additional_bits: &[u8],
        table_log: u32,
    ) {
        let max_sv1 = max_symbol as usize + 1;
        let table_size = 1u32 << table_log;
        // The symbol of each state, then the states' entries.
        let mut symbols = [0u8; 1 << 9];
        let mut high_threshold = table_size - 1;
        let mut symbol_next = [0u16; 64];
        for s in 0..max_sv1 {
            if norm[s] == -1 {
                symbols[high_threshold as usize] = s as u8;
                high_threshold = high_threshold.wrapping_sub(1);
                symbol_next[s] = 1;
            } else {
                symbol_next[s] = norm[s] as u16;
            }
        }
        let mask = table_size as usize - 1;
        let step = table_step(table_size) as usize;
        if high_threshold == table_size - 1 {
            // No low-probability symbol: lay the symbols down in order, 8 bytes at a time,
            // then spread the positions (zstd's branch-free path).
            let mut spread = [0u8; (1 << 9) + 8];
            let mut pos = 0usize;
            let mut sv = 0u64;
            for &count in &norm[..max_sv1] {
                let n = count as usize;
                spread[pos..pos + 8].copy_from_slice(&sv.to_le_bytes());
                let mut i = 8;
                while i < n {
                    spread[pos + i..pos + i + 8].copy_from_slice(&sv.to_le_bytes());
                    i += 8;
                }
                pos += n;
                sv = sv.wrapping_add(0x0101_0101_0101_0101);
            }
            let mut position = 0usize;
            for pair in spread[..table_size as usize].as_chunks::<2>().0 {
                symbols[position] = pair[0];
                symbols[(position + step) & mask] = pair[1];
                position = (position + 2 * step) & mask;
            }
        } else {
            let mut position = 0usize;
            for (s, &count) in norm.iter().enumerate().take(max_sv1) {
                for _ in 0..count.max(0) {
                    symbols[position] = s as u8;
                    position = (position + step) & mask;
                    while position > high_threshold as usize {
                        position = (position + step) & mask;
                    }
                }
            }
        }
        for (entry, &symbol) in self.entries.iter_mut().zip(&symbols[..table_size as usize]) {
            let symbol = usize::from(symbol) & 63;
            let next_state = u32::from(symbol_next[symbol]);
            symbol_next[symbol] += 1;
            let nb_bits = table_log - highbit32(next_state);
            *entry = SeqSymbol::new(
                ((next_state << nb_bits) - table_size) as u16,
                nb_additional_bits[symbol],
                nb_bits as u8,
                base_value[symbol],
            );
        }
        self.table_log = table_log;
    }
}

/// The predefined tables (`LL_defaultDTable`, `OF_defaultDTable`, `ML_defaultDTable`), built
/// from the default distributions as zstd generated them.
struct DefaultTables {
    ll: SeqTable,
    of: SeqTable,
    ml: SeqTable,
}

static DEFAULT_TABLES: LazyLock<DefaultTables> = LazyLock::new(|| {
    let mut ll = SeqTable::new(LL_DEFAULT_NORM_LOG);
    ll.build(&LL_DEFAULT_NORM, MAX_LL, &LL_BASE, &LL_BITS, LL_DEFAULT_NORM_LOG);
    let mut of = SeqTable::new(OF_DEFAULT_NORM_LOG);
    of.build(
        &OF_DEFAULT_NORM,
        OF_DEFAULT_NORM.len() as u32 - 1,
        &OF_BASE,
        &OF_BITS,
        OF_DEFAULT_NORM_LOG,
    );
    let mut ml = SeqTable::new(ML_DEFAULT_NORM_LOG);
    ml.build(&ML_DEFAULT_NORM, MAX_ML, &ML_BASE, &ML_BITS, ML_DEFAULT_NORM_LOG);
    DefaultTables { ll, of, ml }
});

/// Which table a sequence field decodes with (`LLTptr` and its twins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Default,
    Own,
}

/// Where the literals of the current block are.
#[derive(Clone, Copy, Debug)]
enum Literals {
    /// In `lit_buffer`.
    Buffer,
    /// Raw in the block, at this offset (with `WILDCOPY_OVERLENGTH` bytes after them).
    Block(usize),
}

/// The window a block's matches reach (`prefixStart`, `virtualStart`, `dictEnd`), as
/// offsets in the output buffer: the current segment starts at `prefix_start`; the
/// previous one (the extDict, in the same buffer) ends at `dict_end`, and `virtual_start`
/// is where it would start if it preceded the current segment.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Window {
    pub prefix_start: usize,
    pub virtual_start: isize,
    pub dict_end: usize,
}

/// The entropy state of a frame (`ZSTD_entropyDTables_t` and the literal state of
/// `ZSTD_DCtx`).
#[derive(Debug)]
pub(crate) struct BlockDecoder {
    huf: HufTable,
    /// A Huffman table exists for `set_repeat` (`litEntropy`).
    lit_entropy: bool,
    /// Sequence tables exist for `set_repeat` (`fseEntropy`).
    fse_entropy: bool,
    ll: SeqTable,
    of: SeqTable,
    ml: SeqTable,
    ll_choice: Choice,
    of_choice: Choice,
    ml_choice: Choice,
    pub rep: [u32; REP_NUM],
    lit_buffer: Vec<u8>,
    literals: Literals,
    lit_size: usize,
}

impl Default for BlockDecoder {
    fn default() -> Self {
        Self {
            huf: HufTable::default(),
            lit_entropy: false,
            fse_entropy: false,
            ll: SeqTable::new(LL_FSE_LOG),
            of: SeqTable::new(OFF_FSE_LOG),
            ml: SeqTable::new(ML_FSE_LOG),
            ll_choice: Choice::Own,
            of_choice: Choice::Own,
            ml_choice: Choice::Own,
            rep: REP_START,
            lit_buffer: vec![0; BLOCK_SIZE_MAX + WILDCOPY_OVERLENGTH],
            literals: Literals::Buffer,
            lit_size: 0,
        }
    }
}

#[inline(always)]
fn read16(src: &[u8], pos: usize) -> usize {
    usize::from(u16::from_le_bytes([src[pos], src[pos + 1]]))
}

#[inline(always)]
fn read24(src: &[u8], pos: usize) -> usize {
    usize::from(src[pos]) | usize::from(src[pos + 1]) << 8 | usize::from(src[pos + 2]) << 16
}

/// `seq_t`.
#[derive(Clone, Copy, Debug)]
struct Seq {
    lit_length: usize,
    match_length: usize,
    offset: usize,
}

/// `ZSTD_fseState` of the three fields and the bit stream (`seqState_t`).
struct SeqState<'a> {
    bits: BitReader<'a>,
    ll: &'a [SeqSymbol],
    of: &'a [SeqSymbol],
    ml: &'a [SeqSymbol],
    state_ll: usize,
    state_of: usize,
    state_ml: usize,
    /// The repeat offsets, most recent first (`prevOffset`), as three values so that they
    /// stay in registers.
    rep0: usize,
    rep1: usize,
    rep2: usize,
}

impl SeqState<'_> {
    /// `ZSTD_decodeSequence` (64-bit).
    #[inline(always)]
    fn decode(&mut self, is_last: bool) -> Seq {
        // SAFETY: states come from the tables' `next_state` plus at most `nb_bits` bits,
        // which stay below the table size (`ZSTD_buildFSETable`); the initial ones read
        // `table_log` bits.
        let (ll, ml, of) = unsafe {
            (
                *self.ll.get_unchecked(self.state_ll),
                *self.ml.get_unchecked(self.state_ml),
                *self.of.get_unchecked(self.state_of),
            )
        };
        let mut seq = Seq {
            lit_length: ll.base_value() as usize,
            match_length: ml.base_value() as usize,
            offset: 0,
        };
        let (ll_bits, ml_bits, of_bits) = (
            ll.nb_additional_bits(),
            ml.nb_additional_bits(),
            of.nb_additional_bits(),
        );
        let total_bits = ll_bits + ml_bits + of_bits;
        let offset;
        if of_bits > 1 {
            offset = of.base_value() as usize + self.bits.read_fast(of_bits) as usize;
            self.rep2 = self.rep1;
            self.rep1 = self.rep0;
            self.rep0 = offset;
        } else {
            let ll0 = ll.base_value() == 0;
            if of_bits == 0 {
                // `prevOffset[ll0]`; `prevOffset[1] = prevOffset[!ll0]`.
                if ll0 {
                    offset = self.rep1;
                    self.rep1 = self.rep0;
                    self.rep0 = offset;
                } else {
                    offset = self.rep0;
                }
            } else {
                let code = of.base_value() as usize + usize::from(ll0) + self.bits.read_fast(1) as usize;
                let mut temp = match code {
                    1 => self.rep1,
                    2 => self.rep2,
                    _ => self.rep0.wrapping_sub(1),
                };
                // 0 is not a valid offset: corrupt input fails in the execution.
                temp = temp.wrapping_sub(usize::from(temp == 0));
                if code != 1 {
                    self.rep2 = self.rep1;
                }
                self.rep1 = self.rep0;
                self.rep0 = temp;
                offset = temp;
            }
        }
        seq.offset = offset;
        if ml_bits > 0 {
            seq.match_length += self.bits.read_fast(ml_bits) as usize;
        }
        if total_bits >= STREAM_ACCUMULATOR_MIN - (LL_FSE_LOG + ML_FSE_LOG + OFF_FSE_LOG) {
            self.bits.reload();
        }
        if ll_bits > 0 {
            seq.lit_length += self.bits.read_fast(ll_bits) as usize;
        }
        if !is_last {
            self.state_ll = ll.next_state() + self.bits.read(ll.nb_bits()) as usize;
            self.state_ml = ml.next_state() + self.bits.read(ml.nb_bits()) as usize;
            self.state_of = of.next_state() + self.bits.read(of.nb_bits()) as usize;
            self.bits.reload();
        }
        seq
    }
}

/// The table of a field: its own (built, RLE or repeated) or the predefined one.
fn pick<'a>(choice: Choice, own: &'a SeqTable, default: &'a SeqTable) -> &'a SeqTable {
    match choice {
        Choice::Default => default,
        Choice::Own => own,
    }
}

/// The output of a block: `base[..oend]` may be written from the block's start on, and
/// matches reach back through `window`.
struct Output {
    base: *mut u8,
    oend: usize,
    window: Window,
    /// The buffer's length and the end of the literals' readable bytes (an address), for
    /// the debug checks of the raw copies.
    len: usize,
    lit_readable_end: usize,
}

#[inline(always)]
unsafe fn copy8(dst: *mut u8, src: *const u8) {
    // SAFETY: the caller guarantees 8 bytes at both, at least 8 apart.
    unsafe { ptr::copy_nonoverlapping(src, dst, 8) };
}

#[inline(always)]
unsafe fn copy16(dst: *mut u8, src: *const u8) {
    // SAFETY: the caller guarantees 16 bytes at both, at least 16 apart.
    unsafe { ptr::copy_nonoverlapping(src, dst, 16) };
}

/// `ZSTD_wildcopy`: copies `length` bytes in 8- or 16-byte steps, writing up to
/// `WILDCOPY_OVERLENGTH` bytes past the end. With `overlap`, the source precedes the
/// destination by 8 bytes or more; otherwise they are 16 or more bytes apart.
#[inline(always)]
unsafe fn wildcopy(dst: *mut u8, src: *const u8, length: usize, overlap: bool) {
    let diff = (dst as isize).wrapping_sub(src as isize);
    let (mut op, mut ip) = (dst, src);
    // SAFETY: the caller guarantees `WILDCOPY_OVERLENGTH` writable (and readable) bytes
    // past `length` at both.
    unsafe {
        let oend = dst.add(length);
        if overlap && diff < WILDCOPY_VECLEN as isize {
            loop {
                // Keep the loop as written: vectorized with overlap checks (as LLVM would)
                // it costs more than it saves on these short copies (zstd marks the
                // function `DONT_VECTORIZE`).
                std::arch::asm!("", options(nomem, nostack, preserves_flags));
                copy8(op, ip);
                op = op.add(8);
                ip = ip.add(8);
                if op >= oend {
                    break;
                }
            }
        } else {
            copy16(op, ip);
            if length <= 16 {
                return;
            }
            op = op.add(16);
            ip = ip.add(16);
            loop {
                copy16(op, ip);
                copy16(op.add(16), ip.add(16));
                op = op.add(32);
                ip = ip.add(32);
                if op >= oend {
                    break;
                }
            }
        }
    }
}

/// `ZSTD_overlapCopy8`: copies 8 bytes from `*ip` (before `*op`, `offset` bytes back),
/// spreading offsets below 8 so that the source ends up at least 8 bytes back.
#[inline(always)]
unsafe fn overlap_copy8(op: &mut *mut u8, ip: &mut *const u8, offset: usize) {
    const DEC32: [usize; 8] = [0, 1, 2, 1, 4, 4, 4, 4];
    const DEC64: [usize; 8] = [8, 8, 8, 7, 8, 9, 10, 11];
    // SAFETY: the caller guarantees 8 writable bytes at `*op` and that `*ip` precedes it.
    unsafe {
        if offset < 8 {
            let sub2 = DEC64[offset];
            for i in 0..4 {
                *op.add(i) = *ip.add(i);
            }
            *ip = ip.add(DEC32[offset]);
            ptr::copy_nonoverlapping(*ip, op.add(4), 4);
            *ip = ip.sub(sub2);
        } else {
            copy8(*op, *ip);
        }
        *ip = ip.add(8);
        *op = op.add(8);
    }
}

/// `ZSTD_safecopy`: a copy near the end of the output, writing nothing at or past
/// `oend_w + WILDCOPY_OVERLENGTH`.
unsafe fn safecopy(mut op: *mut u8, oend_w: *mut u8, mut ip: *const u8, length: usize, overlap: bool) {
    // SAFETY: the caller guarantees `length` bytes at both and that `oend_w` leaves
    // `WILDCOPY_OVERLENGTH` bytes of room.
    unsafe {
        let oend = op.add(length);
        let mut length = length;
        if length < 8 {
            while op < oend {
                *op = *ip;
                op = op.add(1);
                ip = ip.add(1);
            }
            return;
        }
        if overlap {
            let diff = op as usize - ip as usize;
            overlap_copy8(&mut op, &mut ip, diff);
            length -= 8;
        }
        if oend <= oend_w {
            wildcopy(op, ip, length, overlap);
            return;
        }
        if op <= oend_w {
            let n = oend_w as usize - op as usize;
            wildcopy(op, ip, n, overlap);
            ip = ip.add(n);
            op = op.add(n);
        }
        while op < oend {
            *op = *ip;
            op = op.add(1);
            ip = ip.add(1);
        }
    }
}

impl Output {
    /// Copies the match of `seq` that starts before `prefix_start`: from the extDict, then
    /// on from the prefix. Returns the rest to copy from the prefix, or `None` when the
    /// match ended in the extDict.
    #[inline(always)]
    unsafe fn copy_from_dict(&self, o_lit_end: usize, seq: &Seq) -> Result<Option<(usize, usize)>> {
        let w = self.window;
        if seq.offset > (o_lit_end as isize - w.virtual_start) as usize {
            return Err(Error::CorruptionDetected);
        }
        // How far before the prefix the match starts, mapped into the extDict.
        let back = seq.offset - (o_lit_end - w.prefix_start);
        let matched = w.dict_end - back;
        let copied = back.min(seq.match_length);
        debug_assert!(matched + copied <= self.len && o_lit_end + copied <= self.oend);
        // SAFETY: the extDict precedes `dict_end` in the same buffer; offsets were checked
        // against `virtual_start`.
        unsafe {
            if back >= seq.match_length {
                ptr::copy(self.base.add(matched), self.base.add(o_lit_end), seq.match_length);
                return Ok(None);
            }
            ptr::copy(self.base.add(matched), self.base.add(o_lit_end), back);
        }
        Ok(Some((o_lit_end + back, seq.match_length - back)))
    }

    /// `ZSTD_execSequenceEnd`: a sequence near the end of the output.
    #[cold]
    #[inline(never)]
    unsafe fn exec_end(
        &self,
        op: usize,
        seq: Seq,
        lit_ptr: *const u8,
        lit_limit: *const u8,
    ) -> Result<usize> {
        let o_lit_end = op + seq.lit_length;
        let sequence_length = seq.lit_length + seq.match_length;
        if sequence_length > self.oend - op {
            return Err(Error::DstSizeTooSmall);
        }
        if seq.lit_length > lit_limit as usize - lit_ptr as usize {
            return Err(Error::CorruptionDetected);
        }
        debug_assert!(op + sequence_length <= self.len);
        // SAFETY: both copies stay below `oend` (checked above); the literals are in
        // bounds of their buffer (checked above).
        unsafe {
            let oend_w = self.base.add(self.oend).wrapping_sub(WILDCOPY_OVERLENGTH);
            safecopy(self.base.add(op), oend_w, lit_ptr, seq.lit_length, false);
            let (mut mop, mut match_length) = (o_lit_end, seq.match_length);
            let mut matched = o_lit_end.wrapping_sub(seq.offset);
            if seq.offset > o_lit_end - self.window.prefix_start {
                match self.copy_from_dict(o_lit_end, &seq)? {
                    None => return Ok(sequence_length),
                    Some((at, rest)) => {
                        mop = at;
                        match_length = rest;
                        matched = self.window.prefix_start;
                    }
                }
            }
            safecopy(
                self.base.add(mop),
                oend_w,
                self.base.add(matched),
                match_length,
                true,
            );
        }
        Ok(sequence_length)
    }

    /// `ZSTD_execSequence`: copies the literals (at `lit_ptr`) and the match of `seq` to
    /// `op`; returns the sequence's length.
    #[inline(always)]
    unsafe fn exec(&self, op: usize, seq: Seq, lit_ptr: *const u8, lit_limit: *const u8) -> Result<usize> {
        // Lengths are below 2^18: no sum overflows.
        let o_lit_end = op + seq.lit_length;
        let sequence_length = seq.lit_length + seq.match_length;
        let o_match_end = op + sequence_length;
        let i_lit_end = lit_ptr as usize + seq.lit_length;
        // Fetch the match early (zstd does on aarch64).
        #[cfg(target_arch = "aarch64")]
        // SAFETY: a prefetch does not fault, whatever the address.
        unsafe {
            let matched = (self.base as usize)
                .wrapping_add(o_lit_end)
                .wrapping_sub(seq.offset);
            std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) matched, options(nostack, readonly, preserves_flags));
        }
        // Sequences reaching past the literals, or within `WILDCOPY_OVERLENGTH` of the end,
        // take the careful path.
        if i_lit_end > lit_limit as usize || o_match_end + WILDCOPY_OVERLENGTH > self.oend {
            // SAFETY: as for this function.
            return unsafe { self.exec_end(op, seq, lit_ptr, lit_limit) };
        }
        debug_assert!(o_match_end + WILDCOPY_OVERLENGTH <= self.len);
        debug_assert!(i_lit_end + WILDCOPY_OVERLENGTH - 1 <= self.lit_readable_end);
        // SAFETY: the literals and the match end `WILDCOPY_OVERLENGTH` bytes or more
        // before `oend` (checked above), so wild copies stay in the buffer; the literals
        // have `WILDCOPY_OVERLENGTH` readable bytes past `lit_limit`.
        unsafe {
            let base = self.base;
            copy16(base.add(op), lit_ptr);
            if seq.lit_length > 16 {
                wildcopy(base.add(op + 16), lit_ptr.add(16), seq.lit_length - 16, false);
            }
            let mut mop = o_lit_end;
            let mut match_length = seq.match_length;
            let mut matched = o_lit_end.wrapping_sub(seq.offset);
            if seq.offset > o_lit_end - self.window.prefix_start {
                match self.copy_from_dict(o_lit_end, &seq)? {
                    None => return Ok(sequence_length),
                    Some((at, rest)) => {
                        mop = at;
                        match_length = rest;
                        matched = self.window.prefix_start;
                    }
                }
            }
            let mut dst = base.add(mop);
            let mut src = base.add(matched).cast_const();
            if seq.offset >= WILDCOPY_VECLEN {
                wildcopy(dst, src, match_length, false);
                return Ok(sequence_length);
            }
            overlap_copy8(&mut dst, &mut src, seq.offset);
            if match_length > 8 {
                wildcopy(dst, src, match_length - 8, true);
            }
        }
        Ok(sequence_length)
    }
}

impl BlockDecoder {
    /// `ZSTD_decompressBegin`: a new frame forgets the entropy of the previous one.
    pub(crate) fn begin(&mut self) {
        self.lit_entropy = false;
        self.fse_entropy = false;
        self.rep = REP_START;
        self.ll_choice = Choice::Own;
        self.of_choice = Choice::Own;
        self.ml_choice = Choice::Own;
    }

    /// `ZSTD_decodeLiteralsBlock`: returns the size of the literals section.
    fn decode_literals(&mut self, src: &[u8], block_size_max: usize, dst_capacity: usize) -> Result<usize> {
        if src.len() < MIN_CBLOCK_SIZE {
            return Err(Error::CorruptionDetected);
        }
        let kind = u32::from(src[0] & 3);
        let lhl_code = (src[0] >> 2) & 3;
        let expected_write_size = block_size_max.min(dst_capacity);
        match kind {
            SET_REPEAT | SET_COMPRESSED => {
                if kind == SET_REPEAT && !self.lit_entropy {
                    return Err(Error::DictionaryCorrupted);
                }
                if src.len() < 5 {
                    return Err(Error::CorruptionDetected);
                }
                let lhc = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;
                let (single_stream, lh_size, lit_size, lit_c_size) = match lhl_code {
                    0 | 1 => (lhl_code == 0, 3, (lhc >> 4) & 0x3FF, (lhc >> 14) & 0x3FF),
                    2 => (false, 4, (lhc >> 4) & 0x3FFF, lhc >> 18),
                    _ => (
                        false,
                        5,
                        (lhc >> 4) & 0x3_FFFF,
                        (lhc >> 22) + (usize::from(src[4]) << 10),
                    ),
                };
                if lit_size > block_size_max {
                    return Err(Error::CorruptionDetected);
                }
                if !single_stream && lit_size < MIN_LITERALS_FOR_4_STREAMS {
                    return Err(Error::LiteralsHeaderWrong);
                }
                if lit_c_size + lh_size > src.len() {
                    return Err(Error::CorruptionDetected);
                }
                if expected_write_size < lit_size {
                    return Err(Error::DstSizeTooSmall);
                }
                let dst = &mut self.lit_buffer[..lit_size];
                let huf_src = &src[lh_size..lh_size + lit_c_size];
                let result = match (kind == SET_REPEAT, single_stream) {
                    (true, true) => self.huf.decompress_1x_using_table(dst, huf_src),
                    (true, false) => self.huf.decompress_4x_using_table(dst, huf_src),
                    (false, true) => self.huf.decompress_1x1(dst, huf_src),
                    (false, false) => self.huf.decompress_4x_huf_only(dst, huf_src),
                };
                if result.is_err() {
                    return Err(Error::CorruptionDetected);
                }
                self.literals = Literals::Buffer;
                self.lit_size = lit_size;
                self.lit_entropy = true;
                Ok(lit_c_size + lh_size)
            }
            SET_BASIC => {
                let (lh_size, lit_size) = match lhl_code {
                    0 | 2 => (1, usize::from(src[0] >> 3)),
                    1 => (2, read16(src, 0) >> 4),
                    _ => {
                        if src.len() < 3 {
                            return Err(Error::CorruptionDetected);
                        }
                        (3, read24(src, 0) >> 4)
                    }
                };
                if lit_size > block_size_max {
                    return Err(Error::CorruptionDetected);
                }
                if expected_write_size < lit_size {
                    return Err(Error::DstSizeTooSmall);
                }
                if lh_size + lit_size + WILDCOPY_OVERLENGTH > src.len() {
                    // Too close to the end of the block for wild copies: copy them.
                    if lit_size + lh_size > src.len() {
                        return Err(Error::CorruptionDetected);
                    }
                    self.lit_buffer[..lit_size].copy_from_slice(&src[lh_size..lh_size + lit_size]);
                    self.literals = Literals::Buffer;
                } else {
                    self.literals = Literals::Block(lh_size);
                }
                self.lit_size = lit_size;
                Ok(lh_size + lit_size)
            }
            _ => {
                // `set_rle`.
                let (lh_size, lit_size) = match lhl_code {
                    0 | 2 => (1, usize::from(src[0] >> 3)),
                    1 => {
                        if src.len() < 3 {
                            return Err(Error::CorruptionDetected);
                        }
                        (2, read16(src, 0) >> 4)
                    }
                    _ => {
                        if src.len() < 4 {
                            return Err(Error::CorruptionDetected);
                        }
                        (3, read24(src, 0) >> 4)
                    }
                };
                if lit_size > block_size_max {
                    return Err(Error::CorruptionDetected);
                }
                if expected_write_size < lit_size {
                    return Err(Error::DstSizeTooSmall);
                }
                self.lit_buffer[..lit_size].fill(src[lh_size]);
                self.literals = Literals::Buffer;
                self.lit_size = lit_size;
                Ok(lh_size + 1)
            }
        }
    }

    /// `ZSTD_buildSeqTable`: returns the size of the table description.
    #[allow(clippy::too_many_arguments, reason = "zstd's signature")]
    fn build_seq_table(
        table: &mut SeqTable,
        choice: &mut Choice,
        kind: u32,
        max: u32,
        max_log: u32,
        src: &[u8],
        base_value: &[u32],
        nb_additional_bits: &[u8],
        flag_repeat_table: bool,
    ) -> Result<usize> {
        match kind {
            SET_RLE => {
                let Some(&symbol) = src.first() else {
                    return Err(Error::SrcSizeWrong);
                };
                if u32::from(symbol) > max {
                    return Err(Error::CorruptionDetected);
                }
                table.build_rle(base_value[symbol as usize], nb_additional_bits[symbol as usize]);
                *choice = Choice::Own;
                Ok(1)
            }
            SET_BASIC => {
                *choice = Choice::Default;
                Ok(0)
            }
            SET_REPEAT => {
                if !flag_repeat_table {
                    return Err(Error::CorruptionDetected);
                }
                Ok(0)
            }
            _ => {
                let mut norm = [0i16; MAX_ML as usize + 1];
                let mut max = max;
                let (header_size, table_log) =
                    read_ncount(&mut norm, &mut max, src).map_err(|_| Error::CorruptionDetected)?;
                if table_log > max_log {
                    return Err(Error::CorruptionDetected);
                }
                table.build(&norm, max, base_value, nb_additional_bits, table_log);
                *choice = Choice::Own;
                Ok(header_size)
            }
        }
    }

    /// `ZSTD_decodeSeqHeaders`: returns the number of sequences and the header size.
    fn decode_seq_headers(&mut self, src: &[u8]) -> Result<(usize, usize)> {
        let Some(&first) = src.first() else {
            return Err(Error::SrcSizeWrong);
        };
        let iend = src.len();
        let mut ip = 1;
        let mut nb_seq = usize::from(first);
        if nb_seq > 0x7F {
            if nb_seq == 0xFF {
                if ip + 2 > iend {
                    return Err(Error::SrcSizeWrong);
                }
                nb_seq = read16(src, ip) + LONG_NB_SEQ;
                ip += 2;
            } else {
                if ip >= iend {
                    return Err(Error::SrcSizeWrong);
                }
                nb_seq = ((nb_seq - 0x80) << 8) + usize::from(src[ip]);
                ip += 1;
            }
        }
        if nb_seq == 0 {
            if ip != iend {
                return Err(Error::CorruptionDetected);
            }
            return Ok((0, ip));
        }
        if ip + 1 > iend {
            return Err(Error::SrcSizeWrong);
        }
        if src[ip] & 3 != 0 {
            return Err(Error::CorruptionDetected);
        }
        let ll_type = u32::from(src[ip] >> 6);
        let of_type = u32::from((src[ip] >> 4) & 3);
        let ml_type = u32::from((src[ip] >> 2) & 3);
        ip += 1;
        let repeat = self.fse_entropy;
        ip += Self::build_seq_table(
            &mut self.ll,
            &mut self.ll_choice,
            ll_type,
            MAX_LL,
            LL_FSE_LOG,
            &src[ip..],
            &LL_BASE,
            &LL_BITS,
            repeat,
        )
        .map_err(|_| Error::CorruptionDetected)?;
        ip += Self::build_seq_table(
            &mut self.of,
            &mut self.of_choice,
            of_type,
            MAX_OFF,
            OFF_FSE_LOG,
            &src[ip..],
            &OF_BASE,
            &OF_BITS,
            repeat,
        )
        .map_err(|_| Error::CorruptionDetected)?;
        ip += Self::build_seq_table(
            &mut self.ml,
            &mut self.ml_choice,
            ml_type,
            MAX_ML,
            ML_FSE_LOG,
            &src[ip..],
            &ML_BASE,
            &ML_BITS,
            repeat,
        )
        .map_err(|_| Error::CorruptionDetected)?;
        Ok((nb_seq, ip))
    }

    /// `ZSTD_decompressBlock_internal`: decodes the compressed block `src` into
    /// `out[op..oend]`; returns the decoded size.
    pub(crate) fn decompress_block(
        &mut self,
        src: &[u8],
        out: &mut [u8],
        op: usize,
        oend: usize,
        window: Window,
        block_size_max: usize,
    ) -> Result<usize> {
        if src.len() > block_size_max {
            return Err(Error::SrcSizeWrong);
        }
        debug_assert!(oend <= out.len() && op <= oend);
        let lit_c_size = self.decode_literals(src, block_size_max, oend - op)?;
        let (nb_seq, seq_h_size) = self.decode_seq_headers(&src[lit_c_size..])?;
        if oend == op && nb_seq > 0 {
            return Err(Error::DstSizeTooSmall);
        }
        self.decompress_sequences(src, lit_c_size + seq_h_size, nb_seq, out, op, oend, window)
    }

    /// `ZSTD_decompressSequences_body`, out of line like zstd's (its loop keeps more in
    /// registers that way).
    #[inline(never)]
    #[allow(clippy::too_many_arguments, reason = "zstd's signature")]
    fn decompress_sequences(
        &mut self,
        block: &[u8],
        seq_start: usize,
        nb_seq: usize,
        out: &mut [u8],
        op_start: usize,
        oend: usize,
        window: Window,
    ) -> Result<usize> {
        let (lit_start, lit_end) = match self.literals {
            Literals::Buffer => {
                let start = self.lit_buffer.as_ptr();
                // SAFETY: `lit_size` bytes were decoded into the buffer, which has
                // `WILDCOPY_OVERLENGTH` more.
                (start, unsafe { start.add(self.lit_size) })
            }
            Literals::Block(at) => {
                // SAFETY: the raw literals and `WILDCOPY_OVERLENGTH` bytes follow `at`.
                let start = unsafe { block.as_ptr().add(at) };
                (start, unsafe { start.add(self.lit_size) })
            }
        };
        let output = Output {
            base: out.as_mut_ptr(),
            oend,
            window,
            len: out.len(),
            lit_readable_end: lit_end as usize + WILDCOPY_OVERLENGTH,
        };
        let mut lit_ptr = lit_start;
        let mut op = op_start;
        if nb_seq > 0 {
            self.fse_entropy = true;
            let defaults = &*DEFAULT_TABLES;
            let ll = pick(self.ll_choice, &self.ll, &defaults.ll);
            let of = pick(self.of_choice, &self.of, &defaults.of);
            let ml = pick(self.ml_choice, &self.ml, &defaults.ml);
            let mut bits = BitReader::new(&block[seq_start..]).map_err(|_| Error::CorruptionDetected)?;
            let state_ll = bits.read(ll.table_log) as usize;
            bits.reload();
            let state_of = bits.read(of.table_log) as usize;
            bits.reload();
            let state_ml = bits.read(ml.table_log) as usize;
            bits.reload();
            let mut state = SeqState {
                bits,
                ll: &ll.entries,
                of: &of.entries,
                ml: &ml.entries,
                state_ll,
                state_of,
                state_ml,
                rep0: self.rep[0] as usize,
                rep1: self.rep[1] as usize,
                rep2: self.rep[2] as usize,
            };
            for remaining in (1..=nb_seq).rev() {
                let seq = state.decode(remaining == 1);
                // SAFETY: `exec` checks the sequence against `oend` and the literals, so
                // the literals hold the sequence's.
                unsafe {
                    op += output.exec(op, seq, lit_ptr, lit_end)?;
                    lit_ptr = lit_ptr.add(seq.lit_length);
                }
            }
            if !state.bits.finished() {
                return Err(Error::CorruptionDetected);
            }
            self.rep = [state.rep0 as u32, state.rep1 as u32, state.rep2 as u32];
        }
        // The last literals.
        let last = lit_end as usize - lit_ptr as usize;
        if last > oend - op {
            return Err(Error::DstSizeTooSmall);
        }
        // SAFETY: `last` literals remain and fit before `oend` (checked above).
        unsafe { ptr::copy_nonoverlapping(lit_ptr, output.base.add(op), last) };
        Ok(op + last - op_start)
    }
}
