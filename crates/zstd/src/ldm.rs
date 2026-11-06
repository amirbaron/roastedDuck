//! Automatic long-distance matches and optimal-parser candidates (`zstd_ldm.c`).

use crate::decompress::xxhash::Xxh64;
use crate::ldm_gear::GEAR;
use crate::opt::{Match, OPT_NUM};
use crate::params::BLOCK_SIZE_MAX;
use crate::window::{Mem, Window, count, count_2segments};

const MIN_MATCH: usize = 64;
const BATCH: usize = 64;
const BUCKET_LOG: u32 = 3;

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    offset: u32,
    checksum: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RawSeq {
    pub offset: u32,
    pub literals: u32,
    pub length: u32,
}

#[derive(Debug)]
pub(crate) struct LdmState {
    pub window: Window,
    table: Vec<Entry>,
    heads: Vec<u8>,
    window_log: u32,
    hash_log: u32,
    rate_log: u32,
}

impl LdmState {
    pub fn new(window_log: u32) -> Self {
        let mut result = Self {
            window: Window::new(),
            table: Vec::new(),
            heads: Vec::new(),
            window_log,
            hash_log: 0,
            rate_log: 0,
        };
        result.reset(window_log);
        result
    }

    pub fn reset(&mut self, window_log: u32) {
        self.window = Window::new();
        self.window_log = window_log;
        self.hash_log = window_log.saturating_sub(7).max(6);
        self.rate_log = window_log.saturating_sub(self.hash_log);
        self.table.resize(1usize << self.hash_log, Entry::default());
        self.table.fill(Entry::default());
        self.heads.resize(1usize << (self.hash_log - BUCKET_LOG), 0);
        self.heads.fill(0);
    }

    fn insert(&mut self, hash: usize, entry: Entry) {
        self.table[(hash << BUCKET_LOG) + usize::from(self.heads[hash])] = entry;
        self.heads[hash] = (self.heads[hash] + 1) & ((1 << BUCKET_LOG) - 1);
    }

    fn backwards(&self, mem: Mem<'_>, mut ip: usize, anchor: usize, matched: u32) -> usize {
        let w = self.window;
        let in_dict = matched < w.dict_limit;
        let base = if in_dict { w.dict_base } else { w.base };
        let buffer = if in_dict { mem.dict } else { mem.cur };
        let mut pos = (i64::from(matched) - base) as usize;
        let low = (i64::from(if in_dict { w.low_limit } else { w.dict_limit }) - base) as usize;
        let original = ip;
        while ip > anchor && pos > low && mem.cur[ip - 1] == buffer[pos - 1] {
            ip -= 1;
            pos -= 1;
        }
        if !in_dict && w.has_ext_dict() && pos == low {
            pos = (i64::from(w.dict_limit) - w.dict_base) as usize;
            let low = (i64::from(w.low_limit) - w.dict_base) as usize;
            while ip > anchor && pos > low && mem.cur[ip - 1] == mem.dict[pos - 1] {
                ip -= 1;
                pos -= 1;
            }
        }
        original - ip
    }

    /// The frame encoder invokes this once per source block, at most128KiB.
    pub fn generate(&mut self, mem: Mem<'_>, start: usize, size: usize, sequences: &mut Vec<RawSeq>) {
        debug_assert!(size <= BLOCK_SIZE_MAX);
        sequences.clear();
        let end = start + size;
        if self.window.need_overflow_correction(end) {
            let correction = self.window.correct_overflow(0, 1 << self.window_log, start);
            for entry in &mut self.table {
                entry.offset = entry.offset.saturating_sub(correction);
            }
        }
        let end_index = (end as i64 + self.window.base) as u32;
        self.window.enforce_max_dist(end_index, 1 << self.window_log);
        if size < MIN_MATCH {
            return;
        }
        let window = self.window;
        let low = if window.has_ext_dict() {
            window.low_limit
        } else {
            window.dict_limit
        };
        let prefix = (i64::from(window.dict_limit) - window.base) as usize;
        let dict_end = (i64::from(window.dict_limit) - window.dict_base) as usize;
        let hash_bits = self.hash_log - BUCKET_LOG;
        let stop_mask = if self.rate_log > 0 && self.rate_log <= 64 {
            ((1u64 << self.rate_log) - 1) << (64 - self.rate_log)
        } else {
            (1u64 << self.rate_log) - 1
        };
        let mut rolling = u64::from(u32::MAX);
        // The frozen gear_reset computes a local hash without writing rolling
        // back. Preserve that observable state; the native frame oracle verifies it.
        let mut ip = start + MIN_MATCH;
        let mut anchor = start;
        let limit = end - 8;
        while ip < limit {
            let mut splits = [0usize; BATCH];
            let mut count_splits = 0;
            let mut hashed = 0;
            while hashed < limit - ip && count_splits < BATCH {
                rolling = (rolling << 1).wrapping_add(GEAR[usize::from(mem.cur[ip + hashed])]);
                hashed += 1;
                if rolling & stop_mask == 0 {
                    splits[count_splits] = hashed;
                    count_splits += 1;
                }
            }
            let mut candidates = [(0usize, 0usize, 0u32); BATCH];
            for i in 0..count_splits {
                let split = ip + splits[i] - MIN_MATCH;
                let mut hash = Xxh64::new();
                hash.update(&mem.cur[split..split + MIN_MATCH]);
                let hash = hash.digest();
                candidates[i] = (
                    split,
                    (hash as u32 & ((1 << hash_bits) - 1)) as usize,
                    (hash >> 32) as u32,
                );
            }
            for &(split, hash, checksum) in &candidates[..count_splits] {
                let current = (split as i64 + window.base) as u32;
                let new = Entry {
                    offset: current,
                    checksum,
                };
                if split < anchor {
                    self.insert(hash, new);
                    continue;
                }
                let mut best = 0;
                let mut best_forward = 0;
                let mut best_backward = 0;
                let mut best_offset = None;
                for slot in (hash << BUCKET_LOG)..((hash + 1) << BUCKET_LOG) {
                    let entry = self.table[slot];
                    if entry.checksum != checksum || entry.offset <= low {
                        continue;
                    }
                    let in_dict = entry.offset < window.dict_limit;
                    let base = if in_dict { window.dict_base } else { window.base };
                    let matched = (i64::from(entry.offset) - base) as usize;
                    let forward = if window.has_ext_dict() {
                        count_2segments(
                            mem,
                            split,
                            matched,
                            in_dict,
                            end,
                            if in_dict { dict_end } else { end },
                            prefix,
                        )
                    } else {
                        count(mem.cur, split, matched, end)
                    };
                    if forward < MIN_MATCH {
                        continue;
                    }
                    let backwards = self.backwards(mem, split, anchor, entry.offset);
                    if forward + backwards > best {
                        best = forward + backwards;
                        best_forward = forward;
                        best_backward = backwards;
                        best_offset = Some(entry.offset);
                    }
                }
                let Some(matched) = best_offset else {
                    self.insert(hash, new);
                    continue;
                };
                sequences.push(RawSeq {
                    offset: current - matched,
                    literals: (split - best_backward - anchor) as u32,
                    length: best as u32,
                });
                debug_assert!(sequences.len() <= BLOCK_SIZE_MAX / MIN_MATCH);
                self.insert(hash, new);
                anchor = split + best_forward;
                if anchor > ip + hashed {
                    // Same frozen gear_reset behavior as at block entry.
                    ip = anchor - hashed;
                    break;
                }
            }
            ip += hashed;
        }
    }
}

/// A local cursor, copied from the raw sequence store for each optimal parse pass.
pub(crate) struct Cursor<'a> {
    sequences: &'a [RawSeq],
    pos: usize,
    inside: u32,
    start: u32,
    end: u32,
    offset: u32,
}

impl<'a> Cursor<'a> {
    pub fn new(sequences: &'a [RawSeq], bytes: u32) -> Self {
        let mut result = Self {
            sequences,
            pos: 0,
            inside: 0,
            start: 0,
            end: 0,
            offset: 0,
        };
        result.next(0, bytes);
        result
    }

    fn skip(&mut self, bytes: u32) {
        let mut position = self.inside + bytes;
        while position > 0 && self.pos < self.sequences.len() {
            let seq = self.sequences[self.pos];
            if position >= seq.literals + seq.length {
                position -= seq.literals + seq.length;
                self.pos += 1;
            } else {
                self.inside = position;
                break;
            }
        }
        if position == 0 || self.pos == self.sequences.len() {
            self.inside = 0;
        }
    }

    fn next(&mut self, current: u32, remaining: u32) {
        if self.pos >= self.sequences.len() {
            self.start = u32::MAX;
            self.end = u32::MAX;
            return;
        }
        let seq = self.sequences[self.pos];
        let literals = seq.literals.saturating_sub(self.inside);
        let matches = if literals == 0 {
            seq.length - (self.inside - seq.literals)
        } else {
            seq.length
        };
        if literals >= remaining {
            self.start = u32::MAX;
            self.end = u32::MAX;
            self.skip(remaining);
            return;
        }
        self.start = current + literals;
        self.end = self.start + matches;
        self.offset = seq.offset;
        if self.end > current + remaining {
            self.end = current + remaining;
            self.skip(remaining);
        } else {
            self.skip(literals + matches);
        }
    }

    pub fn process(&mut self, current: u32, remaining: u32, matches: &mut [Match], count: &mut usize) {
        if self.pos >= self.sequences.len() {
            return;
        }
        if current >= self.end {
            if current > self.end {
                self.skip(current - self.end);
            }
            self.next(current, remaining);
        }
        if current < self.start || current >= self.end {
            return;
        }
        let length = self.end - current;
        if length < 3 {
            return;
        }
        if *count == 0 || length > matches[*count - 1].len && *count < OPT_NUM {
            matches[*count] = Match {
                off: self.offset + 3,
                len: length,
            };
            *count += 1;
        }
    }
}
