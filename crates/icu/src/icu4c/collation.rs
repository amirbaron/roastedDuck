//! Port of ICU's `collation.h` / `collation.cpp` (`Collation`): the CE32 and CE formats
//! and the weight constants the collation runtime shares.
//!
//! CEs are `i64` like ICU's `int64_t`: primary weight in bits 63..32, secondary in 31..16,
//! case bits and tertiary in 15..8, quaternary bits in 7..6.

/// `LEVEL_SEPARATOR_BYTE`.
pub const LEVEL_SEPARATOR_BYTE: u8 = 1;
/// `MERGE_SEPARATOR_BYTE`.
pub const MERGE_SEPARATOR_BYTE: u8 = 2;
/// `MERGE_SEPARATOR_PRIMARY` (U+FFFE).
pub const MERGE_SEPARATOR_PRIMARY: u32 = 0x0200_0000;
/// `PRIMARY_COMPRESSION_LOW_BYTE`.
pub const PRIMARY_COMPRESSION_LOW_BYTE: u8 = 3;
/// `PRIMARY_COMPRESSION_HIGH_BYTE`.
pub const PRIMARY_COMPRESSION_HIGH_BYTE: u8 = 0xff;
/// `COMMON_BYTE`.
pub const COMMON_BYTE: u32 = 5;
/// `COMMON_WEIGHT16`.
pub const COMMON_WEIGHT16: u32 = 0x0500;
/// `COMMON_SECONDARY_CE`.
pub const COMMON_SECONDARY_CE: u32 = 0x0500_0000;
/// `COMMON_TERTIARY_CE`.
pub const COMMON_TERTIARY_CE: u32 = 0x0500;
/// `COMMON_SEC_AND_TER_CE`.
pub const COMMON_SEC_AND_TER_CE: u32 = 0x0500_0500;
/// `CASE_MASK`.
pub const CASE_MASK: u32 = 0xc000;
/// `ONLY_TERTIARY_MASK`: only the 2*6 bits of the pure tertiary weight.
pub const ONLY_TERTIARY_MASK: u32 = 0x3f3f;
/// `CASE_AND_TERTIARY_MASK`.
pub const CASE_AND_TERTIARY_MASK: u32 = CASE_MASK | ONLY_TERTIARY_MASK;
/// `UNASSIGNED_IMPLICIT_BYTE`.
pub const UNASSIGNED_IMPLICIT_BYTE: u32 = 0xfe;
/// `TRAIL_WEIGHT_BYTE`.
pub const TRAIL_WEIGHT_BYTE: u32 = 0xff;
/// `MAX_REGULAR_CE32` (U+FFFF).
pub const MAX_REGULAR_CE32: u32 = 0xffff_0505;
/// `FFFD_CE32`: U+FFFD and ill-formed text.
pub const FFFD_CE32: u32 = MAX_REGULAR_CE32 - 0x20000;
/// `SPECIAL_CE32_LOW_BYTE`: a CE32 is special if its low byte is this or greater.
pub const SPECIAL_CE32_LOW_BYTE: u32 = 0xc0;
/// `FALLBACK_CE32`: fall back to the base collator.
pub const FALLBACK_CE32: u32 = SPECIAL_CE32_LOW_BYTE;
/// `LONG_PRIMARY_CE32_LOW_BYTE`.
pub const LONG_PRIMARY_CE32_LOW_BYTE: u32 = 0xc1;
/// `UNASSIGNED_CE32`: compute an unassigned-implicit CE.
pub const UNASSIGNED_CE32: u32 = 0xffff_ffff;
/// `NO_CE32`.
pub const NO_CE32: u32 = 1;
/// `NO_CE_PRIMARY`: not a left-adjusted weight.
pub const NO_CE_PRIMARY: u32 = 1;
/// `NO_CE_WEIGHT16`: the weight of `LEVEL_SEPARATOR_BYTE`.
pub const NO_CE_WEIGHT16: u32 = 0x0100;
/// `NO_CE`: end of input.
pub const NO_CE: i64 = 0x1_0100_0100;

/// `PRIMARY_LEVEL`.
pub const PRIMARY_LEVEL: u32 = 1;
/// `PRIMARY_LEVEL_FLAG`.
pub const PRIMARY_LEVEL_FLAG: u32 = 2;
/// `SECONDARY_LEVEL_FLAG`.
pub const SECONDARY_LEVEL_FLAG: u32 = 4;
/// `CASE_LEVEL_FLAG`.
pub const CASE_LEVEL_FLAG: u32 = 8;
/// `TERTIARY_LEVEL_FLAG`.
pub const TERTIARY_LEVEL_FLAG: u32 = 0x10;
/// `QUATERNARY_LEVEL_FLAG`.
pub const QUATERNARY_LEVEL_FLAG: u32 = 0x20;

/// The special-CE32 tags (bits 3..0 of a special CE32).
pub const FALLBACK_TAG: u32 = 0;
/// `LONG_PRIMARY_TAG`: bits 31..8 are a three-byte primary.
pub const LONG_PRIMARY_TAG: u32 = 1;
/// `LONG_SECONDARY_TAG`: bits 31..16 secondary, 15..8 tertiary.
pub const LONG_SECONDARY_TAG: u32 = 2;
/// `RESERVED_TAG_3`.
pub const RESERVED_TAG_3: u32 = 3;
/// `LATIN_EXPANSION_TAG`: two simple CEs `[pp, 05, tt] [00, ss, 05]`.
pub const LATIN_EXPANSION_TAG: u32 = 4;
/// `EXPANSION32_TAG`: points to CE32s.
pub const EXPANSION32_TAG: u32 = 5;
/// `EXPANSION_TAG`: points to 64-bit CEs.
pub const EXPANSION_TAG: u32 = 6;
/// `BUILDER_DATA_TAG`: never in runtime data.
pub const BUILDER_DATA_TAG: u32 = 7;
/// `PREFIX_TAG`: points to a prefix trie.
pub const PREFIX_TAG: u32 = 8;
/// `CONTRACTION_TAG`: points to contraction data.
pub const CONTRACTION_TAG: u32 = 9;
/// `DIGIT_TAG`.
pub const DIGIT_TAG: u32 = 10;
/// `U0000_TAG`.
pub const U0000_TAG: u32 = 11;
/// `HANGUL_TAG`.
pub const HANGUL_TAG: u32 = 12;
/// `LEAD_SURROGATE_TAG`.
pub const LEAD_SURROGATE_TAG: u32 = 13;
/// `OFFSET_TAG`: primaries in code point order.
pub const OFFSET_TAG: u32 = 14;
/// `IMPLICIT_TAG`.
pub const IMPLICIT_TAG: u32 = 15;

/// `CONTRACT_SINGLE_CP_NO_MATCH`.
pub const CONTRACT_SINGLE_CP_NO_MATCH: u32 = 0x100;
/// `CONTRACT_NEXT_CCC`: the first character of every contraction suffix has lccc!=0.
pub const CONTRACT_NEXT_CCC: u32 = 0x200;
/// `CONTRACT_TRAILING_CCC`: some contraction suffix ends with lccc!=0.
pub const CONTRACT_TRAILING_CCC: u32 = 0x400;
/// `HANGUL_NO_SPECIAL_JAMO`.
pub const HANGUL_NO_SPECIAL_JAMO: u32 = 0x100;
/// `LEAD_ALL_UNASSIGNED`.
pub const LEAD_ALL_UNASSIGNED: u32 = 0;
/// `LEAD_ALL_FALLBACK`.
pub const LEAD_ALL_FALLBACK: u32 = 0x100;
/// `LEAD_TYPE_MASK`.
pub const LEAD_TYPE_MASK: u32 = 0x300;

/// `Hangul::HANGUL_BASE`.
pub const HANGUL_BASE: i32 = 0xac00;
/// `Hangul::JAMO_V_COUNT`.
pub const JAMO_V_COUNT: i32 = 21;
/// `Hangul::JAMO_T_COUNT`.
pub const JAMO_T_COUNT: i32 = 28;

/// `isSpecialCE32`.
#[inline]
pub fn is_special_ce32(ce32: u32) -> bool {
    (ce32 & 0xff) >= SPECIAL_CE32_LOW_BYTE
}

/// `tagFromCE32`.
#[inline]
pub fn tag_from_ce32(ce32: u32) -> u32 {
    ce32 & 0xf
}

/// `hasCE32Tag`.
#[inline]
pub fn has_ce32_tag(ce32: u32, tag: u32) -> bool {
    is_special_ce32(ce32) && tag_from_ce32(ce32) == tag
}

/// `ceFromLongPrimaryCE32`.
#[inline]
pub fn ce_from_long_primary_ce32(ce32: u32) -> i64 {
    (i64::from(ce32 & 0xffff_ff00) << 32) | i64::from(COMMON_SEC_AND_TER_CE)
}

/// `ceFromLongSecondaryCE32`.
#[inline]
pub fn ce_from_long_secondary_ce32(ce32: u32) -> i64 {
    i64::from(ce32 & 0xffff_ff00)
}

/// `latinCE0FromCE32`: the first of the two Latin-expansion CEs.
#[inline]
pub fn latin_ce0_from_ce32(ce32: u32) -> i64 {
    (i64::from(ce32 & 0xff00_0000) << 32)
        | i64::from(COMMON_SECONDARY_CE)
        | i64::from((ce32 & 0xff_0000) >> 8)
}

/// `latinCE1FromCE32`: the second of the two Latin-expansion CEs.
#[inline]
pub fn latin_ce1_from_ce32(ce32: u32) -> i64 {
    i64::from(((ce32 & 0xff00) << 16) | COMMON_TERTIARY_CE)
}

/// `indexFromCE32`.
#[inline]
pub fn index_from_ce32(ce32: u32) -> usize {
    (ce32 >> 13) as usize
}

/// `lengthFromCE32`.
#[inline]
pub fn length_from_ce32(ce32: u32) -> usize {
    ((ce32 >> 8) & 31) as usize
}

/// `digitFromCE32`.
#[inline]
pub fn digit_from_ce32(ce32: u32) -> u8 {
    ((ce32 >> 8) & 0xf) as u8
}

/// `ceFromSimpleCE32`: `ppppsstt` -> `pppp0000ss00tt00`.
#[inline]
pub fn ce_from_simple_ce32(ce32: u32) -> i64 {
    (i64::from(ce32 & 0xffff_0000) << 32) | i64::from((ce32 & 0xff00) << 16) | i64::from((ce32 & 0xff) << 8)
}

/// `ceFromCE32`: a CE from a simple, long-primary or long-secondary CE32.
#[inline]
pub fn ce_from_ce32(ce32: u32) -> i64 {
    let tertiary = ce32 & 0xff;
    if tertiary < SPECIAL_CE32_LOW_BYTE {
        ce_from_simple_ce32(ce32)
    } else {
        let ce32 = ce32 - tertiary;
        if (tertiary & 0xf) == LONG_PRIMARY_TAG {
            (i64::from(ce32) << 32) | i64::from(COMMON_SEC_AND_TER_CE)
        } else {
            i64::from(ce32)
        }
    }
}

/// `makeCE(p)`: a CE from a primary weight.
#[inline]
pub fn make_ce(p: u32) -> i64 {
    (i64::from(p) << 32) | i64::from(COMMON_SEC_AND_TER_CE)
}

/// `incThreeBytePrimaryByOffset`.
pub fn inc_three_byte_primary_by_offset(base_primary: u32, is_compressible: bool, offset: i32) -> u32 {
    // Extract the third byte, minus the minimum byte value,
    // plus the offset, modulo the number of usable byte values, plus the minimum.
    let mut offset = offset + ((base_primary >> 8) & 0xff) as i32 - 2;
    let mut primary = (((offset % 254) + 2) as u32) << 8;
    offset /= 254;
    // Same with the second byte,
    // but reserve the PRIMARY_COMPRESSION_LOW_BYTE and high byte if necessary.
    if is_compressible {
        offset += ((base_primary >> 16) & 0xff) as i32 - 4;
        primary |= (((offset % 251) + 4) as u32) << 16;
        offset /= 251;
    } else {
        offset += ((base_primary >> 16) & 0xff) as i32 - 2;
        primary |= (((offset % 254) + 2) as u32) << 16;
        offset /= 254;
    }
    // First byte, assume no further overflow.
    primary | (base_primary & 0xff00_0000).wrapping_add((offset as u32) << 24)
}

/// `getThreeBytePrimaryForOffsetData`: the primary of `c`'s `OFFSET_TAG` data "CE".
pub fn get_three_byte_primary_for_offset_data(c: i32, data_ce: i64) -> u32 {
    let p = (data_ce >> 32) as u32; // three-byte primary pppppp00
    let lower32 = data_ce as i32; // base code point b & step s: bbbbbbss (bit 7: isCompressible)
    let offset = (c - (lower32 >> 8)) * (lower32 & 0x7f); // delta * increment
    let is_compressible = (lower32 & 0x80) != 0;
    inc_three_byte_primary_by_offset(p, is_compressible, offset)
}

/// `unassignedPrimaryFromCodePoint`: the implicit primary of an unassigned code point.
pub fn unassigned_primary_from_code_point(c: i32) -> u32 {
    // Create a gap before U+0000. Use c=-1 for [first unassigned].
    let mut c = c + 1;
    // Fourth byte: 18 values, every 14th byte value (gap of 13).
    let mut primary = (2 + (c % 18) * 14) as u32;
    c /= 18;
    // Third byte: 254 values.
    primary |= ((2 + (c % 254)) as u32) << 8;
    c /= 254;
    // Second byte: 251 values 04..FE excluding the primary compression bytes.
    primary |= ((4 + (c % 251)) as u32) << 16;
    // One lead byte covers all code points (c < 0x1182B4 = 1*251*254*18).
    primary | (UNASSIGNED_IMPLICIT_BYTE << 24)
}

/// `unassignedCEFromCodePoint`.
#[inline]
pub fn unassigned_ce_from_code_point(c: i32) -> i64 {
    make_ce(unassigned_primary_from_code_point(c))
}
