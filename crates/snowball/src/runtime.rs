//! Byte-oriented Snowball runtime, ported from DuckDB 1.5.6's bundled runtime.
//! The generated Rust backend uses boolean grouping calls; the underlying UTF-8
//! traversal and byte comparisons retain the C runtime's semantics.

type AmongCallback<T> = dyn Fn(&mut SnowballEnv, &mut T) -> bool + Sync;

pub(crate) struct Among<T: 'static>(
    pub &'static str,
    pub i32,
    pub i32,
    pub Option<&'static AmongCallback<T>>,
);

impl<T> std::fmt::Debug for Among<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Among")
            .field(&self.0)
            .field(&self.1)
            .field(&self.2)
            .field(&self.3.is_some())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct SnowballEnv {
    pub current: Vec<u8>,
    pub cursor: i32,
    pub limit: i32,
    pub limit_backward: i32,
    pub bra: i32,
    pub ket: i32,
}

impl SnowballEnv {
    pub fn new(value: &[u8]) -> Self {
        Self {
            current: value.to_vec(),
            cursor: 0,
            limit: value.len() as i32,
            limit_backward: 0,
            bra: 0,
            ket: value.len() as i32,
        }
    }

    pub fn len_utf8(&self) -> i32 {
        self.current
            .iter()
            .filter(|&&b| !(0x80..0xc0).contains(&b))
            .count() as i32
    }

    pub fn eq_s(&mut self, value: impl AsRef<[u8]>) -> bool {
        let value = value.as_ref();
        let end = self.cursor as usize + value.len();
        if end > self.limit as usize || self.current.get(self.cursor as usize..end) != Some(value) {
            return false;
        }
        self.cursor = end as i32;
        true
    }

    pub fn eq_s_b(&mut self, value: impl AsRef<[u8]>) -> bool {
        let value = value.as_ref();
        if self.cursor - self.limit_backward < value.len() as i32 {
            return false;
        }
        let start = self.cursor as usize - value.len();
        if &self.current[start..self.cursor as usize] != value {
            return false;
        }
        self.cursor = start as i32;
        true
    }

    pub fn byte_index_for_hop(&self, delta: i32) -> i32 {
        let mut c = self.cursor;
        if delta >= 0 {
            for _ in 0..delta {
                if c >= self.limit {
                    return -1;
                }
                let b = self.current[c as usize];
                c += 1;
                if b >= 0xc0 {
                    while c < self.limit && (0x80..0xc0).contains(&self.current[c as usize]) {
                        c += 1;
                    }
                }
            }
        } else {
            for _ in delta..0 {
                if c <= self.limit_backward {
                    return -1;
                }
                c -= 1;
                if self.current[c as usize] >= 0x80 {
                    while c > self.limit_backward {
                        let b = self.current[c as usize];
                        if b >= 0xc0 {
                            break;
                        }
                        c -= 1;
                    }
                }
            }
        }
        c
    }

    pub fn next_char(&mut self) {
        self.cursor = self.byte_index_for_hop(1);
    }
    pub fn previous_char(&mut self) {
        self.cursor = self.byte_index_for_hop(-1);
    }

    fn decode_forward(&self) -> Option<(u32, i32)> {
        let mut c = self.cursor as usize;
        let end = self.limit as usize;
        if c >= end {
            return None;
        }
        let b0 = u32::from(self.current[c]);
        c += 1;
        if b0 < 0xc0 || c == end {
            return Some((b0, 1));
        }
        let b1 = u32::from(self.current[c] & 0x3f);
        c += 1;
        if b0 < 0xe0 || c == end {
            return Some((((b0 & 0x1f) << 6) | b1, 2));
        }
        let b2 = u32::from(self.current[c] & 0x3f);
        c += 1;
        if b0 < 0xf0 || c == end {
            return Some((((b0 & 0xf) << 12) | (b1 << 6) | b2, 3));
        }
        Some((
            ((b0 & 0xe) << 18) | (b1 << 12) | (b2 << 6) | u32::from(self.current[c] & 0x3f),
            4,
        ))
    }

    fn decode_backward(&self) -> Option<(u32, i32)> {
        let mut c = self.cursor;
        if c <= self.limit_backward {
            return None;
        }
        c -= 1;
        let mut b = u32::from(self.current[c as usize]);
        if b < 0x80 || c == self.limit_backward {
            return Some((b, 1));
        }
        let mut a = b & 0x3f;
        c -= 1;
        b = u32::from(self.current[c as usize]);
        if b >= 0xc0 || c == self.limit_backward {
            return Some((((b & 0x1f) << 6) | a, 2));
        }
        a |= (b & 0x3f) << 6;
        c -= 1;
        b = u32::from(self.current[c as usize]);
        if b >= 0xe0 || c == self.limit_backward {
            return Some((((b & 0xf) << 12) | a, 3));
        }
        c -= 1;
        Some((
            (u32::from(self.current[c as usize] & 0xe) << 18) | ((b & 0x3f) << 12) | a,
            4,
        ))
    }

    fn grouping(&mut self, chars: &[u8], min: u32, max: u32, backward: bool, inside: bool) -> bool {
        let Some((ch, width)) = (if backward {
            self.decode_backward()
        } else {
            self.decode_forward()
        }) else {
            return false;
        };
        let member =
            ch >= min && ch <= max && (chars[((ch - min) >> 3) as usize] & (1 << ((ch - min) & 7))) != 0;
        if member != inside {
            return false;
        }
        self.cursor += if backward { -width } else { width };
        true
    }

    pub fn in_grouping(&mut self, chars: &[u8], min: u32, max: u32) -> bool {
        self.grouping(chars, min, max, false, true)
    }
    pub fn out_grouping(&mut self, chars: &[u8], min: u32, max: u32) -> bool {
        self.grouping(chars, min, max, false, false)
    }
    pub fn in_grouping_b(&mut self, chars: &[u8], min: u32, max: u32) -> bool {
        self.grouping(chars, min, max, true, true)
    }
    pub fn out_grouping_b(&mut self, chars: &[u8], min: u32, max: u32) -> bool {
        self.grouping(chars, min, max, true, false)
    }

    fn replace(&mut self, bra: i32, ket: i32, value: &[u8]) -> i32 {
        let adjustment = value.len() as i32 - (ket - bra);
        self.current
            .splice(bra as usize..ket as usize, value.iter().copied());
        self.limit += adjustment;
        if self.cursor >= ket {
            self.cursor += adjustment;
        } else if self.cursor > bra {
            self.cursor = bra;
        }
        adjustment
    }

    pub fn slice_from(&mut self, value: impl AsRef<[u8]>) -> bool {
        if self.bra < 0
            || self.bra > self.ket
            || self.ket > self.limit
            || self.limit > self.current.len() as i32
        {
            return false;
        }
        self.replace(self.bra, self.ket, value.as_ref());
        true
    }
    pub fn slice_del(&mut self) -> bool {
        self.slice_from([])
    }
    pub fn slice_to(&self) -> Vec<u8> {
        self.current[self.bra as usize..self.ket as usize].to_vec()
    }
    pub fn insert(&mut self, bra: i32, ket: i32, value: impl AsRef<[u8]>) {
        let adjustment = self.replace(bra, ket, value.as_ref());
        if bra <= self.bra {
            self.bra += adjustment;
        }
        if bra <= self.ket {
            self.ket += adjustment;
        }
    }
    pub fn find_among<T>(&mut self, amongs: &[Among<T>], context: &mut T) -> i32 {
        use std::cmp::min;
        let mut lower: i32 = 0;
        let mut upper: i32 = amongs.len() as i32;

        let cursor = self.cursor;
        let limit = self.limit;

        let mut common_i = 0i32;
        let mut common_j = 0i32;

        let mut first_key_inspected = false;
        loop {
            let middle = lower + ((upper - lower) >> 1);
            let mut diff: i32 = 0;
            let mut common = min(common_i, common_j);
            let candidate = &amongs[middle as usize];
            let first_unmatched = common;
            for lvar in first_unmatched..candidate.0.len() as i32 {
                if cursor + common == limit {
                    diff = -1;
                    break;
                }
                diff = self.current[(cursor + common) as usize] as i32
                    - candidate.0.as_bytes()[lvar as usize] as i32;
                if diff != 0 {
                    break;
                }
                common += 1;
            }
            if diff < 0 {
                upper = middle;
                common_j = common;
            } else {
                lower = middle;
                common_i = common;
            }
            if upper - lower <= 1 {
                if lower > 0 {
                    break;
                }
                if upper == lower {
                    break;
                }
                if first_key_inspected {
                    break;
                }
                first_key_inspected = true;
            }
        }

        loop {
            let candidate = &amongs[lower as usize];
            if common_i >= candidate.0.len() as i32 {
                self.cursor = cursor + candidate.0.len() as i32;
                if let Some(ref method) = candidate.3 {
                    let res = method(self, context);
                    self.cursor = cursor + candidate.0.len() as i32;
                    if res {
                        return candidate.2;
                    }
                } else {
                    return candidate.2;
                }
            }
            lower = candidate.1;
            if lower < 0 {
                return 0;
            }
        }
    }

    pub fn find_among_b<T>(&mut self, amongs: &[Among<T>], context: &mut T) -> i32 {
        let mut lower: i32 = 0;
        let mut upper: i32 = amongs.len() as i32;

        let cursor = self.cursor;
        let lb = self.limit_backward;

        let mut common_i = 0i32;
        let mut common_j = 0i32;

        let mut first_key_inspected = false;

        loop {
            let middle = lower + ((upper - lower) >> 1);
            let mut diff: i32 = 0;
            let mut common = if common_i < common_j { common_i } else { common_j };
            let candidate = &amongs[middle as usize];
            for lvar in (0..candidate.0.len() - common as usize).rev() {
                if cursor - common == lb {
                    diff = -1;
                    break;
                }
                diff =
                    self.current[(cursor - common - 1) as usize] as i32 - candidate.0.as_bytes()[lvar] as i32;
                if diff != 0 {
                    break;
                }
                // Count up commons. But not one character but the byte width of that char
                common += 1;
            }
            if diff < 0 {
                upper = middle;
                common_j = common;
            } else {
                lower = middle;
                common_i = common;
            }
            if upper - lower <= 1 {
                if lower > 0 {
                    break;
                }
                if upper == lower {
                    break;
                }
                if first_key_inspected {
                    break;
                }
                first_key_inspected = true;
            }
        }
        loop {
            let candidate = &amongs[lower as usize];
            if common_i >= candidate.0.len() as i32 {
                self.cursor = cursor - candidate.0.len() as i32;
                if let Some(ref method) = candidate.3 {
                    let res = method(self, context);
                    self.cursor = cursor - candidate.0.len() as i32;
                    if res {
                        return candidate.2;
                    }
                } else {
                    return candidate.2;
                }
            }
            lower = candidate.1;
            if lower < 0 {
                return 0;
            }
        }
    }
}
