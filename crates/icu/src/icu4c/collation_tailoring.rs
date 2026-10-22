//! Ports of ICU's `CollationTailoring` (`collationtailoring.h`), `CollationRoot`
//! (`collationroot.cpp`) and the bundle lookup of `CollationLoader` (`ucol_res.cpp`):
//! the collations of DuckDB's ICU 66 data, loaded once and shared.

use std::sync::OnceLock;

use super::collation_data::CollationData;
use super::collation_data_reader::{read, read_header};
use super::collation_package::CollationPackage;
use super::collation_settings::CollationSettings;

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

/// `CollationLoader::loadTailoring(locale)`: the collation of a locale ID, from the first
/// bundle of the coll tree that exists when the ID is truncated at `_` (`ures_openNoDefault`),
/// else the root's.
pub fn load_tailoring(locale_name: &str) -> Option<&'static CollationTailoring> {
    let package = CollationPackage::get()?;
    let mut name = locale_name;
    loop {
        if let Some(&index) = package.bundles.get(name) {
            return tailoring(index);
        }
        match name.rfind('_') {
            Some(separator) => name = &name[..separator],
            None => break,
        }
    }
    tailoring(*package.bundles.get("root")?)
}

/// `ucol_open(tag)` for the tags of the package.
pub fn load_tag(tag: &str) -> Option<&'static CollationTailoring> {
    let package = CollationPackage::get()?;
    tailoring(*package.tags.get(tag)?)
}
