//! The distributions the generator draws from (the globals of DuckDB's `dbgen/dss.h`,
//! read by `load_dists` of `dbgen/dbgen_gunk.cpp`).
//!
//! DuckDB reads them again for every `dbgen` call; they never change, so they are read
//! once here. `category` and `articles`, which no column uses, are left out.

use std::sync::LazyLock;

use super::bm_utils::{Distribution, read_dist};

/// The distributions of the columns and of the text grammar.
#[derive(Debug)]
pub(crate) struct Distributions {
    pub(crate) p_cntr: Distribution,
    pub(crate) colors: Distribution,
    pub(crate) p_types: Distribution,
    pub(crate) nations: Distribution,
    pub(crate) regions: Distribution,
    pub(crate) o_priority: Distribution,
    pub(crate) l_instruct: Distribution,
    pub(crate) l_smode: Distribution,
    pub(crate) l_rflag: Distribution,
    pub(crate) c_mseg: Distribution,
    // The distributions of text generation.
    pub(crate) nouns: Distribution,
    pub(crate) verbs: Distribution,
    pub(crate) adjectives: Distribution,
    pub(crate) adverbs: Distribution,
    pub(crate) auxillaries: Distribution,
    pub(crate) terminators: Distribution,
    pub(crate) prepositions: Distribution,
    pub(crate) grammar: Distribution,
    pub(crate) np: Distribution,
    pub(crate) vp: Distribution,
}

impl Distributions {
    /// `load_dists` (without the text pool, see `text::text_pool`).
    fn load() -> Self {
        Self {
            p_cntr: read_dist("p_cntr"),
            colors: read_dist("colors"),
            p_types: read_dist("p_types"),
            nations: read_dist("nations"),
            regions: read_dist("regions"),
            o_priority: read_dist("o_oprio"),
            l_instruct: read_dist("instruct"),
            l_smode: read_dist("smode"),
            l_rflag: read_dist("rflag"),
            c_mseg: read_dist("msegmnt"),
            nouns: read_dist("nouns"),
            verbs: read_dist("verbs"),
            adjectives: read_dist("adjectives"),
            adverbs: read_dist("adverbs"),
            auxillaries: read_dist("auxillaries"),
            terminators: read_dist("terminators"),
            prepositions: read_dist("prepositions"),
            grammar: read_dist("grammar"),
            np: read_dist("np"),
            vp: read_dist("vp"),
        }
    }
}

/// The distributions, read on first use.
pub(crate) fn distributions() -> &'static Distributions {
    static DISTRIBUTIONS: LazyLock<Distributions> = LazyLock::new(Distributions::load);
    &DISTRIBUTIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributions_have_their_counts() {
        let d = distributions();
        let counts = [
            (&d.p_cntr, 40),
            (&d.colors, 92),
            (&d.p_types, 150),
            (&d.nations, 25),
            (&d.regions, 5),
            (&d.o_priority, 5),
            (&d.l_instruct, 4),
            (&d.l_smode, 7),
            (&d.l_rflag, 2),
            (&d.c_mseg, 5),
            (&d.nouns, 45),
            (&d.verbs, 40),
            (&d.adjectives, 29),
            (&d.adverbs, 28),
            (&d.auxillaries, 18),
            (&d.terminators, 6),
            (&d.prepositions, 47),
            (&d.grammar, 5),
            (&d.np, 4),
            (&d.vp, 4),
        ];
        for (dist, count) in counts {
            assert_eq!(dist.count(), count);
        }
        // Nation weights are region keys, not a distribution.
        assert_eq!(d.nations.list[4], (4, b"EGYPT".as_slice()));
        assert_eq!(d.nations.list[5], (0, b"ETHIOPIA".as_slice()));
        assert_eq!(d.nouns.max_weight(), 340);
        assert_eq!(d.prepositions.max_weight(), 456);
    }
}
