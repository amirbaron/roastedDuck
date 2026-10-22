//! The part of ICU's `RuleBasedCollator` (`rulebasedcollator.cpp`) DuckDB uses: creating
//! the collator of a locale (`Collator::createInstance`) or a tag (`ucol_open`), and its
//! sort keys (`getSortKey`).

use super::bocsu::write_identical_level_run;
use super::collation::LEVEL_SEPARATOR_BYTE;
use super::collation_iterator::{CollationIterator, FcdUtf16Text, Utf16Text};
use super::collation_keys::write_sort_key_up_to_quaternary;
use super::collation_settings::STRENGTH_IDENTICAL;
use super::collation_tailoring::{CollationTailoring, load_tag, load_tailoring};
use super::locid::locale_name;
use super::normalizer2impl::Normalizer2Impl;

/// Why a collator could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollatorError {
    /// `Locale::isBogus()`.
    BogusLocale,
    /// The collation data is missing or malformed (`U_MISSING_RESOURCE_ERROR`).
    MissingResource,
}

/// A `RuleBasedCollator` with the default settings of its tailoring.
#[derive(Debug, Clone, Copy)]
pub struct RuleBasedCollator {
    tailoring: &'static CollationTailoring,
}

impl RuleBasedCollator {
    /// `Collator::createInstance(Locale(language, country))`.
    pub fn for_locale(language: &str, country: &str) -> Result<Self, CollatorError> {
        let name = locale_name(language, country).ok_or(CollatorError::BogusLocale)?;
        let tailoring = load_tailoring(&name).ok_or(CollatorError::MissingResource)?;
        Ok(Self { tailoring })
    }

    /// `ucol_open(tag)` for the tags DuckDB opens.
    pub fn for_tag(tag: &str) -> Result<Self, CollatorError> {
        let tailoring = load_tag(tag).ok_or(CollatorError::MissingResource)?;
        Ok(Self { tailoring })
    }

    /// `getSortKey(s)`: the sort key of the UTF-16 text `s` without the terminating zero
    /// byte, appended to `sink` (`writeSortKey`).
    pub fn write_sort_key(&self, s: &[u16], sink: &mut Vec<u8>) {
        let data = self.tailoring.data();
        let settings = &self.tailoring.settings;
        let numeric = settings.is_numeric();
        let nfc_impl = Normalizer2Impl::get();
        match nfc_impl {
            Some(nfc_impl) if !settings.dont_check_fcd() => {
                let mut iter = CollationIterator::new(data, numeric, FcdUtf16Text::new(s, nfc_impl));
                write_sort_key_up_to_quaternary(&mut iter, &data.compressible_bytes, settings, sink);
            }
            _ => {
                let mut iter = CollationIterator::new(data, numeric, Utf16Text::new(s));
                write_sort_key_up_to_quaternary(&mut iter, &data.compressible_bytes, settings, sink);
            }
        }
        if settings.get_strength() == STRENGTH_IDENTICAL
            && let Some(nfc_impl) = nfc_impl
        {
            // `writeIdenticalLevel`: the NFD of the text
            let mut nfd = Vec::with_capacity(s.len());
            nfc_impl.decompose(s, &mut nfd);
            sink.push(LEVEL_SEPARATOR_BYTE);
            write_identical_level_run(0, &nfd, sink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(collator: RuleBasedCollator, s: &str) -> String {
        use std::fmt::Write;
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let mut key = Vec::new();
        collator.write_sort_key(&utf16, &mut key);
        key.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02X}");
            hex
        })
    }

    #[test]
    fn sort_keys() {
        let de = RuleBasedCollator::for_locale("de", "").unwrap();
        assert_eq!(key(de, "Äpfel"), "2A483432400145960801DC09");
        assert_eq!(key(de, "a"), "2A01050105");
        assert_eq!(key(de, "hello world"), "38324040460456464C4030010F010F");
        let noaccent = RuleBasedCollator::for_tag("und-u-ks-level1-kc-true").unwrap();
        assert_eq!(key(noaccent, "Äpfel"), "2A4834324001F4");
    }
}
