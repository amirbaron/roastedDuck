//! The pseudo text generator (DuckDB's `dbgen/text.cpp`): a pool of sentences built from
//! a small grammar, from which the comment columns take random slices.
//!
//! DuckDB builds the pool for every `dbgen` call from the same stream; it is built once
//! here. The unused `txt_*` generators of the original dbgen are not ported.

use std::sync::LazyLock;

use super::bm_utils::Distribution;
use super::dists::{Distributions, distributions};
use super::dss::{DbgenContext, Seed, V_STR_HGH, V_STR_LOW, stream};
use super::rnd::dss_random;

/// The size of the text pool (`txtBufferSize`, 10 MiB).
const TEXT_POOL_SIZE: i64 = 10 * 1024 * 1024;

/// A lookup table from a drawn weight to the member it selects (`gen_index`).
struct WordIndex {
    words: Vec<&'static [u8]>,
    max_weight: i64,
}

impl WordIndex {
    fn new(dist: &Distribution) -> Self {
        let max_weight = dist.max_weight();
        let words = (0..=max_weight)
            .map(|w| {
                let i = dist.list.iter().position(|&(weight, _)| weight >= w).unwrap_or(0);
                dist.member(i)
            })
            .collect();
        Self { words, max_weight }
    }

    /// `gen_text_index`: a word chosen by weight, followed by a space.
    fn push(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        let j = dss_random(1, self.max_weight, seed);
        dest.extend_from_slice(self.words[j as usize]);
        dest.push(b' ');
    }
}

/// The form chosen by one draw from a grammar distribution: the number of its first
/// `forms` cumulative weights below the draw.
fn pick_form(dist: &Distribution, forms: usize, seed: &mut Seed) -> usize {
    let j = dss_random(1, dist.max_weight(), seed);
    dist.list[..forms]
        .iter()
        .filter(|&&(weight, _)| weight < j)
        .count()
}

/// The sentence generator of `init_text_pool`.
struct SentenceGenerator<'a> {
    dists: &'a Distributions,
    nouns: WordIndex,
    adjectives: WordIndex,
    adverbs: WordIndex,
    auxillaries: WordIndex,
    verbs: WordIndex,
    prepositions: WordIndex,
}

impl<'a> SentenceGenerator<'a> {
    fn new(dists: &'a Distributions) -> Self {
        Self {
            dists,
            nouns: WordIndex::new(&dists.nouns),
            adjectives: WordIndex::new(&dists.adjectives),
            adverbs: WordIndex::new(&dists.adverbs),
            auxillaries: WordIndex::new(&dists.auxillaries),
            verbs: WordIndex::new(&dists.verbs),
            prepositions: WordIndex::new(&dists.prepositions),
        }
    }

    /// `gen_vp`: a verb phrase (`V`, `X V`, `V D` or `X V D`).
    fn gen_vp(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        match pick_form(&self.dists.vp, 3, seed) {
            0 => self.verbs.push(dest, seed),
            1 => {
                self.auxillaries.push(dest, seed);
                self.verbs.push(dest, seed);
            }
            2 => {
                self.verbs.push(dest, seed);
                self.adverbs.push(dest, seed);
            }
            _ => {
                self.auxillaries.push(dest, seed);
                self.verbs.push(dest, seed);
                self.adverbs.push(dest, seed);
            }
        }
    }

    /// `gen_np`: a noun phrase (`N`, `J N`, `J, J N` or `D J N`).
    fn gen_np(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        match pick_form(&self.dists.np, 3, seed) {
            0 => self.nouns.push(dest, seed),
            1 => {
                self.adjectives.push(dest, seed);
                self.nouns.push(dest, seed);
            }
            2 => {
                self.adjectives.push(dest, seed);
                // "adjective, adjective noun": the comma replaces the space.
                dest.pop();
                dest.extend_from_slice(b", ");
                self.adjectives.push(dest, seed);
                self.nouns.push(dest, seed);
            }
            _ => {
                self.adverbs.push(dest, seed);
                self.adjectives.push(dest, seed);
                self.nouns.push(dest, seed);
            }
        }
    }

    /// `gen_preposition`: a preposition, "the" and a noun phrase.
    fn gen_preposition(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        self.prepositions.push(dest, seed);
        dest.extend_from_slice(b"the ");
        self.gen_np(dest, seed);
    }

    /// `gen_terminator`: the punctuation ending a sentence, which abuts the previous
    /// word, and a space.
    fn gen_terminator(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        dest.pop();
        dest.extend_from_slice(self.dists.terminators.pick(seed));
        dest.push(b' ');
    }

    /// `gen_sentence`: a sentence of one of the five grammar forms, then a space.
    fn gen_sentence(&self, dest: &mut Vec<u8>, seed: &mut Seed) {
        match pick_form(&self.dists.grammar, 4, seed) {
            0 => {
                self.gen_np(dest, seed);
                self.gen_vp(dest, seed);
            }
            1 => {
                self.gen_np(dest, seed);
                self.gen_vp(dest, seed);
                self.gen_preposition(dest, seed);
            }
            2 => {
                self.gen_np(dest, seed);
                self.gen_vp(dest, seed);
                self.gen_np(dest, seed);
            }
            3 => {
                self.gen_np(dest, seed);
                self.gen_preposition(dest, seed);
                self.gen_vp(dest, seed);
                self.gen_np(dest, seed);
            }
            _ => {
                self.gen_np(dest, seed);
                self.gen_preposition(dest, seed);
                self.gen_vp(dest, seed);
                self.gen_preposition(dest, seed);
            }
        }
        self.gen_terminator(dest, seed);
    }
}

/// `init_text_pool`: `size` bytes of sentences generated from the text pregeneration
/// stream of a fresh generator.
fn init_text_pool(size: usize, dists: &Distributions) -> Vec<u8> {
    let generator = SentenceGenerator::new(dists);
    let mut seed = DbgenContext::default().seeds[stream::TEXT_POOL_SD];
    let mut pool = Vec::with_capacity(size + 512);
    while pool.len() < size + 1 {
        generator.gen_sentence(&mut pool, &mut seed);
    }
    pool.truncate(size);
    pool
}

/// The text pool, built on first use.
fn text_pool() -> &'static [u8] {
    static POOL: LazyLock<Vec<u8>> =
        LazyLock::new(|| init_text_pool(TEXT_POOL_SIZE as usize, distributions()));
    &POOL
}

/// `dbg_text`: a slice of the text pool at a random offset, of a length drawn from
/// `[min, max]`.
fn dbg_text(min: i32, max: i32, seed: &mut Seed) -> &'static [u8] {
    let offset = dss_random(0, TEXT_POOL_SIZE - i64::from(max), seed) as usize;
    let length = dss_random(i64::from(min), i64::from(max), seed) as usize;
    &text_pool()[offset..offset + length]
}

/// `TEXT`: text of about `avg` bytes.
pub(crate) fn text(avg: i32, seed: &mut Seed) -> &'static [u8] {
    let avg = f64::from(avg);
    dbg_text((avg * V_STR_LOW) as i32, (avg * V_STR_HGH) as i32, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_is_printable_text() {
        let pool = text_pool();
        assert_eq!(pool.len(), TEXT_POOL_SIZE as usize);
        assert!(pool.iter().all(|&byte| byte.is_ascii_graphic() || byte == b' '));
    }
}
