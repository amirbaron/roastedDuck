//! The part of ICU's `Normalizer2Impl` (`normalizer2impl.h`, `normalizer2impl.cpp`) the
//! collation code uses, over ICU 66's NFC data as exported into the collation package:
//! FCD16 values (`getFCD16`, `nextFCD16`, `previousFCD16`) and NFD decomposition with
//! canonical ordering (`decompose`).

use std::sync::OnceLock;

use super::collation_package::CollationPackage;

/// Code points per block of the FCD16 table.
const BLOCK_SHIFT: u32 = 5;
const BLOCK_LENGTH: usize = 1 << BLOCK_SHIFT;

/// `Hangul` constants for the algorithmic decomposition.
const HANGUL_BASE: u32 = 0xac00;
const HANGUL_END: u32 = 0xd7a3;
const JAMO_L_BASE: u32 = 0x1100;
const JAMO_V_BASE: u32 = 0x1161;
const JAMO_T_BASE: u32 = 0x11a7;
const JAMO_T_COUNT: u32 = 28;
const JAMO_VT_COUNT: u32 = 21 * 28;

/// The NFC data of the collation code.
#[derive(Debug)]
pub struct Normalizer2Impl {
    /// Block number of each block of 32 code points.
    block_index: Vec<u16>,
    /// The distinct blocks of FCD16 values.
    blocks: Vec<[u16; BLOCK_LENGTH]>,
    small_fcd: [u8; 256],
    /// Code point and range of its decomposition in `mappings`, sorted by code point.
    decompositions: Vec<(u32, u32, u8)>,
    mappings: Vec<u32>,
}

impl Normalizer2Impl {
    /// `Normalizer2Factory::getNFCImpl`.
    pub fn get() -> Option<&'static Self> {
        static IMPL: OnceLock<Option<Normalizer2Impl>> = OnceLock::new();
        IMPL.get_or_init(|| CollationPackage::get().map(Self::new))
            .as_ref()
    }

    fn new(package: &CollationPackage) -> Self {
        let mut values = vec![0u16; 0x11_0000];
        for &(start, end, value) in &package.fcd16_runs {
            for c in start..=end.min(0x10_ffff) {
                values[c as usize] = value;
            }
        }
        let mut blocks: Vec<[u16; BLOCK_LENGTH]> = Vec::new();
        let mut known = std::collections::HashMap::new();
        let block_index = values
            .as_chunks::<BLOCK_LENGTH>()
            .0
            .iter()
            .map(|&block| {
                *known.entry(block).or_insert_with(|| {
                    blocks.push(block);
                    (blocks.len() - 1) as u16
                })
            })
            .collect();
        let mut decompositions = Vec::with_capacity(package.decompositions.len());
        let mut mappings = Vec::new();
        for (c, mapping) in &package.decompositions {
            decompositions.push((*c, mappings.len() as u32, mapping.len() as u8));
            mappings.extend_from_slice(mapping);
        }
        decompositions.sort_unstable_by_key(|entry| entry.0);
        Self {
            block_index,
            blocks,
            small_fcd: package.small_fcd,
            decompositions,
            mappings,
        }
    }

    /// `getFCD16FromNormData(c)` for any code point: lccc in bits 15..8, tccc in 7..0.
    #[inline]
    pub fn get_fcd16(&self, c: i32) -> u16 {
        let Ok(c) = u32::try_from(c) else {
            return 0;
        };
        if c > 0x10_ffff {
            return 0;
        }
        let block = usize::from(self.block_index[(c >> BLOCK_SHIFT) as usize]);
        self.blocks[block][(c as usize) & (BLOCK_LENGTH - 1)]
    }

    /// `singleLeadMightHaveNonZeroFCD16(lead)` for a code unit.
    #[inline]
    fn single_lead_might_have_non_zero_fcd16(&self, lead: u16) -> bool {
        let bits = self.small_fcd[usize::from(lead >> 8)];
        bits != 0 && (bits >> ((lead >> 5) & 7)) & 1 != 0
    }

    /// `nextFCD16(s, limit)`: the FCD16 of the code point at `*pos`, advancing past it (or
    /// only past a lead surrogate none of whose code points has FCD data).
    pub fn next_fcd16(&self, s: &[u16], pos: &mut usize, limit: usize) -> u16 {
        let c = s[*pos];
        *pos += 1;
        if !self.single_lead_might_have_non_zero_fcd16(c) {
            return 0;
        }
        let mut cp = i32::from(c);
        if is_lead(c) && *pos != limit && is_trail(s[*pos]) {
            cp = supplementary(c, s[*pos]);
            *pos += 1;
        }
        self.get_fcd16(cp)
    }

    /// `previousFCD16(start, s)`: the FCD16 of the code point before `*pos`, moving back
    /// over it.
    pub fn previous_fcd16(&self, s: &[u16], start: usize, pos: &mut usize) -> u16 {
        *pos -= 1;
        let c = s[*pos];
        let mut cp = i32::from(c);
        if !is_trail(c) {
            if !self.single_lead_might_have_non_zero_fcd16(c) {
                return 0;
            }
        } else if start < *pos && is_lead(s[*pos - 1]) {
            cp = supplementary(s[*pos - 1], c);
            *pos -= 1;
        }
        self.get_fcd16(cp)
    }

    /// The canonical combining class of a code point that does not decompose.
    #[inline]
    fn cc(&self, c: u32) -> u8 {
        (self.get_fcd16(c as i32) >> 8) as u8
    }

    /// `decompose(src, limit, dest, ...)`: the NFD of the text, canonically ordered.
    pub fn decompose(&self, src: &[u16], dest: &mut Vec<u16>) {
        let mut buffer: Vec<(u32, u8)> = Vec::with_capacity(src.len() + 4);
        let mut i = 0;
        while i < src.len() {
            let c = src[i];
            i += 1;
            let mut cp = u32::from(c);
            if is_lead(c) && i < src.len() && is_trail(src[i]) {
                cp = supplementary(c, src[i]) as u32;
                i += 1;
            }
            if (HANGUL_BASE..=HANGUL_END).contains(&cp) {
                let offset = cp - HANGUL_BASE;
                buffer.push((JAMO_L_BASE + offset / JAMO_VT_COUNT, 0));
                buffer.push((JAMO_V_BASE + (offset % JAMO_VT_COUNT) / JAMO_T_COUNT, 0));
                let t = offset % JAMO_T_COUNT;
                if t != 0 {
                    buffer.push((JAMO_T_BASE + t, 0));
                }
            } else if let Ok(index) = self.decompositions.binary_search_by_key(&cp, |entry| entry.0) {
                let (_, start, length) = self.decompositions[index];
                for &d in &self.mappings[start as usize..start as usize + usize::from(length)] {
                    Self::append(&mut buffer, d, self.cc(d));
                }
            } else {
                Self::append(&mut buffer, cp, self.cc(cp));
            }
        }
        dest.clear();
        for (cp, _) in buffer {
            if cp > 0xffff {
                dest.push(((cp >> 10) + 0xd7c0) as u16);
                dest.push(((cp & 0x3ff) | 0xdc00) as u16);
            } else {
                dest.push(cp as u16);
            }
        }
    }

    /// `ReorderingBuffer::append(c, cc)`: a non-starter goes before the preceding
    /// non-starters with a higher combining class.
    fn append(buffer: &mut Vec<(u32, u8)>, c: u32, cc: u8) {
        let mut at = buffer.len();
        if cc != 0 {
            while at > 0 && buffer[at - 1].1 > cc {
                at -= 1;
            }
        }
        buffer.insert(at, (c, cc));
    }
}

/// `U16_IS_LEAD`.
#[inline]
pub fn is_lead(c: u16) -> bool {
    (c & 0xfc00) == 0xd800
}

/// `U16_IS_TRAIL`.
#[inline]
pub fn is_trail(c: u16) -> bool {
    (c & 0xfc00) == 0xdc00
}

/// `U16_GET_SUPPLEMENTARY`.
#[inline]
pub fn supplementary(lead: u16, trail: u16) -> i32 {
    (i32::from(lead) << 10) + i32::from(trail) - ((0xd800 << 10) + 0xdc00 - 0x10000)
}
