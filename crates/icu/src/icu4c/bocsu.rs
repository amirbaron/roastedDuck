//! Port of ICU's `bocsu.h` / `bocsu.cpp` (`u_writeIdenticalLevelRun`): the identical level
//! of a sort key, the code points as byte-encoded differences (Binary Ordered Compression
//! Scheme for Unicode) that keep lexical order.

use super::normalizer2impl::{is_lead, is_trail, supplementary};

const SLOPE_MIN: i32 = 3;
const SLOPE_MAX: i32 = 0xff;
const SLOPE_MIDDLE: i32 = 0x81;
const SLOPE_TAIL_COUNT: i32 = SLOPE_MAX - SLOPE_MIN + 1;
const SLOPE_SINGLE: i32 = 80;
const SLOPE_LEAD_2: i32 = 42;
const SLOPE_LEAD_3: i32 = 3;
const SLOPE_REACH_POS_1: i32 = SLOPE_SINGLE;
const SLOPE_REACH_NEG_1: i32 = -SLOPE_SINGLE;
const SLOPE_REACH_POS_2: i32 = SLOPE_LEAD_2 * SLOPE_TAIL_COUNT + (SLOPE_LEAD_2 - 1);
const SLOPE_REACH_NEG_2: i32 = -SLOPE_REACH_POS_2 - 1;
const SLOPE_REACH_POS_3: i32 = SLOPE_LEAD_3 * SLOPE_TAIL_COUNT * SLOPE_TAIL_COUNT
    + (SLOPE_LEAD_3 - 1) * SLOPE_TAIL_COUNT
    + (SLOPE_TAIL_COUNT - 1);
const SLOPE_REACH_NEG_3: i32 = -SLOPE_REACH_POS_3 - 1;
const SLOPE_START_POS_2: i32 = SLOPE_MIDDLE + SLOPE_SINGLE + 1;
const SLOPE_START_POS_3: i32 = SLOPE_START_POS_2 + SLOPE_LEAD_2;
const SLOPE_START_NEG_2: i32 = SLOPE_MIDDLE + SLOPE_REACH_NEG_1;
const SLOPE_START_NEG_3: i32 = SLOPE_START_NEG_2 - SLOPE_LEAD_2;

/// `NEGDIVMOD(n, d, m)`: division rounding towards minus infinity; the modulo.
fn neg_div_mod(n: &mut i32, d: i32) -> i32 {
    let mut m = *n % d;
    *n /= d;
    if m < 0 {
        *n -= 1;
        m += d;
    }
    m
}

/// `u_writeDiff`: one difference -0x10ffff..+0x10ffff in 1..4 bytes, preserving order.
fn write_diff(mut diff: i32, sink: &mut Vec<u8>) {
    if diff >= SLOPE_REACH_NEG_1 {
        if diff <= SLOPE_REACH_POS_1 {
            sink.push((SLOPE_MIDDLE + diff) as u8);
        } else if diff <= SLOPE_REACH_POS_2 {
            sink.push((SLOPE_START_POS_2 + diff / SLOPE_TAIL_COUNT) as u8);
            sink.push((SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8);
        } else if diff <= SLOPE_REACH_POS_3 {
            let p2 = (SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8;
            diff /= SLOPE_TAIL_COUNT;
            let p1 = (SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8;
            sink.extend_from_slice(&[(SLOPE_START_POS_3 + diff / SLOPE_TAIL_COUNT) as u8, p1, p2]);
        } else {
            let p3 = (SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8;
            diff /= SLOPE_TAIL_COUNT;
            let p2 = (SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8;
            diff /= SLOPE_TAIL_COUNT;
            let p1 = (SLOPE_MIN + diff % SLOPE_TAIL_COUNT) as u8;
            sink.extend_from_slice(&[SLOPE_MAX as u8, p1, p2, p3]);
        }
    } else if diff >= SLOPE_REACH_NEG_2 {
        let m = neg_div_mod(&mut diff, SLOPE_TAIL_COUNT);
        sink.extend_from_slice(&[(SLOPE_START_NEG_2 + diff) as u8, (SLOPE_MIN + m) as u8]);
    } else if diff >= SLOPE_REACH_NEG_3 {
        let p2 = (SLOPE_MIN + neg_div_mod(&mut diff, SLOPE_TAIL_COUNT)) as u8;
        let p1 = (SLOPE_MIN + neg_div_mod(&mut diff, SLOPE_TAIL_COUNT)) as u8;
        sink.extend_from_slice(&[(SLOPE_START_NEG_3 + diff) as u8, p1, p2]);
    } else {
        let p3 = (SLOPE_MIN + neg_div_mod(&mut diff, SLOPE_TAIL_COUNT)) as u8;
        let p2 = (SLOPE_MIN + neg_div_mod(&mut diff, SLOPE_TAIL_COUNT)) as u8;
        let p1 = (SLOPE_MIN + neg_div_mod(&mut diff, SLOPE_TAIL_COUNT)) as u8;
        sink.extend_from_slice(&[SLOPE_MIN as u8, p1, p2, p3]);
    }
}

/// `u_writeIdenticalLevelRun(prev, s, length, sink)`: the code points of the UTF-16 text
/// as differences from the previous one (moved into the middle of its 128-block, or of
/// the Unihan block), U+FFFE as the merge separator 02. Returns the last `prev`.
pub fn write_identical_level_run(mut prev: i32, s: &[u16], sink: &mut Vec<u8>) -> i32 {
    let mut i = 0;
    while i < s.len() {
        if (0x4e00..0xa000).contains(&prev) {
            // Unihan U+4e00..U+9fa5: double-bytes down from the upper end
            prev = 0x9fff - SLOPE_REACH_POS_2;
        } else {
            prev = (prev & !0x7f) - SLOPE_REACH_NEG_1;
        }
        let unit = s[i];
        i += 1;
        let mut c = i32::from(unit);
        if is_lead(unit) && i < s.len() && is_trail(s[i]) {
            c = supplementary(unit, s[i]);
            i += 1;
        }
        if c == 0xfffe {
            sink.push(2); // merge separator
            prev = 0;
        } else {
            write_diff(c - prev, sink);
            prev = c;
        }
    }
    prev
}
