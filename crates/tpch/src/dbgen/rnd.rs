//! The portable random number generators (DuckDB's `dbgen/rnd.cpp`, the Park-Miller
//! "minimal standard" generator by Frank Stephens of Unisys, and `dbgen/rng64.cpp`, the
//! 64-bit linear congruential generator for scale factors of 30000 and more; its
//! `AdvanceRand64` is left out, as DuckDB skips those streams with the 32-bit one).
//!
//! Every column draws from a stream of its own, and every row advances each stream of
//! its table to a fixed number of draws (`row_stop_h`), so that any row can be generated
//! after skipping the streams ahead (`speed_seed`).

use super::dss::{DbgenContext, MAX_LONG, MAX_STREAM, Seed, Table};
use super::speed_seed::nth_element;

/// The modulus of the generator (`2^31 - 1`).
pub(crate) const MODULUS: i64 = 2_147_483_647;
/// The multiplier of the generator.
pub(crate) const MULTIPLIER: i64 = 16807;
/// `DBGenContext::dM`.
const D_M: f64 = 2_147_483_647.0;

/// `RNG_A` / `RNG_C` of the 64-bit generator.
const RNG_A: u64 = 6_364_136_223_846_793_005;
const RNG_C: u64 = 1;

/// `NextRand`: the next value of the generator.
pub(crate) const fn next_rand(seed: i64) -> i64 {
    (seed * MULTIPLIER) % MODULUS
}

/// `UnifInt`: a value uniformly distributed in `[low, high]`.
pub(crate) fn unif_int(low: i64, high: i64, seed: &mut Seed) -> i64 {
    // DuckDB computes the full range in 32 bits; it equals the 64-bit range.
    let range = if high == MAX_LONG && low == 0 {
        (i64::from(high as i32 - low as i32) + 1) as f64
    } else {
        (high - low + 1) as f64
    };
    seed.value = next_rand(seed.value);
    let offset = ((seed.value as f64 / D_M) * range) as i64;
    low + offset
}

/// `dss_random` (`RANDOM`): one draw from `seed`, counted in its usage.
pub(crate) fn dss_random(low: i64, high: i64, seed: &mut Seed) -> i64 {
    let value = unif_int(low, high, seed);
    seed.usage += 1;
    value
}

/// `row_start`: resets the draws of every stream for a new row.
pub(crate) fn row_start(ctx: &mut DbgenContext) {
    for seed in &mut ctx.seeds {
        seed.usage = 0;
    }
}

/// `row_stop_h`: advances every stream of table `t` (and of its child table) to the
/// stream's per-row boundary, so that rows start at predictable stream positions.
pub(crate) fn row_stop_h(t: Table, ctx: &mut DbgenContext) {
    // The master and detail tables are generated together.
    let t = match t {
        Table::OrderLine => Table::Order,
        Table::PartPsupp => Table::Part,
        other => other,
    };
    let child = ctx.tdefs[t.index()].child;
    for seed in &mut ctx.seeds[..=MAX_STREAM] {
        if seed.table == Some(t) || seed.table == child {
            // `set_seeds` is off: streams that drew more than their boundary stay put.
            nth_element(seed.boundary - seed.usage, &mut seed.value);
        }
    }
}

/// `NextRand64`.
pub(crate) const fn next_rand64(seed: i64) -> i64 {
    (seed as u64).wrapping_mul(RNG_A).wrapping_add(RNG_C) as i64
}

/// `dss_random64` (`RANDOM64`): one draw in `[low, high]` from the 64-bit generator.
pub(crate) fn dss_random64(low: i64, high: i64, seed: &mut Seed) -> i64 {
    let (low, high) = if low > high { (high, low) } else { (low, high) };
    seed.value = next_rand64(seed.value);
    let value = seed.value.wrapping_abs() % (high - low + 1);
    seed.usage += 1;
    low + value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unif_int_stays_in_range() {
        let mut seed = Seed {
            table: None,
            value: 1,
            usage: 0,
            boundary: 1,
        };
        for _ in 0..10_000 {
            let value = dss_random(-5, 5, &mut seed);
            assert!((-5..=5).contains(&value));
        }
        assert_eq!(seed.usage, 10_000);
    }
}
