//! The collation data of DuckDB's ICU 66, as `scripts/icu-collation-data.cpp` exports it
//! into `data/icu66_collation.bin` (the format is described there): the root collation
//! data, the tailorings, which tailoring each bundle of the coll tree and each tag loads,
//! the NFC data the collation code reads, and the locale codes `uloc_getName` replaces.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The exported data.
static DATA: &[u8] = include_bytes!("../../data/icu66_collation.bin");

/// A reader over the little-endian data.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, length: usize) -> Option<&'a [u8]> {
        let result = self.bytes.get(self.pos..self.pos.checked_add(length)?)?;
        self.pos += length;
        Some(result)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn name(&mut self) -> Option<&'a str> {
        let length = usize::from(self.u8()?);
        std::str::from_utf8(self.bytes(length)?).ok()
    }

    fn sized(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()? as usize;
        self.bytes(length)
    }
}

/// The parsed sections of the data.
#[derive(Debug)]
pub struct CollationPackage {
    /// `coll/ucadata.icu`, with its data header.
    pub root: &'static [u8],
    /// The tailorings (`RuleBasedCollator::cloneBinary`), with their data headers.
    pub tailorings: Vec<&'static [u8]>,
    /// Bundle name (`de`, `zh_Hant_TW`, `root`, ...) to tailoring index.
    pub bundles: HashMap<&'static str, usize>,
    /// Tag (`ucol_open`) to tailoring index.
    pub tags: HashMap<&'static str, usize>,
    /// Runs `(start, end, fcd16)` of non-zero FCD16 values, in code point order.
    pub fcd16_runs: Vec<(u32, u32, u16)>,
    /// Canonical decompositions (full NFD), except Hangul syllables.
    pub decompositions: Vec<(u32, Vec<u32>)>,
    /// `Normalizer2Impl::smallFCD`.
    pub small_fcd: [u8; 256],
    /// Three-letter language codes `uloc_getName` replaces, and their replacements.
    pub languages: HashMap<&'static str, &'static str>,
    /// Three-character country codes `uloc_getName` replaces, and their replacements.
    pub countries: HashMap<&'static str, &'static str>,
    /// `Collator::getAvailableLocales`, as DuckDB names the collations.
    pub available_locales: Vec<&'static str>,
}

impl CollationPackage {
    /// The package, parsed on first use; `None` if the data is malformed.
    pub fn get() -> Option<&'static Self> {
        static PACKAGE: OnceLock<Option<CollationPackage>> = OnceLock::new();
        PACKAGE.get_or_init(|| Self::parse(DATA)).as_ref()
    }

    fn parse(bytes: &'static [u8]) -> Option<Self> {
        let mut r = Reader { bytes, pos: 0 };
        if r.bytes(8)? != b"ICU66COL" {
            return None;
        }
        let root = r.sized()?;
        let count = r.u32()?;
        let tailorings = (0..count).map(|_| r.sized()).collect::<Option<Vec<_>>>()?;
        let index_map = |r: &mut Reader<'static>| -> Option<HashMap<&'static str, usize>> {
            let count = r.u32()?;
            (0..count)
                .map(|_| {
                    let name = r.name()?;
                    let index = r.u32()? as usize;
                    (index < tailorings.len()).then_some((name, index))
                })
                .collect()
        };
        let bundles = index_map(&mut r)?;
        let tags = index_map(&mut r)?;
        let count = r.u32()?;
        let fcd16_runs = (0..count)
            .map(|_| Some((r.u32()?, r.u32()?, r.u16()?)))
            .collect::<Option<Vec<_>>>()?;
        let count = r.u32()?;
        let decompositions = (0..count)
            .map(|_| {
                let c = r.u32()?;
                let length = r.u8()?;
                let mapping = (0..length).map(|_| r.u32()).collect::<Option<Vec<_>>>()?;
                Some((c, mapping))
            })
            .collect::<Option<Vec<_>>>()?;
        let small_fcd = r.bytes(256)?.try_into().ok()?;
        let code_map = |r: &mut Reader<'static>| -> Option<HashMap<&'static str, &'static str>> {
            let count = r.u32()?;
            (0..count).map(|_| Some((r.name()?, r.name()?))).collect()
        };
        let languages = code_map(&mut r)?;
        let countries = code_map(&mut r)?;
        let count = r.u32()?;
        let available_locales = (0..count).map(|_| r.name()).collect::<Option<Vec<_>>>()?;
        (r.pos == bytes.len()).then_some(Self {
            root,
            tailorings,
            bundles,
            tags,
            fcd16_runs,
            decompositions,
            small_fcd,
            languages,
            countries,
            available_locales,
        })
    }
}
