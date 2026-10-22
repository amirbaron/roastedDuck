//! Port of ICU's `collationkeys.h` / `collationkeys.cpp`
//! (`CollationKeys::writeSortKeyUpToQuaternary`): the sort key bytes of the collation
//! elements, level by level, with ICU's compression of common weights.

use super::collation::{
    self as coll, COMMON_WEIGHT16, LEVEL_SEPARATOR_BYTE, MERGE_SEPARATOR_BYTE, MERGE_SEPARATOR_PRIMARY,
    NO_CE_PRIMARY, NO_CE_WEIGHT16, PRIMARY_COMPRESSION_HIGH_BYTE, PRIMARY_COMPRESSION_LOW_BYTE,
};
use super::collation_iterator::{CollationIterator, CollationText};
use super::collation_settings::{
    self as cs, ALTERNATE_MASK, BACKWARD_SECONDARY, CASE_LEVEL, CollationSettings, UPPER_FIRST,
};

// Secondary level: Compress up to 33 common weights as 05..25 or 25..45.
const SEC_COMMON_LOW: u32 = coll::COMMON_BYTE;
const SEC_COMMON_MIDDLE: u32 = SEC_COMMON_LOW + 0x20;
const SEC_COMMON_HIGH: u32 = SEC_COMMON_LOW + 0x40;
const SEC_COMMON_MAX_COUNT: i32 = 0x21;
// Case level, lowerFirst: Compress up to 7 common weights as 1..7 or 7..13.
const CASE_LOWER_FIRST_COMMON_LOW: u32 = 1;
const CASE_LOWER_FIRST_COMMON_MIDDLE: u32 = 7;
const CASE_LOWER_FIRST_COMMON_HIGH: u32 = 13;
const CASE_LOWER_FIRST_COMMON_MAX_COUNT: i32 = 7;
// Case level, upperFirst: Compress up to 13 common weights as 3..15.
const CASE_UPPER_FIRST_COMMON_LOW: u32 = 3;
const CASE_UPPER_FIRST_COMMON_MAX_COUNT: i32 = 13;
// Tertiary level only (no case): Compress up to 97 common weights as 05..65 or 65..C5.
const TER_ONLY_COMMON_LOW: u32 = coll::COMMON_BYTE;
const TER_ONLY_COMMON_MIDDLE: u32 = TER_ONLY_COMMON_LOW + 0x60;
const TER_ONLY_COMMON_HIGH: u32 = TER_ONLY_COMMON_LOW + 0xc0;
const TER_ONLY_COMMON_MAX_COUNT: i32 = 0x61;
// Tertiary with case, lowerFirst: Compress up to 33 common weights as 05..25 or 25..45.
const TER_LOWER_FIRST_COMMON_LOW: u32 = coll::COMMON_BYTE;
const TER_LOWER_FIRST_COMMON_MIDDLE: u32 = TER_LOWER_FIRST_COMMON_LOW + 0x20;
const TER_LOWER_FIRST_COMMON_HIGH: u32 = TER_LOWER_FIRST_COMMON_LOW + 0x40;
const TER_LOWER_FIRST_COMMON_MAX_COUNT: i32 = 0x21;
// Tertiary with case, upperFirst: Compress up to 33 common weights as 85..A5 or A5..C5.
const TER_UPPER_FIRST_COMMON_LOW: u32 = coll::COMMON_BYTE + 0x80;
const TER_UPPER_FIRST_COMMON_MIDDLE: u32 = TER_UPPER_FIRST_COMMON_LOW + 0x20;
const TER_UPPER_FIRST_COMMON_HIGH: u32 = TER_UPPER_FIRST_COMMON_LOW + 0x40;
const TER_UPPER_FIRST_COMMON_MAX_COUNT: i32 = 0x21;
// Quaternary level: Compress up to 113 common weights as 1C..8C or 8C..FC.
const QUAT_COMMON_LOW: u32 = 0x1c;
const QUAT_COMMON_MIDDLE: u32 = QUAT_COMMON_LOW + 0x70;
const QUAT_COMMON_HIGH: u32 = QUAT_COMMON_LOW + 0xe0;
const QUAT_COMMON_MAX_COUNT: i32 = 0x71;
// Primary weights shifted to quaternary level must be encoded with
// a lead byte below the common-weight compression range.
const QUAT_SHIFTED_LIMIT_BYTE: u32 = QUAT_COMMON_LOW - 1; // 0x1b

/// `levelMasks`: from the strength to the levels up to it, without the case level and the
/// identical level.
const LEVEL_MASKS: [u32; 16] = [
    2,    // UCOL_PRIMARY -> PRIMARY_LEVEL
    6,    // UCOL_SECONDARY -> up to SECONDARY_LEVEL
    0x16, // UCOL_TERTIARY -> up to TERTIARY_LEVEL
    0x36, // UCOL_QUATERNARY -> up to QUATERNARY_LEVEL
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x36, // UCOL_IDENTICAL -> up to QUATERNARY_LEVEL
];

/// The buffers of the levels after the primary one, kept from one sort key to the next
/// (ICU's are on the stack).
#[derive(Debug, Default)]
pub struct SortKeyLevels {
    cases: Vec<u8>,
    secondaries: Vec<u8>,
    tertiaries: Vec<u8>,
    quaternaries: Vec<u8>,
}

/// `SortKeyLevel`: the bytes of one level, ending with the 01 of `NO_CE`.
#[derive(Debug)]
struct SortKeyLevel {
    buffer: Vec<u8>,
}

impl SortKeyLevel {
    /// A level in the (cleared) buffer of a previous key.
    fn new(buffer: &mut Vec<u8>) -> Self {
        let mut buffer = std::mem::take(buffer);
        buffer.clear();
        Self { buffer }
    }

    #[inline]
    fn append_byte(&mut self, b: u32) {
        self.buffer.push(b as u8);
    }

    #[inline]
    fn append_weight16(&mut self, w: u32) {
        let b0 = (w >> 8) as u8;
        let b1 = w as u8;
        self.buffer.push(b0);
        if b1 != 0 {
            self.buffer.push(b1);
        }
    }

    fn append_weight32(&mut self, w: u32) {
        let bytes = w.to_be_bytes();
        self.buffer.push(bytes[0]);
        if bytes[1] != 0 {
            self.buffer.push(bytes[1]);
            if bytes[2] != 0 {
                self.buffer.push(bytes[2]);
                if bytes[3] != 0 {
                    self.buffer.push(bytes[3]);
                }
            }
        }
    }

    fn append_reverse_weight16(&mut self, w: u32) {
        let b0 = (w >> 8) as u8;
        let b1 = w as u8;
        if b1 != 0 {
            self.buffer.push(b1);
        }
        self.buffer.push(b0);
    }

    /// `appendTo(sink)`: all but the last byte, the 01 terminator.
    fn append_to(&self, sink: &mut Vec<u8>) {
        sink.extend_from_slice(&self.buffer[..self.buffer.len().saturating_sub(1)]);
    }
}

/// Writes a run of compressed common weights (`count` > 0) before a weight `w`: low values
/// counting up below `low`, high values counting down above it.
#[inline]
fn append_common(
    level: &mut SortKeyLevel,
    count: &mut i32,
    below: bool,
    low: u32,
    middle: u32,
    high: u32,
    max: i32,
) {
    *count -= 1;
    while *count >= max {
        level.append_byte(middle);
        *count -= max;
    }
    let b = if below {
        low + *count as u32
    } else {
        high - *count as u32
    };
    level.append_byte(b);
    *count = 0;
}

/// `CollationKeys::writeSortKeyUpToQuaternary(iter, compressibleBytes, settings, sink,
/// PRIMARY_LEVEL, callback, preflight=TRUE)`: the sort key up to the settings' strength (and
/// the case level), levels separated by `LEVEL_SEPARATOR_BYTE`, without a terminator.
pub fn write_sort_key_up_to_quaternary<T: CollationText>(
    iter: &mut CollationIterator<'_, T>,
    compressible_bytes: &[bool],
    settings: &CollationSettings,
    sink: &mut Vec<u8>,
    levels_buffers: &mut SortKeyLevels,
) {
    let options = settings.options;
    // Set of levels to process and write.
    let mut levels = LEVEL_MASKS[settings.get_strength() as usize & 0xf];
    if (options & CASE_LEVEL) != 0 {
        levels |= coll::CASE_LEVEL_FLAG;
    }
    // Minus the levels below minLevel.
    levels &= !((1u32 << coll::PRIMARY_LEVEL) - 1);
    if levels == 0 {
        return;
    }

    let variable_top = if (options & ALTERNATE_MASK) == 0 {
        0
    } else {
        // +1 so that we can use "<" and primary ignorables test out early.
        settings.variable_top + 1
    };

    let tertiary_mask = CollationSettings::get_tertiary_mask(options);

    let mut cases = SortKeyLevel::new(&mut levels_buffers.cases);
    let mut secondaries = SortKeyLevel::new(&mut levels_buffers.secondaries);
    let mut tertiaries = SortKeyLevel::new(&mut levels_buffers.tertiaries);
    let mut quaternaries = SortKeyLevel::new(&mut levels_buffers.quaternaries);

    let mut prev_reordered_primary = 0u32; // 0==no compression
    let mut common_cases = 0i32;
    let mut common_secondaries = 0i32;
    let mut common_tertiaries = 0i32;
    let mut common_quaternaries = 0i32;

    let mut prev_secondary = 0u32;
    let mut sec_segment_start = 0usize;

    loop {
        // No need to keep all CEs in the buffer when we write a sort key.
        iter.clear_ces_if_none_remaining();
        let mut ce = iter.next_ce();
        let mut p = (ce >> 32) as u32;
        if p < variable_top && p > MERGE_SEPARATOR_PRIMARY {
            // Variable CE, shift it to quaternary level.
            // Ignore all following primary ignorables, and shift further variable CEs.
            if common_quaternaries != 0 {
                common_quaternaries -= 1;
                while common_quaternaries >= QUAT_COMMON_MAX_COUNT {
                    quaternaries.append_byte(QUAT_COMMON_MIDDLE);
                    common_quaternaries -= QUAT_COMMON_MAX_COUNT;
                }
                // Shifted primary weights are lower than the common weight.
                quaternaries.append_byte(QUAT_COMMON_LOW + common_quaternaries as u32);
                common_quaternaries = 0;
            }
            loop {
                if (levels & coll::QUATERNARY_LEVEL_FLAG) != 0 {
                    p = settings.reorder(p);
                    if (p >> 24) >= QUAT_SHIFTED_LIMIT_BYTE {
                        // Prevent shifted primary lead bytes from
                        // overlapping with the common compression range.
                        quaternaries.append_byte(QUAT_SHIFTED_LIMIT_BYTE);
                    }
                    quaternaries.append_weight32(p);
                }
                loop {
                    ce = iter.next_ce();
                    p = (ce >> 32) as u32;
                    if p != 0 {
                        break;
                    }
                }
                if !(p < variable_top && p > MERGE_SEPARATOR_PRIMARY) {
                    break;
                }
            }
        }
        // ce could be primary ignorable, or NO_CE, or the merge separator,
        // or a regular primary CE, but it is not variable.
        // If ce==NO_CE, then write nothing for the primary level but
        // terminate compression on all levels and then exit the loop.
        if p > NO_CE_PRIMARY && (levels & coll::PRIMARY_LEVEL_FLAG) != 0 {
            // Test the un-reordered primary for compressibility.
            let is_compressible = compressible_bytes[(p >> 24) as usize];
            p = settings.reorder(p);
            let p1 = p >> 24;
            if !is_compressible || p1 != (prev_reordered_primary >> 24) {
                if prev_reordered_primary != 0 {
                    if p < prev_reordered_primary {
                        // No primary compression terminator
                        // at the end of the level or merged segment.
                        if p1 > u32::from(MERGE_SEPARATOR_BYTE) {
                            sink.push(PRIMARY_COMPRESSION_LOW_BYTE);
                        }
                    } else {
                        sink.push(PRIMARY_COMPRESSION_HIGH_BYTE);
                    }
                }
                sink.push(p1 as u8);
                prev_reordered_primary = if is_compressible { p } else { 0 };
            }
            let p2 = (p >> 16) as u8;
            if p2 != 0 {
                sink.push(p2);
                let p3 = (p >> 8) as u8;
                if p3 != 0 {
                    sink.push(p3);
                    let p4 = p as u8;
                    if p4 != 0 {
                        sink.push(p4);
                    }
                }
            }
        }

        let lower32 = ce as u32;
        if lower32 == 0 {
            continue; // completely ignorable, no secondary/case/tertiary/quaternary
        }

        if (levels & coll::SECONDARY_LEVEL_FLAG) != 0 {
            let s = lower32 >> 16;
            if s == 0 {
                // secondary ignorable
            } else if s == COMMON_WEIGHT16
                && ((options & BACKWARD_SECONDARY) == 0 || p != MERGE_SEPARATOR_PRIMARY)
            {
                // s is a common secondary weight, and
                // backwards-secondary is off or the ce is not the merge separator.
                common_secondaries += 1;
            } else if (options & BACKWARD_SECONDARY) == 0 {
                if common_secondaries != 0 {
                    append_common(
                        &mut secondaries,
                        &mut common_secondaries,
                        s < COMMON_WEIGHT16,
                        SEC_COMMON_LOW,
                        SEC_COMMON_MIDDLE,
                        SEC_COMMON_HIGH,
                        SEC_COMMON_MAX_COUNT,
                    );
                }
                secondaries.append_weight16(s);
            } else {
                if common_secondaries != 0 {
                    common_secondaries -= 1;
                    // Append reverse weights. The level will be re-reversed later.
                    let remainder = common_secondaries % SEC_COMMON_MAX_COUNT;
                    let b = if prev_secondary < COMMON_WEIGHT16 {
                        SEC_COMMON_LOW + remainder as u32
                    } else {
                        SEC_COMMON_HIGH - remainder as u32
                    };
                    secondaries.append_byte(b);
                    common_secondaries -= remainder;
                    // commonSecondaries is now a multiple of SEC_COMMON_MAX_COUNT.
                    while common_secondaries > 0 {
                        secondaries.append_byte(SEC_COMMON_MIDDLE);
                        common_secondaries -= SEC_COMMON_MAX_COUNT;
                    }
                }
                if 0 < p && p <= MERGE_SEPARATOR_PRIMARY {
                    // The backwards secondary level compares secondary weights backwards
                    // within segments separated by the merge separator (U+FFFE).
                    let secs = &mut secondaries.buffer;
                    if let Some(last) = secs.len().checked_sub(1)
                        && sec_segment_start < last
                    {
                        secs[sec_segment_start..=last].reverse();
                    }
                    secondaries.append_byte(if p == NO_CE_PRIMARY {
                        u32::from(LEVEL_SEPARATOR_BYTE)
                    } else {
                        u32::from(MERGE_SEPARATOR_BYTE)
                    });
                    prev_secondary = 0;
                    sec_segment_start = secondaries.buffer.len();
                } else {
                    secondaries.append_reverse_weight16(s);
                    prev_secondary = s;
                }
            }
        }

        if (levels & coll::CASE_LEVEL_FLAG) != 0 {
            let ignore = if settings.get_strength() == cs::STRENGTH_PRIMARY {
                p == 0
            } else {
                lower32 <= 0xffff
            };
            if ignore {
                // Primary+caseLevel: Ignore case level weights of primary ignorables.
                // Otherwise: Ignore case level weights of secondary ignorables.
            } else {
                let mut c = (lower32 >> 8) & 0xff; // case bits & tertiary lead byte
                if (c & 0xc0) == 0 && c > u32::from(LEVEL_SEPARATOR_BYTE) {
                    common_cases += 1;
                } else {
                    if (options & UPPER_FIRST) == 0 {
                        // lowerFirst: Compress common weights to nibbles 1..7..13, mixed=14, upper=15.
                        // If there are only common (=lowest) weights in the whole level,
                        // then we need not write anything.
                        // Level length differences are handled already on the next-higher level.
                        if common_cases != 0
                            && (c > u32::from(LEVEL_SEPARATOR_BYTE) || !cases.buffer.is_empty())
                        {
                            common_cases -= 1;
                            while common_cases >= CASE_LOWER_FIRST_COMMON_MAX_COUNT {
                                cases.append_byte(CASE_LOWER_FIRST_COMMON_MIDDLE << 4);
                                common_cases -= CASE_LOWER_FIRST_COMMON_MAX_COUNT;
                            }
                            let b = if c <= u32::from(LEVEL_SEPARATOR_BYTE) {
                                CASE_LOWER_FIRST_COMMON_LOW + common_cases as u32
                            } else {
                                CASE_LOWER_FIRST_COMMON_HIGH - common_cases as u32
                            };
                            cases.append_byte(b << 4);
                            common_cases = 0;
                        }
                        if c > u32::from(LEVEL_SEPARATOR_BYTE) {
                            c = (CASE_LOWER_FIRST_COMMON_HIGH + (c >> 6)) << 4; // 14 or 15
                        }
                    } else {
                        // upperFirst: Compress common weights to nibbles 3..15, mixed=2, upper=1.
                        // The compressed common case weights only go up from the "low" value
                        // because with upperFirst the common weight is the highest one.
                        if common_cases != 0 {
                            common_cases -= 1;
                            while common_cases >= CASE_UPPER_FIRST_COMMON_MAX_COUNT {
                                cases.append_byte(CASE_UPPER_FIRST_COMMON_LOW << 4);
                                common_cases -= CASE_UPPER_FIRST_COMMON_MAX_COUNT;
                            }
                            cases.append_byte((CASE_UPPER_FIRST_COMMON_LOW + common_cases as u32) << 4);
                            common_cases = 0;
                        }
                        if c > u32::from(LEVEL_SEPARATOR_BYTE) {
                            c = (CASE_UPPER_FIRST_COMMON_LOW - (c >> 6)) << 4; // 2 or 1
                        }
                    }
                    // c is a separator byte 01,
                    // or a left-shifted nibble 0x10, 0x20, ... 0xf0.
                    cases.append_byte(c);
                }
            }
        }

        if (levels & coll::TERTIARY_LEVEL_FLAG) != 0 {
            let mut t = lower32 & tertiary_mask;
            if t == COMMON_WEIGHT16 {
                common_tertiaries += 1;
            } else if (tertiary_mask & 0x8000) == 0 {
                // Tertiary weights without case bits.
                // Move lead bytes 06..3F to C6..FF for a large common-weight range.
                if common_tertiaries != 0 {
                    append_common(
                        &mut tertiaries,
                        &mut common_tertiaries,
                        t < COMMON_WEIGHT16,
                        TER_ONLY_COMMON_LOW,
                        TER_ONLY_COMMON_MIDDLE,
                        TER_ONLY_COMMON_HIGH,
                        TER_ONLY_COMMON_MAX_COUNT,
                    );
                }
                if t > COMMON_WEIGHT16 {
                    t += 0xc000;
                }
                tertiaries.append_weight16(t);
            } else if (options & UPPER_FIRST) == 0 {
                // Tertiary weights with caseFirst=lowerFirst.
                // Move lead bytes 06..BF to 46..FF for the common-weight range.
                if common_tertiaries != 0 {
                    append_common(
                        &mut tertiaries,
                        &mut common_tertiaries,
                        t < COMMON_WEIGHT16,
                        TER_LOWER_FIRST_COMMON_LOW,
                        TER_LOWER_FIRST_COMMON_MIDDLE,
                        TER_LOWER_FIRST_COMMON_HIGH,
                        TER_LOWER_FIRST_COMMON_MAX_COUNT,
                    );
                }
                if t > COMMON_WEIGHT16 {
                    t += 0x4000;
                }
                tertiaries.append_weight16(t);
            } else {
                // Tertiary weights with caseFirst=upperFirst.
                // Do not change the artificial uppercase weight of a tertiary CE (0.0.ut),
                // to keep tertiary CEs well-formed.
                // Their case+tertiary weights must be greater than those of
                // primary and secondary CEs.
                //
                // Separator         01 -> 01      (unchanged)
                // Lowercase     02..04 -> 82..84  (includes uncased)
                // Common weight     05 -> 85..C5  (common-weight compression range)
                // Lowercase     06..3F -> C6..FF
                // Mixed case    42..7F -> 42..7F
                // Uppercase     82..BF -> 02..3F
                // Tertiary CE   86..BF -> C6..FF
                if t <= NO_CE_WEIGHT16 {
                    // Keep separators unchanged.
                } else if lower32 > 0xffff {
                    // Invert case bits of primary & secondary CEs.
                    t ^= 0xc000;
                    if t < (TER_UPPER_FIRST_COMMON_HIGH << 8) {
                        t -= 0x4000;
                    }
                } else {
                    // Keep uppercase bits of tertiary CEs.
                    t += 0x4000;
                }
                if common_tertiaries != 0 {
                    append_common(
                        &mut tertiaries,
                        &mut common_tertiaries,
                        t < (TER_UPPER_FIRST_COMMON_LOW << 8),
                        TER_UPPER_FIRST_COMMON_LOW,
                        TER_UPPER_FIRST_COMMON_MIDDLE,
                        TER_UPPER_FIRST_COMMON_HIGH,
                        TER_UPPER_FIRST_COMMON_MAX_COUNT,
                    );
                }
                tertiaries.append_weight16(t);
            }
        }

        if (levels & coll::QUATERNARY_LEVEL_FLAG) != 0 {
            let mut q = lower32 & 0xffff;
            if (q & 0xc0) == 0 && q > NO_CE_WEIGHT16 {
                common_quaternaries += 1;
            } else if q == NO_CE_WEIGHT16 && (options & ALTERNATE_MASK) == 0 && quaternaries.buffer.is_empty()
            {
                // If alternate=non-ignorable and there are only common quaternary weights,
                // then we need not write anything.
                // The only weights greater than the merge separator and less than the common weight
                // are shifted primary weights, which are not generated for alternate=non-ignorable.
                // There are also exactly as many quaternary weights as tertiary weights,
                // so level length differences are handled already on tertiary level.
                // Any above-common quaternary weight will compare greater regardless.
                quaternaries.append_byte(u32::from(LEVEL_SEPARATOR_BYTE));
            } else {
                if q == NO_CE_WEIGHT16 {
                    q = u32::from(LEVEL_SEPARATOR_BYTE);
                } else {
                    q = 0xfc + ((q >> 6) & 3);
                }
                if common_quaternaries != 0 {
                    append_common(
                        &mut quaternaries,
                        &mut common_quaternaries,
                        q < QUAT_COMMON_LOW,
                        QUAT_COMMON_LOW,
                        QUAT_COMMON_MIDDLE,
                        QUAT_COMMON_HIGH,
                        QUAT_COMMON_MAX_COUNT,
                    );
                }
                quaternaries.append_byte(q);
            }
        }

        if (lower32 >> 24) == u32::from(LEVEL_SEPARATOR_BYTE) {
            break; // ce == NO_CE
        }
    }

    // Append the beyond-primary levels.
    if (levels & coll::SECONDARY_LEVEL_FLAG) != 0 {
        sink.push(LEVEL_SEPARATOR_BYTE);
        secondaries.append_to(sink);
    }

    if (levels & coll::CASE_LEVEL_FLAG) != 0 {
        sink.push(LEVEL_SEPARATOR_BYTE);
        // Write pairs of nibbles as bytes, except separator bytes as themselves.
        let length = cases.buffer.len().saturating_sub(1); // Ignore the trailing NO_CE.
        let mut b = 0u8;
        for &c in &cases.buffer[..length] {
            if b == 0 {
                b = c;
            } else {
                sink.push(b | (c >> 4));
                b = 0;
            }
        }
        if b != 0 {
            sink.push(b);
        }
    }

    if (levels & coll::TERTIARY_LEVEL_FLAG) != 0 {
        sink.push(LEVEL_SEPARATOR_BYTE);
        tertiaries.append_to(sink);
    }

    if (levels & coll::QUATERNARY_LEVEL_FLAG) != 0 {
        sink.push(LEVEL_SEPARATOR_BYTE);
        quaternaries.append_to(sink);
    }

    levels_buffers.cases = cases.buffer;
    levels_buffers.secondaries = secondaries.buffer;
    levels_buffers.tertiaries = tertiaries.buffer;
    levels_buffers.quaternaries = quaternaries.buffer;
}
