//! Skipping the random number streams ahead, for generating a part of a table (DuckDB's
//! `dbgen/speed_seed.cpp`).
//!
//! The `sd_*` functions advance the streams of a table by the draws `skip_count` rows
//! make: the per-row boundaries of the streams, which `row_stop_h` pads every row to.

use super::dss::{DbgenContext, O_LCNT_MAX, SUPP_PER_PART, Seed, stream};
use super::rnd::{MODULUS, MULTIPLIER};

/// `MULTIPLIER^n mod MODULUS` for the small `n` rows advance streams by, computed once.
const SMALL_POWERS: [i64; 128] = {
    let mut powers = [1_i64; 128];
    let mut n = 1;
    while n < 128 {
        powers[n] = powers[n - 1] * MULTIPLIER % MODULUS;
        n += 1;
    }
    powers
};

/// `NthElement`: the value of the stream after `n` more draws, `X_n = Mult^n * X_0 mod
/// Modulus`, by squaring. A count that is not positive leaves the value.
pub(crate) fn nth_element(n: i64, start_seed: &mut i64) {
    if n <= 0 {
        return;
    }
    // Rows advance by a few draws; the power then comes from the table (the same value
    // the squaring below computes).
    if let Some(&power) = SMALL_POWERS.get(n as usize) {
        *start_seed = power * *start_seed % MODULUS;
        return;
    }
    let mut n = n;
    let mut mult = MULTIPLIER;
    let mut z = *start_seed;
    while n > 0 {
        if n % 2 != 0 {
            z = (mult * z) % MODULUS;
        }
        n /= 2;
        mult = (mult * mult) % MODULUS;
    }
    *start_seed = z;
}

/// `advanceStream` / `ADVANCE_STREAM`: skips `calls` draws of `seed`.
fn advance_stream(seed: &mut Seed, calls: i64) {
    nth_element(calls, &mut seed.value);
}

/// `sd_part`.
pub(crate) fn sd_part(skip_count: i64, ctx: &mut DbgenContext) {
    for i in stream::P_MFG_SD..=stream::P_CNTR_SD {
        advance_stream(&mut ctx.seeds[i], skip_count);
    }
    advance_stream(&mut ctx.seeds[stream::P_CMNT_SD], skip_count * 2);
    advance_stream(&mut ctx.seeds[stream::P_NAME_SD], skip_count * 92);
}

/// `sd_line`; `child` 1 also skips the order streams lineitems share.
pub(crate) fn sd_line(child: usize, skip_count: i64, ctx: &mut DbgenContext) {
    for _ in 0..O_LCNT_MAX {
        for i in stream::L_QTY_SD..=stream::L_RFLG_SD {
            advance_stream(&mut ctx.seeds[i], skip_count);
        }
        advance_stream(&mut ctx.seeds[stream::L_CMNT_SD], skip_count * 2);
    }
    // The link between master and detail.
    if child == 1 {
        advance_stream(&mut ctx.seeds[stream::O_ODATE_SD], skip_count);
        advance_stream(&mut ctx.seeds[stream::O_LCNT_SD], skip_count);
    }
}

/// `sd_order`.
pub(crate) fn sd_order(skip_count: i64, ctx: &mut DbgenContext) {
    advance_stream(&mut ctx.seeds[stream::O_LCNT_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::O_CKEY_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::O_CMNT_SD], skip_count * 2);
    advance_stream(&mut ctx.seeds[stream::O_SUPP_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::O_CLRK_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::O_PRIO_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::O_ODATE_SD], skip_count);
}

/// `sd_psupp`.
pub(crate) fn sd_psupp(skip_count: i64, ctx: &mut DbgenContext) {
    for _ in 0..SUPP_PER_PART {
        advance_stream(&mut ctx.seeds[stream::PS_QTY_SD], skip_count);
        advance_stream(&mut ctx.seeds[stream::PS_SCST_SD], skip_count);
        advance_stream(&mut ctx.seeds[stream::PS_CMNT_SD], skip_count * 2);
    }
}

/// `sd_cust`.
pub(crate) fn sd_cust(skip_count: i64, ctx: &mut DbgenContext) {
    advance_stream(&mut ctx.seeds[stream::C_ADDR_SD], skip_count * 9);
    advance_stream(&mut ctx.seeds[stream::C_CMNT_SD], skip_count * 2);
    advance_stream(&mut ctx.seeds[stream::C_NTRG_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::C_PHNE_SD], 3 * skip_count);
    advance_stream(&mut ctx.seeds[stream::C_ABAL_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::C_MSEG_SD], skip_count);
}

/// `sd_supp`.
pub(crate) fn sd_supp(skip_count: i64, ctx: &mut DbgenContext) {
    advance_stream(&mut ctx.seeds[stream::S_NTRG_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::S_PHNE_SD], 3 * skip_count);
    advance_stream(&mut ctx.seeds[stream::S_ABAL_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::S_ADDR_SD], skip_count * 9);
    advance_stream(&mut ctx.seeds[stream::S_CMNT_SD], skip_count * 2);
    advance_stream(&mut ctx.seeds[stream::BBB_CMNT_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::BBB_JNK_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::BBB_OFFSET_SD], skip_count);
    advance_stream(&mut ctx.seeds[stream::BBB_TYPE_SD], skip_count);
}

/// `sd_nation`.
pub(crate) fn sd_nation(skip_count: i64, ctx: &mut DbgenContext) {
    advance_stream(&mut ctx.seeds[stream::N_CMNT_SD], skip_count * 2);
}

/// `sd_region`.
pub(crate) fn sd_region(skip_count: i64, ctx: &mut DbgenContext) {
    advance_stream(&mut ctx.seeds[stream::R_CMNT_SD], skip_count * 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbgen::rnd::next_rand;

    #[test]
    fn nth_element_matches_stepping() {
        let start = 933_588_178_i64;
        let mut stepped = start;
        for n in 0..300 {
            let mut skipped = start;
            nth_element(n, &mut skipped);
            assert_eq!(skipped, stepped, "after {n} draws");
            stepped = next_rand(stepped);
        }
        let mut unchanged = start;
        nth_element(-3, &mut unchanged);
        assert_eq!(unchanged, start);
    }
}
