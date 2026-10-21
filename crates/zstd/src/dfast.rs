//! The `ZSTD_dfast` match finder (`zstd_double_fast.c`), without and with an extDict: a
//! table of 8-byte hashes (`hash_table`) and one of `min_match`-byte hashes
//! (`chain_table`).

use crate::seq::{REPCODE1, SeqStore, offset_to_off_base};
use crate::window::{
    HASH_READ_SIZE, MatchState, Mem, SEARCH_STRENGTH, count, count_2segments, get, hash_ptr, read32, read64,
    set, template_mls,
};

/// `ZSTD_compressBlock_doubleFast` / `ZSTD_compressBlock_doubleFast_extDict`.
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

/// `ZSTD_compressBlock_doubleFast_noDict_generic`.
fn compress_no_dict<const MLS: u32>(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    buf: &[u8],
    istart: usize,
    src_size: usize,
) -> usize {
    let params = ms.params;
    let h_bits_l = params.hash_log;
    let h_bits_s = params.chain_log;
    let base = ms.window.base;
    let idx_of = |pos: usize| (pos as i64 + base) as u32;
    let pos_of = |idx: u32| (i64::from(idx) - base) as usize;
    let end_index = idx_of(istart) + src_size as u32;
    let prefix_lowest_index = ms.window.lowest_prefix_index(end_index, params.window_log);
    let prefix_lowest = pos_of(prefix_lowest_index);
    let iend = istart + src_size;
    let ilimit = iend as isize - HASH_READ_SIZE as isize;
    let mut offset_1 = rep[0];
    let mut offset_2 = rep[1];
    let mut offset_saved1 = 0;
    let mut offset_saved2 = 0;
    let k_step_incr = 1usize << SEARCH_STRENGTH;
    let (hash_long, hash_small) = (&mut ms.hash_table, &mut ms.chain_table);

    let mut anchor = istart;
    let mut ip = istart;
    ip += usize::from(ip == prefix_lowest);
    {
        let current = idx_of(ip);
        let window_low = ms.window.lowest_prefix_index(current, params.window_log);
        let max_rep = current - window_low;
        if offset_2 > max_rep {
            offset_saved2 = offset_2;
            offset_2 = 0;
        }
        if offset_1 > max_rep {
            offset_saved1 = offset_1;
            offset_1 = 0;
        }
    }

    loop {
        let mut step = 1usize;
        let mut next_step = ip + k_step_incr;
        let mut ip1 = ip + step;
        if ip1 as isize > ilimit {
            break;
        }
        let mut hl0 = hash_ptr::<8>(buf, ip, h_bits_l);
        let mut idxl0 = get(hash_long, hl0);
        let mut curr;
        let mut hl1;
        // (ip, offset, mLength) of a new match, or `None` after a repeat code was stored
        // with its length.
        let found: Option<(usize, u32, usize)>;
        let length;
        'search: loop {
            let hs0 = hash_ptr::<MLS>(buf, ip, h_bits_s);
            let idxs0 = get(hash_small, hs0);
            curr = idx_of(ip);
            set(hash_long, hl0, curr);
            set(hash_small, hs0, curr);
            if offset_1 > 0 && read32(buf, ip + 1 - offset_1 as usize) == read32(buf, ip + 1) {
                length = count(buf, ip + 1 + 4, ip + 1 + 4 - offset_1 as usize, iend) + 4;
                ip += 1;
                seq.store(buf, anchor, ip - anchor, REPCODE1, length);
                found = None;
                hl1 = 0;
                break 'search;
            }
            hl1 = hash_ptr::<8>(buf, ip1, h_bits_l);
            if idxl0 > prefix_lowest_index {
                let mut matchl0 = pos_of(idxl0);
                if read64(buf, matchl0) == read64(buf, ip) {
                    let mut l = count(buf, ip + 8, matchl0 + 8, iend) + 8;
                    let offset = (ip - matchl0) as u32;
                    while ip > anchor && matchl0 > prefix_lowest && buf[ip - 1] == buf[matchl0 - 1] {
                        ip -= 1;
                        matchl0 -= 1;
                        l += 1;
                    }
                    found = Some((ip, offset, l));
                    length = l;
                    break 'search;
                }
            }
            let idxl1 = get(hash_long, hl1);
            if idxs0 > prefix_lowest_index && read32(buf, pos_of(idxs0)) == read32(buf, ip) {
                // `_search_next_long`.
                if idxl1 > prefix_lowest_index {
                    let mut matchl1 = pos_of(idxl1);
                    if read64(buf, matchl1) == read64(buf, ip1) {
                        ip = ip1;
                        let mut l = count(buf, ip + 8, matchl1 + 8, iend) + 8;
                        let offset = (ip - matchl1) as u32;
                        while ip > anchor && matchl1 > prefix_lowest && buf[ip - 1] == buf[matchl1 - 1] {
                            ip -= 1;
                            matchl1 -= 1;
                            l += 1;
                        }
                        found = Some((ip, offset, l));
                        length = l;
                        break 'search;
                    }
                }
                let mut matchs0 = pos_of(idxs0);
                let mut l = count(buf, ip + 4, matchs0 + 4, iend) + 4;
                let offset = (ip - matchs0) as u32;
                while ip > anchor && matchs0 > prefix_lowest && buf[ip - 1] == buf[matchs0 - 1] {
                    ip -= 1;
                    matchs0 -= 1;
                    l += 1;
                }
                found = Some((ip, offset, l));
                length = l;
                break 'search;
            }
            if ip1 >= next_step {
                step += 1;
                next_step += k_step_incr;
            }
            ip = ip1;
            ip1 += step;
            hl0 = hl1;
            idxl0 = idxl1;
            if ip1 as isize > ilimit {
                offset_saved2 = if offset_saved1 != 0 && offset_1 != 0 {
                    offset_saved1
                } else {
                    offset_saved2
                };
                rep[0] = if offset_1 != 0 { offset_1 } else { offset_saved1 };
                rep[1] = if offset_2 != 0 { offset_2 } else { offset_saved2 };
                return iend - anchor;
            }
        }
        if let Some((match_ip, offset, _)) = found {
            // `_match_found`.
            ip = match_ip;
            offset_2 = offset_1;
            offset_1 = offset;
            if step < 4 {
                set(hash_long, hl1, idx_of(ip1));
            }
            seq.store(buf, anchor, ip - anchor, offset_to_off_base(offset), length);
        }
        // `_match_stored`.
        ip += length;
        anchor = ip;
        if ip as isize <= ilimit {
            let index_to_insert = curr + 2;
            set(
                hash_long,
                hash_ptr::<8>(buf, pos_of(index_to_insert), h_bits_l),
                index_to_insert,
            );
            set(hash_long, hash_ptr::<8>(buf, ip - 2, h_bits_l), idx_of(ip - 2));
            set(
                hash_small,
                hash_ptr::<MLS>(buf, pos_of(index_to_insert), h_bits_s),
                index_to_insert,
            );
            set(hash_small, hash_ptr::<MLS>(buf, ip - 1, h_bits_s), idx_of(ip - 1));
            while ip as isize <= ilimit
                && offset_2 > 0
                && read32(buf, ip) == read32(buf, ip - offset_2 as usize)
            {
                let r_length = count(buf, ip + 4, ip + 4 - offset_2 as usize, iend) + 4;
                std::mem::swap(&mut offset_1, &mut offset_2);
                set(hash_small, hash_ptr::<MLS>(buf, ip, h_bits_s), idx_of(ip));
                set(hash_long, hash_ptr::<8>(buf, ip, h_bits_l), idx_of(ip));
                seq.store(buf, anchor, 0, REPCODE1, r_length);
                ip += r_length;
                anchor = ip;
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

/// `ZSTD_compressBlock_doubleFast_extDict_generic`.
fn compress_ext_dict<const MLS: u32>(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    istart: usize,
    src_size: usize,
) -> usize {
    let params = ms.params;
    let h_bits_l = params.hash_log;
    let h_bits_s = params.chain_log;
    let window = ms.window;
    let base = window.base;
    let dict_base = window.dict_base;
    let buf = mem.cur;
    let dict = mem.dict;
    let idx_of = |pos: usize| (pos as i64 + base) as u32;
    let pos_of = |idx: u32| (i64::from(idx) - base) as usize;
    let dpos_of = |idx: u32| (i64::from(idx) - dict_base) as usize;
    let iend = istart + src_size;
    let ilimit = iend as isize - 8;
    let end_index = idx_of(istart) + src_size as u32;
    let low_limit = window.lowest_match_index(end_index, params.window_log);
    let dict_start_index = low_limit;
    let dict_limit = window.dict_limit;
    let prefix_start_index = dict_limit.max(low_limit);
    let prefix_start = pos_of(prefix_start_index);
    let dict_start = dpos_of(dict_start_index);
    let dict_end = dpos_of(prefix_start_index);
    let mut offset_1 = rep[0];
    let mut offset_2 = rep[1];

    if prefix_start_index == dict_start_index {
        return compress_no_dict::<MLS>(ms, seq, rep, buf, istart, src_size);
    }
    let (hash_long, hash_small) = (&mut ms.hash_table, &mut ms.chain_table);
    // Where index `idx` is: its buffer and position.
    let locate = |idx: u32| -> (&[u8], usize, bool) {
        if idx < prefix_start_index {
            (dict, dpos_of(idx), true)
        } else {
            (buf, pos_of(idx), false)
        }
    };
    let mut ip = istart;
    let mut anchor = istart;
    while (ip as isize) < ilimit {
        let h_small = hash_ptr::<MLS>(buf, ip, h_bits_s);
        let match_index = get(hash_small, h_small);
        let h_long = hash_ptr::<8>(buf, ip, h_bits_l);
        let match_long_index = get(hash_long, h_long);
        let curr = idx_of(ip);
        let rep_index = (curr + 1).wrapping_sub(offset_1);
        set(hash_small, h_small, curr);
        set(hash_long, h_long, curr);
        let length;
        if (prefix_start_index - 1).wrapping_sub(rep_index) >= 3
            && offset_1 <= curr + 1 - dict_start_index
            && {
                let (rep_buf, rep_pos, _) = locate(rep_index);
                read32(rep_buf, rep_pos) == read32(buf, ip + 1)
            }
        {
            let (_, rep_pos, in_dict) = locate(rep_index);
            let rep_end = if in_dict { dict_end } else { iend };
            length = count_2segments(mem, ip + 1 + 4, rep_pos + 4, in_dict, iend, rep_end, prefix_start) + 4;
            ip += 1;
            seq.store(buf, anchor, ip - anchor, REPCODE1, length);
        } else if match_long_index > dict_start_index && {
            let (long_buf, long_pos, _) = locate(match_long_index);
            read64(long_buf, long_pos) == read64(buf, ip)
        } {
            let (long_buf, mut long_pos, in_dict) = locate(match_long_index);
            let (match_end, low_match) = if in_dict {
                (dict_end, dict_start)
            } else {
                (iend, prefix_start)
            };
            let mut l =
                count_2segments(mem, ip + 8, long_pos + 8, in_dict, iend, match_end, prefix_start) + 8;
            let offset = curr - match_long_index;
            while ip > anchor && long_pos > low_match && buf[ip - 1] == long_buf[long_pos - 1] {
                ip -= 1;
                long_pos -= 1;
                l += 1;
            }
            offset_2 = offset_1;
            offset_1 = offset;
            length = l;
            seq.store(buf, anchor, ip - anchor, offset_to_off_base(offset), length);
        } else if match_index > dict_start_index && {
            let (short_buf, short_pos, _) = locate(match_index);
            read32(short_buf, short_pos) == read32(buf, ip)
        } {
            let h3 = hash_ptr::<8>(buf, ip + 1, h_bits_l);
            let match_index3 = get(hash_long, h3);
            set(hash_long, h3, curr + 1);
            let offset;
            if match_index3 > dict_start_index && {
                let (b3, p3, _) = locate(match_index3);
                read64(b3, p3) == read64(buf, ip + 1)
            } {
                let (b3, mut p3, in_dict) = locate(match_index3);
                let (match_end, low_match) = if in_dict {
                    (dict_end, dict_start)
                } else {
                    (iend, prefix_start)
                };
                let mut l = count_2segments(mem, ip + 9, p3 + 8, in_dict, iend, match_end, prefix_start) + 8;
                ip += 1;
                offset = curr + 1 - match_index3;
                while ip > anchor && p3 > low_match && buf[ip - 1] == b3[p3 - 1] {
                    ip -= 1;
                    p3 -= 1;
                    l += 1;
                }
                length = l;
            } else {
                let (short_buf, mut short_pos, in_dict) = locate(match_index);
                let (match_end, low_match) = if in_dict {
                    (dict_end, dict_start)
                } else {
                    (iend, prefix_start)
                };
                let mut l =
                    count_2segments(mem, ip + 4, short_pos + 4, in_dict, iend, match_end, prefix_start) + 4;
                offset = curr - match_index;
                while ip > anchor && short_pos > low_match && buf[ip - 1] == short_buf[short_pos - 1] {
                    ip -= 1;
                    short_pos -= 1;
                    l += 1;
                }
                length = l;
            }
            offset_2 = offset_1;
            offset_1 = offset;
            seq.store(buf, anchor, ip - anchor, offset_to_off_base(offset), length);
        } else {
            ip += ((ip - anchor) >> SEARCH_STRENGTH) + 1;
            continue;
        }
        ip += length;
        anchor = ip;
        if ip as isize <= ilimit {
            let index_to_insert = curr + 2;
            set(
                hash_long,
                hash_ptr::<8>(buf, pos_of(index_to_insert), h_bits_l),
                index_to_insert,
            );
            set(hash_long, hash_ptr::<8>(buf, ip - 2, h_bits_l), idx_of(ip - 2));
            set(
                hash_small,
                hash_ptr::<MLS>(buf, pos_of(index_to_insert), h_bits_s),
                index_to_insert,
            );
            set(hash_small, hash_ptr::<MLS>(buf, ip - 1, h_bits_s), idx_of(ip - 1));
            while ip as isize <= ilimit {
                let current2 = idx_of(ip);
                let rep_index2 = current2.wrapping_sub(offset_2);
                if (prefix_start_index - 1).wrapping_sub(rep_index2) >= 3
                    && offset_2 <= current2 - dict_start_index
                    && {
                        let (rep_buf, rep_pos, _) = locate(rep_index2);
                        read32(rep_buf, rep_pos) == read32(buf, ip)
                    }
                {
                    let (_, rep_pos, in_dict) = locate(rep_index2);
                    let rep_end2 = if in_dict { dict_end } else { iend };
                    let rep_length2 =
                        count_2segments(mem, ip + 4, rep_pos + 4, in_dict, iend, rep_end2, prefix_start) + 4;
                    std::mem::swap(&mut offset_1, &mut offset_2);
                    seq.store(buf, anchor, 0, REPCODE1, rep_length2);
                    set(hash_small, hash_ptr::<MLS>(buf, ip, h_bits_s), current2);
                    set(hash_long, hash_ptr::<8>(buf, ip, h_bits_l), current2);
                    ip += rep_length2;
                    anchor = ip;
                    continue;
                }
                break;
            }
        }
    }
    rep[0] = offset_1;
    rep[1] = offset_2;
    iend - anchor
}
