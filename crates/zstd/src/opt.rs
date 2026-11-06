//! Optimal parsing and adaptive symbol prices (`zstd_opt.c`, no attached dictionary).

use crate::params::{BLOCK_SIZE_MAX, Strategy, highbit32};
use crate::seq::{LL_BITS, ML_BITS, SeqStore, ll_code, ml_code};
use crate::window::{MatchState, Mem};

pub(crate) const OPT_NUM: usize = 1 << 12;
const OPT_SIZE: usize = OPT_NUM + 3;
const MAX_PRICE: i32 = 1 << 30;
const BIT: u32 = 256;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Match {
    pub off: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Optimal {
    price: i32,
    off: u32,
    mlen: u32,
    litlen: u32,
    rep: [u32; 3],
}

#[derive(Debug)]
pub(crate) struct OptState {
    lit: [u32; 256],
    ll: [u32; 36],
    ml: [u32; 53],
    of: [u32; 32],
    sums: [u32; 4],
    bases: [u32; 4],
    predef: bool,
    prices: Vec<Optimal>,
    matches: Vec<Match>,
}

fn weight(stat: u32, accurate: bool) -> u32 {
    let hb = highbit32(stat + 1);
    hb * BIT + if accurate { ((stat + 1) << 8) >> hb } else { 0 }
}

fn downscale(table: &mut [u32], shift: u32, base_one: bool) -> u32 {
    let mut sum = 0;
    for stat in table {
        *stat = u32::from(base_one || *stat > 0) + (*stat >> shift);
        sum += *stat;
    }
    sum
}

fn scale(table: &mut [u32], target: u32) -> u32 {
    let sum: u32 = table.iter().sum();
    let factor = sum >> target;
    if factor <= 1 {
        sum
    } else {
        downscale(table, highbit32(factor), true)
    }
}

impl OptState {
    pub fn new() -> Self {
        Self {
            lit: [0; 256],
            ll: [0; 36],
            ml: [0; 53],
            of: [0; 32],
            sums: [0; 4],
            bases: [0; 4],
            predef: false,
            prices: vec![Optimal::default(); OPT_SIZE],
            matches: vec![Match::default(); OPT_SIZE],
        }
    }

    pub fn reset(&mut self) {
        self.sums[1] = 0;
    }

    fn set_base_prices(&mut self, accurate: bool) {
        for (base, sum) in self.bases.iter_mut().zip(self.sums) {
            *base = weight(sum, accurate);
        }
    }

    /// First-block histograms or downscaled statistics from the preceding block.
    fn rescale(&mut self, src: &[u8], accurate: bool) {
        self.predef = false;
        if self.sums[1] == 0 {
            self.predef = src.len() <= 8;
            self.lit.fill(0);
            for &byte in src {
                self.lit[usize::from(byte)] += 1;
            }
            self.sums[0] = downscale(&mut self.lit, 8, false);
            self.ll.fill(1);
            self.ll[0] = 4;
            self.ll[1] = 2;
            self.sums[1] = self.ll.iter().sum();
            self.ml.fill(1);
            self.sums[2] = 53;
            self.of = [
                6, 2, 1, 1, 2, 3, 4, 4, 4, 3, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1,
            ];
            self.sums[3] = self.of.iter().sum();
        } else {
            self.sums[0] = scale(&mut self.lit, 12);
            self.sums[1] = scale(&mut self.ll, 11);
            self.sums[2] = scale(&mut self.ml, 11);
            self.sums[3] = scale(&mut self.of, 11);
        }
        self.set_base_prices(accurate);
    }

    fn literal_cost(&self, bytes: &[u8], accurate: bool) -> i32 {
        if self.predef {
            return (bytes.len() as u32 * 6 * BIT) as i32;
        }
        let max = self.bases[0] - BIT;
        let cost = bytes.iter().fold(self.bases[0] * bytes.len() as u32, |cost, &b| {
            cost - weight(self.lit[usize::from(b)], accurate).min(max)
        });
        cost as i32
    }

    fn ll_price(&self, len: u32, accurate: bool) -> i32 {
        if self.predef {
            return weight(len, accurate) as i32;
        }
        if len == BLOCK_SIZE_MAX as u32 {
            return BIT as i32 + self.ll_price(len - 1, accurate);
        }
        let code = ll_code(len) as usize;
        (u32::from(LL_BITS[code]) * BIT + self.bases[1] - weight(self.ll[code], accurate)) as i32
    }

    fn ll_increment(&self, len: u32, accurate: bool) -> i32 {
        self.ll_price(len, accurate) - self.ll_price(len - 1, accurate)
    }

    fn match_price(&self, off: u32, len: u32, accurate: bool) -> i32 {
        let code = highbit32(off);
        if self.predef {
            return (weight(len - 3, accurate) + (16 + code) * BIT) as i32;
        }
        let mut price = code * BIT + self.bases[3] - weight(self.of[code as usize], accurate);
        if !accurate && code >= 20 {
            price += (code - 19) * 2 * BIT;
        }
        let ml = ml_code(len - 3) as usize;
        price += u32::from(ML_BITS[ml]) * BIT + self.bases[2] - weight(self.ml[ml], accurate);
        (price + BIT / 5) as i32
    }

    fn update_stats(&mut self, literals: &[u8], off: u32, len: u32) {
        for &b in literals {
            self.lit[usize::from(b)] += 2;
        }
        self.sums[0] += literals.len() as u32 * 2;
        self.ll[ll_code(literals.len() as u32) as usize] += 1;
        self.sums[1] += 1;
        self.of[highbit32(off) as usize] += 1;
        self.sums[3] += 1;
        self.ml[ml_code(len - 3) as usize] += 1;
        self.sums[2] += 1;
    }
}

pub(crate) fn new_rep(mut rep: [u32; 3], off: u32, ll0: bool) -> [u32; 3] {
    if off > 3 {
        [off - 3, rep[0], rep[1]]
    } else {
        let code = off - 1 + u32::from(ll0);
        if code > 0 {
            let current = if code == 3 { rep[0] - 1 } else { rep[code as usize] };
            if code >= 2 {
                rep[2] = rep[1];
            }
            rep[1] = rep[0];
            rep[0] = current;
        }
        rep
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the reference parser's explicit working state"
)]
fn parse(
    ms: &mut MatchState,
    state: &mut OptState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    start: usize,
    size: usize,
    accurate: bool,
    ldm_sequences: &[crate::ldm::RawSeq],
) -> usize {
    let end = start + size;
    let limit = end as isize - 8;
    let prefix = (i64::from(ms.window.dict_limit) - ms.window.base) as usize;
    let mut ip = start + usize::from(start == prefix);
    let mut anchor = start;
    let min_match = if ms.params.min_match == 3 { 3 } else { 4 };
    let sufficient = (ms.params.target_length as usize).min(OPT_NUM - 1);
    let mut next3 = ms.next_to_update;
    let mut ldm = crate::ldm::Cursor::new(ldm_sequences, size as u32);
    state.rescale(&mem.cur[start..end], accurate);
    while (ip as isize) < limit {
        let litlen = (ip - anchor) as u32;
        let mut count = crate::opt_tree::get_all_matches(
            ms,
            mem,
            ip,
            end,
            *rep,
            litlen == 0,
            min_match,
            &mut next3,
            &mut state.matches,
        );
        ldm.process(
            (ip - start) as u32,
            (end - ip) as u32,
            &mut state.matches,
            &mut count,
        );
        if count == 0 {
            ip += 1;
            continue;
        }
        state.prices[0] = Optimal {
            price: state.ll_price(litlen, accurate),
            off: 0,
            mlen: 0,
            litlen,
            rep: *rep,
        };
        let max_match = state.matches[count - 1];
        let (mut cur, last_pos, last_stretch) = if max_match.len as usize > sufficient {
            (
                0,
                max_match.len as usize,
                Optimal {
                    off: max_match.off,
                    mlen: max_match.len,
                    ..Optimal::default()
                },
            )
        } else {
            let mut pos = 1;
            while pos < min_match as usize {
                state.prices[pos].price = MAX_PRICE;
                state.prices[pos].mlen = 0;
                state.prices[pos].litlen = litlen + pos as u32;
                pos += 1;
            }
            for m in 0..count {
                let matched = state.matches[m];
                while pos <= matched.len as usize {
                    let price = state.prices[0].price + state.match_price(matched.off, pos as u32, accurate);
                    state.prices[pos].mlen = pos as u32;
                    state.prices[pos].off = matched.off;
                    state.prices[pos].litlen = 0;
                    state.prices[pos].price = price + state.ll_price(0, accurate);
                    pos += 1;
                }
            }
            let mut last = pos - 1;
            state.prices[pos].price = MAX_PRICE;
            let mut cursor = 1;
            let mut shortcut = None;
            while cursor <= last {
                let at = cursor;
                cursor += 1;
                let inr = ip + at;
                let len = state.prices[at - 1].litlen + 1;
                let price = state.prices[at - 1].price
                    + state.literal_cost(&mem.cur[inr - 1..inr], accurate)
                    + state.ll_increment(len, accurate);
                if price <= state.prices[at].price {
                    let prev_match = state.prices[at];
                    state.prices[at] = state.prices[at - 1];
                    state.prices[at].litlen = len;
                    state.prices[at].price = price;
                    if accurate && prev_match.litlen == 0 && state.ll_increment(1, accurate) < 0 && inr < end
                    {
                        let literal = state.literal_cost(&mem.cur[inr..=inr], accurate);
                        let with_one = prev_match.price + literal + state.ll_increment(1, accurate);
                        let with_more = price + literal + state.ll_increment(len + 1, accurate);
                        if with_one < with_more && with_one < state.prices[at + 1].price {
                            let prev = at - prev_match.mlen as usize;
                            let reps = new_rep(
                                state.prices[prev].rep,
                                prev_match.off,
                                state.prices[prev].litlen == 0,
                            );
                            state.prices[at + 1] = prev_match;
                            state.prices[at + 1].rep = reps;
                            state.prices[at + 1].litlen = 1;
                            state.prices[at + 1].price = with_one;
                            last = last.max(at + 1);
                        }
                    }
                }
                if state.prices[at].litlen == 0 {
                    let prev = at - state.prices[at].mlen as usize;
                    state.prices[at].rep = new_rep(
                        state.prices[prev].rep,
                        state.prices[at].off,
                        state.prices[prev].litlen == 0,
                    );
                }
                if inr as isize > limit {
                    continue;
                }
                if at == last {
                    break;
                }
                if !accurate && state.prices[at + 1].price <= state.prices[at].price + (BIT / 2) as i32 {
                    continue;
                }
                let base_price = state.prices[at].price + state.ll_price(0, accurate);
                let mut count = crate::opt_tree::get_all_matches(
                    ms,
                    mem,
                    inr,
                    end,
                    state.prices[at].rep,
                    state.prices[at].litlen == 0,
                    min_match,
                    &mut next3,
                    &mut state.matches,
                );
                ldm.process(
                    (inr - start) as u32,
                    (end - inr) as u32,
                    &mut state.matches,
                    &mut count,
                );
                if count == 0 {
                    continue;
                }
                let longest = state.matches[count - 1];
                if longest.len as usize > sufficient
                    || at + longest.len as usize >= OPT_NUM
                    || inr + longest.len as usize >= end
                {
                    last = at + longest.len as usize;
                    shortcut = Some((
                        at,
                        Optimal {
                            off: longest.off,
                            mlen: longest.len,
                            ..Optimal::default()
                        },
                    ));
                    break;
                }
                for m in 0..count {
                    let matched = state.matches[m];
                    let first = if m > 0 {
                        state.matches[m - 1].len + 1
                    } else {
                        min_match
                    };
                    for len in (first..=matched.len).rev() {
                        let pos = at + len as usize;
                        let price = base_price + state.match_price(matched.off, len, accurate);
                        if pos > last || price < state.prices[pos].price {
                            while last < pos {
                                last += 1;
                                state.prices[last].price = MAX_PRICE;
                                state.prices[last].litlen = 1;
                            }
                            state.prices[pos].mlen = len;
                            state.prices[pos].off = matched.off;
                            state.prices[pos].litlen = 0;
                            state.prices[pos].price = price;
                        } else if !accurate {
                            break;
                        }
                    }
                }
                state.prices[last + 1].price = MAX_PRICE;
            }
            let (cur, stretch) = match shortcut {
                Some(value) => value,
                None => (last - state.prices[last].mlen as usize, state.prices[last]),
            };
            (cur, last, stretch)
        };
        if last_stretch.mlen == 0 {
            ip += last_pos;
            continue;
        }
        if last_stretch.litlen == 0 {
            *rep = new_rep(
                state.prices[cur].rep,
                last_stretch.off,
                state.prices[cur].litlen == 0,
            );
        } else {
            *rep = last_stretch.rep;
            cur -= last_stretch.litlen as usize;
        }
        let store_end = cur + 2;
        // Preserve the frozen parser's partial copy before its unconditional
        // last-stretch assignment (the two C++ scopes are intentionally retained).
        if last_stretch.litlen > 0 {
            state.prices[store_end - 1] = last_stretch;
        }
        state.prices[store_end] = last_stretch;
        let mut store_start = store_end;
        let mut stretch_pos = cur;
        loop {
            let next = state.prices[stretch_pos];
            state.prices[store_start].litlen = next.litlen;
            if next.mlen == 0 {
                break;
            }
            store_start -= 1;
            state.prices[store_start] = next;
            stretch_pos -= (next.litlen + next.mlen) as usize;
        }
        for pos in store_start..=store_end {
            let selected = state.prices[pos];
            if selected.mlen == 0 {
                ip = anchor + selected.litlen as usize;
                continue;
            }
            let literals = &mem.cur[anchor..anchor + selected.litlen as usize];
            state.update_stats(literals, selected.off, selected.mlen);
            seq.store(
                mem.cur,
                anchor,
                selected.litlen as usize,
                selected.off,
                selected.mlen as usize,
            );
            anchor += (selected.litlen + selected.mlen) as usize;
            ip = anchor;
        }
        state.set_base_prices(accurate);
    }
    end - anchor
}

pub(crate) fn compress_block(
    ms: &mut MatchState,
    seq: &mut SeqStore,
    rep: &mut [u32; 3],
    mem: Mem<'_>,
    start: usize,
    size: usize,
) -> usize {
    let Some(mut state) = ms.opt.take() else {
        unreachable!("optimal state is allocated at frame initialization")
    };
    let ldm = std::mem::take(&mut ms.ldm_sequences);
    let accurate = ms.params.strategy >= Strategy::BtUltra;
    let current = (start as i64 + ms.window.base) as u32;
    if ms.params.strategy == Strategy::BtUltra2
        && state.sums[1] == 0
        && seq.sequences.is_empty()
        && ms.window.dict_limit == ms.window.low_limit
        && current == ms.window.dict_limit
        && size > 8
    {
        let mut temporary_rep = *rep;
        parse(
            ms,
            &mut state,
            seq,
            &mut temporary_rep,
            mem,
            start,
            size,
            true,
            &ldm,
        );
        seq.reset();
        ms.window.base += size as i64;
        ms.window.dict_limit += size as u32;
        ms.window.low_limit = ms.window.dict_limit;
        ms.next_to_update = ms.window.dict_limit;
    }
    let result = parse(ms, &mut state, seq, rep, mem, start, size, accurate, &ldm);
    ms.opt = Some(state);
    ms.ldm_sequences = ldm;
    result
}
