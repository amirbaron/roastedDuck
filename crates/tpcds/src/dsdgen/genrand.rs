//! The random number streams (`genrand.c`): one Lehmer generator per column, seeded by
//! `init_rand`, and the values built from them.

use super::columns::MAX_COLUMN;
use super::decimal::Decimal;

/// `MAXINT`.
pub(crate) const MAXINT: i64 = i32::MAX as i64;
/// The multiplier of the generator.
const MULT: i64 = 16807;
/// `MAXINT / Mult`.
const Q: i64 = 127_773;
/// `MAXINT % Mult`.
const R: i64 = 2836;
/// `RNG_SEED`, the default of the `RNGSEED` parameter.
const RNG_SEED: i64 = 19_620_718;

/// `DIST_UNIFORM`.
pub(crate) const DIST_UNIFORM: i32 = 0x0001;
/// `DIST_EXPONENTIAL`.
pub(crate) const DIST_EXPONENTIAL: i32 = 0x0002;

/// `ALPHANUM` (without `w` and `W`).
pub(crate) const ALPHANUM: &[u8] = b"abcdefghijklmnopqrstuvxyzABCDEFGHIJKLMNOPQRSTUVXYZ0123456789";
/// `DIGITS`.
pub(crate) const DIGITS: &[u8] = b"0123456789";

/// The seeds of the streams (`Streams[].nSeed` and `nInitialSeed`). DuckDB's dsdgen never
/// calls `row_stop`, so the per-row usage counts of the streams are never read and are not
/// kept.
#[derive(Clone, Debug)]
pub(crate) struct Streams {
    seeds: Vec<i64>,
    initial_seeds: Vec<i64>,
}

impl Streams {
    /// `init_rand`: stream `i` starts `MAXINT / MAX_COLUMN` steps of the seed after stream
    /// `i - 1`, wrapping around like the `int` the original code computed in.
    pub(crate) fn new() -> Self {
        let seed = RNG_SEED;
        let skip = MAXINT / MAX_COLUMN as i64;
        // `Streams[]` has an entry per column; `init_rand` leaves the last one at 0.
        let mut seeds = vec![0; MAX_COLUMN + 1];
        for (i, slot) in seeds.iter_mut().enumerate().take(MAX_COLUMN) {
            let i = i as i64;
            *slot = if i != 0 && (i64::from(i32::MAX) - seed) / i < skip {
                // Simulate the overflow as if it were an int.
                (seed + skip * i) % MAXINT - MAXINT - 2
            } else {
                seed + skip * i
            };
        }
        Self {
            initial_seeds: seeds.clone(),
            seeds,
        }
    }

    /// `next_random`.
    #[inline]
    pub(crate) fn next_random(&mut self, stream: usize) -> i64 {
        let seed = &mut self.seeds[stream];
        let div = *seed / Q;
        let rem = *seed - Q * div;
        let mut next = MULT * rem - div * R;
        if next < 0 {
            next += MAXINT;
        }
        *seed = next;
        next
    }

    /// `skip_random`: sets `stream` to its value `count` draws after its initial seed.
    pub(crate) fn skip_random(&mut self, stream: usize, mut count: i64) {
        let mut multiplier = MULT;
        let mut seed = self.initial_seeds[stream];
        while count > 0 {
            if count % 2 != 0 {
                seed = (multiplier * seed) % MAXINT;
            }
            count /= 2;
            multiplier = (multiplier * multiplier) % MAXINT;
        }
        self.seeds[stream] = seed;
    }

    /// `genrand_integer`: an integer of distribution `dist` in `[min, max]`.
    #[inline]
    pub(crate) fn genrand_integer(
        &mut self,
        dist: i32,
        min: i32,
        max: i32,
        _mean: i32,
        stream: usize,
    ) -> i32 {
        match dist {
            DIST_UNIFORM => {
                let res = self.next_random(stream) as i32;
                (res % max.wrapping_sub(min).wrapping_add(1)).wrapping_add(min)
            }
            DIST_EXPONENTIAL => {
                // The integer division makes every term -0.5, as in the original.
                let mut fres = 0.0;
                for _ in 0..12 {
                    fres += (self.next_random(stream) / MAXINT) as f64 - 0.5;
                }
                min.wrapping_add((f64::from(max.wrapping_sub(min).wrapping_add(1)) * fres) as i32)
            }
            _ => 0,
        }
    }

    /// `genrand_key`: a key of distribution `dist` in `[min, max]`, computed in `int` as the
    /// original does.
    #[inline]
    pub(crate) fn genrand_key(&mut self, dist: i32, min: i64, max: i64, _mean: i64, stream: usize) -> i64 {
        match dist {
            DIST_UNIFORM => {
                let res = self.next_random(stream) as i32;
                i64::from((res % ((max - min + 1) as i32)).wrapping_add(min as i32))
            }
            DIST_EXPONENTIAL => {
                let mut fres = 0.0;
                for _ in 0..12 {
                    fres += (self.next_random(stream) / MAXINT) as f64 - 0.5;
                }
                i64::from((min as i32).wrapping_add(((max - min + 1) as f64 * fres) as i32))
            }
            _ => 0,
        }
    }

    /// `genrand_decimal`: a decimal of distribution `dist` in `[min, max]`, with the
    /// smaller precision of the two.
    pub(crate) fn genrand_decimal(
        &mut self,
        dist: i32,
        min: &Decimal,
        max: &Decimal,
        mean: Option<&Decimal>,
        stream: usize,
    ) -> Decimal {
        let precision = if min.precision < max.precision {
            min.precision
        } else {
            max.precision
        };
        let number = match dist {
            DIST_UNIFORM => {
                let res = self.next_random(stream);
                res % (max.number - min.number + 1) + min.number
            }
            DIST_EXPONENTIAL => {
                let mut fres = 0.0;
                for _ in 0..12 {
                    fres /= 2.0;
                    fres += self.next_random(stream) as f64 / MAXINT as f64 - 0.5;
                }
                let mean = mean.map_or(0, |mean| mean.number);
                mean + i64::from(((max.number - min.number + 1) as f64 * fres) as i32)
            }
            _ => 0,
        };
        let mut scale = 0;
        let mut rest = number;
        while rest > 10 {
            rest /= 10;
            scale += 1;
        }
        Decimal {
            precision,
            scale,
            number,
        }
    }

    /// `gen_charset`: a string of `[min, max]` characters of `set`; draws `max` characters
    /// whatever the length.
    pub(crate) fn gen_charset(&mut self, dest: &mut String, set: &[u8], min: i32, max: i32, stream: usize) {
        let len = self.genrand_integer(DIST_UNIFORM, min, max, 0, stream);
        dest.clear();
        for i in 0..max {
            let temp = self.genrand_integer(DIST_UNIFORM, 0, set.len() as i32 - 1, 0, stream);
            if i < len {
                dest.push(char::from(set[temp as usize]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skipping a stream is drawing from it.
    #[test]
    fn skip_random_draws() {
        let mut drawn = Streams::new();
        let mut skipped = Streams::new();
        for stream in [1, 150, 260, 500, 790] {
            for count in 0..50 {
                skipped.skip_random(stream, count);
                assert_eq!(skipped.seeds[stream], drawn.seeds[stream], "stream {stream} after {count}");
                drawn.next_random(stream);
            }
        }
    }
}
