//! Ports of ICU's `CollationTailoring` (`collationtailoring.h`), `CollationRoot`
//! (`collationroot.cpp`) and the bundle lookup of `CollationLoader` (`ucol_res.cpp`):
//! the collations of DuckDB's ICU 66 data, loaded once and shared.

use std::sync::OnceLock;

use super::collation_data::CollationData;
use super::collation_data_reader::{read, read_header};
use super::collation_package::CollationPackage;
use super::collation_settings::CollationSettings;
use super::locid::Locale;

/// The mappings of a tailoring: its own, or the base's when only settings are tailored.
#[derive(Debug)]
pub enum TailoringData {
    /// Data of its own (with the root as base).
    Owned(Box<CollationData>),
    /// The root's data.
    Base(&'static CollationData),
}

impl TailoringData {
    /// The collation data.
    pub fn get(&self) -> &CollationData {
        match self {
            Self::Owned(data) => data,
            Self::Base(data) => data,
        }
    }
}

/// `CollationTailoring`: the data and settings of a collator.
#[derive(Debug)]
pub struct CollationTailoring {
    /// The mappings.
    pub data: TailoringData,
    /// The default settings.
    pub settings: CollationSettings,
    /// The data version.
    pub version: [u8; 4],
}

impl CollationTailoring {
    /// The collation data.
    pub fn data(&self) -> &CollationData {
        self.data.get()
    }
}

/// `CollationRoot::getRoot()`: the root collation, from `coll/ucadata.icu`.
pub fn root() -> Option<&'static CollationTailoring> {
    static ROOT: OnceLock<Option<CollationTailoring>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let bytes = CollationPackage::get()?.root;
        let (version, header_length) = read_header(bytes)?;
        let mut root = read(None, &bytes[header_length..])?;
        root.version = version;
        Some(root)
    })
    .as_ref()
}

/// The tailoring at `index` of the package, read on first use.
fn tailoring(index: usize) -> Option<&'static CollationTailoring> {
    static TAILORINGS: OnceLock<Vec<OnceLock<Option<CollationTailoring>>>> = OnceLock::new();
    let package = CollationPackage::get()?;
    let tailorings =
        TAILORINGS.get_or_init(|| (0..package.tailorings.len()).map(|_| OnceLock::new()).collect());
    tailorings
        .get(index)?
        .get_or_init(|| read(Some(root()?), package.tailorings.get(index)?))
        .as_ref()
}

/// `CollationLoader::loadTailoring(locale)`: the collation of a locale, by its base name
/// and collation type: from the first bundle of the coll tree that exists when the base
/// name is truncated at `_` (`ures_openNoDefault`), else the root bundle. `None` for
/// `U_MISSING_RESOURCE_ERROR` (a collation type without binary data).
pub fn load_tailoring(locale: &Locale) -> Option<&'static CollationTailoring> {
    let package = CollationPackage::get()?;
    if locale.name.is_empty() || locale.name == "root" {
        return root();
    }
    // `CollationLoader`: the collation type, lowercase; "default" is no type
    let collation_type = locale
        .keyword_value("collation")
        .filter(|value| !value.eq_ignore_ascii_case("default"))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let collation_type = collation_type.as_str();
    let base_name = locale.base_name.as_str();
    let mut name = base_name;
    let bundle = loop {
        if let Some(bundle) = package.bundles.get(name) {
            break bundle;
        }
        match name.rfind('_') {
            Some(separator) => name = &name[..separator],
            None => break package.bundles.get("root")?,
        }
    };
    let index = if collation_type.is_empty() {
        Some(bundle.default)
    } else if let Some(&index) = bundle.types.get(collation_type) {
        index
    } else if !package.types.contains(&collation_type)
        && collation_type.len() > 6
        && collation_type.starts_with("search")
    {
        // `loadFromCollations`: fall back from something like "searchjl" to "search"
        bundle
            .types
            .get("search")
            .copied()
            .unwrap_or(Some(bundle.default))
    } else {
        // the type falls back to the default one
        Some(bundle.default)
    };
    tailoring(index?)
}

/// `ucol_open(tag)` for the tags of the package.
pub fn load_tag(tag: &str) -> Option<&'static CollationTailoring> {
    tailoring(*CollationPackage::get()?.tags.get(tag)?)
}
