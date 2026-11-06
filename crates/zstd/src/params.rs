//! Compression levels and their parameters (`clevels.h`, `ZSTD_getCParams_internal`,
//! `ZSTD_adjustCParams_internal`).

/// The default compression level (`ZSTD_CLEVEL_DEFAULT`).
pub const DEFAULT_LEVEL: i32 = 3;
/// The strongest level (`ZSTD_MAX_CLEVEL`).
pub const MAX_LEVEL: i32 = 22;
/// The fastest level (`ZSTD_minCLevel`: minus `ZSTD_TARGETLENGTH_MAX`).
pub const MIN_LEVEL: i32 = -(BLOCK_SIZE_MAX as i32);

/// The largest block (`ZSTD_BLOCKSIZE_MAX`).
pub(crate) const BLOCK_SIZE_MAX: usize = 1 << 17;
/// The content size of a frame whose size is not known up front.
pub(crate) const CONTENT_SIZE_UNKNOWN: u64 = u64::MAX;
const WINDOW_LOG_MAX: u32 = 31;
pub(crate) const WINDOW_LOG_ABSOLUTE_MIN: u32 = 10;
const HASH_LOG_MIN: u32 = 6;
const ROW_HASH_TAG_BITS: u32 = 8;

/// A match finding strategy (`ZSTD_strategy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Strategy {
    Fast = 1,
    DFast = 2,
    Greedy = 3,
    Lazy = 4,
    Lazy2 = 5,
    BtLazy2 = 6,
    BtOpt = 7,
    BtUltra = 8,
    BtUltra2 = 9,
}

/// `ZSTD_compressionParameters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CParams {
    pub window_log: u32,
    pub chain_log: u32,
    pub hash_log: u32,
    pub search_log: u32,
    pub min_match: u32,
    pub target_length: u32,
    pub strategy: Strategy,
}

#[allow(clippy::many_single_char_names, reason = "the columns of zstd's table")]
const fn p(w: u32, c: u32, h: u32, s: u32, l: u32, t: u32, strategy: Strategy) -> CParams {
    CParams {
        window_log: w,
        chain_log: c,
        hash_log: h,
        search_log: s,
        min_match: l,
        target_length: t,
        strategy,
    }
}

use Strategy::{BtLazy2, BtOpt, BtUltra, BtUltra2, DFast, Fast, Greedy, Lazy, Lazy2};

/// `ZSTD_defaultCParameters`: by source size (above 256 KB, up to 256 KB, 128 KB, 16 KB)
/// and level (row 0 is the base of the negative levels).
#[rustfmt::skip]
static DEFAULT_CPARAMS: [[CParams; 23]; 4] = [
    [
        p(19, 12, 13, 1, 6, 1, Fast), p(19, 13, 14, 1, 7, 0, Fast), p(20, 15, 16, 1, 6, 0, Fast),
        p(21, 16, 17, 1, 5, 0, DFast), p(21, 18, 18, 1, 5, 0, DFast), p(21, 18, 19, 3, 5, 2, Greedy),
        p(21, 18, 19, 3, 5, 4, Lazy), p(21, 19, 20, 4, 5, 8, Lazy), p(21, 19, 20, 4, 5, 16, Lazy2),
        p(22, 20, 21, 4, 5, 16, Lazy2), p(22, 21, 22, 5, 5, 16, Lazy2), p(22, 21, 22, 6, 5, 16, Lazy2),
        p(22, 22, 23, 6, 5, 32, Lazy2), p(22, 22, 22, 4, 5, 32, BtLazy2), p(22, 22, 23, 5, 5, 32, BtLazy2),
        p(22, 23, 23, 6, 5, 32, BtLazy2), p(22, 22, 22, 5, 5, 48, BtOpt), p(23, 23, 22, 5, 4, 64, BtOpt),
        p(23, 23, 22, 6, 3, 64, BtUltra), p(23, 24, 22, 7, 3, 256, BtUltra2), p(25, 25, 23, 7, 3, 256, BtUltra2),
        p(26, 26, 24, 7, 3, 512, BtUltra2), p(27, 27, 25, 9, 3, 999, BtUltra2),
    ],
    [
        p(18, 12, 13, 1, 5, 1, Fast), p(18, 13, 14, 1, 6, 0, Fast), p(18, 14, 14, 1, 5, 0, DFast),
        p(18, 16, 16, 1, 4, 0, DFast), p(18, 16, 17, 3, 5, 2, Greedy), p(18, 17, 18, 5, 5, 2, Greedy),
        p(18, 18, 19, 3, 5, 4, Lazy), p(18, 18, 19, 4, 4, 4, Lazy), p(18, 18, 19, 4, 4, 8, Lazy2),
        p(18, 18, 19, 5, 4, 8, Lazy2), p(18, 18, 19, 6, 4, 8, Lazy2), p(18, 18, 19, 5, 4, 12, BtLazy2),
        p(18, 19, 19, 7, 4, 12, BtLazy2), p(18, 18, 19, 4, 4, 16, BtOpt), p(18, 18, 19, 4, 3, 32, BtOpt),
        p(18, 18, 19, 6, 3, 128, BtOpt), p(18, 19, 19, 6, 3, 128, BtUltra), p(18, 19, 19, 8, 3, 256, BtUltra),
        p(18, 19, 19, 6, 3, 128, BtUltra2), p(18, 19, 19, 8, 3, 256, BtUltra2), p(18, 19, 19, 10, 3, 512, BtUltra2),
        p(18, 19, 19, 12, 3, 512, BtUltra2), p(18, 19, 19, 13, 3, 999, BtUltra2),
    ],
    [
        p(17, 12, 12, 1, 5, 1, Fast), p(17, 12, 13, 1, 6, 0, Fast), p(17, 13, 15, 1, 5, 0, Fast),
        p(17, 15, 16, 2, 5, 0, DFast), p(17, 17, 17, 2, 4, 0, DFast), p(17, 16, 17, 3, 4, 2, Greedy),
        p(17, 16, 17, 3, 4, 4, Lazy), p(17, 16, 17, 3, 4, 8, Lazy2), p(17, 16, 17, 4, 4, 8, Lazy2),
        p(17, 16, 17, 5, 4, 8, Lazy2), p(17, 16, 17, 6, 4, 8, Lazy2), p(17, 17, 17, 5, 4, 8, BtLazy2),
        p(17, 18, 17, 7, 4, 12, BtLazy2), p(17, 18, 17, 3, 4, 12, BtOpt), p(17, 18, 17, 4, 3, 32, BtOpt),
        p(17, 18, 17, 6, 3, 256, BtOpt), p(17, 18, 17, 6, 3, 128, BtUltra), p(17, 18, 17, 8, 3, 256, BtUltra),
        p(17, 18, 17, 10, 3, 512, BtUltra), p(17, 18, 17, 5, 3, 256, BtUltra2), p(17, 18, 17, 7, 3, 512, BtUltra2),
        p(17, 18, 17, 9, 3, 512, BtUltra2), p(17, 18, 17, 11, 3, 999, BtUltra2),
    ],
    [
        p(14, 12, 13, 1, 5, 1, Fast), p(14, 14, 15, 1, 5, 0, Fast), p(14, 14, 15, 1, 4, 0, Fast),
        p(14, 14, 15, 2, 4, 0, DFast), p(14, 14, 14, 4, 4, 2, Greedy), p(14, 14, 14, 3, 4, 4, Lazy),
        p(14, 14, 14, 4, 4, 8, Lazy2), p(14, 14, 14, 6, 4, 8, Lazy2), p(14, 14, 14, 8, 4, 8, Lazy2),
        p(14, 15, 14, 5, 4, 8, BtLazy2), p(14, 15, 14, 9, 4, 8, BtLazy2), p(14, 15, 14, 3, 4, 12, BtOpt),
        p(14, 15, 14, 4, 3, 24, BtOpt), p(14, 15, 14, 5, 3, 32, BtUltra), p(14, 15, 15, 6, 3, 64, BtUltra),
        p(14, 15, 15, 7, 3, 256, BtUltra), p(14, 15, 15, 5, 3, 48, BtUltra2), p(14, 15, 15, 6, 3, 128, BtUltra2),
        p(14, 15, 15, 7, 3, 256, BtUltra2), p(14, 15, 15, 8, 3, 256, BtUltra2), p(14, 15, 15, 8, 3, 512, BtUltra2),
        p(14, 15, 15, 9, 3, 512, BtUltra2), p(14, 15, 15, 10, 3, 999, BtUltra2),
    ],
];

/// `ZSTD_highbit32`.
pub(crate) fn highbit32(value: u32) -> u32 {
    debug_assert!(value != 0);
    value.ilog2()
}

/// `ZSTD_cycleLog`.
fn cycle_log(hash_log: u32, strategy: Strategy) -> u32 {
    hash_log - u32::from(strategy >= BtLazy2)
}

/// Whether `strategy` uses the row-based match finder (`ZSTD_rowMatchFinderSupported`),
/// which `ZSTD_adjustCParams_internal` assumes.
pub(crate) fn row_match_finder_supported(strategy: Strategy) -> bool {
    (Greedy..=Lazy2).contains(&strategy)
}

/// `ZSTD_resolveRowMatchFinderMode` with its automatic platform/window policy.
pub(crate) fn row_match_finder_used(params: CParams) -> bool {
    let simd = cfg!(any(target_feature = "sse2", target_feature = "neon"));
    row_match_finder_supported(params.strategy) && params.window_log > if simd { 14 } else { 17 }
}

/// `ZSTD_adjustCParams_internal` without a dictionary (`ZSTD_cpm_noAttachDict`), with the
/// row match finder assumed (`ZSTD_ps_auto`). `src_size` may be `CONTENT_SIZE_UNKNOWN`.
fn adjust(mut params: CParams, src_size: u64) -> CParams {
    let max_window_resize = 1u64 << (WINDOW_LOG_MAX - 1);
    if src_size <= max_window_resize {
        let t_size = src_size as u32;
        let src_log = if t_size < (1 << HASH_LOG_MIN) {
            HASH_LOG_MIN
        } else {
            highbit32(t_size - 1) + 1
        };
        if params.window_log > src_log {
            params.window_log = src_log;
        }
    }
    if src_size != CONTENT_SIZE_UNKNOWN {
        let dict_and_window_log = params.window_log;
        let cycle = cycle_log(params.chain_log, params.strategy);
        if params.hash_log > dict_and_window_log + 1 {
            params.hash_log = dict_and_window_log + 1;
        }
        if cycle > dict_and_window_log {
            params.chain_log -= cycle - dict_and_window_log;
        }
    }
    if params.window_log < WINDOW_LOG_ABSOLUTE_MIN {
        params.window_log = WINDOW_LOG_ABSOLUTE_MIN;
    }
    if row_match_finder_supported(params.strategy) {
        let row_log = params.search_log.clamp(4, 6);
        let max_hash_log = 32 - ROW_HASH_TAG_BITS + row_log;
        if params.hash_log > max_hash_log {
            params.hash_log = max_hash_log;
        }
    }
    params
}

/// The parameters of `level` for a source of `src_size` bytes (`ZSTD_getCParams_internal`
/// and `ZSTD_getCParamsFromCCtxParams`, which adjusts once more).
pub(crate) fn get_cparams(level: i32, src_size: u64) -> CParams {
    let table = if src_size == CONTENT_SIZE_UNKNOWN {
        0
    } else {
        usize::from(src_size <= 256 << 10)
            + usize::from(src_size <= 128 << 10)
            + usize::from(src_size <= 16 << 10)
    };
    let row = match level {
        0 => DEFAULT_LEVEL as usize,
        ..0 => 0,
        _ => level.min(MAX_LEVEL) as usize,
    };
    let mut params = DEFAULT_CPARAMS[table][row];
    if level < 0 {
        params.target_length = level.max(MIN_LEVEL).unsigned_abs();
    }
    adjust(adjust(params, src_size), src_size)
}

/// `ZSTD_compressBound`.
pub fn compress_bound(src_size: usize) -> usize {
    let margin = if src_size < BLOCK_SIZE_MAX {
        (BLOCK_SIZE_MAX - src_size) >> 11
    } else {
        0
    };
    src_size + (src_size >> 8) + margin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_three_for_unknown_and_small_sizes() {
        let unknown = get_cparams(3, CONTENT_SIZE_UNKNOWN);
        assert_eq!(
            (unknown.window_log, unknown.chain_log, unknown.hash_log),
            (21, 16, 17)
        );
        assert_eq!(unknown.strategy, Strategy::DFast);
        let small = get_cparams(3, 4096);
        assert_eq!((small.window_log, small.hash_log, small.chain_log), (12, 13, 12));
        let negative = get_cparams(-5, 262_144);
        assert_eq!((negative.strategy, negative.target_length), (Strategy::Fast, 5));
    }
}
