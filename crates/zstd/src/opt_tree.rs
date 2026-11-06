//! All-match binary-tree search used by the optimal parser (`zstd_opt.c`).

use crate::bt_lazy::{byte_at, common_length, hash, node, position, set_link};
use crate::opt::{Match, OPT_NUM};
use crate::seq::offset_to_off_base;
use crate::window::{MatchState, Mem, read32};

fn insert(ms: &mut MatchState, mem: Mem<'_>, current: u32, target: u32, end: usize) -> u32 {
    let window = ms.window;
    let ip = position(current, window.base);
    let h = hash(mem.cur, ip, ms.params.hash_log, ms.params.min_match);
    let mask = (ms.chain_table.len() / 2 - 1) as u32;
    let tree_low = current.saturating_sub(mask);
    let low = window
        .low_limit
        .max(target.saturating_sub(1 << ms.params.window_log));
    let mut matched = ms.hash_table[h];
    ms.hash_table[h] = current;
    let mut smaller = Some(node(current, mask));
    let mut larger = Some(node(current, mask) + 1);
    let mut common_small = 0;
    let mut common_large = 0;
    let mut best = 8;
    let mut match_end = current + 9;
    let mut comparisons = 1u32 << ms.params.search_log;
    while comparisons > 0 && matched >= low {
        let next = node(matched, mask);
        let length = common_length(window, mem, current, matched, common_small.min(common_large), end);
        if length > best {
            best = length;
            match_end = match_end.max(matched + length as u32);
        }
        if ip + length == end {
            break;
        }
        if byte_at(window, mem, matched + length as u32) < mem.cur[ip + length] {
            set_link(&mut ms.chain_table, smaller, matched);
            common_small = length;
            if matched <= tree_low {
                smaller = None;
                break;
            }
            smaller = Some(next + 1);
            matched = ms.chain_table[next + 1];
        } else {
            set_link(&mut ms.chain_table, larger, matched);
            common_large = length;
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
    let skip = if best > 384 {
        (best - 384).min(192) as u32
    } else {
        0
    };
    skip.max(match_end - current - 8)
}

fn hash3(buf: &[u8], pos: usize, bits: u32) -> usize {
    ((read32(buf, pos) << 8).wrapping_mul(506_832_829) >> (32 - bits)) as usize
}

fn read_min(buf: &[u8], pos: usize, min: u32) -> u32 {
    let value = read32(buf, pos);
    if min == 3 { value << 8 } else { value }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the frozen parser's all-match interface"
)]
pub(crate) fn get_all_matches(
    ms: &mut MatchState,
    mem: Mem<'_>,
    ip: usize,
    end: usize,
    rep: [u32; 3],
    ll0: bool,
    length_to_beat: u32,
    next3: &mut u32,
    matches: &mut [Match],
) -> usize {
    let current = (ip as i64 + ms.window.base) as u32;
    if current < ms.next_to_update {
        return 0;
    }
    let mut index = ms.next_to_update;
    while index < current {
        index += insert(ms, mem, index, current, end);
    }
    ms.next_to_update = current;
    let params = ms.params;
    let window = ms.window;
    let low = window
        .low_limit
        .max(current.saturating_sub(1 << params.window_log));
    let match_low = low.max(1);
    let sufficient = params.target_length.min(OPT_NUM as u32 - 1);
    let min = if params.min_match == 3 { 3 } else { 4 };
    let mut best = length_to_beat - 1;
    let mut count_matches = 0;
    for code in u32::from(ll0)..3 + u32::from(ll0) {
        let offset = if code == 3 {
            rep[0].wrapping_sub(1)
        } else {
            rep[code as usize]
        };
        let index = current.wrapping_sub(offset);
        let length = if offset.wrapping_sub(1) < current - window.dict_limit {
            if index >= low && read_min(mem.cur, ip, min) == read_min(mem.cur, ip - offset as usize, min) {
                common_length(window, mem, current, index, min as usize, end)
            } else {
                0
            }
        } else if window.has_ext_dict()
            && offset.wrapping_sub(1) < current - low
            && window.dict_limit.wrapping_sub(1).wrapping_sub(index) >= 3
            && read_min(mem.cur, ip, min) == read_min(mem.dict, position(index, window.dict_base), min)
        {
            common_length(window, mem, current, index, min as usize, end)
        } else {
            0
        };
        if length > best as usize {
            best = length as u32;
            matches[count_matches] = Match {
                off: code - u32::from(ll0) + 1,
                len: best,
            };
            count_matches += 1;
            if best > sufficient || ip + length == end {
                return count_matches;
            }
        }
    }
    let h = hash(mem.cur, ip, params.hash_log, params.min_match);
    let mut matched = ms.hash_table[h];
    if params.min_match == 3 && best < 3 {
        let h3 = hash3(mem.cur, ip, ms.hash3_log);
        for index in *next3..current {
            ms.hash_table3[hash3(mem.cur, position(index, window.base), ms.hash3_log)] = index;
        }
        *next3 = current;
        let index = ms.hash_table3[h3];
        if index >= match_low && current - index < 1 << 18 {
            let length = common_length(window, mem, current, index, 0, end) as u32;
            if length >= 3 {
                best = length;
                matches[0] = Match {
                    off: offset_to_off_base(current - index),
                    len: length,
                };
                count_matches = 1;
                if length > sufficient || ip + length as usize == end {
                    ms.next_to_update = current + 1;
                    return 1;
                }
            }
        }
    }
    ms.hash_table[h] = current;
    let mask = (ms.chain_table.len() / 2 - 1) as u32;
    let tree_low = current.saturating_sub(mask);
    let mut smaller = Some(node(current, mask));
    let mut larger = Some(node(current, mask) + 1);
    let mut common_small = 0;
    let mut common_large = 0;
    let mut match_end = current + 9;
    let mut comparisons = 1u32 << params.search_log;
    while comparisons > 0 && matched >= match_low {
        let next = node(matched, mask);
        let length = common_length(window, mem, current, matched, common_small.min(common_large), end);
        if length > best as usize {
            match_end = match_end.max(matched + length as u32);
            best = length as u32;
            matches[count_matches] = Match {
                off: offset_to_off_base(current - matched),
                len: best,
            };
            count_matches += 1;
            if length > OPT_NUM || ip + length == end {
                break;
            }
        }
        if byte_at(window, mem, matched + length as u32) < mem.cur[ip + length] {
            set_link(&mut ms.chain_table, smaller, matched);
            common_small = length;
            if matched <= tree_low {
                smaller = None;
                break;
            }
            smaller = Some(next + 1);
            matched = ms.chain_table[next + 1];
        } else {
            set_link(&mut ms.chain_table, larger, matched);
            common_large = length;
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
    count_matches
}
