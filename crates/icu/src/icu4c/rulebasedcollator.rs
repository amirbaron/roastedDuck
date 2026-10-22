//! The part of ICU's `RuleBasedCollator` (`rulebasedcollator.cpp`) DuckDB uses: creating
//! the collator of a locale (`Collator::createInstance`) or a tag (`ucol_open`), and its
//! sort keys (`getSortKey`).

use std::borrow::Cow;

use super::bocsu::write_identical_level_run;
use super::collation::LEVEL_SEPARATOR_BYTE;
use super::collation_data::{REORDER_CODE_FIRST, REORDER_CODE_NONE};
use super::collation_iterator::{CollationIterator, FcdUtf16Text, Utf16Text};
use super::collation_keys::{SortKeyLevels, write_sort_key_up_to_quaternary};
use super::collation_package::CollationPackage;
use super::collation_settings::{
    self as cs, BACKWARD_SECONDARY, CASE_LEVEL, CHECK_FCD, CollationSettings, NUMERIC, STRENGTH_IDENTICAL,
};
use super::collation_tailoring::{CollationTailoring, load_tag, load_tailoring};
use super::locid::{Locale, locale};
use super::normalizer2impl::Normalizer2Impl;

/// Why a collator could not be created (`UErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollatorError {
    /// `Locale::isBogus()`.
    BogusLocale,
    /// `U_ILLEGAL_ARGUMENT_ERROR`: a malformed collation keyword.
    IllegalArgument,
    /// `U_UNSUPPORTED_ERROR`: a deprecated collation keyword.
    Unsupported,
    /// `U_MISSING_RESOURCE_ERROR`: the collation has no data.
    MissingResource,
}

impl CollatorError {
    /// `u_errorName`.
    pub fn name(self) -> &'static str {
        match self {
            Self::BogusLocale | Self::IllegalArgument => "U_ILLEGAL_ARGUMENT_ERROR",
            Self::Unsupported => "U_UNSUPPORTED_ERROR",
            Self::MissingResource => "U_MISSING_RESOURCE_ERROR",
        }
    }
}

/// `UColAttribute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attribute {
    FrenchCollation,
    AlternateHandling,
    CaseFirst,
    CaseLevel,
    NormalizationMode,
    Strength,
    NumericCollation,
}

/// `UColAttributeValue`: `UCOL_PRIMARY`..`UCOL_IDENTICAL`, `UCOL_OFF`, `UCOL_ON`, ...
const OFF: i32 = 16;
const ON: i32 = 17;
const SHIFTED: i32 = 20;
const NON_IGNORABLE: i32 = 21;
const LOWER_FIRST: i32 = 24;
const UPPER_FIRST: i32 = 25;

/// `collAttributes`: the locale keywords of the attributes.
const COLL_ATTRIBUTES: [(&str, Attribute); 7] = [
    ("colStrength", Attribute::Strength),
    ("colBackwards", Attribute::FrenchCollation),
    ("colCaseLevel", Attribute::CaseLevel),
    ("colCaseFirst", Attribute::CaseFirst),
    ("colAlternate", Attribute::AlternateHandling),
    ("colNormalization", Attribute::NormalizationMode),
    ("colNumeric", Attribute::NumericCollation),
];

/// `collAttributeValues`.
const COLL_ATTRIBUTE_VALUES: [(&str, i32); 11] = [
    ("primary", 0),
    ("secondary", 1),
    ("tertiary", 2),
    ("quaternary", 3),
    ("identical", 15),
    ("no", OFF),
    ("yes", ON),
    ("shifted", SHIFTED),
    ("non-ignorable", NON_IGNORABLE),
    ("lower", LOWER_FIRST),
    ("upper", UPPER_FIRST),
];

/// `collReorderCodes`: the special reorder codes from `UCOL_REORDER_CODE_FIRST`.
const COLL_REORDER_CODES: [&str; 5] = ["space", "punct", "symbol", "currency", "digit"];

/// `getReorderCode(s)`.
fn get_reorder_code(s: &str) -> Option<i32> {
    COLL_REORDER_CODES
        .iter()
        .position(|code| code.eq_ignore_ascii_case(s))
        .map(|i| REORDER_CODE_FIRST + i as i32)
}

/// `u_getPropertyValueEnum(UCHAR_SCRIPT, name)`: names match ignoring case, spaces, `-`
/// and `_` (`uprv_compareASCIIPropertyNames`).
fn script_code(name: &str) -> Option<i32> {
    fn normalized(name: &str) -> String {
        name.chars()
            .filter(|c| !matches!(c, ' ' | '-' | '_' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'))
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
    let name = normalized(name);
    CollationPackage::get()?
        .scripts
        .iter()
        .find(|(script, _)| normalized(script) == name)
        .map(|&(_, code)| code)
}

/// A `RuleBasedCollator`: a tailoring and its settings (the tailoring's default ones,
/// unless keywords change them).
#[derive(Debug, Clone)]
pub struct RuleBasedCollator {
    tailoring: &'static CollationTailoring,
    settings: Cow<'static, CollationSettings>,
}

impl RuleBasedCollator {
    /// `Collator::createInstance(Locale(language, country))`.
    pub fn for_locale(language: &str, country: &str) -> Result<Self, CollatorError> {
        let locale = locale(language, country).ok_or(CollatorError::BogusLocale)?;
        let tailoring = load_tailoring(&locale).ok_or(CollatorError::MissingResource)?;
        let mut collator = Self::new(tailoring);
        collator.set_attributes_from_keywords(&locale)?;
        Ok(collator)
    }

    /// `ucol_open(tag)` for the tags DuckDB opens.
    pub fn for_tag(tag: &str) -> Result<Self, CollatorError> {
        let tailoring = load_tag(tag).ok_or(CollatorError::MissingResource)?;
        Ok(Self::new(tailoring))
    }

    fn new(tailoring: &'static CollationTailoring) -> Self {
        Self {
            tailoring,
            settings: Cow::Borrowed(&tailoring.settings),
        }
    }

    /// `setAttributesFromKeywords(loc, coll)`: the collation attributes of the locale's
    /// keywords (UTS #35 old locale extension syntax).
    fn set_attributes_from_keywords(&mut self, locale: &Locale) -> Result<(), CollatorError> {
        if locale.name == locale.base_name {
            return Ok(()); // No keywords.
        }
        // `getKeywordValue` into a buffer of 1024 chars
        let value_of = |name: &str| -> Result<Option<&str>, CollatorError> {
            match locale.keyword_value(name) {
                Some(value) if value.len() >= 1024 => Err(CollatorError::IllegalArgument),
                value => Ok(value),
            }
        };
        // Check for collation keywords that were already deprecated
        // before any were supported in createInstance() (except for "collation").
        if value_of("colHiraganaQuaternary")?.is_some() || value_of("variableTop")?.is_some() {
            return Err(CollatorError::Unsupported);
        }
        // Parse known collation keywords, ignore others.
        for (name, attribute) in COLL_ATTRIBUTES {
            let Some(value) = value_of(name)? else {
                continue;
            };
            let value = COLL_ATTRIBUTE_VALUES
                .iter()
                .find(|(value_name, _)| value_name.eq_ignore_ascii_case(value))
                .ok_or(CollatorError::IllegalArgument)?
                .1;
            self.set_attribute(attribute, value)?;
        }
        if let Some(value) = value_of("colReorder")? {
            let mut codes = Vec::new();
            for script_name in value.split('-') {
                // Strict parsing, accept only 4-letter script codes, not long names.
                let code = if script_name.len() == 4 {
                    script_code(script_name)
                } else {
                    get_reorder_code(script_name)
                };
                codes.push(code.ok_or(CollatorError::IllegalArgument)?);
            }
            self.set_reorder_codes(&codes)?;
        }
        if let Some(value) = value_of("kv")? {
            let code = get_reorder_code(value).ok_or(CollatorError::IllegalArgument)?;
            self.set_max_variable(code)?;
        }
        Ok(())
    }

    /// `getAttribute(attr)`.
    fn get_attribute(&self, attribute: Attribute) -> i32 {
        let flag = |option: i32| {
            if self.settings.options & option == 0 {
                OFF
            } else {
                ON
            }
        };
        match attribute {
            Attribute::FrenchCollation => flag(BACKWARD_SECONDARY),
            Attribute::AlternateHandling => {
                if self.settings.options & cs::ALTERNATE_MASK == 0 {
                    NON_IGNORABLE
                } else {
                    SHIFTED
                }
            }
            Attribute::CaseFirst => match self.settings.options & (cs::CASE_FIRST | cs::UPPER_FIRST) {
                0 => OFF,
                cs::CASE_FIRST => LOWER_FIRST,
                _ => UPPER_FIRST,
            },
            Attribute::CaseLevel => flag(CASE_LEVEL),
            Attribute::NormalizationMode => flag(CHECK_FCD),
            Attribute::Strength => self.settings.get_strength(),
            Attribute::NumericCollation => flag(NUMERIC),
        }
    }

    /// `setAttribute(attr, value)` for the values of keywords (never `UCOL_DEFAULT`).
    fn set_attribute(&mut self, attribute: Attribute, value: i32) -> Result<(), CollatorError> {
        if value == self.get_attribute(attribute) {
            return Ok(());
        }
        let settings = self.settings.to_mut();
        let set_flag = |settings: &mut CollationSettings, bit: i32| match value {
            ON => {
                settings.options |= bit;
                Ok(())
            }
            OFF => {
                settings.options &= !bit;
                Ok(())
            }
            _ => Err(CollatorError::IllegalArgument),
        };
        match attribute {
            Attribute::FrenchCollation => set_flag(settings, BACKWARD_SECONDARY),
            Attribute::AlternateHandling => {
                let no_alternate = settings.options & !cs::ALTERNATE_MASK;
                settings.options = match value {
                    NON_IGNORABLE => no_alternate,
                    SHIFTED => no_alternate | cs::SHIFTED,
                    _ => return Err(CollatorError::IllegalArgument),
                };
                Ok(())
            }
            Attribute::CaseFirst => {
                let no_case_first = settings.options & !(cs::CASE_FIRST | cs::UPPER_FIRST);
                settings.options = match value {
                    OFF => no_case_first,
                    LOWER_FIRST => no_case_first | cs::CASE_FIRST,
                    UPPER_FIRST => no_case_first | cs::CASE_FIRST | cs::UPPER_FIRST,
                    _ => return Err(CollatorError::IllegalArgument),
                };
                Ok(())
            }
            Attribute::CaseLevel => set_flag(settings, CASE_LEVEL),
            Attribute::NormalizationMode => set_flag(settings, CHECK_FCD),
            Attribute::Strength => {
                if !matches!(value, 0..=3 | STRENGTH_IDENTICAL) {
                    return Err(CollatorError::IllegalArgument);
                }
                settings.options = (settings.options & !cs::STRENGTH_MASK) | (value << cs::STRENGTH_SHIFT);
                Ok(())
            }
            Attribute::NumericCollation => set_flag(settings, NUMERIC),
        }
    }

    /// `setReorderCodes(codes)`.
    fn set_reorder_codes(&mut self, codes: &[i32]) -> Result<(), CollatorError> {
        let codes = if codes == [REORDER_CODE_NONE] {
            &[][..]
        } else {
            codes
        };
        if codes == self.settings.reorder_codes() {
            return Ok(());
        }
        let data = self.tailoring.data();
        self.settings
            .to_mut()
            .set_reordering(data, codes)
            .ok_or(CollatorError::IllegalArgument)
    }

    /// `setMaxVariable(group)`.
    fn set_max_variable(&mut self, group: i32) -> Result<(), CollatorError> {
        // Convert the reorder code into a MaxVariable number.
        let value = group - REORDER_CODE_FIRST;
        if !(0..=3).contains(&value) {
            return Err(CollatorError::IllegalArgument);
        }
        if value == self.settings.get_max_variable() {
            return Ok(());
        }
        let variable_top = self.tailoring.data().get_last_primary_for_group(group);
        let settings = self.settings.to_mut();
        settings.options = (settings.options & !cs::MAX_VARIABLE_MASK) | (value << cs::MAX_VARIABLE_SHIFT);
        settings.variable_top = variable_top;
        Ok(())
    }

    /// `getSortKey(s)`: the sort key of the UTF-16 text `s` without the terminating zero
    /// byte, appended to `sink` (`writeSortKey`), with the buffers of previous keys.
    pub fn write_sort_key(&self, s: &[u16], sink: &mut Vec<u8>, buffers: &mut SortKeyBuffers) {
        let data = self.tailoring.data();
        let settings = self.settings.as_ref();
        let numeric = settings.is_numeric();
        let nfc_impl = Normalizer2Impl::get();
        let ces = std::mem::take(&mut buffers.ces);
        buffers.ces = match nfc_impl {
            Some(nfc_impl) if !settings.dont_check_fcd() => {
                let mut iter = CollationIterator::new(data, numeric, FcdUtf16Text::new(s, nfc_impl), ces);
                write_sort_key_up_to_quaternary(
                    &mut iter,
                    &data.compressible_bytes,
                    settings,
                    sink,
                    &mut buffers.levels,
                );
                iter.into_ce_buffer()
            }
            _ => {
                let mut iter = CollationIterator::new(data, numeric, Utf16Text::new(s), ces);
                write_sort_key_up_to_quaternary(
                    &mut iter,
                    &data.compressible_bytes,
                    settings,
                    sink,
                    &mut buffers.levels,
                );
                iter.into_ce_buffer()
            }
        };
        if settings.get_strength() == STRENGTH_IDENTICAL
            && let Some(nfc_impl) = nfc_impl
        {
            // `writeIdenticalLevel`: the NFD of the text
            nfc_impl.decompose(s, &mut buffers.nfd);
            sink.push(LEVEL_SEPARATOR_BYTE);
            write_identical_level_run(0, &buffers.nfd, sink);
        }
    }
}

/// The buffers of the sort key writer, kept from one key to the next.
#[derive(Debug, Default)]
pub struct SortKeyBuffers {
    ces: Vec<i64>,
    levels: SortKeyLevels,
    nfd: Vec<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(collator: &RuleBasedCollator, s: &str) -> String {
        use std::fmt::Write;
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let mut key = Vec::new();
        collator.write_sort_key(&utf16, &mut key, &mut SortKeyBuffers::default());
        key.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02X}");
            hex
        })
    }

    #[test]
    fn sort_keys() {
        let de = RuleBasedCollator::for_locale("de", "").unwrap();
        assert_eq!(key(&de, "Äpfel"), "2A483432400145960801DC09");
        assert_eq!(key(&de, "a"), "2A01050105");
        assert_eq!(key(&de, "hello world"), "38324040460456464C4030010F010F");
        let noaccent = RuleBasedCollator::for_tag("und-u-ks-level1-kc-true").unwrap();
        assert_eq!(key(&noaccent, "Äpfel"), "2A4834324001F4");
        let phonebook = RuleBasedCollator::for_locale("de@collation=phonebook", "").unwrap();
        assert_eq!(key(&phonebook, "Äpfel"), "2A32483432400145480801DCC608");
        let numeric = RuleBasedCollator::for_locale("en@colNumeric=yes;colStrength=identical", "").unwrap();
        assert_eq!(key(&numeric, "a10"), "2A0F0C010601060101926261");
    }
}
