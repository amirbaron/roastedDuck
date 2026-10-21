//! The collations of DuckDB's `icu_extension.cpp`: one `icu_collate_<locale>` function
//! per locale with collation data (computing the hex of the ICU sort key, which binary
//! comparison orders like the locale), `icu_noaccent`, and `icu_sort_key(text, locale)`.
//! The collators are ICU4X's, with its compiled CLDR data.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use icu_collator::options::{CaseLevel, CollatorOptions, Strength};
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::executor::{Str, StrOut, unary};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

/// The locales ICU 77's collation data has (`Collator::getAvailableLocales`), as DuckDB
/// names the collations (lowercase `language[_country]`).
pub(crate) const COLLATION_LOCALES: [&str; 129] = [
    "af", "am", "ar", "ar_sa", "as", "az", "be", "bg", "bn", "bo", "br", "bs", "ca", "ceb", "chr", "cs", "cy", "da",
    "de", "de_at", "dsb", "dz", "ee", "el", "en", "en_us", "eo", "es", "et", "fa", "fa_af", "ff", "fi", "fil", "fo",
    "fr", "fr_ca", "ga", "gl", "gu", "ha", "haw", "he", "he_il", "hi", "hr", "hsb", "hu", "hy", "id", "id_id", "ig",
    "is", "it", "ja", "ka", "kk", "kl", "km", "kn", "ko", "kok", "ku", "ky", "lb", "lkt", "ln", "lo", "lt", "lv",
    "mk", "ml", "mn", "mr", "ms", "mt", "my", "nb", "nb_no", "ne", "nl", "nn", "om", "or", "pa", "pa_in", "pl",
    "ps", "pt", "ro", "ru", "sa", "se", "si", "sk", "sl", "smn", "sq", "sr", "sr_ba", "sr_me", "sr_rs", "sv", "sw",
    "ta", "te", "th", "tk", "to", "tr", "ug", "uk", "ur", "uz", "vi", "wae", "wo", "xh", "yi", "yo", "yue",
    "yue_cn", "zh", "zh_cn", "zh_hk", "zh_mo", "zh_sg", "zh_tw", "zu",
];

/// `IcuBindData::FUNCTION_PREFIX`.
const FUNCTION_PREFIX: &str = "icu_collate_";

/// The tag of `icu_noaccent`: Postgres's `ignore_accents` (`und-u-ks-level1-kc-true`).
const NOACCENT_TAG: &str = "und-u-ks-level1-kc-true";

/// A collator shared by every call with the same locale.
type SharedCollator = Arc<CollatorBorrowed<'static>>;

/// The collators made so far, by locale (or tag).
fn collator_cache() -> &'static Mutex<HashMap<String, SharedCollator>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SharedCollator>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The collator of `key`, made by `create` the first time.
fn cached_collator(key: String, create: impl FnOnce() -> Result<CollatorBorrowed<'static>>) -> Result<SharedCollator> {
    let mut cache = collator_cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(collator) = cache.get(&key) {
        return Ok(Arc::clone(collator));
    }
    let collator = Arc::new(create()?);
    cache.insert(key, Arc::clone(&collator));
    Ok(collator)
}

/// The ICU4X locale of a DuckDB collation's language and country; unknown spellings fall
/// back to the root locale, like ICU's lenient `Locale(language, country)`.
fn locale_of(language: &str, country: &str) -> icu_locale_core::Locale {
    let candidates = if country.is_empty() {
        vec![language.to_owned()]
    } else {
        vec![format!("{language}-{country}"), language.to_owned()]
    };
    candidates
        .iter()
        .find_map(|candidate| icu_locale_core::Locale::try_from_str(candidate).ok())
        .unwrap_or(icu_locale_core::Locale::UNKNOWN)
}

/// `IcuBindData`: the collator of a collation.
#[derive(Clone)]
struct IcuBindData {
    collator: SharedCollator,
    language: String,
    country: String,
    tag: String,
}

impl std::fmt::Debug for IcuBindData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcuBindData")
            .field("language", &self.language)
            .field("country", &self.country)
            .field("tag", &self.tag)
            .finish_non_exhaustive()
    }
}

impl IcuBindData {
    /// `IcuBindData(string language, string country)`.
    fn for_locale(language: &str, country: &str) -> Result<Self> {
        let key = format!("{language}_{country}");
        let collator = cached_collator(key, || {
            let locale = locale_of(language, country);
            CollatorBorrowed::try_new(CollatorPreferences::from(&locale), CollatorOptions::default()).map_err(
                |error| {
                    Error::invalid_input(format!(
                        "Failed to create ICU collator: {error} (language: {language}, country: {country})"
                    ))
                },
            )
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
        let collator = cached_collator(tag.to_owned(), || {
            let mut options = CollatorOptions::default();
            if tag == NOACCENT_TAG {
                options.strength = Some(Strength::Primary);
                options.case_level = Some(CaseLevel::On);
            }
            let locale = icu_locale_core::Locale::try_from_str(tag).unwrap_or(icu_locale_core::Locale::UNKNOWN);
            CollatorBorrowed::try_new(CollatorPreferences::from(&locale), options).map_err(|error| {
                Error::invalid_input(format!("Failed to create ICU collator with tag {tag}: {error}"))
            })
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

/// `ICUCollateFunction`: the hex of the sort key of every string.
fn icu_collate_function(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";
    let info = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<IcuBindData>())
        .ok_or_else(|| Error::internal("ICU collation without its collator"))?;
    let collator = Arc::clone(&info.collator);
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    let mut buffer = Vec::new();
    unary::<Str, StrOut>(&strings, result, |input, out| {
        // create a sort key from the string
        buffer.clear();
        let Ok(()) = collator.write_sort_key_utf8_to(input, &mut buffer);
        // the key ends with a 0 byte, which is not part of the hex
        if buffer.last() == Some(&0) {
            buffer.pop();
        }
        // convert the sort key to hexadecimal
        out.reserve(buffer.len() * 2);
        for &byte in &buffer {
            out.push(HEX_TABLE[usize::from(byte / 16)]);
            out.push(HEX_TABLE[usize::from(byte % 16)]);
        }
        Ok(true)
    })
}

/// `GetICUCollateFunction(collation, tag)`: `icu_collate_<collation>(VARCHAR)`.
fn icu_collate_function_for(collation: &str, tag: &'static str) -> ScalarFunction {
    let name = format!("{FUNCTION_PREFIX}{collation}");
    let collation = collation.to_owned();
    ScalarFunction::new(name, vec![LogicalType::VARCHAR], LogicalType::VARCHAR, icu_collate_function).with_bind(
        move |_, _| {
            // `ICUCollateBind`: a tagged collator, else the collator of the name.
            let data = if tag.is_empty() {
                IcuBindData::for_name(&collation)?
            } else {
                IcuBindData::for_tag(tag)?
            };
            Ok(Some(Arc::new(data) as Arc<dyn FunctionData>))
        },
    )
}

/// The collations (`LoadInternal`): one per locale, then `icu_noaccent`.
pub(crate) fn collations() -> Vec<(String, ScalarFunction)> {
    let mut result: Vec<(String, ScalarFunction)> = COLLATION_LOCALES
        .iter()
        .map(|collation| ((*collation).to_owned(), icu_collate_function_for(collation, "")))
        .collect();
    result.push(("icu_noaccent".to_owned(), icu_collate_function_for("noaccent", NOACCENT_TAG)));
    result
}

/// `ICUSortKeyBind`: the collation must be a non-NULL constant.
fn icu_sort_key_bind(_function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let Some(Some(value)) = input.constants.get(1) else {
        return Err(Error::not_implemented(
            "ICU_SORT_KEY(VARCHAR, VARCHAR) with non-constant collation is not supported",
        ));
    };
    if input.argument_types.get(1).is_some_and(|ty| ty.id() == LogicalTypeId::Unknown) {
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
