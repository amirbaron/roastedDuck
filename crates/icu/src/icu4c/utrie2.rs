//! Port of the lookups of ICU's `UTrie2` (`utrie2.h`, `utrie2.cpp`) over a serialized
//! trie with 32-bit values (`utrie2_openFromSerialized(UTRIE2_32_VALUE_BITS, ...)`).

/// `UTRIE2_SHIFT_1`.
const SHIFT_1: u32 = 6 + 5;
/// `UTRIE2_SHIFT_2`.
const SHIFT_2: u32 = 5;
/// `UTRIE2_SHIFT_1_2`.
const SHIFT_1_2: u32 = SHIFT_1 - SHIFT_2;
/// `UTRIE2_OMITTED_BMP_INDEX_1_LENGTH`.
const OMITTED_BMP_INDEX_1_LENGTH: u32 = 0x10000 >> SHIFT_1;
/// `UTRIE2_INDEX_2_BLOCK_LENGTH`.
const INDEX_2_BLOCK_LENGTH: u32 = 1 << SHIFT_1_2;
/// `UTRIE2_INDEX_2_MASK`.
const INDEX_2_MASK: u32 = INDEX_2_BLOCK_LENGTH - 1;
/// `UTRIE2_DATA_BLOCK_LENGTH`.
const DATA_BLOCK_LENGTH: u32 = 1 << SHIFT_2;
/// `UTRIE2_DATA_MASK`.
const DATA_MASK: u32 = DATA_BLOCK_LENGTH - 1;
/// `UTRIE2_INDEX_SHIFT`.
const INDEX_SHIFT: u32 = 2;
/// `UTRIE2_DATA_GRANULARITY`.
const DATA_GRANULARITY: u32 = 1 << INDEX_SHIFT;
/// `UTRIE2_LSCP_INDEX_2_OFFSET`.
const LSCP_INDEX_2_OFFSET: u32 = 0x10000 >> SHIFT_2;
/// `UTRIE2_LSCP_INDEX_2_LENGTH`.
const LSCP_INDEX_2_LENGTH: u32 = 0x400 >> SHIFT_2;
/// `UTRIE2_INDEX_2_BMP_LENGTH`.
const INDEX_2_BMP_LENGTH: u32 = LSCP_INDEX_2_OFFSET + LSCP_INDEX_2_LENGTH;
/// `UTRIE2_UTF8_2B_INDEX_2_LENGTH`.
const UTF8_2B_INDEX_2_LENGTH: u32 = 0x800 >> 6;
/// `UTRIE2_INDEX_1_OFFSET`.
const INDEX_1_OFFSET: u32 = INDEX_2_BMP_LENGTH + UTF8_2B_INDEX_2_LENGTH;
/// `UTRIE2_BAD_UTF8_DATA_OFFSET`.
const BAD_UTF8_DATA_OFFSET: u32 = 0x80;
/// `UTRIE2_SIG`: "Tri2".
const SIGNATURE: u32 = 0x5472_6932;
/// `UTRIE2_32_VALUE_BITS`.
const VALUE_BITS_32: u16 = 1;
/// `sizeof(UTrie2Header)`.
const HEADER_LENGTH: usize = 16;

/// A `UTrie2` with 32-bit values.
#[derive(Debug, Clone)]
pub struct UTrie2 {
    index: Vec<u16>,
    data32: Vec<u32>,
    high_start: u32,
    high_value_index: u32,
}

impl UTrie2 {
    /// `utrie2_openFromSerialized`: `None` when the bytes are not a 32-bit `UTrie2`.
    pub fn from_serialized(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_LENGTH {
            return None;
        }
        let u16_at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let signature = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if signature != SIGNATURE || (u16_at(4) & 0xf) != VALUE_BITS_32 {
            return None;
        }
        let index_length = usize::from(u16_at(6));
        let data_length = usize::from(u16_at(8)) << INDEX_SHIFT;
        let high_start = u32::from(u16_at(14)) << SHIFT_1;
        let high_value_index = (data_length as u32).checked_sub(DATA_GRANULARITY)?;
        let data_start = HEADER_LENGTH + index_length * 2;
        if bytes.len() < data_start + data_length * 4 {
            return None;
        }
        let index = bytes[HEADER_LENGTH..data_start]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect();
        let data32 = bytes[data_start..data_start + data_length * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        Some(Self {
            index,
            data32,
            high_start,
            high_value_index,
        })
    }

    /// `_UTRIE2_INDEX_RAW(offset, trieIndex, c)`.
    #[inline]
    fn index_raw(&self, offset: u32, c: u32) -> u32 {
        (u32::from(self.index[(offset + (c >> SHIFT_2)) as usize]) << INDEX_SHIFT) + (c & DATA_MASK)
    }

    /// `_UTRIE2_INDEX_FROM_SUPP`.
    #[inline]
    fn index_from_supp(&self, c: u32) -> u32 {
        let i1 =
            u32::from(self.index[((INDEX_1_OFFSET - OMITTED_BMP_INDEX_1_LENGTH) + (c >> SHIFT_1)) as usize]);
        (u32::from(self.index[(i1 + ((c >> SHIFT_2) & INDEX_2_MASK)) as usize]) << INDEX_SHIFT)
            + (c & DATA_MASK)
    }

    /// `UTRIE2_GET32(trie, c)`: the value of code point `c` (any `i32`).
    #[inline]
    pub fn get32(&self, c: i32) -> u32 {
        let c = c as u32;
        let index = if c < 0xd800 {
            self.index_raw(0, c)
        } else if c <= 0xffff {
            let offset = if c <= 0xdbff {
                LSCP_INDEX_2_OFFSET - (0xd800 >> SHIFT_2)
            } else {
                0
            };
            self.index_raw(offset, c)
        } else if c > 0x10_ffff {
            BAD_UTF8_DATA_OFFSET
        } else if c >= self.high_start {
            self.high_value_index
        } else {
            self.index_from_supp(c)
        };
        self.data32[index as usize]
    }

    /// `UTRIE2_GET32_FROM_U16_SINGLE_LEAD(trie, c)`: the value of a UTF-16 code unit, where
    /// a lead surrogate gets its code unit value.
    #[inline]
    pub fn get32_from_u16_single_lead(&self, c: u16) -> u32 {
        self.data32[self.index_raw(0, u32::from(c)) as usize]
    }

    /// `UTRIE2_GET32_FROM_SUPP(trie, c)`: the value of a supplementary code point.
    #[inline]
    pub fn get32_from_supp(&self, c: i32) -> u32 {
        let c = c as u32;
        let index = if c >= self.high_start {
            self.high_value_index
        } else {
            self.index_from_supp(c)
        };
        self.data32[index as usize]
    }
}
