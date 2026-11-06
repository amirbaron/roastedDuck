//! The match window and the memory the match finders read (`ZSTD_window_t`,
//! `ZSTD_matchState_t` and the helpers of `zstd_compress_internal.h`).

use crate::params::{CParams, Strategy};

/// `HASH_READ_SIZE`.
pub(crate) const HASH_READ_SIZE: usize = 8;
/// `kSearchStrength`.
pub(crate) const SEARCH_STRENGTH: u32 = 8;
/// The first index of a new window (`ZSTD_WINDOW_START_INDEX`).
const WINDOW_START_INDEX: u32 = 2;
/// The largest index before the window is moved down (`ZSTD_CURRENT_MAX` of 64-bit
/// builds).
const CURRENT_MAX: i64 = (3 << 29) + (1 << 31);
/// How close to `CURRENT_MAX` the indexes of a reused context may start a frame
/// (`ZSTD_INDEXOVERFLOW_MARGIN`).
const INDEX_OVERFLOW_MARGIN: i64 = 16 << 20;
/// The rows the hash tables are reduced by (`ZSTD_ROWSIZE`).
const ROW_SIZE: usize = 16;

/// The buffer a segment of the window is in. Indexes of the same buffer may be adjacent;
/// different buffers never overlap (zstd compares their addresses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Buf {
    /// The static placeholder of a new window.
    None,
    /// The compressor's input buffer.
    InBuff,
    /// The caller's input.
    Src,
}

/// `ZSTD_window_t`: index `i` of the current segment is byte `i - base` of buffer `cur`;
/// the previous segment (`low_limit..dict_limit`, the "extDict") is in `dict` at
/// `dict_base`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    pub cur: Buf,
    pub base: i64,
    pub dict: Buf,
    pub dict_base: i64,
    /// Below this index, the data is in `dict`.
    pub dict_limit: u32,
    /// Below this index, no data is valid.
    pub low_limit: u32,
    /// The position in `cur` after the last data.
    pub next_pos: usize,
}

impl Window {
    /// `ZSTD_window_init`.
    pub(crate) fn new() -> Self {
        Self {
            cur: Buf::None,
            base: 0,
            dict: Buf::None,
            dict_base: 0,
            dict_limit: WINDOW_START_INDEX,
            low_limit: WINDOW_START_INDEX,
            next_pos: WINDOW_START_INDEX as usize,
        }
    }

    /// `ZSTD_window_update`: appends `size` bytes at `pos` of `buf`; a segment that does not
    /// follow the previous one makes the current segment the extDict. Returns whether it
    /// was contiguous.
    pub(crate) fn update(&mut self, buf: Buf, pos: usize, size: usize) -> bool {
        if size == 0 {
            return true;
        }
        let mut contiguous = true;
        if buf != self.cur || pos != self.next_pos {
            let distance_from_base = self.next_pos as i64 + self.base;
            self.low_limit = self.dict_limit;
            self.dict_limit = distance_from_base as u32;
            self.dict = self.cur;
            self.dict_base = self.base;
            self.cur = buf;
            self.base = distance_from_base - pos as i64;
            if self.dict_limit - self.low_limit < HASH_READ_SIZE as u32 {
                self.low_limit = self.dict_limit;
            }
            contiguous = false;
        }
        self.next_pos = pos + size;
        // Input overwriting the extDict reduces it.
        if self.dict == buf {
            let high = (pos + size) as i64 + self.dict_base;
            let low = pos as i64 + self.dict_base;
            if high > i64::from(self.low_limit) && low < i64::from(self.dict_limit) {
                self.low_limit = if high > i64::from(self.dict_limit) {
                    self.dict_limit
                } else {
                    high as u32
                };
            }
        }
        contiguous
    }

    /// `ZSTD_window_enforceMaxDist`, without a dictionary.
    pub(crate) fn enforce_max_dist(&mut self, block_end_index: u32, max_dist: u32) {
        if block_end_index > max_dist {
            let new_low_limit = block_end_index - max_dist;
            if self.low_limit < new_low_limit {
                self.low_limit = new_low_limit;
            }
            if self.dict_limit < self.low_limit {
                self.dict_limit = self.low_limit;
            }
        }
    }

    /// `ZSTD_window_clear`: nothing before the end is valid any more.
    pub(crate) fn clear(&mut self) {
        let end = (self.next_pos as i64 + self.base) as u32;
        self.low_limit = end;
        self.dict_limit = end;
    }

    /// `ZSTD_indexTooCloseToMax`: a new frame should restart the indexes.
    fn index_too_close_to_max(&self) -> bool {
        self.next_pos as i64 + self.base > CURRENT_MAX - INDEX_OVERFLOW_MARGIN
    }

    /// `ZSTD_window_needOverflowCorrection`: whether the index of `end` (a position of
    /// the current segment) passes `ZSTD_CURRENT_MAX`.
    pub(crate) fn need_overflow_correction(&self, end: usize) -> bool {
        end as i64 + self.base > CURRENT_MAX
    }

    /// `ZSTD_window_correctOverflow`: moves the indexes down so that the index of `pos`
    /// keeps its position in a cycle of `2^cycle_log` and the full window stays
    /// addressable; returns the correction.
    pub(crate) fn correct_overflow(&mut self, cycle_log: u32, max_dist: u32, pos: usize) -> u32 {
        let cycle_size = 1u32 << cycle_log;
        let cycle_mask = cycle_size - 1;
        let current = (pos as i64 + self.base) as u32;
        let current_cycle = current & cycle_mask;
        let current_cycle_correction = if current_cycle < WINDOW_START_INDEX {
            cycle_size.max(WINDOW_START_INDEX)
        } else {
            0
        };
        let new_current = current_cycle + current_cycle_correction + max_dist.max(cycle_size);
        let correction = current - new_current;
        debug_assert!(max_dist.is_power_of_two());
        debug_assert!(correction > 1 << 28);
        self.base -= i64::from(correction);
        self.dict_base -= i64::from(correction);
        self.low_limit = if self.low_limit < correction + WINDOW_START_INDEX {
            WINDOW_START_INDEX
        } else {
            self.low_limit - correction
        };
        self.dict_limit = if self.dict_limit < correction + WINDOW_START_INDEX {
            WINDOW_START_INDEX
        } else {
            self.dict_limit - correction
        };
        correction
    }

    /// `ZSTD_window_hasExtDict`.
    pub(crate) fn has_ext_dict(&self) -> bool {
        self.low_limit < self.dict_limit
    }

    /// `ZSTD_getLowestMatchIndex`, without a dictionary.
    pub(crate) fn lowest_match_index(&self, current: u32, window_log: u32) -> u32 {
        let max_distance = 1u32 << window_log;
        if current - self.low_limit > max_distance {
            current - max_distance
        } else {
            self.low_limit
        }
    }

    /// `ZSTD_getLowestPrefixIndex`, without a dictionary.
    pub(crate) fn lowest_prefix_index(&self, current: u32, window_log: u32) -> u32 {
        let max_distance = 1u32 << window_log;
        if current - self.dict_limit > max_distance {
            current - max_distance
        } else {
            self.dict_limit
        }
    }
}

/// The tables of the match finders and their window (`ZSTD_matchState_t`).
#[derive(Debug)]
pub(crate) struct MatchState {
    pub window: Window,
    pub params: CParams,
    pub hash_table: Vec<u32>,
    pub hash_table3: Vec<u32>,
    pub hash3_log: u32,
    pub opt: Option<Box<crate::opt::OptState>>,
    pub ldm_sequences: Vec<crate::ldm::RawSeq>,
    /// The chain table, or for dfast the table of short hashes.
    pub chain_table: Vec<u32>,
    pub next_to_update: u32,
    pub row: crate::row::RowState,
    /// The tables hold indexes of this window (`ZSTD_CCtx::initialized`).
    initialized: bool,
}

impl MatchState {
    pub(crate) fn new(params: CParams) -> Self {
        Self {
            window: Window::new(),
            params,
            hash_table: Vec::new(),
            hash_table3: Vec::new(),
            hash3_log: 0,
            opt: None,
            ldm_sequences: Vec::new(),
            chain_table: Vec::new(),
            next_to_update: 0,
            row: crate::row::RowState::default(),
            initialized: false,
        }
    }

    /// `ZSTD_reset_matchState`: a new frame, continuing the indexes of a context used before
    /// (the tables keep their stale entries) or starting a new window with clean tables.
    pub(crate) fn reset(&mut self, params: CParams, chain_table: bool) {
        self.params = params;
        self.hash3_log = if params.min_match == 3 {
            params.window_log.min(17)
        } else {
            0
        };
        let hash3_size = if self.hash3_log == 0 {
            0
        } else {
            1usize << self.hash3_log
        };
        if params.strategy >= Strategy::BtOpt {
            self.opt
                .get_or_insert_with(|| Box::new(crate::opt::OptState::new()))
                .reset();
        }
        let row = crate::params::row_match_finder_used(params);
        if row {
            self.row.reset(params);
        }
        let hash_size = 1usize << params.hash_log;
        let chain_size = if chain_table && !row {
            1usize << params.chain_log
        } else {
            0
        };
        if self.initialized
            && !self.window.index_too_close_to_max()
            && self.hash_table.len() == hash_size
            && self.chain_table.len() == chain_size
            && self.hash_table3.len() == hash3_size
        {
            // `ZSTDirp_continue`: the indexes go on from the previous frame, so the
            // tables' entries all lie below the new window and need no clearing
            // (`ZSTD_invalidateMatchState`).
            self.window.clear();
            self.next_to_update = self.window.dict_limit;
            return;
        }
        self.window = Window::new();
        self.next_to_update = self.window.dict_limit;
        self.hash_table.clear();
        self.hash_table.resize(hash_size, 0);
        self.chain_table.clear();
        self.chain_table.resize(chain_size, 0);
        self.hash_table3.clear();
        self.hash_table3.resize(hash3_size, 0);
        self.initialized = true;
    }

    /// `ZSTD_overflowCorrectIfNeeded` for the block `start..end` of the current segment:
    /// before an index would pass `ZSTD_CURRENT_MAX`, the window and every table entry
    /// move down by the same correction.
    pub(crate) fn overflow_correct_if_needed(&mut self, start: usize, end: usize) {
        if !self.window.need_overflow_correction(end) {
            return;
        }
        // `ZSTD_cycleLog`.
        let cycle_log = self.params.chain_log - u32::from(self.params.strategy >= Strategy::BtLazy2);
        let max_dist = 1u32 << self.params.window_log;
        let correction = self.window.correct_overflow(cycle_log, max_dist, start);
        reduce_table::<false>(&mut self.hash_table, correction);
        reduce_table::<false>(&mut self.hash_table3, correction);
        if self.params.strategy == Strategy::BtLazy2 {
            reduce_table::<true>(&mut self.chain_table, correction);
        } else {
            reduce_table::<false>(&mut self.chain_table, correction);
        }
        self.next_to_update = self.next_to_update.saturating_sub(correction);
    }
}

/// `ZSTD_reduceTable`: entries below the correction (and the reserved indexes above it)
/// become 0, the others move down.
fn reduce_table<const PRESERVE_MARK: bool>(table: &mut [u32], reducer: u32) {
    debug_assert!(table.len().is_multiple_of(ROW_SIZE));
    let threshold = reducer + WINDOW_START_INDEX;
    for entry in table {
        *entry = if PRESERVE_MARK && *entry == 1 {
            1
        } else if *entry < threshold {
            0
        } else {
            *entry - reducer
        };
    }
}

/// The memory a block's match finder reads: the current segment's buffer (which holds the
/// block) and the extDict's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mem<'a> {
    pub cur: &'a [u8],
    pub dict: &'a [u8],
}

/// `PREFETCH_L1`: brings the cache line at `pos` of `buf` closer (a hint; the address
/// may lie past the buffer).
#[inline(always)]
pub(crate) fn prefetch(buf: &[u8], pos: usize) {
    let address = buf.as_ptr().wrapping_add(pos);
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a prefetch does not fault, whatever the address.
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) address, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: as above.
    unsafe {
        std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(address.cast());
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let _ = address;
}

#[inline(always)]
pub(crate) fn read32(buf: &[u8], pos: usize) -> u32 {
    debug_assert!(pos + 4 <= buf.len(), "read of 4 bytes at {pos} of {}", buf.len());
    // SAFETY: the match finders only read inside the window, whose segments `buf` holds
    // (the same reads zstd makes); the tests run with the debug assertion.
    u32::from_le(unsafe { buf.as_ptr().add(pos).cast::<u32>().read_unaligned() })
}

#[inline(always)]
pub(crate) fn read64(buf: &[u8], pos: usize) -> u64 {
    debug_assert!(pos + 8 <= buf.len(), "read of 8 bytes at {pos} of {}", buf.len());
    // SAFETY: as for `read32`.
    u64::from_le(unsafe { buf.as_ptr().add(pos).cast::<u64>().read_unaligned() })
}

/// Entry `hash` of a hash table, whose size is a power of two the hashes are shifted to.
#[inline(always)]
pub(crate) fn get(table: &[u32], hash: usize) -> u32 {
    debug_assert!(hash < table.len());
    // SAFETY: `hash_ptr` returns hashes of the table's bit width.
    unsafe { *table.get_unchecked(hash) }
}

/// Sets entry `hash` of a hash table.
#[inline(always)]
pub(crate) fn set(table: &mut [u32], hash: usize, value: u32) {
    debug_assert!(hash < table.len());
    // SAFETY: as for `get`.
    unsafe { *table.get_unchecked_mut(hash) = value };
}

const PRIME4: u32 = 2_654_435_761;
const PRIME5: u64 = 889_523_592_379;
const PRIME6: u64 = 227_718_039_650_203;
const PRIME7: u64 = 58_295_818_150_454_627;
const PRIME8: u64 = 0xCF1B_BCDC_B7A5_6463;

/// The `mls` a match finder is instantiated for: 5 to 7, otherwise 4 (the `default` of the
/// `switch (mls)` that picks the template).
pub(crate) fn template_mls(min_match: u32) -> u32 {
    if (5..=7).contains(&min_match) {
        min_match
    } else {
        4
    }
}

/// `ZSTD_hashPtr`: the hash of the `MLS` bytes at `pos`, `h_bits` wide.
#[inline(always)]
pub(crate) fn hash_ptr<const MLS: u32>(buf: &[u8], pos: usize, h_bits: u32) -> usize {
    match MLS {
        5 => ((read64(buf, pos) << 24).wrapping_mul(PRIME5) >> (64 - h_bits)) as usize,
        6 => ((read64(buf, pos) << 16).wrapping_mul(PRIME6) >> (64 - h_bits)) as usize,
        7 => ((read64(buf, pos) << 8).wrapping_mul(PRIME7) >> (64 - h_bits)) as usize,
        8 => (read64(buf, pos).wrapping_mul(PRIME8) >> (64 - h_bits)) as usize,
        _ => (read32(buf, pos).wrapping_mul(PRIME4) >> (32 - h_bits)) as usize,
    }
}

/// `ZSTD_count`: the length of the common prefix of `buf[ip..limit]` and the bytes at
/// `matched` of the same buffer.
#[inline(always)]
pub(crate) fn count(buf: &[u8], ip: usize, matched: usize, limit: usize) -> usize {
    count_between(buf, ip, buf, matched, limit)
}

/// `ZSTD_count` with the match in another buffer.
#[inline(always)]
pub(crate) fn count_between(input: &[u8], ip: usize, other: &[u8], matched: usize, limit: usize) -> usize {
    let start = ip;
    let (mut ip, mut matched) = (ip, matched);
    while ip + 8 <= limit {
        let diff = read64(other, matched) ^ read64(input, ip);
        if diff != 0 {
            return ip - start + (diff.trailing_zeros() >> 3) as usize;
        }
        ip += 8;
        matched += 8;
    }
    if ip + 4 <= limit && read32(other, matched) == read32(input, ip) {
        ip += 4;
        matched += 4;
    }
    if ip + 2 <= limit && read16(other, matched) == read16(input, ip) {
        ip += 2;
        matched += 2;
    }
    if ip < limit && byte_at(other, matched) == byte_at(input, ip) {
        ip += 1;
    }
    ip - start
}

#[inline(always)]
fn read16(buf: &[u8], pos: usize) -> u16 {
    debug_assert!(pos + 2 <= buf.len());
    // SAFETY: as for `read32`.
    u16::from_le(unsafe { buf.as_ptr().add(pos).cast::<u16>().read_unaligned() })
}

#[inline(always)]
fn byte_at(buf: &[u8], pos: usize) -> u8 {
    debug_assert!(pos < buf.len());
    // SAFETY: as for `read32`.
    unsafe { *buf.get_unchecked(pos) }
}

/// `ZSTD_count_2segments`: a match in the extDict ending at `match_end` continues at the
/// start of the current segment (`i_start`).
#[inline]
pub(crate) fn count_2segments(
    mem: Mem<'_>,
    ip: usize,
    matched: usize,
    match_in_dict: bool,
    i_end: usize,
    match_end: usize,
    i_start: usize,
) -> usize {
    let match_buf = if match_in_dict { mem.dict } else { mem.cur };
    let v_end = (ip + (match_end - matched)).min(i_end);
    let length = count_between(mem.cur, ip, match_buf, matched, v_end);
    if matched + length != match_end {
        return length;
    }
    length + count(mem.cur, ip + length, i_start, i_end)
}

#[cfg(test)]
mod tests {
    use super::reduce_table;

    #[test]
    fn binary_tree_unsorted_marks_survive_index_reduction() {
        // Frozen ZSTD_reduceTable_internal: indexes below reducer+2 disappear;
        // the btlazy2 chain's reserved unsorted marker survives unchanged.
        let original = [
            0,
            1,
            2,
            99,
            100,
            101,
            102,
            103,
            150,
            199,
            200,
            201,
            202,
            300,
            400,
            u32::MAX,
        ];
        let mut tree = original;
        reduce_table::<true>(&mut tree, 100);
        assert_eq!(
            tree,
            [
                0,
                1,
                0,
                0,
                0,
                0,
                2,
                3,
                50,
                99,
                100,
                101,
                102,
                200,
                300,
                u32::MAX - 100
            ]
        );
        let mut plain = original;
        reduce_table::<false>(&mut plain, 100);
        tree[1] = 0;
        assert_eq!(plain, tree);
    }
}
