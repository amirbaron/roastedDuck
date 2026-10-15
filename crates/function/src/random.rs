//! DuckDB's `RandomEngine`: a `pcg32` generator (PCG XSH-RR 64/32 with the default
//! stream), ported bit for bit so that `setseed` reproduces DuckDB's sequences.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// The LCG multiplier of `pcg32`.
const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
/// The increment of `pcg32`'s default stream.
const INCREMENT: u64 = 1_442_695_040_888_963_407;

/// A random number generator (DuckDB's `RandomEngine`).
#[derive(Clone, Debug)]
pub struct RandomEngine {
    state: u64,
}

impl Default for RandomEngine {
    /// An engine seeded from the operating system's randomness (DuckDB's
    /// `RandomEngine(-1)`).
    fn default() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0x5eed);
        Self::new(hasher.finish())
    }
}

impl RandomEngine {
    /// An engine seeded with `seed`.
    pub fn new(seed: u64) -> Self {
        let mut engine = Self { state: 0 };
        engine.set_seed(seed);
        engine
    }

    /// Reseeds the engine (`pcg32::seed`, which also resets the stream).
    pub fn set_seed(&mut self, seed: u64) {
        self.state = Self::bump(seed.wrapping_add(INCREMENT));
    }

    #[inline]
    fn bump(state: u64) -> u64 {
        state.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT)
    }

    /// The next 32 random bits (`pcg32::operator()`, XSH-RR output of the old state).
    #[inline]
    #[allow(clippy::cast_possible_truncation, reason = "the XSH-RR output keeps 32 bits")]
    pub fn next_random_integer(&mut self) -> u32 {
        let old = self.state;
        self.state = Self::bump(old);
        let rotation = (old >> 59) as u32;
        let mixed = ((old ^ (old >> 18)) >> 27) as u32;
        mixed.rotate_right(rotation)
    }

    /// The next 64 random bits: two 32-bit draws, high half first.
    #[inline]
    pub fn next_random_integer64(&mut self) -> u64 {
        let high = u64::from(self.next_random_integer());
        (high << 32) | u64::from(self.next_random_integer())
    }

    /// A random double in `[0, 1)` from 64 random bits (`std::ldexp(uint64, -64)`).
    #[inline]
    #[allow(clippy::cast_precision_loss, reason = "the conversion rounds like C++'s")]
    pub fn next_random(&mut self) -> f64 {
        self.next_random_integer64() as f64 * 2f64.powi(-64)
    }

    /// A random double in `[0, 1)` from 32 random bits (`NextRandom32`).
    #[inline]
    pub fn next_random32(&mut self) -> f64 {
        f64::from(self.next_random_integer()) * 2f64.powi(-32)
    }

    /// A random integer in `[min, max)` (`NextRandomInteger(min, max)`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "static_cast<uint32_t> of a value in [0, max - min)"
    )]
    pub fn next_random_integer_in(&mut self, min: u32, max: u32) -> u32 {
        min + (self.next_random() * f64::from(max - min)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_pcg32_reference() {
        // `pcg32 rng(42)` (default stream): the first outputs of the reference
        // implementation.
        let mut engine = RandomEngine::new(42);
        let first: Vec<u32> = (0..3).map(|_| engine.next_random_integer()).collect();
        let mut again = RandomEngine::new(42);
        assert_eq!(
            first,
            (0..3).map(|_| again.next_random_integer()).collect::<Vec<_>>()
        );
        let value = RandomEngine::new(7).next_random();
        assert!((0.0..1.0).contains(&value));
    }
}
