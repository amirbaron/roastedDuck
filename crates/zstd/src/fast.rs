//! The `ZSTD_fast` match finder (`zstd_fast.c`), without and with an extDict.

use crate::seq::{REPCODE1, SeqStore, offset_to_off_base};
use crate::window::{
    HASH_READ_SIZE, MatchState, Mem, SEARCH_STRENGTH, count, count_2segments, get, hash_ptr, prefetch,
    read32, set, template_mls,
};

/// `ZSTD_compressBlock_fast` / `ZSTD_compressBlock_fast_extDict`: stores the sequences of
/// `src_size` bytes at `istart` of the current segment; returns the size of the last
/// literals.
pub(crate) fn compress_block(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    istart: usize,
    src_size: usize,
) -> usize {
    match (template_mls(ms.params.min_match), ms.window.has_ext_dict()) {
        (5, false) => compress_no_dict::<5>(ms, seq, rep, mem.cur, istart, src_size),
        (6, false) => compress_no_dict::<6>(ms, seq, rep, mem.cur, istart, src_size),
        (7, false) => compress_no_dict::<7>(ms, seq, rep, mem.cur, istart, src_size),
        (_, false) => compress_no_dict::<4>(ms, seq, rep, mem.cur, istart, src_size),
        (5, true) => compress_ext_dict::<5>(ms, seq, rep, mem, istart, src_size),
        (6, true) => compress_ext_dict::<6>(ms, seq, rep, mem, istart, src_size),
        (7, true) => compress_ext_dict::<7>(ms, seq, rep, mem, istart, src_size),
        (_, true) => compress_ext_dict::<4>(ms, seq, rep, mem, istart, src_size),
    }
}

/// `ZSTD_compressBlock_fast_noDict_generic`.
fn compress_no_dict<const MLS: u32>(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    buf: &[u8],
    istart: usize,
    src_size: usize,
) -> usize {
    let params = ms.params;
    let hlog = params.hash_log;
    let step_size = (params.target_length + u32::from(params.target_length == 0) + 1) as usize;
    let base = ms.window.base;
    let idx_of = |pos: usize| (pos as i64 + base) as u32;
    let pos_of = |idx: u32| (i64::from(idx) - base) as usize;
    let end_index = idx_of(istart) + src_size as u32;
    let prefix_start_index = ms.window.lowest_prefix_index(end_index, params.window_log);
    let prefix_start = pos_of(prefix_start_index);
    let iend = istart + src_size;
    let ilimit = iend as isize - HASH_READ_SIZE as isize;
    let hash_table = &mut ms.hash_table;

    let mut anchor = istart;
    let mut ip0 = istart;
    let mut rep_offset1 = rep[0];
    let mut rep_offset2 = rep[1];
    let mut offset_saved1 = 0;
    let mut offset_saved2 = 0;
    let k_step_incr = 1usize << (SEARCH_STRENGTH - 1);

    ip0 += usize::from(ip0 == prefix_start);
    {
        let current = idx_of(ip0);
        let window_low = ms.window.lowest_prefix_index(current, params.window_log);
        let max_rep = current - window_low;
        if rep_offset2 > max_rep {
            offset_saved2 = rep_offset2;
            rep_offset2 = 0;
        }
        if rep_offset1 > max_rep {
            offset_saved1 = rep_offset1;
            rep_offset1 = 0;
        }
    }

    'start: loop {
        let mut step = step_size;
        let mut next_step = ip0 + k_step_incr;
        let mut ip1 = ip0 + 1;
        let mut ip2 = ip0 + step;
        let mut ip3 = ip2 + 1;
        if ip3 as isize >= ilimit {
            break 'start;
        }
        let mut hash0 = hash_ptr::<MLS>(buf, ip0, hlog);
        let mut hash1 = hash_ptr::<MLS>(buf, ip1, hlog);
        let mut idx = get(hash_table, hash0);
        let mut current0;
        // (ip0, match0, offcode, mLength) of the match found.
        let found: (usize, usize, u32, usize);
        loop {
            let rval = read32(buf, ip2 - rep_offset1 as usize);
            current0 = idx_of(ip0);
            set(hash_table, hash0, current0);
            if read32(buf, ip2) == rval && rep_offset1 > 0 {
                let mut ip = ip2;
                let mut matched = ip - rep_offset1 as usize;
                let back = usize::from(buf[ip - 1] == buf[matched - 1]);
                ip -= back;
                matched -= back;
                set(hash_table, hash1, idx_of(ip1));
                found = (ip, matched, REPCODE1, back + 4);
                break;
            }
            let mval = if idx >= prefix_start_index {
                read32(buf, pos_of(idx))
            } else {
                read32(buf, ip0) ^ 1
            };
            if read32(buf, ip0) == mval {
                set(hash_table, hash1, idx_of(ip1));
                found = offset_match(
                    buf,
                    &mut rep_offset1,
                    &mut rep_offset2,
                    ip0,
                    pos_of(idx),
                    anchor,
                    prefix_start,
                );
                break;
            }
            idx = get(hash_table, hash1);
            hash0 = hash1;
            hash1 = hash_ptr::<MLS>(buf, ip2, hlog);
            ip0 = ip1;
            ip1 = ip2;
            ip2 = ip3;
            current0 = idx_of(ip0);
            set(hash_table, hash0, current0);
            let mval = if idx >= prefix_start_index {
                read32(buf, pos_of(idx))
            } else {
                read32(buf, ip0) ^ 1
            };
            if read32(buf, ip0) == mval {
                if step <= 4 {
                    set(hash_table, hash1, idx_of(ip1));
                }
                found = offset_match(
                    buf,
                    &mut rep_offset1,
                    &mut rep_offset2,
                    ip0,
                    pos_of(idx),
                    anchor,
                    prefix_start,
                );
                break;
            }
            idx = get(hash_table, hash1);
            hash0 = hash1;
            hash1 = hash_ptr::<MLS>(buf, ip2, hlog);
            ip0 = ip1;
            ip1 = ip2;
            ip2 = ip0 + step;
            ip3 = ip1 + step;
            if ip2 >= next_step {
                step += 1;
                prefetch(buf, ip1 + 64);
                prefetch(buf, ip1 + 128);
                next_step += k_step_incr;
            }
            if ip3 as isize >= ilimit {
                break 'start;
            }
        }
        // `_match`: count the forward length and store the sequence.
        let (ip, matched, offcode, mut length) = found;
        length += count(buf, ip + length, matched + length, iend);
        seq.store(buf, anchor, ip - anchor, offcode, length);
        ip0 = ip + length;
        anchor = ip0;
        if ip0 as isize <= ilimit {
            let insert = pos_of(current0 + 2);
            set(hash_table, hash_ptr::<MLS>(buf, insert, hlog), current0 + 2);
            set(hash_table, hash_ptr::<MLS>(buf, ip0 - 2, hlog), idx_of(ip0 - 2));
            if rep_offset2 > 0 {
                while ip0 as isize <= ilimit && read32(buf, ip0) == read32(buf, ip0 - rep_offset2 as usize) {
                    let r_length = count(buf, ip0 + 4, ip0 + 4 - rep_offset2 as usize, iend) + 4;
                    std::mem::swap(&mut rep_offset1, &mut rep_offset2);
                    set(hash_table, hash_ptr::<MLS>(buf, ip0, hlog), idx_of(ip0));
                    ip0 += r_length;
                    seq.store(buf, anchor, 0, REPCODE1, r_length);
                    anchor = ip0;
                }
            }
        }
    }
    offset_saved2 = if offset_saved1 != 0 && rep_offset1 != 0 {
        offset_saved1
    } else {
        offset_saved2
    };
    rep[0] = if rep_offset1 != 0 {
        rep_offset1
    } else {
        offset_saved1
    };
    rep[1] = if rep_offset2 != 0 {
        rep_offset2
    } else {
        offset_saved2
    };
    iend - anchor
}

/// `_offset` of the no-dict fast match finder: the new offset and the backward extension.
#[inline(always)]
fn offset_match(
    buf: &[u8],
    rep_offset1: &mut u32,
    rep_offset2: &mut u32,
    mut ip0: usize,
    mut match0: usize,
    anchor: usize,
    prefix_start: usize,
) -> (usize, usize, u32, usize) {
    *rep_offset2 = *rep_offset1;
    *rep_offset1 = (ip0 - match0) as u32;
    let offcode = offset_to_off_base(*rep_offset1);
    let mut length = 4;
    while ip0 > anchor && match0 > prefix_start && buf[ip0 - 1] == buf[match0 - 1] {
        ip0 -= 1;
        match0 -= 1;
        length += 1;
    }
    (ip0, match0, offcode, length)
}

/// `ZSTD_compressBlock_fast_extDict_generic`.
fn compress_ext_dict<const MLS: u32>(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    istart: usize,
    src_size: usize,
) -> usize {
    let params = ms.params;
    let hlog = params.hash_log;
    let step_size = (params.target_length + u32::from(params.target_length == 0) + 1) as usize;
    let window = ms.window;
    let base = window.base;
    let dict_base = window.dict_base;
    let buf = mem.cur;
    let dict = mem.dict;
    let idx_of = |pos: usize| (pos as i64 + base) as u32;
    let pos_of = |idx: u32| (i64::from(idx) - base) as usize;
    let dpos_of = |idx: u32| (i64::from(idx) - dict_base) as usize;
    let end_index = idx_of(istart) + src_size as u32;
    let low_limit = window.lowest_match_index(end_index, params.window_log);
    let dict_start_index = low_limit;
    let dict_start = dpos_of(dict_start_index);
    let dict_limit = window.dict_limit;
    let prefix_start_index = dict_limit.max(low_limit);
    let prefix_start = pos_of(prefix_start_index);
    let dict_end = dpos_of(prefix_start_index);
    let iend = istart + src_size;
    let ilimit = iend as isize - 8;
    let mut offset_1 = rep[0];
    let mut offset_2 = rep[1];
    let mut offset_saved1 = 0;
    let mut offset_saved2 = 0;
    let k_step_incr = 1usize << (SEARCH_STRENGTH - 1);

    if prefix_start_index == dict_start_index {
        return compress_no_dict::<MLS>(ms, seq, rep, buf, istart, src_size);
    }
    let hash_table = &mut ms.hash_table;
    let mut anchor = istart;
    let mut ip0 = istart;
    {
        let current = idx_of(ip0);
        let max_rep = current - dict_start_index;
        if offset_2 >= max_rep {
            offset_saved2 = offset_2;
            offset_2 = 0;
        }
        if offset_1 >= max_rep {
            offset_saved1 = offset_1;
            offset_1 = 0;
        }
    }
    // Reads 4 bytes at `idx`, from the extDict below the prefix.
    let read_at = |idx: u32| -> u32 {
        if idx < prefix_start_index {
            read32(dict, dpos_of(idx))
        } else {
            read32(buf, pos_of(idx))
        }
    };

    'start: loop {
        let mut step = step_size;
        let mut next_step = ip0 + k_step_incr;
        let mut ip1 = ip0 + 1;
        let mut ip2 = ip0 + step;
        let mut ip3 = ip2 + 1;
        if ip3 as isize >= ilimit {
            break 'start;
        }
        let mut hash0 = hash_ptr::<MLS>(buf, ip0, hlog);
        let mut hash1 = hash_ptr::<MLS>(buf, ip1, hlog);
        let mut idx = get(hash_table, hash0);
        let mut current0;
        // (ip0, match index, offcode, mLength, match end, match in dict).
        let found: (usize, u32, u32, usize, usize, bool);
        loop {
            {
                let current2 = idx_of(ip2);
                let rep_index = current2.wrapping_sub(offset_1);
                let rval = if prefix_start_index.wrapping_sub(rep_index) >= 4 && offset_1 > 0 {
                    read_at(rep_index)
                } else {
                    read32(buf, ip2) ^ 1
                };
                current0 = idx_of(ip0);
                set(hash_table, hash0, current0);
                if read32(buf, ip2) == rval {
                    let in_dict = rep_index < prefix_start_index;
                    let match_end = if in_dict { dict_end } else { iend };
                    let match_buf = if in_dict { dict } else { buf };
                    let match_pos = if in_dict {
                        dpos_of(rep_index)
                    } else {
                        pos_of(rep_index)
                    };
                    let back = usize::from(buf[ip2 - 1] == match_buf[match_pos - 1]);
                    found = (
                        ip2 - back,
                        rep_index - back as u32,
                        REPCODE1,
                        back + 4,
                        match_end,
                        in_dict,
                    );
                    break;
                }
            }
            {
                let mval = if idx >= dict_start_index {
                    read_at(idx)
                } else {
                    read32(buf, ip0) ^ 1
                };
                if read32(buf, ip0) == mval {
                    found = ext_offset_match(
                        mem,
                        &mut offset_1,
                        &mut offset_2,
                        (ip0, current0, idx, anchor),
                        (prefix_start_index, prefix_start, dict_start, dict_end, iend),
                        (base, dict_base),
                    );
                    break;
                }
            }
            idx = get(hash_table, hash1);
            hash0 = hash1;
            hash1 = hash_ptr::<MLS>(buf, ip2, hlog);
            ip0 = ip1;
            ip1 = ip2;
            ip2 = ip3;
            current0 = idx_of(ip0);
            set(hash_table, hash0, current0);
            {
                let mval = if idx >= dict_start_index {
                    read_at(idx)
                } else {
                    read32(buf, ip0) ^ 1
                };
                if read32(buf, ip0) == mval {
                    found = ext_offset_match(
                        mem,
                        &mut offset_1,
                        &mut offset_2,
                        (ip0, current0, idx, anchor),
                        (prefix_start_index, prefix_start, dict_start, dict_end, iend),
                        (base, dict_base),
                    );
                    break;
                }
            }
            idx = get(hash_table, hash1);
            hash0 = hash1;
            hash1 = hash_ptr::<MLS>(buf, ip2, hlog);
            ip0 = ip1;
            ip1 = ip2;
            ip2 = ip0 + step;
            ip3 = ip1 + step;
            if ip2 >= next_step {
                step += 1;
                prefetch(buf, ip1 + 64);
                prefetch(buf, ip1 + 128);
                next_step += k_step_incr;
            }
            if ip3 as isize >= ilimit {
                break 'start;
            }
        }
        // `_match`.
        let (ip, match_index, offcode, mut length, match_end, in_dict) = found;
        let match_pos = if in_dict {
            dpos_of(match_index)
        } else {
            pos_of(match_index)
        };
        length += count_2segments(
            mem,
            ip + length,
            match_pos + length,
            in_dict,
            iend,
            match_end,
            prefix_start,
        );
        seq.store(buf, anchor, ip - anchor, offcode, length);
        ip0 = ip + length;
        anchor = ip0;
        if ip1 < ip0 {
            set(hash_table, hash1, idx_of(ip1));
        }
        if ip0 as isize <= ilimit {
            let insert = pos_of(current0 + 2);
            set(hash_table, hash_ptr::<MLS>(buf, insert, hlog), current0 + 2);
            set(hash_table, hash_ptr::<MLS>(buf, ip0 - 2, hlog), idx_of(ip0 - 2));
            while ip0 as isize <= ilimit {
                let rep_index2 = idx_of(ip0).wrapping_sub(offset_2);
                if (prefix_start_index - 1).wrapping_sub(rep_index2) >= 3
                    && offset_2 > 0
                    && read_at(rep_index2) == read32(buf, ip0)
                {
                    let in_dict = rep_index2 < prefix_start_index;
                    let rep_end2 = if in_dict { dict_end } else { iend };
                    let rep_pos = if in_dict {
                        dpos_of(rep_index2)
                    } else {
                        pos_of(rep_index2)
                    };
                    let rep_length2 =
                        count_2segments(mem, ip0 + 4, rep_pos + 4, in_dict, iend, rep_end2, prefix_start) + 4;
                    std::mem::swap(&mut offset_1, &mut offset_2);
                    seq.store(buf, anchor, 0, REPCODE1, rep_length2);
                    set(hash_table, hash_ptr::<MLS>(buf, ip0, hlog), idx_of(ip0));
                    ip0 += rep_length2;
                    anchor = ip0;
                    continue;
                }
                break;
            }
        }
    }
    offset_saved2 = if offset_saved1 != 0 && offset_1 != 0 {
        offset_saved1
    } else {
        offset_saved2
    };
    rep[0] = if offset_1 != 0 { offset_1 } else { offset_saved1 };
    rep[1] = if offset_2 != 0 { offset_2 } else { offset_saved2 };
    iend - anchor
}

/// `_offset` of the extDict fast match finder.
#[inline(always)]
fn ext_offset_match(
    mem: Mem<'_>,
    offset_1: &mut u32,
    offset_2: &mut u32,
    (mut ip0, current0, idx, anchor): (usize, u32, u32, usize),
    (prefix_start_index, prefix_start, dict_start, dict_end, iend): (u32, usize, usize, usize, usize),
    (base, dict_base): (i64, i64),
) -> (usize, u32, u32, usize, usize, bool) {
    let offset = current0 - idx;
    let in_dict = idx < prefix_start_index;
    let (match_buf, mut match_pos, low_match, match_end) = if in_dict {
        (
            mem.dict,
            (i64::from(idx) - dict_base) as usize,
            dict_start,
            dict_end,
        )
    } else {
        (mem.cur, (i64::from(idx) - base) as usize, prefix_start, iend)
    };
    *offset_2 = *offset_1;
    *offset_1 = offset;
    let offcode = offset_to_off_base(offset);
    let mut length = 4;
    let mut match_index = idx;
    while ip0 > anchor && match_pos > low_match && mem.cur[ip0 - 1] == match_buf[match_pos - 1] {
        ip0 -= 1;
        match_pos -= 1;
        match_index -= 1;
        length += 1;
    }
    (ip0, match_index, offcode, length, match_end, in_dict)
}
