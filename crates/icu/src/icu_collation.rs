//! The collations of DuckDB's `icu_extension.cpp`: one `icu_collate_<locale>` function
//! per locale with collation data (computing the hex of the ICU sort key, which binary
//! comparison orders like the locale), `icu_noaccent`, and `icu_sort_key(text, locale)`.
//! The collators are the port of ICU 66's collation runtime (`icu4c::rulebasedcollator`)
//! over DuckDB's ICU 66 collation data, so orders and sort key bytes are DuckDB's.

use std::any::Any;
use std::borrow::Cow;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::executor::{Str, StrOut, unary};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use crate::icu4c::collation_package::CollationPackage;
use crate::icu4c::rulebasedcollator::{CollatorError, RuleBasedCollator};

/// `IcuBindData::FUNCTION_PREFIX`.
const FUNCTION_PREFIX: &str = "icu_collate_";

/// The tag of `icu_noaccent`: Postgres's `ignore_accents` (`und-u-ks-level1-kc-true`).
const NOACCENT_TAG: &str = "und-u-ks-level1-kc-true";

/// `u_errorName` of a collator creation failure.
fn error_name(error: CollatorError) -> &'static str {
    match error {
        CollatorError::BogusLocale => "U_ILLEGAL_ARGUMENT_ERROR",
        CollatorError::MissingResource => "U_MISSING_RESOURCE_ERROR",
    }
}

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
            CollatorError::MissingResource => Error::invalid_input(format!(
                "Failed to create ICU collator: {} (language: {language}, country: {country})",
                error_name(error)
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
                error_name(error)
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

/// `ICUCollateFunction`: the hex of the sort key of every string.
fn icu_collate_function(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";
    let info = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<IcuBindData>())
        .ok_or_else(|| Error::internal("ICU collation without its collator"))?;
    let collator = info.collator;
    let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
    let mut utf16 = Vec::new();
    let mut buffer = Vec::new();
    unary::<Str, StrOut>(&strings, result, |input, out| {
        // `UnicodeString::fromUTF8`: ill-formed sequences become U+FFFD
        let text = match std::str::from_utf8(input) {
            Ok(text) => Cow::Borrowed(text),
            Err(_) => String::from_utf8_lossy(input),
        };
        utf16.clear();
        utf16.extend(text.encode_utf16());
        // create a sort key from the string (without its terminating 0 byte)
        buffer.clear();
        collator.write_sort_key(&utf16, &mut buffer);
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
