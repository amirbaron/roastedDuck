//! Random text (`text.c`): sentences built from a grammar of the `sentences` distribution.

use super::context::Context;
use super::genrand::DIST_UNIFORM;

impl Context {
    /// `mk_sentence`: appends a sentence to `dest`.
    fn mk_sentence(&mut self, dest: &mut String, stream: usize) {
        let dists = &self.dists;
        let rng = &mut self.rng;
        let (syntax, _) = dists.sentences.pick_str(rng, 1, 1, stream);
        for c in syntax.chars() {
            let word = match c {
                'N' => dists.nouns,
                'V' => dists.verbs,
                'J' => dists.adjectives,
                'D' => dists.adverbs,
                'X' => dists.auxiliaries,
                'P' => dists.prepositions,
                'A' => dists.articles,
                'T' => dists.terminators,
                _ => {
                    dest.push(c);
                    continue;
                }
            };
            dest.push_str(word.pick_str(rng, 1, 1, stream).0);
        }
    }

    /// `gen_text`: text of a random length in `[min, max]`, made of sentences, the last one
    /// cut at that length.
    pub(crate) fn gen_text(&mut self, dest: &mut String, min: i32, max: i32, stream: usize) {
        let mut target_len = self.rng.genrand_integer(DIST_UNIFORM, min, max, 0, stream);
        dest.clear();
        let mut capitalize = true;
        let mut sentence = String::new();
        while target_len > 0 {
            sentence.clear();
            self.mk_sentence(&mut sentence, stream);
            let generated = sentence.len() as i32;
            if capitalize {
                // `toupper` of the first character.
                if let Some(first) = sentence.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
            }
            capitalize = sentence.ends_with('.');
            if target_len <= generated {
                dest.push_str(&sentence[..target_len as usize]);
            } else {
                dest.push_str(&sentence);
            }
            target_len -= generated;
            if target_len > 0 {
                dest.push(' ');
                target_len -= 1;
            }
        }
    }
}
