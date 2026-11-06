//! Greedy/Lazy/Lazy2 parsing with hash-chain or row-hash search (`zstd_lazy.c`).

use crate::params::{Strategy, highbit32};
use crate::seq::{REPCODE1, SeqStore, offset_to_off_base};
use crate::window::{MatchState, Mem, SEARCH_STRENGTH, count, count_2segments, hash_ptr, read32};

#[inline(always)]
fn hash(buf: &[u8], pos: usize, bits: u32, min_match: u32) -> usize {
    match min_match {
        5 => hash_ptr::<5>(buf, pos, bits),
        6 => hash_ptr::<6>(buf, pos, bits),
        _ => hash_ptr::<4>(buf, pos, bits),
    }
}

/// Select hash-chain or row search without an attached dictionary.
fn find_best_match(
    ms: &mut MatchState,
    mem: Mem<'_>,
    ip: usize,
    end: usize,
    lazy_skipping: bool,
) -> (usize, u32) {
    if ms.params.strategy == Strategy::BtLazy2 {
        return crate::bt_lazy::find_best_match(ms, mem, ip, end);
    }
    if crate::params::row_match_finder_used(ms.params) {
        return crate::row::find_best_match(ms, mem, ip, end, lazy_skipping);
    }
    let buf = mem.cur;
    let params = ms.params;
    let base = ms.window.base;
    let current = (ip as i64 + base) as u32;
    let low_limit = ms
        .window
        .low_limit
        .max(current.saturating_sub(1 << params.window_log));
    let chain_size = ms.chain_table.len() as u32;
    let chain_mask = chain_size - 1;
    let min_chain = current.saturating_sub(chain_size);
    let min_match = params.min_match.clamp(4, 6);
    let pos_of = |index: u32| (i64::from(index) - base) as usize;

    // Insert every preceding position, or just the first while lazily skipping
    // a long incompressible span. The current position is inserted next time.
    let mut index = ms.next_to_update;
    while index < current {
        let h = hash(buf, pos_of(index), params.hash_log, min_match);
        ms.chain_table[(index & chain_mask) as usize] = ms.hash_table[h];
        ms.hash_table[h] = index;
        index += 1;
        if lazy_skipping {
            break;
        }
    }
    ms.next_to_update = current;
    let mut matched_index = ms.hash_table[hash(buf, ip, params.hash_log, min_match)];
    let mut attempts = 1u32 << params.search_log;
    let mut best = 3;
    let mut off_base = 999_999_999;
    while matched_index >= low_limit && attempts > 0 {
        let length = if matched_index >= ms.window.dict_limit {
            let matched = pos_of(matched_index);
            if read32(buf, matched + best - 3) == read32(buf, ip + best - 3) {
                count(buf, ip, matched, end)
            } else {
                0
            }
        } else {
            let matched = (i64::from(matched_index) - ms.window.dict_base) as usize;
            let dict_end = (i64::from(ms.window.dict_limit) - ms.window.dict_base) as usize;
            let prefix_start = pos_of(ms.window.dict_limit);
            if read32(mem.dict, matched) == read32(buf, ip) {
                count_2segments(mem, ip + 4, matched + 4, true, end, dict_end, prefix_start) + 4
            } else {
                0
            }
        };
        if length > best {
            best = length;
            off_base = offset_to_off_base(current - matched_index);
            if ip + length == end {
                break;
            }
        }
        if matched_index <= min_chain {
            break;
        }
        matched_index = ms.chain_table[(matched_index & chain_mask) as usize];
        attempts -= 1;
    }
    (best, off_base)
}

/// Read a repeat match, rejecting references outside the window or spanning
/// the final three bytes of the external dictionary.
fn rep_length(ms: &MatchState, mem: Mem<'_>, ip: usize, offset: u32, end: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    if !ms.window.has_ext_dict() {
        if read32(mem.cur, ip) != read32(mem.cur, ip - offset as usize) {
            return 0;
        }
        return count(mem.cur, ip + 4, ip + 4 - offset as usize, end) + 4;
    }
    let window = ms.window;
    let current = (ip as i64 + window.base) as u32;
    let low = window
        .low_limit
        .max(current.saturating_sub(1 << ms.params.window_log));
    if offset > current - low {
        return 0;
    }
    let index = current - offset;
    if window.dict_limit.wrapping_sub(1).wrapping_sub(index) < 3 {
        return 0;
    }
    let in_dict = index < window.dict_limit;
    let base = if in_dict { window.dict_base } else { window.base };
    let buf = if in_dict { mem.dict } else { mem.cur };
    let matched = (i64::from(index) - base) as usize;
    if read32(mem.cur, ip) != read32(buf, matched) {
        return 0;
    }
    let match_end = if in_dict {
        (i64::from(window.dict_limit) - window.dict_base) as usize
    } else {
        end
    };
    let prefix_start = (i64::from(window.dict_limit) - window.base) as usize;
    count_2segments(mem, ip + 4, matched + 4, in_dict, end, match_end, prefix_start) + 4
}

/// `ZSTD_compressBlock_lazy_generic` and its external-dictionary variant.
#[allow(
    clippy::too_many_lines,
    reason = "the frozen parser's sequence/repeat-offset lifecycle"
)]
pub(crate) fn compress_block(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    istart: usize,
    src_size: usize,
) -> usize {
    let ext_dict = ms.window.has_ext_dict();
    let depth = match ms.params.strategy {
        Strategy::Greedy => 0,
        Strategy::Lazy => 1,
        Strategy::Lazy2 | Strategy::BtLazy2 => 2,
        _ => unreachable!("hash-chain parser requires a lazy strategy"),
    };
    let buf = mem.cur;
    let base = ms.window.base;
    let prefix_lowest = (i64::from(ms.window.dict_limit) - base) as usize;
    let end = istart + src_size;
    let row = crate::params::row_match_finder_used(ms.params);
    let limit = end as isize - if row { 16 } else { 8 };
    let mut anchor = istart;
    let mut ip = istart + usize::from(istart == prefix_lowest);
    let mut offset1 = rep[0];
    let mut offset2 = rep[1];
    let mut saved1 = 0;
    let mut saved2 = 0;
    let current = (ip as i64 + base) as u32;
    let window_low = ms.window.lowest_prefix_index(current, ms.params.window_log);
    let max_rep = current - window_low;
    if !ext_dict && offset2 > max_rep {
        saved2 = offset2;
        offset2 = 0;
    }
    if !ext_dict && offset1 > max_rep {
        saved1 = offset1;
        offset1 = 0;
    }
    let mut lazy_skipping = false;
    if row {
        crate::row::fill_cache(ms, buf, limit);
    }
    while (ip as isize) < limit {
        let mut off_base = REPCODE1;
        let mut start = ip + 1;
        let mut length = rep_length(ms, mem, ip + 1, offset1, end);
        let greedy_rep = depth == 0 && length >= 4;
        if !greedy_rep {
            let (candidate, candidate_offset) = find_best_match(ms, mem, ip, end, lazy_skipping);
            if candidate > length {
                length = candidate;
                start = ip;
                off_base = candidate_offset;
            }
            if length < 4 {
                let step = ((ip - anchor) >> SEARCH_STRENGTH) + 1;
                ip += step;
                lazy_skipping = if ext_dict { step - 1 > 8 } else { step > 8 };
                continue;
            }
            if depth >= 1 {
                while (ip as isize) < limit {
                    ip += 1;
                    if off_base != 0 {
                        let rep_length = rep_length(ms, mem, ip, offset1, end);
                        if rep_length >= 4
                            && rep_length as i64 * 3 > length as i64 * 3 - i64::from(highbit32(off_base)) + 1
                        {
                            length = rep_length;
                            off_base = REPCODE1;
                            start = ip;
                        }
                    }
                    let (candidate, candidate_offset) = find_best_match(ms, mem, ip, end, lazy_skipping);
                    let gain2 = candidate as i64 * 4 - i64::from(highbit32(candidate_offset));
                    let gain1 = length as i64 * 4 - i64::from(highbit32(off_base)) + 4;
                    if candidate >= 4 && gain2 > gain1 {
                        length = candidate;
                        off_base = candidate_offset;
                        start = ip;
                        continue;
                    }
                    if depth == 2 && (ip as isize) < limit {
                        ip += 1;
                        if off_base != 0 {
                            let rep_length = rep_length(ms, mem, ip, offset1, end);
                            if rep_length >= 4
                                && rep_length as i64 * 4
                                    > length as i64 * 4 - i64::from(highbit32(off_base)) + 1
                            {
                                length = rep_length;
                                off_base = REPCODE1;
                                start = ip;
                            }
                        }
                        let (candidate, candidate_offset) = find_best_match(ms, mem, ip, end, lazy_skipping);
                        let gain2 = candidate as i64 * 4 - i64::from(highbit32(candidate_offset));
                        let gain1 = length as i64 * 4 - i64::from(highbit32(off_base)) + 7;
                        if candidate >= 4 && gain2 > gain1 {
                            length = candidate;
                            off_base = candidate_offset;
                            start = ip;
                            continue;
                        }
                    }
                    break;
                }
            }
            if off_base > 3 {
                let offset = off_base - 3;
                let matched_index = (start as i64 + base) as u32 - offset;
                let in_dict = ext_dict && matched_index < ms.window.dict_limit;
                let matched_base = if in_dict { ms.window.dict_base } else { base };
                let matched_buf = if in_dict { mem.dict } else { buf };
                let mut matched = (i64::from(matched_index) - matched_base) as usize;
                let lowest = if in_dict {
                    (i64::from(ms.window.low_limit) - matched_base) as usize
                } else {
                    prefix_lowest
                };
                while start > anchor && matched > lowest && buf[start - 1] == matched_buf[matched - 1] {
                    start -= 1;
                    matched -= 1;
                    length += 1;
                }
                offset2 = offset1;
                offset1 = off_base - 3;
            }
        }
        seq.store(buf, anchor, start - anchor, off_base, length);
        ip = start + length;
        anchor = ip;
        if lazy_skipping && row {
            crate::row::fill_cache(ms, buf, limit);
        }
        lazy_skipping = false;
        while (ip as isize) <= limit {
            let length = rep_length(ms, mem, ip, offset2, end);
            if length < 4 {
                break;
            }
            std::mem::swap(&mut offset1, &mut offset2);
            seq.store(buf, anchor, 0, REPCODE1, length);
            ip += length;
            anchor = ip;
        }
    }
    if !ext_dict && saved1 != 0 && offset1 != 0 {
        saved2 = saved1;
    }
    rep[0] = if offset1 == 0 { saved1 } else { offset1 };
    rep[1] = if offset2 == 0 { saved2 } else { offset2 };
    end - anchor
}
