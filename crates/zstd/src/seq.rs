//! The sequences a match finder stores (`seqStore_t`, `ZSTD_storeSeq`, `ZSTD_seqToCodes`).

use crate::params::highbit32;
use crate::sequences::{MAX_LL, MAX_ML};

pub(crate) const REP_NUM: usize = 3;
pub(crate) const MINMATCH: usize = 3;
/// The repeat offsets of a new frame (`repStartValue`).
pub(crate) const REP_START: [u32; REP_NUM] = [1, 4, 8];

#[rustfmt::skip]
pub(crate) const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15, 16,
];
#[rustfmt::skip]
pub(crate) const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];

/// A stored sequence (`seqDef`): literals, then a match.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SeqDef {
    /// `offBase`: offset + 3, or a repeat code 1..=3.
    pub off_base: u32,
    pub lit_length: u16,
    /// Match length minus `MINMATCH`.
    pub ml_base: u16,
}

/// Whether a sequence has a literal or match length above 65535 (`ZSTD_longLengthType_e`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LongLength {
    None,
    Literal,
    Match,
}

/// The sequences and literals of a block (`seqStore_t`).
#[derive(Debug)]
pub(crate) struct SeqStore {
    pub sequences: Vec<SeqDef>,
    pub literals: Vec<u8>,
    pub ll_code: Vec<u8>,
    pub ml_code: Vec<u8>,
    pub of_code: Vec<u8>,
    pub long_length: LongLength,
    pub long_length_pos: usize,
}

impl SeqStore {
    pub(crate) fn new() -> Self {
        Self {
            sequences: Vec::new(),
            literals: Vec::new(),
            ll_code: Vec::new(),
            ml_code: Vec::new(),
            of_code: Vec::new(),
            long_length: LongLength::None,
            long_length_pos: 0,
        }
    }

    /// `ZSTD_resetSeqStore`.
    pub(crate) fn reset(&mut self) {
        self.sequences.clear();
        self.literals.clear();
        self.long_length = LongLength::None;
    }

    /// Makes room for the sequences of a block of `block_size` bytes, so that storing them
    /// does not reallocate.
    pub(crate) fn reserve(&mut self, block_size: usize) {
        self.literals.reserve(block_size + 32);
        self.sequences.reserve(block_size / 3 + 1);
    }

    /// `ZSTD_storeSeq`: the `lit_length` literals at `anchor` of `buf` precede a match of
    /// `match_length` at `off_base`.
    #[inline(always)]
    pub(crate) fn store(
        &mut self,
        buf: &[u8],
        anchor: usize,
        lit_length: usize,
        off_base: u32,
        match_length: usize,
    ) {
        const WILDCOPY_OVERLENGTH: usize = 32;
        let len = self.literals.len();
        if anchor + lit_length + WILDCOPY_OVERLENGTH <= buf.len()
            && self.literals.capacity() - len >= lit_length + WILDCOPY_OVERLENGTH
        {
            // `ZSTD_copy16`, then `ZSTD_wildcopy` for longer literals: 16 bytes at a time,
            // over-reading and over-writing up to `WILDCOPY_OVERLENGTH` bytes.
            // SAFETY: `WILDCOPY_OVERLENGTH` bytes past the literals are readable in `buf` and
            // writable in the capacity (both checked above); the two buffers are distinct;
            // the length only grows over the `lit_length` copied bytes.
            unsafe {
                let mut src = buf.as_ptr().add(anchor);
                let mut dst = self.literals.as_mut_ptr().add(len);
                let end = dst.add(lit_length);
                std::ptr::copy_nonoverlapping(src, dst, 16);
                if lit_length > 16 {
                    loop {
                        src = src.add(16);
                        dst = dst.add(16);
                        std::ptr::copy_nonoverlapping(src, dst, 16);
                        if dst.add(16) >= end {
                            break;
                        }
                    }
                }
                self.literals.set_len(len + lit_length);
            }
        } else {
            // `ZSTD_safecopyLiterals`.
            self.literals.extend_from_slice(&buf[anchor..anchor + lit_length]);
        }
        if lit_length > 0xFFFF {
            self.long_length = LongLength::Literal;
            self.long_length_pos = self.sequences.len();
        }
        let ml_base = match_length - MINMATCH;
        if ml_base > 0xFFFF {
            self.long_length = LongLength::Match;
            self.long_length_pos = self.sequences.len();
        }
        self.sequences.push(SeqDef {
            off_base,
            lit_length: lit_length as u16,
            ml_base: ml_base as u16,
        });
    }

    /// `ZSTD_seqToCodes`.
    pub(crate) fn compute_codes(&mut self) {
        let n = self.sequences.len();
        self.ll_code.resize(n, 0);
        self.of_code.resize(n, 0);
        self.ml_code.resize(n, 0);
        let codes = self
            .ll_code
            .iter_mut()
            .zip(&mut self.of_code)
            .zip(&mut self.ml_code);
        for (((ll, of), ml), sequence) in codes.zip(&self.sequences) {
            *ll = ll_code(u32::from(sequence.lit_length)) as u8;
            *of = highbit32(sequence.off_base) as u8;
            *ml = ml_code(u32::from(sequence.ml_base)) as u8;
        }
        match self.long_length {
            LongLength::Literal => self.ll_code[self.long_length_pos] = MAX_LL as u8,
            LongLength::Match => self.ml_code[self.long_length_pos] = MAX_ML as u8,
            LongLength::None => {}
        }
    }
}

/// `ZSTD_LLcode`.
pub(crate) fn ll_code(lit_length: u32) -> u32 {
    #[rustfmt::skip]
    const LL_CODE: [u8; 64] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21,
        22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23,
        24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    ];
    if lit_length > 63 {
        highbit32(lit_length) + 19
    } else {
        u32::from(LL_CODE[lit_length as usize])
    }
}

/// `ZSTD_MLcode` of a match length minus `MINMATCH`.
pub(crate) fn ml_code(ml_base: u32) -> u32 {
    #[rustfmt::skip]
    const ML_CODE: [u8; 128] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        32, 32, 33, 33, 34, 34, 35, 35, 36, 36, 36, 36, 37, 37, 37, 37,
        38, 38, 38, 38, 38, 38, 38, 38, 39, 39, 39, 39, 39, 39, 39, 39,
        40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,
        41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41,
        42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
    ];
    if ml_base > 127 {
        highbit32(ml_base) + 36
    } else {
        u32::from(ML_CODE[ml_base as usize])
    }
}

/// `OFFSET_TO_OFFBASE`.
#[inline(always)]
pub(crate) fn offset_to_off_base(offset: u32) -> u32 {
    offset + REP_NUM as u32
}

/// `REPCODE1_TO_OFFBASE`.
pub(crate) const REPCODE1: u32 = 1;
