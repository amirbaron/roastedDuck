//! Ports of ICU's `collationiterator.h` / `collationiterator.cpp` (`CollationIterator`,
//! forward iteration) and `utf16collationiterator.h` / `utf16collationiterator.cpp`
//! (`UTF16CollationIterator`, `FCDUTF16CollationIterator`): the collation elements of a
//! UTF-16 string, as the sort key writer reads them.
//!
//! The C++ subclasses override the text access of `CollationIterator`; here the text is a
//! [`CollationText`] the iterator is generic over.

use super::collation::{self as coll, FALLBACK_CE32, NO_CE, NO_CE32, SPECIAL_CE32_LOW_BYTE};
use super::collation_data::CollationData;
use super::collation_fcd;
use super::normalizer2impl::{Normalizer2Impl, is_lead, is_trail, supplementary};
use super::ucharstrie::{UCharsTrie, UCharsTrieState, UStringTrieResult};

/// `U_SENTINEL`.
const SENTINEL: i32 = -1;

/// The text access a `CollationIterator` subclass provides.
pub trait CollationText {
    /// `handleNextCE32(c)`: the next code unit (or `SENTINEL` at the end) and its CE32 in
    /// the data's trie; `FALLBACK_CE32` at the end.
    fn handle_next_ce32(&mut self, data: &CollationData) -> (i32, u32);
    /// `handleGetTrailSurrogate()`: the trail surrogate after a lead surrogate (consumed),
    /// or any other code unit (not consumed), or 0 at the end.
    fn handle_get_trail_surrogate(&mut self) -> u16;
    /// `nextCodePoint()`.
    fn next_code_point(&mut self) -> i32;
    /// `previousCodePoint()`.
    fn previous_code_point(&mut self) -> i32;
    /// `forwardNumCodePoints(num)`.
    fn forward_num_code_points(&mut self, num: i32);
    /// `backwardNumCodePoints(num)`.
    fn backward_num_code_points(&mut self, num: i32);
}

/// `UTF16CollationIterator`: text in FCD (or with normalization off).
#[derive(Debug)]
pub struct Utf16Text<'s> {
    s: &'s [u16],
    pos: usize,
}

impl<'s> Utf16Text<'s> {
    /// The text `s` from its start.
    pub fn new(s: &'s [u16]) -> Self {
        Self { s, pos: 0 }
    }
}

impl CollationText for Utf16Text<'_> {
    #[inline]
    fn handle_next_ce32(&mut self, data: &CollationData) -> (i32, u32) {
        let Some(&c) = self.s.get(self.pos) else {
            return (SENTINEL, FALLBACK_CE32);
        };
        self.pos += 1;
        (i32::from(c), data.trie.get32_from_u16_single_lead(c))
    }

    fn handle_get_trail_surrogate(&mut self) -> u16 {
        let Some(&trail) = self.s.get(self.pos) else {
            return 0;
        };
        if is_trail(trail) {
            self.pos += 1;
        }
        trail
    }

    fn next_code_point(&mut self) -> i32 {
        let Some(&c) = self.s.get(self.pos) else {
            return SENTINEL;
        };
        self.pos += 1;
        if is_lead(c)
            && let Some(&trail) = self.s.get(self.pos)
            && is_trail(trail)
        {
            self.pos += 1;
            return supplementary(c, trail);
        }
        i32::from(c)
    }

    fn previous_code_point(&mut self) -> i32 {
        if self.pos == 0 {
            return SENTINEL;
        }
        self.pos -= 1;
        let c = self.s[self.pos];
        if is_trail(c) && self.pos != 0 && is_lead(self.s[self.pos - 1]) {
            self.pos -= 1;
            return supplementary(self.s[self.pos], c);
        }
        i32::from(c)
    }

    fn forward_num_code_points(&mut self, mut num: i32) {
        while num > 0 && self.pos != self.s.len() {
            let c = self.s[self.pos];
            self.pos += 1;
            num -= 1;
            if is_lead(c) && self.pos != self.s.len() && is_trail(self.s[self.pos]) {
                self.pos += 1;
            }
        }
    }

    fn backward_num_code_points(&mut self, mut num: i32) {
        while num > 0 && self.pos != 0 {
            self.pos -= 1;
            let c = self.s[self.pos];
            num -= 1;
            if is_trail(c) && self.pos != 0 && is_lead(self.s[self.pos - 1]) {
                self.pos -= 1;
            }
        }
    }
}

/// `FCDUTF16CollationIterator`: checks the text for FCD incrementally and normalizes
/// segments that fail the check.
///
/// The positions `start`, `pos` and `limit` index the raw text, or the normalized segment
/// when `in_normalized` (C++ points them into either buffer); `segment_start` and
/// `segment_limit` always index the raw text.
///
/// ICU's iterator never ends on some texts: moving back over code points out of a
/// normalized segment counts the raw text's code points, which differ from the normalized
/// ones, so a discontiguous contraction replays forever (`as`, `bn`, `hi`, ... on
/// U+0627 U+0357 U+101FD U+0344; DuckDB hangs, or crashes once it runs out of memory).
/// The iterator counts its checked steps and gives up past a budget quadratic in the
/// length of the text ([`FcdUtf16Text::exhausted`]); the caller then collates the NFD of
/// the text, which is what the FCD check stands for.
#[derive(Debug)]
pub struct FcdUtf16Text<'s> {
    raw: &'s [u16],
    normalized: Vec<u16>,
    in_normalized: bool,
    start: usize,
    pos: usize,
    limit: usize,
    segment_start: usize,
    segment_limit: usize,
    nfc_impl: &'static Normalizer2Impl,
    /// Direction of the incremental FCD check.
    check_dir: i8,
    /// The steps left before the iterator gives up.
    budget: u64,
}

impl<'s> FcdUtf16Text<'s> {
    /// The text `s` from its start.
    pub fn new(s: &'s [u16], nfc_impl: &'static Normalizer2Impl) -> Self {
        Self {
            raw: s,
            normalized: Vec::new(),
            in_normalized: false,
            start: 0,
            pos: 0,
            limit: s.len(),
            segment_start: 0,
            segment_limit: 0,
            nfc_impl,
            check_dir: 1,
            budget: 1_000_000 + 32 * (s.len() as u64 + 1).pow(2),
        }
    }

    /// Whether the iterator gave up on the text (see the type's documentation).
    pub fn exhausted(&self) -> bool {
        self.budget == 0
    }

    /// Counts a step of the checked paths; `false` once the budget is spent.
    #[inline]
    fn step(&mut self) -> bool {
        if self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        true
    }

    /// The code unit at `i` of the current text.
    #[inline]
    fn unit(&self, i: usize) -> u16 {
        if self.in_normalized {
            self.normalized[i]
        } else {
            self.raw[i]
        }
    }

    /// `start == segmentStart` of the C++ (both pointers into the raw text).
    #[inline]
    fn start_is_segment_start(&self) -> bool {
        !self.in_normalized && self.start == self.segment_start
    }

    /// The next code unit in forward checking, normalizing the segment around it when
    /// needed; `None` at the end.
    #[inline]
    fn next_unit(&mut self) -> Option<u16> {
        // Fast path: forward checking (in the raw text) at a unit without tccc.
        if self.check_dir > 0
            && let Some(&c) = self.raw[..self.limit].get(self.pos)
            && !collation_fcd::has_tccc(i32::from(c))
        {
            self.pos += 1;
            return Some(c);
        }
        self.next_unit_checked()
    }

    /// The general case of [`Self::next_unit`].
    #[inline(never)]
    fn next_unit_checked(&mut self) -> Option<u16> {
        if !self.step() {
            return None;
        }
        loop {
            if self.check_dir > 0 {
                if self.pos == self.limit {
                    return None;
                }
                let mut c = self.unit(self.pos);
                self.pos += 1;
                if collation_fcd::has_tccc(i32::from(c))
                    && (collation_fcd::maybe_tibetan_composite_vowel(i32::from(c))
                        || (self.pos != self.limit
                            && collation_fcd::has_lccc(i32::from(self.unit(self.pos)))))
                {
                    self.pos -= 1;
                    self.next_segment();
                    c = self.unit(self.pos);
                    self.pos += 1;
                }
                return Some(c);
            } else if self.check_dir == 0 && self.pos != self.limit {
                let c = self.unit(self.pos);
                self.pos += 1;
                return Some(c);
            }
            self.switch_to_forward();
        }
    }

    /// `switchToForward()`.
    #[cold]
    fn switch_to_forward(&mut self) {
        if self.check_dir < 0 {
            // Turn around from backward checking.
            self.start = self.pos;
            self.segment_start = self.pos;
            if self.pos == self.segment_limit {
                self.limit = self.raw.len();
                self.check_dir = 1; // Check forward.
            } else {
                self.check_dir = 0; // Stay in FCD segment.
            }
        } else {
            // Reached the end of the FCD segment.
            if !self.start_is_segment_start() {
                // The input text segment needed to be normalized.
                // Switch to checking forward from it.
                self.in_normalized = false;
                self.pos = self.segment_limit;
                self.start = self.segment_limit;
                self.segment_start = self.segment_limit;
            }
            self.limit = self.raw.len();
            self.check_dir = 1;
        }
    }

    /// `nextSegment()`: extends the FCD text segment forward or normalizes around `pos`.
    #[cold]
    fn next_segment(&mut self) {
        // The input text [segmentStart..pos[ passes the FCD check.
        let raw_limit = self.raw.len();
        let mut p = self.pos;
        let mut prev_cc = 0u8;
        loop {
            // Fetch the next character's fcd16 value.
            let mut q = p;
            let fcd16 = self.nfc_impl.next_fcd16(self.raw, &mut p, raw_limit);
            let lead_cc = (fcd16 >> 8) as u8;
            if lead_cc == 0 && q != self.pos {
                // FCD boundary before the [q, p[ character.
                self.limit = q;
                self.segment_limit = q;
                break;
            }
            if lead_cc != 0
                && (prev_cc > lead_cc || collation_fcd::is_fcd16_of_tibetan_composite_vowel(fcd16))
            {
                // Fails FCD check. Find the next FCD boundary and normalize.
                loop {
                    q = p;
                    if p == raw_limit || self.nfc_impl.next_fcd16(self.raw, &mut p, raw_limit) <= 0xff {
                        break;
                    }
                }
                self.normalize(self.pos, q);
                self.pos = self.start;
                break;
            }
            prev_cc = fcd16 as u8;
            if p == raw_limit || prev_cc == 0 {
                // FCD boundary after the last character.
                self.limit = p;
                self.segment_limit = p;
                break;
            }
        }
        self.check_dir = 0;
    }

    /// `switchToBackward()`.
    fn switch_to_backward(&mut self) {
        if self.check_dir > 0 {
            // Turn around from forward checking.
            self.limit = self.pos;
            self.segment_limit = self.pos;
            if self.pos == self.segment_start {
                self.start = 0;
                self.check_dir = -1; // Check backward.
            } else {
                self.check_dir = 0; // Stay in FCD segment.
            }
        } else {
            // Reached the start of the FCD segment.
            if !self.start_is_segment_start() {
                // The input text segment needed to be normalized.
                // Switch to checking backward from it.
                self.in_normalized = false;
                self.pos = self.segment_start;
                self.limit = self.segment_start;
                self.segment_limit = self.segment_start;
            }
            self.start = 0;
            self.check_dir = -1;
        }
    }

    /// `previousSegment()`: extends the FCD text segment backward or normalizes around `pos`.
    fn previous_segment(&mut self) {
        // The input text [pos..segmentLimit[ passes the FCD check.
        let mut p = self.pos;
        let mut next_cc = 0u8;
        loop {
            // Fetch the previous character's fcd16 value.
            let mut q = p;
            let mut fcd16 = self.nfc_impl.previous_fcd16(self.raw, 0, &mut p);
            let trail_cc = fcd16 as u8;
            if trail_cc == 0 && q != self.pos {
                // FCD boundary after the [p, q[ character.
                self.start = q;
                self.segment_start = q;
                break;
            }
            if trail_cc != 0
                && ((next_cc != 0 && trail_cc > next_cc)
                    || collation_fcd::is_fcd16_of_tibetan_composite_vowel(fcd16))
            {
                // Fails FCD check. Find the previous FCD boundary and normalize.
                loop {
                    q = p;
                    if fcd16 <= 0xff || p == 0 {
                        break;
                    }
                    fcd16 = self.nfc_impl.previous_fcd16(self.raw, 0, &mut p);
                    if fcd16 == 0 {
                        break;
                    }
                }
                self.normalize(q, self.pos);
                self.pos = self.limit;
                break;
            }
            next_cc = (fcd16 >> 8) as u8;
            if p == 0 || next_cc == 0 {
                // FCD boundary before the following character.
                self.start = p;
                self.segment_start = p;
                break;
            }
        }
        self.check_dir = 0;
    }

    /// `normalize(from, to)`: switches to the NFD of the raw text `[from, to[`.
    fn normalize(&mut self, from: usize, to: usize) {
        self.nfc_impl.decompose(&self.raw[from..to], &mut self.normalized);
        self.segment_start = from;
        self.segment_limit = to;
        self.in_normalized = true;
        self.start = 0;
        self.limit = self.normalized.len();
    }
}

impl CollationText for FcdUtf16Text<'_> {
    #[inline]
    fn handle_next_ce32(&mut self, data: &CollationData) -> (i32, u32) {
        match self.next_unit() {
            Some(c) => (i32::from(c), data.trie.get32_from_u16_single_lead(c)),
            None => (SENTINEL, FALLBACK_CE32),
        }
    }

    fn handle_get_trail_surrogate(&mut self) -> u16 {
        if self.pos == self.limit {
            return 0;
        }
        let trail = self.unit(self.pos);
        if is_trail(trail) {
            self.pos += 1;
        }
        trail
    }

    fn next_code_point(&mut self) -> i32 {
        let Some(c) = self.next_unit() else {
            return SENTINEL;
        };
        if is_lead(c) && self.pos != self.limit {
            let trail = self.unit(self.pos);
            if is_trail(trail) {
                self.pos += 1;
                return supplementary(c, trail);
            }
        }
        i32::from(c)
    }

    fn previous_code_point(&mut self) -> i32 {
        if !self.step() {
            return SENTINEL;
        }
        let c = loop {
            if self.check_dir < 0 {
                if self.pos == self.start {
                    return SENTINEL;
                }
                self.pos -= 1;
                let mut c = self.unit(self.pos);
                if collation_fcd::has_lccc(i32::from(c))
                    && (collation_fcd::maybe_tibetan_composite_vowel(i32::from(c))
                        || (self.pos != self.start
                            && collation_fcd::has_tccc(i32::from(self.unit(self.pos - 1)))))
                {
                    self.pos += 1;
                    self.previous_segment();
                    self.pos -= 1;
                    c = self.unit(self.pos);
                }
                break c;
            } else if self.check_dir == 0 && self.pos != self.start {
                self.pos -= 1;
                break self.unit(self.pos);
            }
            self.switch_to_backward();
        };
        if is_trail(c) && self.pos != self.start {
            let lead = self.unit(self.pos - 1);
            if is_lead(lead) {
                self.pos -= 1;
                return supplementary(lead, c);
            }
        }
        i32::from(c)
    }

    fn forward_num_code_points(&mut self, mut num: i32) {
        while num > 0 && self.next_code_point() >= 0 {
            num -= 1;
        }
    }

    fn backward_num_code_points(&mut self, mut num: i32) {
        while num > 0 && self.previous_code_point() >= 0 {
            num -= 1;
        }
    }
}

/// The state of combining marks skipped in discontiguous contraction (`SkippedState`).
#[derive(Debug, Default)]
struct SkippedState {
    /// Combining marks skipped in previous discontiguous-contraction matching.
    old_buffer: Vec<u16>,
    /// Combining marks newly skipped in current discontiguous-contraction matching.
    new_buffer: Vec<u16>,
    /// Reading index in `old_buffer`, or counter for how many code points have been read
    /// beyond it (`pos - old_buffer.len()`).
    pos: i32,
    /// `new_buffer.len()` at the time of the last matching character.
    skip_length_at_match: usize,
    /// The trie state before the attempt to match a character.
    state: UCharsTrieState,
}

impl SkippedState {
    fn clear(&mut self) {
        self.old_buffer.clear();
        self.pos = 0;
    }

    fn is_empty(&self) -> bool {
        self.old_buffer.is_empty()
    }

    fn has_next(&self) -> bool {
        (self.pos as usize) < self.old_buffer.len()
    }

    /// Requires `has_next()`.
    fn next(&mut self) -> i32 {
        let c = char32_at(&self.old_buffer, self.pos as usize);
        self.pos += if c > 0xffff { 2 } else { 1 };
        c
    }

    fn inc_beyond(&mut self) {
        self.pos += 1;
    }

    /// Goes backward through the skipped-marks buffer; the number of code points read
    /// beyond the skipped marks that need to be backtracked through normal input.
    fn backward_num_code_points(&mut self, n: i32) -> i32 {
        let length = self.old_buffer.len() as i32;
        let beyond = self.pos - length;
        if beyond > 0 {
            if beyond >= n {
                // Not back far enough to re-enter the oldBuffer.
                self.pos -= n;
                n
            } else {
                // Back out all beyond-oldBuffer code points and re-enter the buffer.
                self.pos = move_index32(&self.old_buffer, length, beyond - n);
                beyond
            }
        } else {
            // Go backwards from inside the oldBuffer.
            self.pos = move_index32(&self.old_buffer, self.pos, -n);
            0
        }
    }

    fn set_first_skipped(&mut self, c: i32) {
        self.skip_length_at_match = 0;
        self.new_buffer.clear();
        push_code_point(&mut self.new_buffer, c);
    }

    fn skip(&mut self, c: i32) {
        push_code_point(&mut self.new_buffer, c);
    }

    fn record_match(&mut self) {
        self.skip_length_at_match = self.new_buffer.len();
    }

    /// Replaces the characters we consumed with the newly skipped ones.
    fn replace_match(&mut self) {
        // UnicodeString.replace() pins pos to at most length().
        let consumed = (self.pos.max(0) as usize).min(self.old_buffer.len());
        self.old_buffer.splice(
            ..consumed,
            self.new_buffer[..self.skip_length_at_match].iter().copied(),
        );
        self.pos = 0;
    }
}

/// `UnicodeString::char32At(offset)`.
fn char32_at(s: &[u16], offset: usize) -> i32 {
    let c = s[offset];
    if is_lead(c) && offset + 1 < s.len() && is_trail(s[offset + 1]) {
        supplementary(c, s[offset + 1])
    } else if is_trail(c) && offset > 0 && is_lead(s[offset - 1]) {
        supplementary(s[offset - 1], c)
    } else {
        i32::from(c)
    }
}

/// `UnicodeString::moveIndex32(index, delta)`.
fn move_index32(s: &[u16], index: i32, delta: i32) -> i32 {
    let mut index = index.clamp(0, s.len() as i32) as usize;
    if delta > 0 {
        for _ in 0..delta {
            if index >= s.len() {
                break;
            }
            index += 1;
            if is_lead(s[index - 1]) && index < s.len() && is_trail(s[index]) {
                index += 1;
            }
        }
    } else {
        for _ in 0..-delta {
            if index == 0 {
                break;
            }
            index -= 1;
            if is_trail(s[index]) && index > 0 && is_lead(s[index - 1]) {
                index -= 1;
            }
        }
    }
    index as i32
}

/// Appends a code point as UTF-16.
fn push_code_point(s: &mut Vec<u16>, c: i32) {
    if c > 0xffff {
        s.push(((c >> 10) + 0xd7c0) as u16);
        s.push(((c & 0x3ff) | 0xdc00) as u16);
    } else {
        s.push(c as u16);
    }
}

/// `CollationIterator`: the collation elements of a text.
#[derive(Debug)]
pub struct CollationIterator<'d, T> {
    data: &'d CollationData,
    text: T,
    ce_buffer: Vec<i64>,
    ces_index: usize,
    skipped: Option<SkippedState>,
    /// Number of code points to read forward, or -1.
    num_cp_fwd: i32,
    /// Numeric collation (`CollationSettings::NUMERIC`).
    is_numeric: bool,
}

impl<'d, T: CollationText> CollationIterator<'d, T> {
    /// `CollationIterator(d, numeric)` over `text`, with the (cleared) buffer of a previous
    /// iterator for its CEs.
    pub fn new(data: &'d CollationData, numeric: bool, text: T, mut ce_buffer: Vec<i64>) -> Self {
        ce_buffer.clear();
        Self {
            data,
            text,
            ce_buffer,
            ces_index: 0,
            skipped: None,
            num_cp_fwd: -1,
            is_numeric: numeric,
        }
    }

    /// The buffer of the CEs, for the next iterator.
    pub fn into_ce_buffer(self) -> Vec<i64> {
        self.ce_buffer
    }

    /// The text.
    pub fn text(&self) -> &T {
        &self.text
    }

    /// `clearCEsIfNoneRemaining()`.
    #[inline]
    pub fn clear_ces_if_none_remaining(&mut self) {
        if self.ces_index == self.ce_buffer.len() {
            self.ces_index = 0;
            self.ce_buffer.clear();
        }
    }

    /// `nextCE()`: the next collation element.
    #[inline]
    pub fn next_ce(&mut self) -> i64 {
        if self.ces_index < self.ce_buffer.len() {
            // Return the next buffered CE.
            let ce = self.ce_buffer[self.ces_index];
            self.ces_index += 1;
            return ce;
        }
        let (c, mut ce32) = self.text.handle_next_ce32(self.data);
        let mut t = ce32 & 0xff;
        if t < SPECIAL_CE32_LOW_BYTE {
            // Normal CE from the main data.
            return self.push_returned(coll::ce_from_simple_ce32(ce32));
        }
        let d;
        if t == SPECIAL_CE32_LOW_BYTE {
            if c < 0 {
                return self.push_returned(NO_CE);
            }
            let Some(base) = self.data.base else {
                return self.push_returned(NO_CE);
            };
            d = base;
            ce32 = d.get_ce32(c);
            t = ce32 & 0xff;
            if t < SPECIAL_CE32_LOW_BYTE {
                // Normal CE from the base data.
                return self.push_returned(coll::ce_from_simple_ce32(ce32));
            }
        } else {
            d = self.data;
        }
        if t == coll::LONG_PRIMARY_CE32_LOW_BYTE {
            return self.push_returned((i64::from(ce32 - t) << 32) | i64::from(coll::COMMON_SEC_AND_TER_CE));
        }
        if t == SPECIAL_CE32_LOW_BYTE | coll::DIGIT_TAG && !self.is_numeric {
            // A digit (in non-numeric collation) with a normal CE: the shortcut through
            // `appendCEsFromCE32` to its non-numeric CE32.
            let digit_ce32 = d.ce32s[coll::index_from_ce32(ce32)];
            if !coll::is_special_ce32(digit_ce32) {
                return self.push_returned(coll::ce_from_simple_ce32(digit_ce32));
            }
        }
        // `nextCEFromCE32`
        self.append_ces_from_ce32(d, c, ce32, true);
        let ce = self.ce_buffer.get(self.ces_index).copied().unwrap_or(NO_CE);
        self.ces_index += 1;
        ce
    }

    /// Appends `ce` to the buffer and returns it as the next CE.
    #[inline]
    fn push_returned(&mut self, ce: i64) -> i64 {
        self.ce_buffer.push(ce);
        self.ces_index += 1;
        ce
    }

    /// `appendCEsFromCE32(d, c, ce32, forward)`.
    fn append_ces_from_ce32(&mut self, mut d: &'d CollationData, mut c: i32, mut ce32: u32, forward: bool) {
        while coll::is_special_ce32(ce32) {
            match coll::tag_from_ce32(ce32) {
                coll::FALLBACK_TAG | coll::RESERVED_TAG_3 | coll::BUILDER_DATA_TAG => {
                    // U_INTERNAL_PROGRAM_ERROR: never in runtime data
                    self.ce_buffer.push(NO_CE);
                    return;
                }
                coll::LONG_PRIMARY_TAG => {
                    self.ce_buffer.push(coll::ce_from_long_primary_ce32(ce32));
                    return;
                }
                coll::LONG_SECONDARY_TAG => {
                    self.ce_buffer.push(coll::ce_from_long_secondary_ce32(ce32));
                    return;
                }
                coll::LATIN_EXPANSION_TAG => {
                    self.ce_buffer.push(coll::latin_ce0_from_ce32(ce32));
                    self.ce_buffer.push(coll::latin_ce1_from_ce32(ce32));
                    return;
                }
                coll::EXPANSION32_TAG => {
                    let index = coll::index_from_ce32(ce32);
                    let length = coll::length_from_ce32(ce32);
                    for &ce32 in &d.ce32s[index..index + length] {
                        self.ce_buffer.push(coll::ce_from_ce32(ce32));
                    }
                    return;
                }
                coll::EXPANSION_TAG => {
                    let index = coll::index_from_ce32(ce32);
                    let length = coll::length_from_ce32(ce32);
                    self.ce_buffer.extend_from_slice(&d.ces[index..index + length]);
                    return;
                }
                coll::PREFIX_TAG => {
                    if forward {
                        self.text.backward_num_code_points(1);
                    }
                    ce32 = self.get_ce32_from_prefix(d, ce32);
                    if forward {
                        self.text.forward_num_code_points(1);
                    }
                }
                coll::CONTRACTION_TAG => {
                    let p = coll::index_from_ce32(ce32);
                    let default_ce32 = d.read_ce32(p); // Default if no suffix match.
                    if !forward {
                        ce32 = default_ce32;
                        continue;
                    }
                    let next_cp;
                    if self.skipped.is_none() && self.num_cp_fwd < 0 {
                        next_cp = self.text.next_code_point();
                        if next_cp < 0 {
                            // No more text.
                            ce32 = default_ce32;
                            continue;
                        } else if (ce32 & coll::CONTRACT_NEXT_CCC) != 0
                            && !collation_fcd::may_have_lccc(next_cp)
                        {
                            // All contraction suffixes start with characters with lccc!=0
                            // but the next code point has lccc==0.
                            self.text.backward_num_code_points(1);
                            ce32 = default_ce32;
                            continue;
                        }
                    } else {
                        next_cp = self.next_skipped_code_point();
                        if next_cp < 0 {
                            // No more text.
                            ce32 = default_ce32;
                            continue;
                        } else if (ce32 & coll::CONTRACT_NEXT_CCC) != 0
                            && !collation_fcd::may_have_lccc(next_cp)
                        {
                            self.backward_num_skipped(1);
                            ce32 = default_ce32;
                            continue;
                        }
                    }
                    ce32 = self.next_ce32_from_contraction(d, ce32, p + 2, default_ce32, next_cp);
                    if ce32 == NO_CE32 {
                        // CEs from a discontiguous contraction plus the skipped combining
                        // marks have been appended already.
                        return;
                    }
                }
                coll::DIGIT_TAG => {
                    if self.is_numeric {
                        self.append_numeric_ces(ce32);
                        return;
                    }
                    // Fetch the non-numeric-collation CE32 and continue.
                    ce32 = d.ce32s[coll::index_from_ce32(ce32)];
                }
                coll::U0000_TAG => {
                    // The text has a length, so U+0000 is not a terminator:
                    // fetch the normal ce32 for U+0000 and continue.
                    ce32 = d.ce32s[0];
                }
                coll::HANGUL_TAG => {
                    let jamo_ce32s = &d.jamo_ce32s;
                    c -= coll::HANGUL_BASE;
                    let t = c % coll::JAMO_T_COUNT;
                    c /= coll::JAMO_T_COUNT;
                    let v = c % coll::JAMO_V_COUNT;
                    c /= coll::JAMO_V_COUNT;
                    if (ce32 & coll::HANGUL_NO_SPECIAL_JAMO) != 0 {
                        // None of the Jamo CE32s are isSpecialCE32().
                        self.ce_buffer.push(coll::ce_from_ce32(jamo_ce32s[c as usize]));
                        self.ce_buffer
                            .push(coll::ce_from_ce32(jamo_ce32s[(19 + v) as usize]));
                        if t != 0 {
                            self.ce_buffer
                                .push(coll::ce_from_ce32(jamo_ce32s[(39 + t) as usize]));
                        }
                        return;
                    }
                    // We should not need to compute each Jamo code point.
                    // In particular, there should be no offset or implicit ce32.
                    let (l_ce32, v_ce32) = (jamo_ce32s[c as usize], jamo_ce32s[(19 + v) as usize]);
                    self.append_ces_from_ce32(d, SENTINEL, l_ce32, forward);
                    self.append_ces_from_ce32(d, SENTINEL, v_ce32, forward);
                    if t == 0 {
                        return;
                    }
                    // offset 39 = 19 + 21 - 1
                    ce32 = d.jamo_ce32s[(39 + t) as usize];
                    c = SENTINEL;
                }
                coll::LEAD_SURROGATE_TAG => {
                    let trail = self.text.handle_get_trail_surrogate();
                    if is_trail(trail) {
                        c = supplementary(c as u16, trail);
                        ce32 &= coll::LEAD_TYPE_MASK;
                        if ce32 == coll::LEAD_ALL_UNASSIGNED {
                            ce32 = coll::UNASSIGNED_CE32; // unassigned-implicit
                        } else if ce32 == coll::LEAD_ALL_FALLBACK || {
                            ce32 = d.get_ce32_from_supplementary(c);
                            ce32 == FALLBACK_CE32
                        } {
                            // fall back to the base data
                            if let Some(base) = d.base {
                                d = base;
                            }
                            ce32 = d.get_ce32_from_supplementary(c);
                        }
                    } else {
                        // c is an unpaired surrogate.
                        ce32 = coll::UNASSIGNED_CE32;
                    }
                }
                coll::OFFSET_TAG => {
                    self.ce_buffer.push(d.get_ce_from_offset_ce32(c, ce32));
                    return;
                }
                _ => {
                    // IMPLICIT_TAG; surrogate code points map to their own implicit
                    // primary weights in UTF-16 text
                    self.ce_buffer.push(coll::unassigned_ce_from_code_point(c));
                    return;
                }
            }
        }
        self.ce_buffer.push(coll::ce_from_simple_ce32(ce32));
    }

    /// `getCE32FromPrefix(d, ce32)`.
    fn get_ce32_from_prefix(&mut self, d: &CollationData, ce32: u32) -> u32 {
        let p = coll::index_from_ce32(ce32);
        let mut ce32 = d.read_ce32(p); // Default if no prefix match.
        // Number of code points read before the original code point.
        let mut look_behind = 0;
        let mut prefixes = UCharsTrie::new(&d.contexts[p + 2..]);
        loop {
            let c = self.text.previous_code_point();
            if c < 0 {
                break;
            }
            look_behind += 1;
            let result = prefixes.next_for_code_point(c);
            if result.has_value() {
                ce32 = prefixes.get_value() as u32;
            }
            if !result.has_next() {
                break;
            }
        }
        self.text.forward_num_code_points(look_behind);
        ce32
    }

    /// `nextSkippedCodePoint()`.
    fn next_skipped_code_point(&mut self) -> i32 {
        if let Some(skipped) = &mut self.skipped
            && skipped.has_next()
        {
            return skipped.next();
        }
        if self.num_cp_fwd == 0 {
            return SENTINEL;
        }
        let c = self.text.next_code_point();
        if let Some(skipped) = &mut self.skipped
            && !skipped.is_empty()
            && c >= 0
        {
            skipped.inc_beyond();
        }
        if self.num_cp_fwd > 0 && c >= 0 {
            self.num_cp_fwd -= 1;
        }
        c
    }

    /// `backwardNumSkipped(n)`.
    fn backward_num_skipped(&mut self, mut n: i32) {
        if let Some(skipped) = &mut self.skipped
            && !skipped.is_empty()
        {
            n = skipped.backward_num_code_points(n);
        }
        self.text.backward_num_code_points(n);
        if self.num_cp_fwd >= 0 {
            self.num_cp_fwd += n;
        }
    }

    /// Whether there are skipped marks being re-read (`skipped != NULL && !skipped->isEmpty()`).
    fn has_skipped(&self) -> bool {
        self.skipped.as_ref().is_some_and(|skipped| !skipped.is_empty())
    }

    /// `nextCE32FromContraction(d, contractionCE32, p, ce32, c)`.
    fn next_ce32_from_contraction(
        &mut self,
        d: &'d CollationData,
        contraction_ce32: u32,
        p: usize,
        mut ce32: u32,
        mut c: i32,
    ) -> u32 {
        // c: next code point after the original one

        // Number of code points read beyond the original code point.
        let mut look_ahead = 1;
        // Number of code points read since the last match (initially only c).
        let mut since_match = 1;
        let mut suffixes = UCharsTrie::new(&d.contexts[p..]);
        if self.has_skipped()
            && let Some(skipped) = &mut self.skipped
        {
            skipped.state = suffixes.save_state();
        }
        let mut result = suffixes.first_for_code_point(c);
        loop {
            if result.has_value() {
                ce32 = suffixes.get_value() as u32;
                if !result.has_next() {
                    return ce32;
                }
                c = self.next_skipped_code_point();
                if c < 0 {
                    return ce32;
                }
                if self.has_skipped()
                    && let Some(skipped) = &mut self.skipped
                {
                    skipped.state = suffixes.save_state();
                }
                since_match = 1;
            } else {
                let next_cp = if result == UStringTrieResult::NoMatch {
                    SENTINEL
                } else {
                    self.next_skipped_code_point()
                };
                if next_cp < 0 {
                    // No match for c, or partial match (USTRINGTRIE_NO_VALUE) and no further
                    // text. Back up if necessary, and try a discontiguous contraction.
                    if (contraction_ce32 & coll::CONTRACT_TRAILING_CCC) != 0
                        // Discontiguous contraction matching extends an existing match.
                        // If there is no match yet, then there is nothing to do.
                        && ((contraction_ce32 & coll::CONTRACT_SINGLE_CP_NO_MATCH) == 0
                            || since_match < look_ahead)
                    {
                        // The last character of at least one suffix has lccc!=0,
                        // allowing for discontiguous contractions.
                        // UCA S2.1.1 only processes non-starters immediately following
                        // "a match in the table" (sinceMatch=1).
                        if since_match > 1 {
                            // Return to the state after the last match.
                            // (Return to sinceMatch=0 and re-fetch the first partially-matched character.)
                            self.backward_num_skipped(since_match);
                            c = self.next_skipped_code_point();
                            look_ahead -= since_match - 1;
                            since_match = 1;
                        }
                        if d.get_fcd16(c) > 0xff {
                            return self
                                .next_ce32_from_discontiguous_contraction(d, suffixes, ce32, look_ahead, c);
                        }
                    }
                    break;
                }
                // Continue after partial match (USTRINGTRIE_NO_VALUE) for c.
                // It does not have a result value, therefore it is not itself "a match in the table".
                // If a partially-matched c has ccc!=0 then
                // it might be skipped in discontiguous contraction.
                c = next_cp;
                since_match += 1;
            }
            look_ahead += 1;
            result = suffixes.next_for_code_point(c);
        }
        self.backward_num_skipped(since_match);
        ce32
    }

    /// `nextCE32FromDiscontiguousContraction(d, suffixes, ce32, lookAhead, c)`.
    fn next_ce32_from_discontiguous_contraction(
        &mut self,
        d: &'d CollationData,
        mut suffixes: UCharsTrie<'d>,
        mut ce32: u32,
        mut look_ahead: i32,
        mut c: i32,
    ) -> u32 {
        // UCA section 3.3.2 Contractions: contractions that end with non-starter characters
        // are known as discontiguous contractions.

        // First: Is a discontiguous contraction even possible?
        let mut fcd16 = d.get_fcd16(c);
        let next_cp = self.next_skipped_code_point();
        if next_cp < 0 {
            // No further text.
            self.backward_num_skipped(1);
            return ce32;
        }
        look_ahead += 1;
        let mut prev_cc = fcd16 as u8;
        fcd16 = d.get_fcd16(next_cp);
        if fcd16 <= 0xff {
            // The next code point after c is a starter (S2.1.1 "process each non-starter").
            self.backward_num_skipped(2);
            return ce32;
        }

        // We have read and matched (lookAhead-2) code points,
        // read non-matching c and peeked ahead at nextCp.
        // Return to the state before the mismatch and continue matching with nextCp.
        if !self.has_skipped() {
            self.skipped.get_or_insert_with(SkippedState::default);
            suffixes.reset();
            if look_ahead > 2 {
                // Replay the partial match so far.
                self.text.backward_num_code_points(look_ahead);
                suffixes.first_for_code_point(self.text.next_code_point());
                for _ in 3..look_ahead {
                    suffixes.next_for_code_point(self.text.next_code_point());
                }
                // Skip c (which did not match) and nextCp (which we will try now).
                self.text.forward_num_code_points(2);
            }
            if let Some(skipped) = &mut self.skipped {
                skipped.state = suffixes.save_state();
            }
        } else if let Some(skipped) = &self.skipped {
            // Reset to the trie state before the failed match of c.
            suffixes.reset_to_state(skipped.state);
        }

        let Some(skipped) = &mut self.skipped else {
            return ce32;
        };
        skipped.set_first_skipped(c);
        // Number of code points read since the last match (at this point: c and nextCp).
        let mut since_match = 2;
        c = next_cp;
        loop {
            // "If C is not blocked from S, find if S + C has a match in the table." (S2.1.2)
            let mut matched = false;
            if prev_cc < (fcd16 >> 8) as u8 {
                let result = suffixes.next_for_code_point(c);
                if result.has_value() {
                    matched = true;
                    // "If there is a match, replace S by S + C, and remove C." (S2.1.3)
                    // Keep prevCC unchanged.
                    ce32 = suffixes.get_value() as u32;
                    since_match = 0;
                    if let Some(skipped) = &mut self.skipped {
                        skipped.record_match();
                    }
                    if !result.has_next() {
                        break;
                    }
                    if let Some(skipped) = &mut self.skipped {
                        skipped.state = suffixes.save_state();
                    }
                }
            }
            if !matched {
                // No match for "S + C", skip C.
                if let Some(skipped) = &mut self.skipped {
                    skipped.skip(c);
                    suffixes.reset_to_state(skipped.state);
                }
                prev_cc = fcd16 as u8;
            }
            c = self.next_skipped_code_point();
            if c < 0 {
                break;
            }
            since_match += 1;
            fcd16 = d.get_fcd16(c);
            if fcd16 <= 0xff {
                // The next code point after c is a starter (S2.1.1 "process each non-starter").
                break;
            }
        }
        self.backward_num_skipped(since_match);
        let Some(skipped) = &mut self.skipped else {
            return ce32;
        };
        let is_top_discontiguous = skipped.is_empty();
        skipped.replace_match();
        if is_top_discontiguous && !skipped.is_empty() {
            // We did get a match after skipping one or more combining marks,
            // and we are not in a recursive discontiguous contraction.
            // Append CEs from the contraction ce32
            // and then from the combining marks that we skipped before the match.
            let mut d = d;
            c = SENTINEL;
            loop {
                self.append_ces_from_ce32(d, c, ce32, true);
                // Fetch CE32s for skipped combining marks from the normal data, with fallback,
                // rather than from the CollationData where we found the contraction.
                let Some(skipped) = &mut self.skipped else {
                    break;
                };
                if !skipped.has_next() {
                    break;
                }
                c = skipped.next();
                (d, ce32) = self.data.ce32_with_fallback(c);
                // Note: A nested discontiguous-contraction match
                // replaces consumed combining marks with newly skipped ones
                // and resets the reading position to the beginning.
            }
            if let Some(skipped) = &mut self.skipped {
                skipped.clear();
            }
            ce32 = NO_CE32; // Signal to the caller that the result is in the ceBuffer.
        }
        ce32
    }

    /// `appendNumericCEs(ce32, forward=TRUE)`: the CEs of a digit string in numeric order.
    fn append_numeric_ces(&mut self, mut ce32: u32) {
        // Collect digits.
        let mut digits: Vec<u8> = Vec::new();
        loop {
            digits.push(coll::digit_from_ce32(ce32));
            if self.num_cp_fwd == 0 {
                break;
            }
            let c = self.text.next_code_point();
            if c < 0 {
                break;
            }
            ce32 = self.data.ce32_with_fallback(c).1;
            if !coll::has_ce32_tag(ce32, coll::DIGIT_TAG) {
                self.text.backward_num_code_points(1);
                break;
            }
            if self.num_cp_fwd > 0 {
                self.num_cp_fwd -= 1;
            }
        }
        let mut pos = 0;
        loop {
            // Skip leading zeros.
            while pos < digits.len() - 1 && digits[pos] == 0 {
                pos += 1;
            }
            // Write a sequence of CEs for at most 254 digits at a time.
            let segment_length = (digits.len() - pos).min(254);
            self.append_numeric_segment_ces(&digits[pos..pos + segment_length]);
            pos += segment_length;
            if pos >= digits.len() {
                break;
            }
        }
    }

    /// `appendNumericSegmentCEs(digits, length)`: 1..254 digits as CEs.
    fn append_numeric_segment_ces(&mut self, digits: &[u8]) {
        let numeric_primary = self.data.numeric_primary;
        let mut length = digits.len();
        // Note: We use primary byte values 2..255: digits are not compressible.
        if length <= 7 {
            // Very dense encoding for small numbers.
            let mut value = digits
                .iter()
                .fold(0i32, |value, &digit| value * 10 + i32::from(digit));
            // Primary weight second byte values:
            //     74 byte values   2.. 75 for small numbers in two-byte primary weights.
            //     40 byte values  76..115 for medium numbers in three-byte primary weights.
            //     16 byte values 116..131 for large numbers in four-byte primary weights.
            //    124 byte values 132..255 for very large numbers with 4..127 digit pairs.
            let mut first_byte = 2;
            let mut num_bytes = 74;
            if value < num_bytes {
                // Two-byte primary for 0..73, good for day & month numbers etc.
                let primary = numeric_primary | (((first_byte + value) as u32) << 16);
                self.ce_buffer.push(coll::make_ce(primary));
                return;
            }
            value -= num_bytes;
            first_byte += num_bytes;
            num_bytes = 40;
            if value < num_bytes * 254 {
                // Three-byte primary for 74..10233=74+40*254-1, good for year numbers and more.
                let primary = numeric_primary
                    | (((first_byte + value / 254) as u32) << 16)
                    | (((2 + value % 254) as u32) << 8);
                self.ce_buffer.push(coll::make_ce(primary));
                return;
            }
            value -= num_bytes * 254;
            first_byte += num_bytes;
            num_bytes = 16;
            if value < num_bytes * 254 * 254 {
                // Four-byte primary for 10234..1042489=10234+16*254*254-1.
                let mut primary = numeric_primary | (2 + value % 254) as u32;
                value /= 254;
                primary |= ((2 + value % 254) as u32) << 8;
                value /= 254;
                primary |= ((first_byte + value % 254) as u32) << 16;
                self.ce_buffer.push(coll::make_ce(primary));
                return;
            }
            // original value > 1042489
        }

        // The second primary byte value 132..255 indicates the number of digit pairs (4..127),
        // then we generate primary bytes with those pairs.
        // Omit trailing 00 pairs.
        // Decrement the value for the last pair.

        // Set the exponent. 4 pairs->132, 5 pairs->133, ..., 127 pairs->255.
        let num_pairs = (length as u32).div_ceil(2);
        let mut primary = numeric_primary | ((132 - 4 + num_pairs) << 16);
        // Find the length without trailing 00 pairs.
        while digits[length - 1] == 0 && digits[length - 2] == 0 {
            length -= 2;
        }
        // Read the first pair.
        let (mut pair, mut pos) = if length & 1 != 0 {
            // Only "half a pair" if we have an odd number of digits.
            (u32::from(digits[0]), 1)
        } else {
            (u32::from(digits[0]) * 10 + u32::from(digits[1]), 2)
        };
        pair = 11 + 2 * pair;
        // Add the pairs of digits between pos and length.
        let mut shift = 8;
        while pos < length {
            if shift == 0 {
                // Every three pairs/bytes we need to store a 4-byte-primary CE
                // and start with a new CE with the '0' primary lead byte.
                primary |= pair;
                self.ce_buffer.push(coll::make_ce(primary));
                primary = numeric_primary;
                shift = 16;
            } else {
                primary |= pair << shift;
                shift -= 8;
            }
            pair = 11 + 2 * (u32::from(digits[pos]) * 10 + u32::from(digits[pos + 1]));
            pos += 2;
        }
        primary |= (pair - 1) << shift;
        self.ce_buffer.push(coll::make_ce(primary));
    }
}
