//! Port of ICU's `collationdata.h` / `collationdata.cpp` (`CollationData`): the mappings
//! of a collation (root or tailoring) and its script reordering data.

use super::collation::{self as coll, FALLBACK_CE32, MERGE_SEPARATOR_BYTE, TRAIL_WEIGHT_BYTE};
use super::normalizer2impl::Normalizer2Impl;
use super::utrie2::UTrie2;

/// `UCOL_REORDER_CODE_FIRST`.
pub const REORDER_CODE_FIRST: i32 = 0x1000;
/// `UCOL_REORDER_CODE_DEFAULT`.
pub const REORDER_CODE_DEFAULT: i32 = -1;
/// `UCOL_REORDER_CODE_NONE` (`USCRIPT_UNKNOWN`).
pub const REORDER_CODE_NONE: i32 = 103;
/// `USCRIPT_UNKNOWN`.
const SCRIPT_UNKNOWN: i32 = 103;
/// `USCRIPT_LATIN`.
const SCRIPT_LATIN: i32 = 25;
/// `REORDER_RESERVED_BEFORE_LATIN`.
const REORDER_RESERVED_BEFORE_LATIN: i32 = REORDER_CODE_FIRST + 14;
/// `REORDER_RESERVED_AFTER_LATIN`.
const REORDER_RESERVED_AFTER_LATIN: i32 = REORDER_CODE_FIRST + 15;
/// `MAX_NUM_SPECIAL_REORDER_CODES`.
const MAX_NUM_SPECIAL_REORDER_CODES: i32 = 8;
/// `MAX_NUM_SCRIPT_RANGES`.
pub const MAX_NUM_SCRIPT_RANGES: usize = 256;
/// `JAMO_CE32S_LENGTH`: 19 + 21 + 27.
pub const JAMO_CE32S_LENGTH: usize = 19 + 21 + 27;

/// `CollationData`.
#[derive(Debug)]
pub struct CollationData {
    /// Main lookup trie.
    pub trie: UTrie2,
    /// CE32 values; at index 0 there is CE32(U+0000).
    pub ce32s: Vec<u32>,
    /// CE values for expansions and `OFFSET_TAG`.
    pub ces: Vec<i64>,
    /// Prefix and contraction-suffix matching data.
    pub contexts: Vec<u16>,
    /// The base collation data, or `None` if this data is a base.
    pub base: Option<&'static CollationData>,
    /// The CE32s of the conjoining Jamo L/V/T, for `HANGUL_TAG`.
    pub jamo_ce32s: Vec<u32>,
    /// The single-byte primary weight (xx000000) for numeric collation.
    pub numeric_primary: u32,
    /// 256 flags for which primary-weight lead bytes are compressible.
    pub compressible_bytes: Vec<bool>,
    /// `numScripts`.
    pub num_scripts: i32,
    /// `scriptsIndex`: maps a script or special reorder code to an entry in `script_starts`.
    pub scripts_index: Vec<u16>,
    /// `scriptStarts`: start primary weight (top 16 bits) of each group/script/reserved range.
    pub script_starts: Vec<u16>,
}

impl CollationData {
    /// `getCE32(c)`.
    #[inline]
    pub fn get_ce32(&self, c: i32) -> u32 {
        self.trie.get32(c)
    }

    /// `getCE32FromSupplementary(c)`.
    #[inline]
    pub fn get_ce32_from_supplementary(&self, c: i32) -> u32 {
        self.trie.get32_from_supp(c)
    }

    /// `isCompressibleLeadByte(b)`.
    #[inline]
    pub fn is_compressible_lead_byte(&self, b: u32) -> bool {
        self.compressible_bytes[b as usize]
    }

    /// `readCE32(p)`: the CE32 in two context units.
    #[inline]
    pub fn read_ce32(&self, index: usize) -> u32 {
        (u32::from(self.contexts[index]) << 16) | u32::from(self.contexts[index + 1])
    }

    /// `getCEFromOffsetCE32(c, ce32)`.
    #[inline]
    pub fn get_ce_from_offset_ce32(&self, c: i32, ce32: u32) -> i64 {
        let data_ce = self.ces[coll::index_from_ce32(ce32)];
        coll::make_ce(coll::get_three_byte_primary_for_offset_data(c, data_ce))
    }

    /// `getFCD16(c)`: the FCD16 value of code point `c` (`nfcImpl.getFCD16`).
    #[inline]
    pub fn get_fcd16(&self, c: i32) -> u16 {
        Normalizer2Impl::get().map_or(0, |nfc_impl| nfc_impl.get_fcd16(c))
    }

    /// `getCE32(c)` with the fallback to the base data: the data and CE32 of `c`.
    #[inline]
    pub fn ce32_with_fallback(&self, c: i32) -> (&CollationData, u32) {
        let ce32 = self.get_ce32(c);
        match self.base {
            Some(base) if ce32 == FALLBACK_CE32 => (base, base.get_ce32(c)),
            _ => (self, ce32),
        }
    }

    /// `getLastPrimaryForGroup(script)`: the last primary of the script's reordering group,
    /// or 0 if the script is unknown.
    pub fn get_last_primary_for_group(&self, script: i32) -> u32 {
        let index = self.get_script_index(script);
        if index == 0 {
            return 0;
        }
        let limit = u32::from(self.script_starts[index + 1]);
        (limit << 16).wrapping_sub(1)
    }

    fn get_script_index(&self, script: i32) -> usize {
        if script < 0 {
            0
        } else if script < self.num_scripts {
            usize::from(self.scripts_index[script as usize])
        } else if script < REORDER_CODE_FIRST {
            0
        } else {
            let script = script - REORDER_CODE_FIRST;
            if script < MAX_NUM_SPECIAL_REORDER_CODES {
                usize::from(self.scripts_index[(self.num_scripts + script) as usize])
            } else {
                0
            }
        }
    }

    /// `makeReorderRanges(reorder, length, ranges)`: the (limit, offset) pairs of the
    /// permutation of primary-weight ranges for the reordering; `None` for an illegal one.
    pub fn make_reorder_ranges(&self, reorder: &[i32]) -> Option<Vec<u32>> {
        self.make_reorder_ranges_impl(reorder, false)
    }

    fn make_reorder_ranges_impl(&self, reorder: &[i32], latin_must_move: bool) -> Option<Vec<u32>> {
        let mut ranges = Vec::new();
        let mut length = reorder.len();
        if length == 0 || (length == 1 && reorder[0] == SCRIPT_UNKNOWN) {
            return Some(ranges);
        }

        // Maps each script-or-group range to a new lead byte.
        let mut table = [0u8; MAX_NUM_SCRIPT_RANGES];

        // Set "don't care" values for reserved ranges.
        for reserved in [REORDER_RESERVED_BEFORE_LATIN, REORDER_RESERVED_AFTER_LATIN] {
            let index =
                usize::from(self.scripts_index[(self.num_scripts + reserved - REORDER_CODE_FIRST) as usize]);
            if index != 0 {
                table[index] = 0xff;
            }
        }

        // Never reorder special low and high primary lead bytes.
        let script_starts_length = self.script_starts.len();
        let mut low_start = i32::from(self.script_starts[1]);
        let mut high_limit = i32::from(self.script_starts[script_starts_length - 1]);

        // Get the set of special reorder codes in the input list.
        let mut specials = 0u32;
        for &code in reorder {
            let reorder_code = code - REORDER_CODE_FIRST;
            if (0..MAX_NUM_SPECIAL_REORDER_CODES).contains(&reorder_code) {
                specials |= 1 << reorder_code;
            }
        }

        // Start the reordering with the special low reorder codes that do not occur in the input.
        for i in 0..MAX_NUM_SPECIAL_REORDER_CODES {
            let index = usize::from(self.scripts_index[(self.num_scripts + i) as usize]);
            if index != 0 && (specials & (1 << i)) == 0 {
                low_start = self.add_low_script_range(&mut table, index, low_start);
            }
        }

        // Skip the reserved range before Latin if Latin is the first script,
        // so that we do not move it unnecessarily.
        let mut skipped_reserved = 0;
        if specials == 0 && reorder[0] == SCRIPT_LATIN && !latin_must_move {
            let index = usize::from(self.scripts_index[SCRIPT_LATIN as usize]);
            let start = i32::from(self.script_starts[index]);
            skipped_reserved = start - low_start;
            low_start = start;
        }

        // Reorder according to the input scripts, continuing from the bottom of the primary range.
        let mut has_reorder_to_end = false;
        let mut i = 0;
        while i < length {
            let script = reorder[i];
            i += 1;
            if script == SCRIPT_UNKNOWN {
                // Put the remaining scripts at the top.
                has_reorder_to_end = true;
                while i < length {
                    length -= 1;
                    let script = reorder[length];
                    if script == SCRIPT_UNKNOWN || script == REORDER_CODE_DEFAULT {
                        return None;
                    }
                    let index = self.get_script_index(script);
                    if index == 0 {
                        continue;
                    }
                    if table[index] != 0 {
                        return None; // Duplicate or equivalent script.
                    }
                    high_limit = self.add_high_script_range(&mut table, index, high_limit);
                }
                break;
            }
            if script == REORDER_CODE_DEFAULT {
                return None;
            }
            let index = self.get_script_index(script);
            if index == 0 {
                continue;
            }
            if table[index] != 0 {
                return None; // Duplicate or equivalent script.
            }
            low_start = self.add_low_script_range(&mut table, index, low_start);
        }

        // Put all remaining scripts into the middle.
        for i in 1..script_starts_length - 1 {
            if table[i] != 0 {
                continue;
            }
            let start = i32::from(self.script_starts[i]);
            if !has_reorder_to_end && start > low_start {
                // No need to move this script.
                low_start = start;
            }
            low_start = self.add_low_script_range(&mut table, i, low_start);
        }
        if low_start > high_limit {
            if (low_start - (skipped_reserved & 0xff00)) <= high_limit {
                // Try not skipping the before-Latin reserved range.
                return self.make_reorder_ranges_impl(reorder, true);
            }
            // We need more primary lead bytes than available, despite the reserved ranges.
            return None;
        }

        // Turn lead bytes into a list of (limit, offset) pairs.
        // Upper 16 bits = limit, lower 16 = signed lead byte offset.
        let mut offset = 0i32;
        let mut i = 1;
        loop {
            let mut next_offset = offset;
            while i < script_starts_length - 1 {
                let new_lead_byte = i32::from(table[i]);
                if new_lead_byte != 0xff {
                    next_offset = new_lead_byte - (i32::from(self.script_starts[i]) >> 8);
                    if next_offset != offset {
                        break;
                    }
                }
                i += 1;
            }
            if offset != 0 || i < script_starts_length - 1 {
                ranges.push((u32::from(self.script_starts[i]) << 16) | (offset as u32 & 0xffff));
            }
            if i == script_starts_length - 1 {
                break;
            }
            offset = next_offset;
            i += 1;
        }
        Some(ranges)
    }

    fn add_low_script_range(
        &self,
        table: &mut [u8; MAX_NUM_SCRIPT_RANGES],
        index: usize,
        low_start: i32,
    ) -> i32 {
        let mut low_start = low_start;
        let start = i32::from(self.script_starts[index]);
        if (start & 0xff) < (low_start & 0xff) {
            low_start += 0x100;
        }
        table[index] = (low_start >> 8) as u8;
        let limit = i32::from(self.script_starts[index + 1]);
        ((low_start & 0xff00) + ((limit & 0xff00) - (start & 0xff00))) | (limit & 0xff)
    }

    fn add_high_script_range(
        &self,
        table: &mut [u8; MAX_NUM_SCRIPT_RANGES],
        index: usize,
        high_limit: i32,
    ) -> i32 {
        let mut high_limit = high_limit;
        let limit = i32::from(self.script_starts[index + 1]);
        if (limit & 0xff) > (high_limit & 0xff) {
            high_limit -= 0x100;
        }
        let start = i32::from(self.script_starts[index]);
        high_limit = ((high_limit & 0xff00) - ((limit & 0xff00) - (start & 0xff00))) | (start & 0xff);
        table[index] = (high_limit >> 8) as u8;
        high_limit
    }

    /// Checks the script data: the first range is for separators and terminators, the last
    /// one for trailing weights.
    pub fn script_starts_are_valid(&self) -> bool {
        let n = self.script_starts.len();
        n > 2
            && n <= MAX_NUM_SCRIPT_RANGES
            && self.script_starts[0] == 0
            && u32::from(self.script_starts[1]) == ((u32::from(MERGE_SEPARATOR_BYTE) + 1) << 8)
            && u32::from(self.script_starts[n - 1]) == (TRAIL_WEIGHT_BYTE << 8)
    }
}
