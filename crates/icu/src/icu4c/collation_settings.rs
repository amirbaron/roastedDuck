//! Port of ICU's `collationsettings.h` / `collationsettings.cpp` (`CollationSettings`):
//! the attributes of a collator and its script reordering.

use super::collation::{CASE_AND_TERTIARY_MASK, NO_CE_PRIMARY, ONLY_TERTIARY_MASK};
use super::collation_data::{CollationData, REORDER_CODE_NONE};

/// `CHECK_FCD`: perform the FCD check on the input text.
pub const CHECK_FCD: i32 = 1;
/// `NUMERIC`: numeric collation.
pub const NUMERIC: i32 = 2;
/// `SHIFTED`: "shifted" alternate handling.
pub const SHIFTED: i32 = 4;
/// `ALTERNATE_MASK`: 0 for non-ignorable, `SHIFTED` (4) for shifted.
pub const ALTERNATE_MASK: i32 = 0xc;
/// `MAX_VARIABLE_SHIFT`.
pub const MAX_VARIABLE_SHIFT: i32 = 4;
/// `MAX_VARIABLE_MASK`.
pub const MAX_VARIABLE_MASK: i32 = 0x70;
/// `UPPER_FIRST`.
pub const UPPER_FIRST: i32 = 0x100;
/// `CASE_FIRST`.
pub const CASE_FIRST: i32 = 0x200;
/// `CASE_LEVEL`.
pub const CASE_LEVEL: i32 = 0x400;
/// `BACKWARD_SECONDARY`.
pub const BACKWARD_SECONDARY: i32 = 0x800;
/// `STRENGTH_SHIFT`.
pub const STRENGTH_SHIFT: i32 = 12;
/// `STRENGTH_MASK`.
pub const STRENGTH_MASK: i32 = 0xf000;
/// `UCOL_PRIMARY`.
pub const STRENGTH_PRIMARY: i32 = 0;
/// `UCOL_IDENTICAL`.
pub const STRENGTH_IDENTICAL: i32 = 15;
/// `MAX_VAR_PUNCT`.
const MAX_VAR_PUNCT: i32 = 1;
/// `UCOL_DEFAULT_STRENGTH` (`UCOL_TERTIARY`).
const DEFAULT_STRENGTH: i32 = 2;

/// `CollationSettings`.
#[derive(Debug, Clone)]
pub struct CollationSettings {
    /// `CHECK_FCD` etc.
    pub options: i32,
    /// Variable-top primary weight.
    pub variable_top: u32,
    /// Permutation of primary lead bytes; `None` without reordering. A 0 entry at a non-zero
    /// index marks a lead byte split between ranges with different offsets.
    reorder_table: Option<Box<[u8; 256]>>,
    /// Limit of the last reordered range; 0 without reordering or split bytes.
    min_high_no_reorder: u32,
    /// The (limit, offset) ranges for split-reordered lead bytes.
    reorder_ranges: Vec<u32>,
    /// The reorder codes.
    reorder_codes: Vec<i32>,
}

impl Default for CollationSettings {
    fn default() -> Self {
        Self {
            options: (DEFAULT_STRENGTH << STRENGTH_SHIFT) | (MAX_VAR_PUNCT << MAX_VARIABLE_SHIFT),
            variable_top: 0,
            reorder_table: None,
            min_high_no_reorder: 0,
            reorder_ranges: Vec::new(),
            reorder_codes: Vec::new(),
        }
    }
}

impl CollationSettings {
    /// `reorderCodes`.
    pub fn reorder_codes(&self) -> &[i32] {
        &self.reorder_codes
    }

    /// `resetReordering()`.
    fn reset_reordering(&mut self) {
        self.reorder_table = None;
        self.min_high_no_reorder = 0;
        self.reorder_ranges.clear();
        self.reorder_codes.clear();
    }

    /// `aliasReordering`: the reordering of `codes`, with the table and ranges stored in the
    /// data when they are complete, else regenerated (`setReordering`). `None` for an
    /// illegal reordering.
    pub fn alias_reordering(
        &mut self,
        data: &CollationData,
        codes: &[i32],
        ranges: &[u32],
        table: Option<&[u8; 256]>,
    ) -> Option<()> {
        if let Some(table) = table {
            let complete = if ranges.is_empty() {
                !reorder_table_has_split_bytes(table)
            } else {
                // The first offset must be 0. The last offset must not be 0.
                ranges.len() >= 2 && ranges[0] as u16 == 0 && ranges[ranges.len() - 1] as u16 != 0
            };
            if complete {
                self.reorder_table = Some(Box::new(*table));
                self.reorder_codes = codes.to_vec();
                // Drop ranges before the first split byte. They are reordered by the table.
                let first_split_byte_range_index = ranges
                    .iter()
                    .position(|&range| (range & 0xff_0000) != 0)
                    .unwrap_or(ranges.len());
                if first_split_byte_range_index == ranges.len() {
                    self.min_high_no_reorder = 0;
                    self.reorder_ranges.clear();
                } else {
                    self.min_high_no_reorder = ranges[ranges.len() - 1] & 0xffff_0000;
                    self.reorder_ranges = ranges[first_split_byte_range_index..].to_vec();
                }
                return Some(());
            }
        }
        // Regenerate missing data.
        self.set_reordering(data, codes)
    }

    /// `setReordering(data, codes)`.
    pub fn set_reordering(&mut self, data: &CollationData, codes: &[i32]) -> Option<()> {
        if codes.is_empty() || (codes.len() == 1 && codes[0] == REORDER_CODE_NONE) {
            self.reset_reordering();
            return Some(());
        }
        let ranges = data.make_reorder_ranges(codes)?;
        if ranges.is_empty() {
            self.reset_reordering();
            return Some(());
        }
        // ranges[] contains at least two (limit, offset) pairs.
        // The first offset must be 0. The last offset must not be 0.
        self.min_high_no_reorder = ranges[ranges.len() - 1] & 0xffff_0000;

        // Write the lead byte permutation table.
        // Set a 0 for each lead byte that has a range boundary in the middle.
        let mut table = [0u8; 256];
        let mut b = 0usize;
        let mut first_split_byte_range_index = None;
        for (i, &pair) in ranges.iter().enumerate() {
            let limit1 = (pair >> 24) as usize;
            while b < limit1 {
                table[b] = (b as u32).wrapping_add(pair) as u8;
                b += 1;
            }
            // Check the second byte of the limit.
            if (pair & 0xff_0000) != 0 {
                table[limit1] = 0;
                b = limit1 + 1;
                if first_split_byte_range_index.is_none() {
                    first_split_byte_range_index = Some(i);
                }
            }
        }
        while b <= 0xff {
            table[b] = b as u8;
            b += 1;
        }
        self.reorder_ranges = match first_split_byte_range_index {
            // The lead byte permutation table alone suffices for reordering.
            None => Vec::new(),
            // Remove the ranges below the first split byte.
            Some(index) => ranges[index..].to_vec(),
        };
        self.reorder_table = Some(Box::new(table));
        self.reorder_codes = codes.to_vec();
        Some(())
    }

    /// `hasReordering()`.
    #[inline]
    pub fn has_reordering(&self) -> bool {
        self.reorder_table.is_some()
    }

    /// `reorder(p)`: the reordered primary weight.
    #[inline]
    pub fn reorder(&self, p: u32) -> u32 {
        let Some(table) = &self.reorder_table else {
            return p;
        };
        let b = table[(p >> 24) as usize];
        if b != 0 || p <= NO_CE_PRIMARY {
            (u32::from(b) << 24) | (p & 0xff_ffff)
        } else {
            self.reorder_ex(p)
        }
    }

    fn reorder_ex(&self, p: u32) -> u32 {
        if p >= self.min_high_no_reorder {
            return p;
        }
        // Round up p so that its lower 16 bits are >= any offset bits.
        // Then compare q directly with (limit, offset) pairs.
        let q = p | 0xffff;
        let r = self.reorder_ranges.iter().copied().find(|&r| q < r).unwrap_or(0);
        p.wrapping_add(r << 24)
    }

    /// `getStrength()`.
    #[inline]
    pub fn get_strength(&self) -> i32 {
        self.options >> STRENGTH_SHIFT
    }

    /// `getMaxVariable()`.
    #[inline]
    pub fn get_max_variable(&self) -> i32 {
        (self.options & MAX_VARIABLE_MASK) >> MAX_VARIABLE_SHIFT
    }

    /// `getTertiaryMask(options)`: case bits stay in the tertiary weight when caseLevel is
    /// off and caseFirst is on.
    #[inline]
    pub fn get_tertiary_mask(options: i32) -> u32 {
        if (options & (CASE_LEVEL | CASE_FIRST)) == CASE_FIRST {
            CASE_AND_TERTIARY_MASK
        } else {
            ONLY_TERTIARY_MASK
        }
    }

    /// `dontCheckFCD()`.
    #[inline]
    pub fn dont_check_fcd(&self) -> bool {
        (self.options & CHECK_FCD) == 0
    }

    /// `isNumeric()`.
    #[inline]
    pub fn is_numeric(&self) -> bool {
        (self.options & NUMERIC) != 0
    }
}

/// `reorderTableHasSplitBytes(table)`.
fn reorder_table_has_split_bytes(table: &[u8; 256]) -> bool {
    table[1..].contains(&0)
}
