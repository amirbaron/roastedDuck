//! The collations of DuckDB's `icu_extension.cpp`: one `icu_collate_<locale>` function
//! per locale with collation data (computing the hex of the ICU sort key, which binary
//! comparison orders like the locale), `icu_noaccent`, and `icu_sort_key(text, locale)`.
//! The collators are the port of ICU 66's collation runtime (`icu4c::rulebasedcollator`)
//! over DuckDB's ICU 66 collation data, so orders and sort key bytes are DuckDB's.

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_functions::scalar::executor::{Str, StrOut, unary};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use crate::icu4c::collation_package::CollationPackage;
use crate::icu4c::rulebasedcollator::{CollatorError, RuleBasedCollator, SortKeyBuffers};

/// `IcuBindData::FUNCTION_PREFIX`.
const FUNCTION_PREFIX: &str = "icu_collate_";

/// The tag of `icu_noaccent`: Postgres's `ignore_accents` (`und-u-ks-level1-kc-true`).
const NOACCENT_TAG: &str = "und-u-ks-level1-kc-true";

/// `IcuBindData`: the collator of a collation.
#[derive(Debug, Clone)]
struct IcuBindData {
    collator: RuleBasedCollator,
    language: String,
    country: String,
    tag: String,
}

impl IcuBindData {
    /// `IcuBindData(string language, string country)`.
    fn for_locale(language: &str, country: &str) -> Result<Self> {
        let collator = RuleBasedCollator::for_locale(language, country).map_err(|error| match error {
            CollatorError::BogusLocale => Error::invalid_input("Locale is bogus!?"),
            _ => Error::invalid_input(format!(
                "Failed to create ICU collator: {} (language: {language}, country: {country})",
                error.name()
            )),
        })?;
        Ok(Self {
            collator,
            language: language.to_owned(),
            country: country.to_owned(),
            tag: String::new(),
        })
    }

    /// `IcuBindData(string tag)`: a collator from a BCP 47 tag (`ucol_open`).
    fn for_tag(tag: &str) -> Result<Self> {
        let collator = RuleBasedCollator::for_tag(tag).map_err(|error| {
            Error::invalid_input(format!(
                "Failed to create ICU collator with tag {tag}: {}",
                error.name()
            ))
        })?;
        Ok(Self {
            collator,
            language: String::new(),
            country: String::new(),
            tag: tag.to_owned(),
        })
    }

    /// The collator of a `language[_country]` name (`StringUtil::Split(collation, "_")`).
    fn for_name(collation: &str) -> Result<Self> {
        let mut splits: Vec<&str> = collation.split('_').filter(|split| !split.is_empty()).collect();
        if splits.is_empty() {
            splits.push(collation);
        }
        match splits.as_slice() {
            [language] => Self::for_locale(language, ""),
            [language, country] => Self::for_locale(language, country),
            _ => Err(Error::invalid_input("Expected one or two splits")),
        }
    }
}

impl FunctionData for IcuBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.language == other.language && self.country == other.country && self.tag == other.tag
        })
    }
}

/// The two hex digits of every byte.
static HEX_PAIRS: [[u8; 2]; 256] = {
    const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";
    let mut pairs = [[0u8; 2]; 256];
    let mut byte = 0;
    while byte < 256 {
        pairs[byte] = [HEX_TABLE[byte / 16], HEX_TABLE[byte % 16]];
        byte += 1;
    }
    pairs
};

/// `UnicodeString::fromUTF8`: the UTF-16 of the text, ill-formed sequences as U+FFFD
/// (one per maximal subpart, as `String::from_utf8_lossy`).
fn utf16_from_utf8(input: &[u8], out: &mut Vec<u16>) {
    out.clear();
    let continuation = |i: usize| input.get(i).copied().filter(|&b| b & 0xc0 == 0x80);
    let mut i = 0;
    while i < input.len() {
        let lead = input[i];
        if lead < 0x80 {
            out.push(u16::from(lead));
            i += 1;
            continue;
        }
        // the range of the second byte of a well-formed sequence
        let (length, second) = match lead {
            0xc2..=0xdf => (2, 0x80..=0xbf),
            0xe0 => (3, 0xa0..=0xbf),
            0xe1..=0xec | 0xee..=0xef => (3, 0x80..=0xbf),
            0xed => (3, 0x80..=0x9f),
            0xf0 => (4, 0x90..=0xbf),
            0xf1..=0xf3 => (4, 0x80..=0xbf),
            0xf4 => (4, 0x80..=0x8f),
            _ => (0, 0..=0),
        };
        let well_formed = length != 0
            && input.get(i + 1).is_some_and(|b| second.contains(b))
            && (2..length).all(|k| continuation(i + k).is_some());
        if !well_formed {
            out.clear();
            out.extend(String::from_utf8_lossy(input).encode_utf16());
            return;
        }
        let mut c = u32::from(lead) & (0x7f >> length);
        for k in 1..length {
            c = (c << 6) | u32::from(input[i + k] & 0x3f);
        }
        if c > 0xffff {
            out.push(((c >> 10) + 0xd7c0) as u16);
            out.push(((c & 0x3ff) | 0xdc00) as u16);
        } else {
            out.push(c as u16);
        }
        i += length;
    }
}

/// `ICUCollateFunction`: the hex of the sort key of every string.
fn icu_collate_function(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<IcuBindData>())
        .ok_or_else(|| Error::internal("ICU collation without its collator"))?;
    let collator = &info.collator;
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    let mut utf16 = Vec::new();
    let mut buffer = Vec::new();
    let mut buffers = SortKeyBuffers::default();
    unary::<Str, StrOut>(&strings, result, |input, out| {
        utf16_from_utf8(input, &mut utf16);
        // create a sort key from the string (without its terminating 0 byte)
        buffer.clear();
        collator.write_sort_key(&utf16, &mut buffer, &mut buffers);
        // convert the sort key to hexadecimal
        out.reserve(buffer.len() * 2);
        for &byte in &buffer {
            out.extend_from_slice(&HEX_PAIRS[usize::from(byte)]);
        }
        Ok(true)
    })
}

/// `GetICUCollateFunction(collation, tag)`: `icu_collate_<collation>(VARCHAR)`.
fn icu_collate_function_for(collation: &str, tag: &'static str) -> ScalarFunction {
    let name = format!("{FUNCTION_PREFIX}{collation}");
    let collation = collation.to_owned();
    ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        icu_collate_function,
    )
    .with_bind(move |_, _| {
        // `ICUCollateBind`: a tagged collator, else the collator of the name.
        let data = if tag.is_empty() {
            IcuBindData::for_name(&collation)?
        } else {
            IcuBindData::for_tag(tag)?
        };
        Ok(Some(Arc::new(data) as Arc<dyn FunctionData>))
    })
}

/// The collations (`LoadInternal`): one per locale of `Collator::getAvailableLocales`
/// (lowercase `language[_country]`), then `icu_noaccent`.
pub(crate) fn collations() -> Vec<(String, ScalarFunction)> {
    let mut result: Vec<(String, ScalarFunction)> = Vec::new();
    for locale in CollationPackage::get()
        .map(|package| package.available_locales.as_slice())
        .unwrap_or_default()
    {
        let collation = locale.to_lowercase();
        if result.iter().all(|(name, _)| *name != collation) {
            let function = icu_collate_function_for(&collation, "");
            result.push((collation, function));
        }
    }
    result.push((
        "icu_noaccent".to_owned(),
        icu_collate_function_for("noaccent", NOACCENT_TAG),
    ));
    result
}

/// `ICUSortKeyBind`: the collation must be a non-NULL constant.
fn icu_sort_key_bind(
    _function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(Some(value)) = input.constants.get(1) else {
        return Err(Error::not_implemented(
            "ICU_SORT_KEY(VARCHAR, VARCHAR) with non-constant collation is not supported",
        ));
    };
    if input
        .argument_types
        .get(1)
        .is_some_and(|ty| ty.id() == LogicalTypeId::Unknown)
    {
        return Err(Error::not_implemented(
            "ICU_SORT_KEY(VARCHAR, VARCHAR) with non-constant collation is not supported",
        ));
    }
    if value.is_null() {
        return Err(Error::not_implemented(
            "ICU_SORT_KEY(VARCHAR, VARCHAR) expected a non-null collation",
        ));
    }
    let collation = value.to_varchar()?;
    Ok(Some(Arc::new(IcuBindData::for_name(&collation)?)))
}

/// `icu_sort_key(VARCHAR, VARCHAR)`.
pub(crate) fn icu_sort_key_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("icu_sort_key");
    set.add(
        ScalarFunction::new(
            "icu_sort_key",
            vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
            LogicalType::VARCHAR,
            icu_collate_function,
        )
        .with_bind(icu_sort_key_bind),
    );
    set
}

#[cfg(test)]
mod tests {
    use super::utf16_from_utf8;

    #[test]
    fn utf16_like_from_utf8_lossy() {
        let mut inputs: Vec<Vec<u8>> = vec![
            b"plain".to_vec(),
            "Äpfel 中国 ক্ষ 😀".as_bytes().to_vec(),
            vec![0xff],
            vec![0xc0, 0x80],
            vec![0xe0, 0x80, 0x80],
            vec![0xed, 0xa0, 0x80],
            vec![0xf4, 0x90, 0x80, 0x80],
            vec![0xf0, 0x9f, 0x98],
            vec![b'a', 0xe2, 0x82, b'b'],
            vec![0xf8, 0x88, 0x80, 0x80, 0x80],
        ];
        // random byte strings
        let mut seed = 0x1234_5678u32;
        for _ in 0..20_000 {
            let length = (seed % 7) as usize;
            let mut bytes = Vec::new();
            for _ in 0..length {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                bytes.push((seed >> 16) as u8);
            }
            inputs.push(bytes);
        }
        let mut out = Vec::new();
        for input in inputs {
            utf16_from_utf8(&input, &mut out);
            let expected: Vec<u16> = String::from_utf8_lossy(&input).encode_utf16().collect();
            assert_eq!(out, expected, "{input:?}");
        }
    }
}
