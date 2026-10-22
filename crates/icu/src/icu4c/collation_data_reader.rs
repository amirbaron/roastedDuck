//! Port of ICU's `collationdatareader.h` / `collationdatareader.cpp`
//! (`CollationDataReader::read`): a collation (the root's `ucadata.icu`, or a tailoring
//! in the format of `RuleBasedCollator::cloneBinary`) from its binary data, format
//! version 5.
//!
//! The parts of the format only the comparison and builder code use (the unsafe-backward
//! set, the fast Latin table, the root elements) are not read.

use super::collation_data::{CollationData, JAMO_CE32S_LENGTH, REORDER_CODE_FIRST};
use super::collation_tailoring::{CollationTailoring, TailoringData};
use super::utrie2::UTrie2;

/// Number of `int32_t` indexes; can be 2 if there are only options.
const IX_INDEXES_LENGTH: usize = 0;
/// Bits 31..24: numericPrimary; 23..16: fast Latin version; 15..0: options.
const IX_OPTIONS: usize = 1;
/// Array offset to the Jamo CE32s in `ce32s[]`, or <0 if none.
const IX_JAMO_CE32S_START: usize = 4;
/// Byte offset to `int32_t reorderCodes[]`.
const IX_REORDER_CODES_OFFSET: usize = 5;
/// Byte offset to `uint8_t reorderTable[]`.
const IX_REORDER_TABLE_OFFSET: usize = 6;
/// Byte offset to the collation trie.
const IX_TRIE_OFFSET: usize = 7;
/// Byte offset to `int64_t ces[]`.
const IX_CES_OFFSET: usize = 9;
/// Byte offset to `uint32_t ce32s[]`.
const IX_CE32S_OFFSET: usize = 11;
/// Byte offset to `UChar contexts[]`.
const IX_CONTEXTS_OFFSET: usize = 13;
/// Byte offset to `uint16_t scripts[]`.
const IX_SCRIPTS_OFFSET: usize = 16;
/// Byte offset to `UBool compressibleBytes[]`.
const IX_COMPRESSIBLE_BYTES_OFFSET: usize = 17;
/// The total size.
const IX_TOTAL_SIZE: usize = 19;

/// The data version of the data header (`isAcceptable`), or `None` if the header is not
/// that of little-endian collation data in format version 5; and the header length.
pub fn read_header(bytes: &[u8]) -> Option<([u8; 4], usize)> {
    if bytes.len() < 24 || bytes[2] != 0xda || bytes[3] != 0x27 {
        return None;
    }
    let header_size = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let info_size = u16::from_le_bytes([bytes[4], bytes[5]]);
    if info_size < 20
        || bytes.len() < header_size
        || bytes[8] != 0 // isBigEndian
        || bytes[9] != 0 // charsetFamily ASCII
        || &bytes[12..16] != b"UCol"
        || bytes[16] != 5
    {
        return None;
    }
    let version = [bytes[20], bytes[21], bytes[22], bytes[23]];
    Some((version, header_size))
}

/// `CollationTailoring::getUCAVersion()`.
fn uca_version(version: [u8; 4]) -> u32 {
    (u32::from(version[1]) << 4) | (u32::from(version[2]) >> 6)
}

/// `CollationDataReader::read(base, inBytes, inLength, tailoring)`: the root collation
/// (`base` is `None`; `bytes` start after the data header) or a tailoring of the root
/// (`bytes` start with the data header). `None` for malformed data.
pub fn read(base: Option<&'static CollationTailoring>, bytes: &[u8]) -> Option<CollationTailoring> {
    let mut version = [0u8; 4];
    let mut bytes = bytes;
    if let Some(base) = base {
        let (data_version, header_length) = read_header(bytes)?;
        version = data_version;
        if uca_version(base.version) != uca_version(version) {
            return None;
        }
        bytes = &bytes[header_length..];
    }

    let i32_at = |offset: usize| -> Option<i32> {
        let b = bytes.get(offset..offset + 4)?;
        Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let indexes_length = usize::try_from(i32_at(0)?).ok()?;
    if indexes_length < 2 || bytes.len() < indexes_length * 4 {
        return None;
    }
    let indexes = (0..indexes_length)
        .map(|i| i32_at(i * 4))
        .collect::<Option<Vec<i32>>>()?;
    debug_assert_eq!(indexes[IX_INDEXES_LENGTH] as usize, indexes_length);
    let index = |i: usize| -> i32 { indexes.get(i).copied().unwrap_or(-1) };
    let total_length = if indexes_length > IX_TOTAL_SIZE {
        indexes[IX_TOTAL_SIZE]
    } else if indexes_length > IX_REORDER_CODES_OFFSET {
        indexes[indexes_length - 1]
    } else {
        0
    };
    if bytes.len() < usize::try_from(total_length).ok()? {
        return None;
    }
    // The bytes of the data item that starts at indexes[i]: up to indexes[i + 1].
    let item = |i: usize| -> Option<&[u8]> {
        let offset = index(i);
        let length = index(i + 1) - offset;
        if offset < 0 || length <= 0 {
            return Some(&[]);
        }
        bytes.get(offset as usize..(offset + length) as usize)
    };

    let base_data = base.map(CollationTailoring::data);
    let mut reorder_codes: Vec<i32> = Vec::new();
    let mut reorder_ranges: Vec<u32> = Vec::new();
    let codes = item(IX_REORDER_CODES_OFFSET)?;
    if codes.len() >= 4 {
        base_data?; // the base data has no reordering of its own
        let words: Vec<i32> = codes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| i32::from_le_bytes(*w))
            .collect();
        // The reorderRanges (if any) are the trailing reorderCodes entries.
        // Range limits are stored in the upper 16 bits, and are never 0.
        let mut ranges_length = 0;
        while ranges_length < words.len()
            && (words[words.len() - ranges_length - 1] as u32 & 0xffff_0000) != 0
        {
            ranges_length += 1;
        }
        let codes_length = words.len() - ranges_length;
        reorder_codes = words[..codes_length].to_vec();
        reorder_ranges = words[codes_length..].iter().map(|&w| w as u32).collect();
    }

    // There should be a reorder table only if there are reorder codes.
    let table = item(IX_REORDER_TABLE_OFFSET)?;
    let reorder_table: Option<&[u8; 256]> = if table.len() >= 256 {
        if reorder_codes.is_empty() {
            return None; // Reordering table without reordering codes.
        }
        table[..256].try_into().ok()
    } else {
        None
    };

    let options_word = indexes[IX_OPTIONS];
    if let Some(base_data) = base_data
        && base_data.numeric_primary != (options_word as u32 & 0xff00_0000)
    {
        return None;
    }

    let trie_bytes = item(IX_TRIE_OFFSET)?;
    let mut data = if trie_bytes.len() >= 8 {
        Some(CollationData {
            trie: UTrie2::from_serialized(trie_bytes)?,
            ce32s: Vec::new(),
            ces: Vec::new(),
            contexts: Vec::new(),
            base: base_data,
            jamo_ce32s: Vec::new(),
            numeric_primary: options_word as u32 & 0xff00_0000,
            compressible_bytes: Vec::new(),
            num_scripts: 0,
            scripts_index: Vec::new(),
            script_starts: Vec::new(),
        })
    } else if base_data.is_some() {
        // Use the base data. Only the settings are tailored.
        None
    } else {
        return None; // No mappings.
    };

    let ces = item(IX_CES_OFFSET)?;
    if ces.len() >= 8 {
        data.as_mut()?.ces = ces
            .as_chunks::<8>()
            .0
            .iter()
            .map(|w| i64::from_le_bytes(*w))
            .collect();
    }

    let ce32s = item(IX_CE32S_OFFSET)?;
    if ce32s.len() >= 4 {
        data.as_mut()?.ce32s = ce32s
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
    }

    let jamo_ce32s_start = index(IX_JAMO_CE32S_START);
    if jamo_ce32s_start >= 0 {
        let data = data.as_mut()?;
        let start = jamo_ce32s_start as usize;
        data.jamo_ce32s = data.ce32s.get(start..start + JAMO_CE32S_LENGTH)?.to_vec();
    } else if let Some(data) = data.as_mut() {
        data.jamo_ce32s.clone_from(&base_data?.jamo_ce32s);
    }

    let contexts = item(IX_CONTEXTS_OFFSET)?;
    if contexts.len() >= 2 {
        data.as_mut()?.contexts = contexts
            .as_chunks::<2>()
            .0
            .iter()
            .map(|w| u16::from_le_bytes(*w))
            .collect();
    }

    let scripts = item(IX_SCRIPTS_OFFSET)?;
    if scripts.len() >= 2 {
        let data = data.as_mut()?;
        let scripts: Vec<u16> = scripts
            .as_chunks::<2>()
            .0
            .iter()
            .map(|w| u16::from_le_bytes(*w))
            .collect();
        let num_scripts = usize::from(scripts[0]);
        // There must be enough entries for both arrays, including more than two range starts.
        let index_end = 1 + num_scripts + 16;
        if scripts.len() <= index_end {
            return None;
        }
        data.num_scripts = num_scripts as i32;
        data.scripts_index = scripts[1..index_end].to_vec();
        data.script_starts = scripts[index_end..].to_vec();
        if !data.script_starts_are_valid() {
            return None;
        }
    } else if let Some(data) = data.as_mut()
        && let Some(base_data) = base_data
    {
        data.num_scripts = base_data.num_scripts;
        data.scripts_index.clone_from(&base_data.scripts_index);
        data.script_starts.clone_from(&base_data.script_starts);
    }

    let compressible = item(IX_COMPRESSIBLE_BYTES_OFFSET)?;
    if compressible.len() >= 256 {
        data.as_mut()?.compressible_bytes = compressible[..256].iter().map(|&b| b != 0).collect();
    } else if let Some(data) = data.as_mut() {
        data.compressible_bytes.clone_from(&base_data?.compressible_bytes);
    }

    let data = match data {
        Some(data) => TailoringData::Owned(Box::new(data)),
        None => TailoringData::Base(base_data?),
    };
    let initial = base.map(|base| base.settings.clone()).unwrap_or_default();
    let options = options_word & 0xffff;
    let mut settings = initial.clone();
    if !(options == initial.options && initial.variable_top != 0 && reorder_codes == initial.reorder_codes())
    {
        settings.options = options;
        // Set variableTop from options and scripts data.
        settings.variable_top = data
            .get()
            .get_last_primary_for_group(REORDER_CODE_FIRST + settings.get_max_variable());
        if settings.variable_top == 0 {
            return None;
        }
        if !reorder_codes.is_empty() {
            settings.alias_reordering(base_data?, &reorder_codes, &reorder_ranges, reorder_table)?;
        }
    }
    Some(CollationTailoring {
        data,
        settings,
        version,
    })
}
