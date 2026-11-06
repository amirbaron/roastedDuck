//! Rendering-only port of the frozen DuckDB utf8proc wrapper and grapheme rules.
//! Property data and algorithm are covered by `data/utf8proc-LICENSE`.
//! This follows `UTF8ToCodepoint`'s byte interpretation, not strict VARCHAR validation.

use crate::{Error, Result};

const DATA: &[u8] = include_bytes!("../../data/utf8proc_render.bin");

fn property(codepoint: i32) -> (usize, u8, u8) {
    if !(0..0x11_0000).contains(&codepoint) {
        return (1, 1, 0);
    }
    let cp = codepoint as u32;
    let mut low = 0;
    let mut high = DATA.len() / 6;
    while low < high {
        let mid = usize::midpoint(low, high);
        let start = u32::from_le_bytes([
            DATA[mid * 6],
            DATA[mid * 6 + 1],
            DATA[mid * 6 + 2],
            DATA[mid * 6 + 3],
        ]);
        if start <= cp {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let offset = (low - 1) * 6 + 4;
    let value = u16::from_le_bytes([DATA[offset], DATA[offset + 1]]);
    (
        usize::from(value & 3),
        ((value >> 2) & 31) as u8,
        (value >> 7) as u8,
    )
}

fn codepoint(bytes: &[u8], offset: usize) -> Result<(i32, usize)> {
    let byte = |n| i32::from(bytes.get(offset + n).copied().unwrap_or(0));
    let u0 = byte(0);
    if u0 <= 127 {
        return Ok((u0, 1));
    }
    let u1 = byte(1);
    if (192..=223).contains(&u0) {
        return Ok(((u0 - 192) * 64 + u1 - 128, 2));
    }
    if u0 == 0xed && u1 & 0xa0 == 0xa0 {
        return Err(Error::internal(
            "invalid code point detected in Utf8Proc::UTF8ToCodepoint (0xd800 to 0xdfff), likely due to invalid UTF-8",
        ));
    }
    let u2 = byte(2);
    if (224..=239).contains(&u0) {
        return Ok(((u0 - 224) * 4096 + (u1 - 128) * 64 + u2 - 128, 3));
    }
    let u3 = byte(3);
    if (240..=247).contains(&u0) {
        return Ok((
            (u0 - 240) * 262_144 + (u1 - 128) * 4096 + (u2 - 128) * 64 + u3 - 128,
            4,
        ));
    }
    Err(Error::internal(
        "invalid code point detected in Utf8Proc::UTF8ToCodepoint, likely due to invalid UTF-8",
    ))
}

fn simple_break(left: u8, right: u8) -> bool {
    if left == 0 {
        return true;
    }
    if left == 2 && right == 3 {
        return false;
    }
    if (2..=4).contains(&left) || (2..=4).contains(&right) {
        return true;
    }
    if left == 6 && matches!(right, 6 | 7 | 9 | 10) {
        return false;
    }
    if matches!(left, 7 | 9) && matches!(right, 7 | 8) {
        return false;
    }
    if matches!(left, 8 | 10) && right == 8 {
        return false;
    }
    if matches!(right, 5 | 12 | 14) || left == 13 {
        return false;
    }
    if left == 20 && right == 19 {
        return false;
    }
    !(left == 11 && right == 11)
}

fn grapheme_break(left: i32, right: i32, state: &mut (u8, u8)) -> bool {
    let (_, left_bc, left_icb) = property(left);
    let (_, right_bc, right_icb) = property(right);
    let (mut bc, mut icb) = if *state == (0, 0) {
        (left_bc, if left_icb == 2 { 2 } else { 0 })
    } else {
        *state
    };
    let permitted = simple_break(bc, right_bc) && !(icb == 1 && right_icb == 2);
    if right_icb == 2 || icb == 2 || icb == 3 {
        icb = right_icb;
    } else if icb == 1 {
        icb = if right_icb == 3 { 1 } else { right_icb };
    }
    bc = if bc == right_bc && right_bc == 11 {
        1
    } else if bc == 19 {
        match right_bc {
            5 => 19,
            14 => 20,
            _ => right_bc,
        }
    } else {
        right_bc
    };
    *state = (bc, icb);
    permitted
}

/// Next cluster boundary and the width of its first codepoint.
pub(crate) fn cluster(bytes: &[u8], start: usize) -> Result<(usize, usize)> {
    let (mut previous, mut size) = codepoint(bytes, start)?;
    let width = property(previous).0;
    let mut offset = start;
    let mut state = (0, 0);
    loop {
        offset += size;
        if offset >= bytes.len() {
            return Ok((bytes.len(), width));
        }
        let (next, next_size) = codepoint(bytes, offset)?;
        if grapheme_break(previous, next, &mut state) {
            return Ok((offset, width));
        }
        previous = next;
        size = next_size;
    }
}
