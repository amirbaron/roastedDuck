//! Compressing one block (`ZSTD_compressBlock_internal`, `ZSTD_buildSeqStore` and
//! `ZSTD_entropyCompressSeqStore`).

use crate::literals::{HufTables, compress_literals, min_gain};
use crate::params::{CParams, Strategy};
use crate::seq::{REP_START, SeqStore};
use crate::sequences::{FseTables, build_statistics, encode_sequences};
use crate::window::{MatchState, Mem};
use crate::{dfast, fast};

/// The repeat offsets and entropy tables a block leaves for the next
/// (`ZSTD_compressedBlockState_t`).
#[derive(Clone, Debug)]
pub(crate) struct BlockState {
    pub rep: [u32; 3],
    pub huf: HufTables,
    pub fse: FseTables,
}

impl BlockState {
    /// `ZSTD_reset_compressedBlockState`.
    pub(crate) fn new() -> Self {
        Self {
            rep: REP_START,
            huf: HufTables::default(),
            fse: FseTables::default(),
        }
    }
}

const SUSPECT_UNCOMPRESSIBLE_LITERAL_RATIO: usize = 20;
/// `MIN_CBLOCK_SIZE + ZSTD_blockHeaderSize + 1 + 1`: smaller blocks are not compressed.
const MIN_COMPRESSIBLE_BLOCK: usize = 7;
/// `rleMaxLength`.
const RLE_MAX_LENGTH: usize = 25;

/// `ZSTD_literalsCompressionIsDisabled` (`ZSTD_ps_auto`): negative levels store literals
/// raw.
fn literals_compression_disabled(params: &CParams) -> bool {
    params.strategy == Strategy::Fast && params.target_length > 0
}

/// `ZSTD_entropyCompressSeqStore`: appends the compressed block body; returns its size, or 0
/// when the block is not compressible (nothing is appended then).
fn entropy_compress(
    out: &mut Vec<u8>,
    seq: &mut SeqStore,
    prev: &BlockState,
    next: &mut BlockState,
    params: &CParams,
    src_size: usize,
) -> usize {
    let start = out.len();
    let strategy = params.strategy;
    let nb_seq = seq.sequences.len();
    let num_literals = seq.literals.len();
    let suspect_uncompressible = nb_seq == 0 || num_literals / nb_seq >= SUSPECT_UNCOMPRESSIBLE_LITERAL_RATIO;
    compress_literals(
        out,
        &seq.literals,
        &prev.huf,
        &mut next.huf,
        strategy,
        literals_compression_disabled(params),
        suspect_uncompressible,
    );
    if nb_seq < 128 {
        out.push(nb_seq as u8);
    } else if nb_seq < 0x7F00 {
        out.push(((nb_seq >> 8) + 0x80) as u8);
        out.push(nb_seq as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&((nb_seq - 0x7F00) as u16).to_le_bytes());
    }
    let size = if nb_seq == 0 {
        next.fse.clone_from(&prev.fse);
        out.len() - start
    } else {
        let seq_head = out.len();
        out.push(0);
        seq.compute_codes();
        match build_statistics(out, seq, &prev.fse, &mut next.fse, strategy) {
            Some(stats) => {
                out[seq_head] = ((stats.ll << 6) + (stats.of << 4) + (stats.ml << 2)) as u8;
                let bitstream = encode_sequences(out, &next.fse, seq);
                // 0: the stream overflowed (`dstSize_tooSmall`); the block is stored raw.
                if bitstream == 0 || stats.last_count_size != 0 && stats.last_count_size + bitstream < 4 {
                    0
                } else {
                    out.len() - start
                }
            }
            // An error in zstd; the block is stored raw.
            None => 0,
        }
    };
    if size == 0 || size >= src_size - min_gain(src_size, strategy) {
        out.truncate(start);
        return 0;
    }
    size
}

/// `ZSTD_isRLE`: compared 32 bytes at a time, as zstd unrolls it.
fn is_rle(src: &[u8]) -> bool {
    let value = src[0];
    let pattern = u64::from_ne_bytes([value; 8]);
    let (chunks, rest) = src.as_chunks::<32>();
    rest.iter().all(|&byte| byte == value)
        && chunks.iter().all(|chunk| {
            let (words, _) = chunk.as_chunks::<8>();
            words
                .iter()
                .fold(0, |differ, word| differ | (u64::from_ne_bytes(*word) ^ pattern))
                == 0
        })
}

/// The state of the block compression of a frame.
#[derive(Debug)]
pub(crate) struct BlockCompressor {
    pub ms: MatchState,
    pub seq: SeqStore,
    pub prev: BlockState,
    pub next: BlockState,
    pub is_first_block: bool,
}

impl BlockCompressor {
    pub(crate) fn new(params: CParams) -> Self {
        Self {
            ms: MatchState::new(params),
            seq: SeqStore::new(),
            prev: BlockState::new(),
            next: BlockState::new(),
            is_first_block: true,
        }
    }

    /// Prepares a new frame with `params` (`ZSTD_resetCCtx_internal`).
    pub(crate) fn reset(&mut self, params: CParams) {
        let chain_table = params.strategy != Strategy::Fast;
        self.ms.reset(params, chain_table);
        self.prev = BlockState::new();
        self.is_first_block = true;
    }

    /// `ZSTD_compressBlock_internal`: appends the compressed body of the block at `istart`
    /// of the current segment; returns its size, 1 for an RLE block (whose byte is
    /// appended) or 0 if the block must be stored raw (nothing appended).
    pub(crate) fn compress_block(
        &mut self,
        out: &mut Vec<u8>,
        mem: Mem<'_>,
        istart: usize,
        src_size: usize,
    ) -> usize {
        let src = &mem.cur[istart..istart + src_size];
        let size = if src_size < MIN_COMPRESSIBLE_BLOCK {
            0
        } else {
            self.build_seq_store(mem, istart, src_size);
            let size = entropy_compress(
                out,
                &mut self.seq,
                &self.prev,
                &mut self.next,
                &self.ms.params,
                src_size,
            );
            if !self.is_first_block && size < RLE_MAX_LENGTH && is_rle(src) {
                out.truncate(out.len() - size);
                out.push(src[0]);
                1
            } else {
                size
            }
        };
        if size > 1 {
            // `ZSTD_blockState_confirmRepcodesAndEntropyTables`.
            std::mem::swap(&mut self.prev, &mut self.next);
        }
        size
    }

    /// `ZSTD_buildSeqStore`: runs the match finder over the block.
    fn build_seq_store(&mut self, mem: Mem<'_>, istart: usize, src_size: usize) {
        self.seq.reset();
        self.seq.reserve(src_size);
        let current = (istart as i64 + self.ms.window.base) as u32;
        if current > self.ms.next_to_update + 384 {
            self.ms.next_to_update = current - 192.min(current - self.ms.next_to_update - 384);
        }
        self.next.rep = self.prev.rep;
        let last_literals = match self.ms.params.strategy {
            Strategy::Fast => fast::compress_block(
                &mut self.ms,
                &mut self.seq,
                &mut self.next.rep,
                mem,
                istart,
                src_size,
            ),
            _ => dfast::compress_block(
                &mut self.ms,
                &mut self.seq,
                &mut self.next.rep,
                mem,
                istart,
                src_size,
            ),
        };
        let end = istart + src_size;
        self.seq
            .literals
            .extend_from_slice(&mem.cur[end - last_literals..end]);
    }
}
