//! Delayed unsorted binary-tree search for `ZSTD_btlazy2` (`zstd_lazy.c`).

use crate::params::highbit32;
use crate::seq::offset_to_off_base;
use crate::window::{MatchState, Mem, Window, count_2segments, count_between, hash_ptr};

const UNSORTED: u32 = 1;

#[inline(always)]
pub(crate) fn hash(buf: &[u8], pos: usize, bits: u32, min_match: u32) -> usize {
    match min_match.clamp(4, 6) {
        5 => hash_ptr::<5>(buf, pos, bits),
        6 => hash_ptr::<6>(buf, pos, bits),
        _ => hash_ptr::<4>(buf, pos, bits),
    }
}

#[inline(always)]
pub(crate) fn node(index: u32, mask: u32) -> usize {
    2 * (index & mask) as usize
}

#[inline(always)]
pub(crate) fn set_link(table: &mut [u32], link: Option<usize>, value: u32) {
    if let Some(index) = link {
        table[index] = value;
    }
}

#[inline(always)]
pub(crate) fn position(index: u32, base: i64) -> usize {
    (i64::from(index) - base) as usize
}

pub(crate) fn byte_at(window: Window, mem: Mem<'_>, index: u32) -> u8 {
    if index >= window.dict_limit {
        mem.cur[position(index, window.base)]
    } else {
        mem.dict[position(index, window.dict_base)]
    }
}

/// Common bytes of two indexed suffixes, including an external dictionary seam.
pub(crate) fn common_length(
    window: Window,
    mem: Mem<'_>,
    current: u32,
    matched: u32,
    known: usize,
    end: usize,
) -> usize {
    let matched = matched + known as u32;
    if current < window.dict_limit {
        let ip = position(current, window.dict_base);
        let end = position(window.dict_limit, window.dict_base);
        known
            + count_between(
                mem.dict,
                ip + known,
                mem.dict,
                position(matched, window.dict_base),
                end,
            )
    } else {
        let ip = position(current, window.base);
        if matched >= window.dict_limit {
            known + count_between(mem.cur, ip + known, mem.cur, position(matched, window.base), end)
        } else {
            let dict_end = position(window.dict_limit, window.dict_base);
            let prefix = position(window.dict_limit, window.base);
            known
                + count_2segments(
                    mem,
                    ip + known,
                    position(matched, window.dict_base),
                    true,
                    end,
                    dict_end,
                    prefix,
                )
        }
    }
}

/// `ZSTD_updateDUBT`: initially link preceding positions as an unsorted chain.
fn update(ms: &mut MatchState, mem: Mem<'_>, current: u32) {
    let mask = (ms.chain_table.len() / 2 - 1) as u32;
    for index in ms.next_to_update..current {
        let h = hash(
            mem.cur,
            position(index, ms.window.base),
            ms.params.hash_log,
            ms.params.min_match,
        );
        let matched = ms.hash_table[h];
        ms.hash_table[h] = index;
        let slot = node(index, mask);
        ms.chain_table[slot] = matched;
        ms.chain_table[slot + 1] = UNSORTED;
    }
    ms.next_to_update = current;
}

/// `ZSTD_insertDUBT1`: sort one position whose later link holds the reverse chain.
fn sort_one(
    ms: &mut MatchState,
    mem: Mem<'_>,
    current: u32,
    end: usize,
    mut comparisons: u32,
    tree_low: u32,
) {
    let mask = (ms.chain_table.len() / 2 - 1) as u32;
    let mut smaller = Some(node(current, mask));
    let mut larger = Some(node(current, mask) + 1);
    let mut matched = ms.chain_table[node(current, mask)];
    let mut common_smaller = 0;
    let mut common_larger = 0;
    let window = ms.window;
    let window_low = window
        .low_limit
        .max(current.saturating_sub(1 << ms.params.window_log));
    let input_end = if current < window.dict_limit {
        position(window.dict_limit, window.dict_base)
    } else {
        end
    };
    let input_pos = position(
        current,
        if current < window.dict_limit {
            window.dict_base
        } else {
            window.base
        },
    );
    while comparisons > 0 && matched > window_low {
        let next = node(matched, mask);
        let known = common_smaller.min(common_larger);
        let length = common_length(window, mem, current, matched, known, end);
        if input_pos + length == input_end {
            break;
        }
        if byte_at(window, mem, matched + length as u32) < byte_at(window, mem, current + length as u32) {
            set_link(&mut ms.chain_table, smaller, matched);
            common_smaller = length;
            if matched <= tree_low {
                smaller = None;
                break;
            }
            smaller = Some(next + 1);
            matched = ms.chain_table[next + 1];
        } else {
            set_link(&mut ms.chain_table, larger, matched);
            common_larger = length;
            if matched <= tree_low {
                larger = None;
                break;
            }
            larger = Some(next);
            matched = ms.chain_table[next];
        }
        comparisons -= 1;
    }
    set_link(&mut ms.chain_table, smaller, 0);
    set_link(&mut ms.chain_table, larger, 0);
}

/// `ZSTD_BtFindBestMatch` / `ZSTD_DUBT_findBestMatch`, without an attached dictionary.
pub(crate) fn find_best_match(ms: &mut MatchState, mem: Mem<'_>, ip: usize, end: usize) -> (usize, u32) {
    let current = (ip as i64 + ms.window.base) as u32;
    if current < ms.next_to_update {
        return (0, 999_999_999);
    }
    update(ms, mem, current);
    let params = ms.params;
    let h = hash(mem.cur, ip, params.hash_log, params.min_match);
    let mask = (ms.chain_table.len() / 2 - 1) as u32;
    let tree_low = current.saturating_sub(mask);
    let window = ms.window;
    let window_low = window
        .low_limit
        .max(current.saturating_sub(1 << params.window_log));
    let unsort_limit = tree_low.max(window_low);
    let mut comparisons = 1u32 << params.search_log;
    let mut candidates = comparisons;
    let mut matched = ms.hash_table[h];
    let mut previous = 0;
    while matched > unsort_limit && ms.chain_table[node(matched, mask) + 1] == UNSORTED && candidates > 1 {
        let slot = node(matched, mask);
        ms.chain_table[slot + 1] = previous;
        previous = matched;
        matched = ms.chain_table[slot];
        candidates -= 1;
    }
    if matched > unsort_limit && ms.chain_table[node(matched, mask) + 1] == UNSORTED {
        let slot = node(matched, mask);
        ms.chain_table[slot] = 0;
        ms.chain_table[slot + 1] = 0;
    }
    matched = previous;
    while matched != 0 {
        let next = ms.chain_table[node(matched, mask) + 1];
        sort_one(ms, mem, matched, end, candidates, unsort_limit);
        matched = next;
        candidates += 1;
    }
    let mut smaller = Some(node(current, mask));
    let mut larger = Some(node(current, mask) + 1);
    let mut common_smaller = 0;
    let mut common_larger = 0;
    let mut match_end = current + 9;
    let mut best = 0;
    let mut off_base = 999_999_999;
    matched = ms.hash_table[h];
    ms.hash_table[h] = current;
    while comparisons > 0 && matched > window_low {
        let next = node(matched, mask);
        let known = common_smaller.min(common_larger);
        let length = common_length(window, mem, current, matched, known, end);
        if length > best {
            match_end = match_end.max(matched + length as u32);
            let gain = i64::from(highbit32(current - matched + 1)) - i64::from(highbit32(off_base));
            if 4 * (length - best) as i64 > gain {
                best = length;
                off_base = offset_to_off_base(current - matched);
            }
            if ip + length == end {
                break;
            }
        }
        if byte_at(window, mem, matched + length as u32) < byte_at(window, mem, current + length as u32) {
            set_link(&mut ms.chain_table, smaller, matched);
            common_smaller = length;
            if matched <= tree_low {
                smaller = None;
                break;
            }
            smaller = Some(next + 1);
            matched = ms.chain_table[next + 1];
        } else {
            set_link(&mut ms.chain_table, larger, matched);
            common_larger = length;
            if matched <= tree_low {
                larger = None;
                break;
            }
            larger = Some(next);
            matched = ms.chain_table[next];
        }
        comparisons -= 1;
    }
    set_link(&mut ms.chain_table, smaller, 0);
    set_link(&mut ms.chain_table, larger, 0);
    ms.next_to_update = match_end - 8;
    (best, off_base)
}
