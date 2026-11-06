//! Salted row-hash matching (`ZSTD_RowFindBestMatch` and its hash-cache helpers).

use crate::params::CParams;
use crate::seq::offset_to_off_base;
use crate::window::{MatchState, Mem, Window, count, count_2segments, read32, read64};

const CACHE_SIZE: usize = 8;

/// State retained across blocks and context resets by the row matcher.
#[derive(Debug, Default)]
pub(crate) struct RowState {
    pub tags: Vec<u8>,
    cache: [u32; CACHE_SIZE],
    salt: u64,
    entropy: u32,
    hash_log: u32,
    row_log: u32,
}

fn bitmix(mut value: u64, length: u64) -> u64 {
    value ^= value.rotate_right(49) ^ value.rotate_right(24);
    value = value.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    value ^= (value >> 35).wrapping_add(length);
    value = value.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    value ^ (value >> 28)
}

impl RowState {
    pub fn reset(&mut self, params: CParams) {
        let size = 1usize << params.hash_log;
        if self.tags.len() != size {
            self.tags.clear();
            self.tags.resize(size, 0);
        }
        self.salt = bitmix(self.salt, 8) ^ bitmix(u64::from(self.entropy), 4);
        self.row_log = params.search_log.clamp(4, 6);
        self.hash_log = params.hash_log - self.row_log;
    }

    /// `ZSTD_hashPtrSalted`: salt is combined with `XOR` after multiplication, before reduction.
    #[inline(always)]
    fn hash(&self, buf: &[u8], pos: usize, min_match: u32) -> u32 {
        let bits = self.hash_log + 8;
        match min_match.clamp(4, 6) {
            5 => (((read64(buf, pos) << 24).wrapping_mul(889_523_592_379) ^ self.salt) >> (64 - bits)) as u32,
            6 => {
                (((read64(buf, pos) << 16).wrapping_mul(227_718_039_650_203) ^ self.salt) >> (64 - bits))
                    as u32
            }
            _ => (read32(buf, pos).wrapping_mul(2_654_435_761) ^ self.salt as u32) >> (32 - bits),
        }
    }

    fn fill_cache(&mut self, buf: &[u8], base: i64, index: u32, limit: isize, min_match: u32) {
        for step in 0..CACHE_SIZE {
            let index = index + step as u32;
            let pos = i64::from(index) - base;
            if pos > limit as i64 {
                break;
            }
            self.cache[index as usize & (CACHE_SIZE - 1)] = self.hash(buf, pos as usize, min_match);
        }
    }

    #[inline(always)]
    fn next_hash(&mut self, buf: &[u8], base: i64, index: u32, min_match: u32) -> u32 {
        let pos = (i64::from(index) - base) as usize;
        let new_hash = self.hash(buf, pos + CACHE_SIZE, min_match);
        std::mem::replace(&mut self.cache[index as usize & (CACHE_SIZE - 1)], new_hash)
    }

    #[inline(always)]
    fn insert(&mut self, table: &mut [u32], hash: u32, index: u32) {
        let row = ((hash >> 8) << self.row_log) as usize;
        let mask = (1u8 << self.row_log) - 1;
        let mut next = self.tags[row].wrapping_sub(1) & mask;
        if next == 0 {
            next += mask;
        }
        self.tags[row] = next;
        self.tags[row + usize::from(next)] = hash as u8;
        table[row + usize::from(next)] = index;
    }

    fn update_range(
        &mut self,
        table: &mut [u32],
        buf: &[u8],
        window: Window,
        start: u32,
        end: u32,
        min_match: u32,
    ) {
        for index in start..end {
            let hash = self.next_hash(buf, window.base, index, min_match);
            self.insert(table, hash, index);
        }
    }
}

/// `ZSTD_row_fillHashCache`, at parser entry and when leaving lazy skipping.
pub(crate) fn fill_cache(ms: &mut MatchState, buf: &[u8], limit: isize) {
    ms.row
        .fill_cache(buf, ms.window.base, ms.next_to_update, limit, ms.params.min_match);
}

/// `ZSTD_RowFindBestMatch`, without an attached dictionary; extDict is supported.
#[allow(clippy::too_many_lines, reason = "the frozen row update/search lifecycle")]
pub(crate) fn find_best_match(
    ms: &mut MatchState,
    mem: Mem<'_>,
    ip: usize,
    end: usize,
    lazy_skipping: bool,
) -> (usize, u32) {
    let window = ms.window;
    let params = ms.params;
    let current = (ip as i64 + window.base) as u32;
    let low_limit = window
        .low_limit
        .max(current.saturating_sub(1 << params.window_log));
    let row = &mut ms.row;
    let hash = if lazy_skipping {
        ms.next_to_update = current;
        row.hash(mem.cur, ip, params.min_match)
    } else {
        let mut index = ms.next_to_update;
        if current - index > 384 {
            row.update_range(
                &mut ms.hash_table,
                mem.cur,
                window,
                index,
                index + 96,
                params.min_match,
            );
            index = current - 32;
            row.fill_cache(mem.cur, window.base, index, (ip + 1) as isize, params.min_match);
        }
        row.update_range(
            &mut ms.hash_table,
            mem.cur,
            window,
            index,
            current,
            params.min_match,
        );
        ms.next_to_update = current;
        row.next_hash(mem.cur, window.base, current, params.min_match)
    };
    row.entropy = row.entropy.wrapping_add(hash);
    let row_start = ((hash >> 8) << row.row_log) as usize;
    let row_entries = 1usize << row.row_log;
    let mask = row_entries - 1;
    let head = usize::from(row.tags[row_start]) & mask;
    let tag = hash as u8;
    let mut attempts = 1u32 << params.search_log.min(row.row_log);
    let mut matches = [0u32; 64];
    let mut count_matches = 0;
    // Rotated SIMD masks visit exactly this circular order, skipping slot zero.
    for relative in 0..row_entries {
        let pos = (head + relative) & mask;
        if pos == 0 || row.tags[row_start + pos] != tag {
            continue;
        }
        let index = ms.hash_table[row_start + pos];
        if index < low_limit || attempts == 0 {
            break;
        }
        matches[count_matches] = index;
        count_matches += 1;
        attempts -= 1;
    }
    row.insert(&mut ms.hash_table, hash, ms.next_to_update);
    ms.next_to_update += 1;
    let prefix_start = (i64::from(window.dict_limit) - window.base) as usize;
    let mut best = 3;
    let mut off_base = 999_999_999;
    for &index in &matches[..count_matches] {
        let length = if index >= window.dict_limit {
            let matched = (i64::from(index) - window.base) as usize;
            if read32(mem.cur, matched + best - 3) == read32(mem.cur, ip + best - 3) {
                count(mem.cur, ip, matched, end)
            } else {
                0
            }
        } else {
            let matched = (i64::from(index) - window.dict_base) as usize;
            let dict_end = (i64::from(window.dict_limit) - window.dict_base) as usize;
            if read32(mem.cur, ip) == read32(mem.dict, matched) {
                count_2segments(mem, ip + 4, matched + 4, true, end, dict_end, prefix_start) + 4
            } else {
                0
            }
        };
        if length > best {
            best = length;
            off_base = offset_to_off_base(current - index);
            if ip + length == end {
                break;
            }
        }
    }
    (best, off_base)
}
