//! Helpers of the table generators (`build_support.c`, `permute.c` and the string
//! generators of `genrand.c`).

use super::context::Context;
use super::dist::Dist;
use super::genrand::{ALPHANUM, DIST_UNIFORM, Streams};

/// `mk_word`: a gibberish word made of the syllables of `syllables` that spell `src`, at
/// most `char_count` characters long.
pub(crate) fn mk_word(dest: &mut String, syllables: &Dist, src: i64, char_count: usize) {
    dest.clear();
    let mut i = src;
    while i > 0 {
        let count = syllables.size() as i64;
        let syllable = syllables.member_str((i % count) as i32 + 1, 1);
        i /= count;
        if dest.len() + syllable.len() <= char_count {
            dest.push_str(syllable);
        } else {
            break;
        }
    }
}

/// `szXlate`.
const XLATE: [u8; 16] = *b"ABCDEFGHIJKLMNOP";

/// `ltoc`: eight characters for the low 32 bits of `value`, low nibble first.
fn ltoc(dest: &mut [u8], mut value: u64) {
    for slot in dest.iter_mut().take(8) {
        *slot = XLATE[(value & 0xF) as usize];
        value >>= 4;
    }
}

/// `mk_bkey`: the 16 character business key of `primary`.
pub(crate) fn mk_bkey(primary: i64) -> [u8; 16] {
    let mut key = [0; 16];
    ltoc(&mut key[..8], (primary >> 32) as u64);
    ltoc(&mut key[8..], (primary & 0xFFFF_FFFF) as u64);
    key
}

/// A business key as a string.
pub(crate) fn bkey_str(key: &[u8; 16]) -> &str {
    // The key is made of `XLATE`'s letters.
    std::str::from_utf8(key).unwrap_or_default()
}

/// `embed_string`: overwrites a random part of `dest` with a word picked from `dist`.
pub(crate) fn embed_string(
    dest: &mut String,
    dist: &Dist,
    value: usize,
    weight: usize,
    rng: &mut Streams,
    stream: usize,
) {
    let (word, _) = dist.pick_str(rng, value, weight, stream);
    // `strlen(szDest) - strlen(szWord) - 1`, in `size_t` and then `int`.
    let max = (dest.len().wrapping_sub(word.len()).wrapping_sub(1)) as i32;
    let position = rng.genrand_integer(DIST_UNIFORM, 0, max, 0, stream) as usize;
    let mut bytes = std::mem::take(dest).into_bytes();
    let end = position + word.len();
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[position..end].copy_from_slice(word.as_bytes());
    // The words and the strings they are embedded in are ASCII.
    *dest = String::from_utf8(bytes).unwrap_or_default();
}

/// `bitmap_to_dist`: the value of `dist`'s row selected by the low part of `modulus`,
/// which keeps the rest.
pub(crate) fn bitmap_to_dist_str(dist: &Dist, modulus: &mut i64, vset: usize) -> &'static str {
    let size = dist.size() as i64;
    let row = ((*modulus % size) + 1) as i32;
    *modulus /= size;
    dist.member_str(row, vset)
}

/// `bitmap_to_dist` into an `int`.
pub(crate) fn bitmap_to_dist_int(dist: &Dist, modulus: &mut i64, vset: usize) -> i32 {
    let size = dist.size() as i64;
    let row = ((*modulus % size) + 1) as i32;
    *modulus /= size;
    dist.member_int(row, vset)
}

/// `makePermutation`: the integers `0..size` shuffled.
pub(crate) fn make_permutation(rng: &mut Streams, size: i32, stream: usize) -> Vec<i32> {
    if size <= 0 {
        return Vec::new();
    }
    let mut set: Vec<i32> = (0..size).collect();
    for i in 0..size as usize {
        let index = rng.genrand_integer(DIST_UNIFORM, 0, size - 1, 0, stream) as usize;
        set.swap(i, index);
    }
    set
}

/// `getPermutationEntry`: the 1-based entry `index` of a permutation of 1-based values.
pub(crate) fn get_permutation_entry(permutation: &[i32], index: i32) -> i32 {
    permutation[(index - 1) as usize] + 1
}

impl Context {
    /// `genrand_email`: `first.last@company.domain`.
    pub(crate) fn genrand_email(&mut self, dest: &mut String, first: &str, last: &str, column: usize) {
        let (domain, _) = self.dists.top_domains.pick_str(&mut self.rng, 1, 1, column);
        let company_length = self.rng.genrand_integer(DIST_UNIFORM, 10, 20, 0, column);
        let mut company = String::new();
        self.rng.gen_charset(&mut company, ALPHANUM, 1, 20, column);
        // `szCompany[nCompanyLength] = '\0'` ends a longer name.
        company.truncate(company_length as usize);
        dest.clear();
        dest.push_str(first);
        dest.push('.');
        dest.push_str(last);
        dest.push('@');
        dest.push_str(&company);
        dest.push('.');
        dest.push_str(domain);
    }
}
