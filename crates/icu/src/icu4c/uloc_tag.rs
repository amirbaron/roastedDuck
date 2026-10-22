//! Port of the part of ICU's `uloc_tag.cpp` and `uloc_keytype.cpp` that `uloc_getName`
//! uses for IDs with a one-character subtag (`_hasBCP47Extension`): a BCP 47 language
//! tag to an ICU locale ID (`uloc_forLanguageTag`), with the legacy keys and types of the
//! Unicode extension (`uloc_toLegacyKey`, `uloc_toLegacyType`, from keyTypeData).

use super::collation_package::CollationPackage;

/// `ULOC_FULLNAME_CAPACITY`: the buffer of `_ConvertBCP47`.
const FULLNAME_CAPACITY: usize = 157;

/// `GRANDFATHERED`: grandfathered tags and their preferred values.
const GRANDFATHERED: [(&str, &str); 26] = [
    ("art-lojban", "jbo"),
    ("en-gb-oed", "en-gb-oxendict"),
    ("i-ami", "ami"),
    ("i-bnn", "bnn"),
    ("i-hak", "hak"),
    ("i-klingon", "tlh"),
    ("i-lux", "lb"),
    ("i-navajo", "nv"),
    ("i-pwn", "pwn"),
    ("i-tao", "tao"),
    ("i-tay", "tay"),
    ("i-tsu", "tsu"),
    ("no-bok", "nb"),
    ("no-nyn", "nn"),
    ("sgn-be-fr", "sfb"),
    ("sgn-be-nl", "vgt"),
    ("sgn-ch-de", "sgg"),
    ("zh-guoyu", "cmn"),
    ("zh-hakka", "hak"),
    ("zh-min-nan", "nan"),
    ("zh-xiang", "hsn"),
    ("cel-gaulish", "xtg-x-cel-gaulish"),
    ("i-default", "en-x-i-default"),
    ("i-enochian", "und-x-i-enochian"),
    ("i-mingo", "see-x-i-mingo"),
    ("zh-min", "nan-x-zh-min"),
];

/// `REDUNDANT`: redundant tags and their preferred values.
const REDUNDANT: [(&str, &str); 26] = [
    ("sgn-br", "bzs"),
    ("sgn-co", "csn"),
    ("sgn-de", "gsg"),
    ("sgn-dk", "dsl"),
    ("sgn-es", "ssp"),
    ("sgn-fr", "fsl"),
    ("sgn-gb", "bfi"),
    ("sgn-gr", "gss"),
    ("sgn-ie", "isg"),
    ("sgn-it", "ise"),
    ("sgn-jp", "jsl"),
    ("sgn-mx", "mfs"),
    ("sgn-ni", "ncs"),
    ("sgn-nl", "dse"),
    ("sgn-no", "nsl"),
    ("sgn-pt", "psr"),
    ("sgn-se", "swl"),
    ("sgn-us", "ase"),
    ("sgn-za", "sfs"),
    ("zh-cmn", "cmn"),
    ("zh-cmn-hans", "cmn-hans"),
    ("zh-cmn-hant", "cmn-hant"),
    ("zh-gan", "gan"),
    ("zh-wuu", "wuu"),
    ("zh-yue", "yue"),
    ("ja-latn-hepburn-heploc", "ja-latn-alalc97"),
];

// The subtags `next` may be.
const LANG: u32 = 0x01;
const EXTL: u32 = 0x02;
const SCRT: u32 = 0x04;
const REGN: u32 = 0x08;
const VART: u32 = 0x10;
const EXTS: u32 = 0x20;
const EXTV: u32 = 0x40;
const PRIV: u32 = 0x80;

/// `_hasBCP47Extension(id)`: no `@`, and the shortest subtag (`getShortestSubtagLength`,
/// which does not count the last one) has one character.
pub fn has_bcp47_extension(id: &[u8]) -> bool {
    let mut length = id.len();
    let mut subtag_length = 0;
    let mut reset = true;
    for &c in id {
        if c != b'_' && c != b'-' {
            if reset {
                subtag_length = 0;
                reset = false;
            }
            subtag_length += 1;
        } else {
            if subtag_length != 0 && subtag_length < length {
                length = subtag_length;
            }
            reset = true;
        }
    }
    !id.contains(&b'@') && length == 1
}

fn is_alpha(s: &[u8]) -> bool {
    s.iter().all(u8::is_ascii_alphabetic)
}

fn is_numeric(s: &[u8]) -> bool {
    s.iter().all(u8::is_ascii_digit)
}

fn is_alphanumeric(s: &[u8], min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.iter().all(u8::is_ascii_alphanumeric)
}

/// `ultag_isLanguageSubtag`: alpha{2,8}.
fn is_language(s: &[u8]) -> bool {
    (2..=8).contains(&s.len()) && is_alpha(s)
}

/// `_isVariantSubtag`: 5*8alphanum / (DIGIT 3alphanum).
fn is_variant(s: &[u8]) -> bool {
    is_alphanumeric(s, 5, 8) || (s.len() == 4 && s[0].is_ascii_digit() && is_alphanumeric(&s[1..], 3, 3))
}

/// `_isExtensionSingleton`: an alphanumeric other than x.
fn is_extension_singleton(s: &[u8]) -> bool {
    s.len() == 1 && s[0].is_ascii_alphanumeric() && !s[0].eq_ignore_ascii_case(&b'x')
}

/// `ultag_isUnicodeLocaleKey`: alphanum alpha.
fn is_unicode_locale_key(s: &[u8]) -> bool {
    s.len() == 2 && s[0].is_ascii_alphanumeric() && s[1].is_ascii_alphabetic()
}

fn lower(s: &[u8]) -> String {
    String::from_utf8_lossy(s).to_ascii_lowercase()
}

/// `_addExtensionToList(list, ext, FALSE)`: sorted by key; `false` for a duplicate key.
fn add_extension(list: &mut Vec<(String, String)>, key: String, value: String) -> bool {
    match list.binary_search_by(|(existing, _)| existing.as_str().cmp(key.as_str())) {
        Ok(_) => false,
        Err(at) => {
            list.insert(at, (key, value));
            true
        }
    }
}

/// `ULanguageTag`.
#[derive(Debug, Default)]
struct LanguageTag {
    language: String,
    extlang: Vec<String>,
    script: String,
    region: String,
    variants: Vec<String>,
    extensions: Vec<(String, String)>,
    privateuse: String,
}

/// `ultag_parse(tag, ...)`: the well-formed start of a language tag.
fn parse(tag: &[u8]) -> LanguageTag {
    let mut t = LanguageTag::default();
    if tag.len() < 2 {
        // the input tag is too short - return empty ULanguageTag
        return t;
    }
    let starts_with = |prefix: &str| {
        tag.len() >= prefix.len() && tag[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    };
    let mut buf: Vec<u8> = tag.to_vec();
    let mut grandfathered = false;
    for (old, preferred) in GRANDFATHERED {
        if tag.len() < old.len() || (tag.len() > old.len() && tag[old.len()] != b'-') {
            continue;
        }
        if starts_with(old) {
            grandfathered = true;
            buf = preferred.as_bytes().to_vec();
            buf.extend_from_slice(&tag[old.len()..]);
            break;
        }
    }
    if !grandfathered {
        for (old, preferred) in REDUNDANT {
            if starts_with(old) && (tag.len() == old.len() || tag[old.len()] == b'-') {
                buf = preferred.as_bytes().to_vec();
                buf.extend_from_slice(&tag[old.len()..]);
                break;
            }
        }
    }

    let subtags: Vec<&[u8]> = buf.split(|&c| c == b'-').collect();
    let mut next = LANG | PRIV;
    let mut privateuse_var = false;
    // the extension being read: its key and the range of its value subtags
    let mut extension: Option<(String, Option<(usize, usize)>)> = None;
    let value_of = |range: (usize, usize)| lower(&subtags[range.0..=range.1].join(&b'-'));
    let mut i = 0;
    while i < subtags.len() {
        let subtag = subtags[i];
        i += 1;
        if next & LANG != 0 && is_language(subtag) {
            t.language = lower(subtag);
            next = SCRT | REGN | VART | EXTS | PRIV;
            if subtag.len() <= 3 {
                next |= EXTL;
            }
            continue;
        }
        if next & EXTL != 0 && subtag.len() == 3 && is_alpha(subtag) {
            t.extlang.push(lower(subtag));
            next = if t.extlang.len() < 3 {
                EXTL | SCRT | REGN | VART | EXTS | PRIV
            } else {
                SCRT | REGN | VART | EXTS | PRIV
            };
            continue;
        }
        if next & SCRT != 0 && subtag.len() == 4 && is_alpha(subtag) {
            let mut script = lower(subtag);
            script[..1].make_ascii_uppercase();
            t.script = script;
            next = REGN | VART | EXTS | PRIV;
            continue;
        }
        if next & REGN != 0
            && ((subtag.len() == 2 && is_alpha(subtag)) || (subtag.len() == 3 && is_numeric(subtag)))
        {
            t.region = lower(subtag).to_ascii_uppercase();
            next = VART | EXTS | PRIV;
            continue;
        }
        if next & VART != 0 && (is_variant(subtag) || (privateuse_var && is_alphanumeric(subtag, 1, 8))) {
            let variant = lower(subtag).to_ascii_uppercase();
            if t.variants.contains(&variant) {
                break; // duplicated variant entry
            }
            t.variants.push(variant);
            next = VART | EXTS | PRIV;
            continue;
        }
        if next & EXTS != 0 && is_extension_singleton(subtag) {
            if let Some((key, range)) = extension.take() {
                // the previous extension is incomplete, or a duplicate: stop parsing here
                let Some(range) = range else { break };
                if !add_extension(&mut t.extensions, key, value_of(range)) {
                    break;
                }
            }
            extension = Some((lower(subtag), None));
            next = EXTV;
            continue;
        }
        if next & EXTV != 0 && is_alphanumeric(subtag, 2, 8) {
            if let Some((_, range)) = &mut extension {
                let start = range.map_or(i - 1, |(start, _)| start);
                *range = Some((start, i - 1));
            }
            next = EXTS | EXTV | PRIV;
            continue;
        }
        if next & PRIV != 0 && subtag.len() == 1 && subtag[0].eq_ignore_ascii_case(&b'x') {
            if let Some((key, range)) = extension.take() {
                let Some(range) = range else { break };
                if !add_extension(&mut t.extensions, key, value_of(range)) {
                    break;
                }
            }
            if i == subtags.len() {
                break; // empty private use subtag
            }
            let start = i;
            let mut last_good = None;
            let mut lvariant = false;
            while i < subtags.len() {
                let value = subtags[i];
                i += 1;
                if value.starts_with(b"lvariant") {
                    lvariant = true;
                    break;
                } else if is_alphanumeric(value, 1, 8) {
                    last_good = Some(i - 1);
                } else {
                    break;
                }
            }
            if lvariant {
                next = VART;
                privateuse_var = true;
                continue;
            }
            if let Some(end) = last_good {
                t.privateuse = lower(&subtags[start..=end].join(&b'-'));
            }
            break;
        }
        // this subtag is illegal - quit parsing
        break;
    }
    if let Some((key, Some(range))) = extension {
        add_extension(&mut t.extensions, key, value_of(range));
    }
    t
}

/// `uloc_toLegacyKey(key)`: keyTypeData's legacy key, or a well-formed key itself.
fn to_legacy_key(key: &str, package: &CollationPackage) -> Option<String> {
    if let Some(key_type) = package.key_types.get(&key.to_ascii_lowercase()) {
        return Some(key_type.legacy.to_owned());
    }
    key.bytes()
        .all(|c| c.is_ascii_alphanumeric())
        .then(|| key.to_ascii_lowercase())
}

/// `isSpecialTypeCodepoints`: hex subtags of 4..6 digits.
fn is_special_type_codepoints(value: &str) -> bool {
    value
        .split('-')
        .all(|subtag| (4..=6).contains(&subtag.len()) && subtag.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// `isSpecialTypeReorderCode`: alpha subtags of 3..8 letters.
fn is_special_type_reorder_code(value: &str) -> bool {
    value
        .split('-')
        .all(|subtag| (3..=8).contains(&subtag.len()) && subtag.bytes().all(|c| c.is_ascii_alphabetic()))
}

/// `isSpecialTypeRgKeyValue`: two letters and four z.
fn is_special_type_rg_key_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 6
        && bytes[..2].iter().all(u8::is_ascii_alphabetic)
        && bytes[2..].iter().all(|&c| c == b'z' || c == b'Z')
}

/// `isWellFormedLegacyType`: alphanumeric runs separated by `_`, `/` or `-`.
fn is_well_formed_legacy_type(value: &str) -> bool {
    !value.is_empty()
        && value
            .split(['_', '/', '-'])
            .all(|run| !run.is_empty() && run.bytes().all(|c| c.is_ascii_alphanumeric()))
}

/// `uloc_toLegacyType(key, type)`: keyTypeData's legacy type, a special type or a
/// well-formed type itself.
fn to_legacy_type(key: &str, value: &str, package: &CollationPackage) -> Option<String> {
    if let Some(key_type) = package.key_types.get(&key.to_ascii_lowercase()) {
        if let Some(legacy) = key_type.types.get(&value.to_ascii_lowercase()) {
            return Some((*legacy).to_owned());
        }
        let special = key_type.special_types;
        if (special & 1 != 0 && is_special_type_codepoints(value))
            || (special & 2 != 0 && is_special_type_reorder_code(value))
            || (special & 4 != 0 && is_special_type_rg_key_value(value))
        {
            return Some(value.to_ascii_lowercase());
        }
    }
    is_well_formed_legacy_type(value).then(|| value.to_ascii_lowercase())
}

/// `_appendLDMLExtensionAsKeywords`: the keywords of a Unicode extension value
/// (`attribute` for its attributes, then legacy key = legacy type); `Err` for a key or
/// type without a legacy form. `posix_variant` is in: whether there are variants, out:
/// whether the extension has `va-posix` (then a variant, not a keyword).
fn append_ldml_extension_as_keywords(
    extension: &str,
    keywords: &mut Vec<(String, String)>,
    posix_variant: &mut bool,
    package: &CollationPackage,
) -> Option<()> {
    let variant_exists = *posix_variant;
    *posix_variant = false;
    let subtags: Vec<&str> = extension.split('-').collect();
    let mut local: Vec<(String, String)> = Vec::new();
    // the attributes, up to the first key
    let first_key = subtags
        .iter()
        .position(|subtag| is_unicode_locale_key(subtag.as_bytes()))
        .unwrap_or(subtags.len());
    let mut attributes: Vec<&str> = Vec::new();
    for attribute in &subtags[..first_key] {
        // duplicate attribute is ignored, causes no error
        if let Err(at) = attributes.binary_search(attribute) {
            attributes.insert(at, attribute);
        }
    }
    if !attributes.is_empty() {
        add_extension(&mut local, "attribute".to_owned(), attributes.join("-"));
    }
    // key-type pairs
    let mut i = first_key;
    while i < subtags.len() {
        let bcp_key = subtags[i];
        i += 1;
        let types_start = i;
        while i < subtags.len() && !is_unicode_locale_key(subtags[i].as_bytes()) {
            i += 1;
        }
        let key = to_legacy_key(bcp_key, package)?;
        let value = if types_start < i {
            let bcp_type = subtags[types_start..i].join("-");
            if bcp_type.len() >= 128 {
                return None; // the BCP type is too long
            }
            to_legacy_type(&key, &bcp_type, package)?
        } else {
            "yes".to_owned() // typeless - default type value is "yes"
        };
        // u-va-posix is a variant, not a keyword
        if !variant_exists && key == "va" && value == "posix" {
            *posix_variant = true;
        } else {
            // duplicate keyword is allowed, only the first is honored
            add_extension(&mut local, key, value);
        }
    }
    for (key, value) in local {
        add_extension(keywords, key, value);
    }
    Some(())
}

/// `uloc_forLanguageTag(tag)` as `_ConvertBCP47` uses it: `""` when the tag does not
/// start with a well-formed language tag (the ID stays as it is), `None` when the locale
/// is bogus (an extension without a legacy form, or an ID too long for the buffer).
pub fn for_language_tag(tag: &[u8]) -> Option<String> {
    let package = CollationPackage::get()?;
    let t = parse(tag);
    let mut id = String::new();
    let mut is_empty = true;
    // language
    let language = t.extlang.first().unwrap_or(&t.language);
    if language != "und" && !language.is_empty() {
        id.push_str(language);
        is_empty = false;
    }
    // script
    if !t.script.is_empty() {
        id.push('_');
        id.push_str(&t.script);
        is_empty = false;
    }
    // region
    if !t.region.is_empty() {
        id.push('_');
        id.push_str(&t.region);
        is_empty = false;
    }
    // variants
    if !t.variants.is_empty() {
        if t.region.is_empty() {
            id.push('_');
        }
        is_empty = false;
        for variant in &t.variants {
            id.push('_');
            id.push_str(variant);
        }
    }
    // keywords
    if !t.extensions.is_empty() || !t.privateuse.is_empty() {
        if is_empty && !t.extensions.is_empty() {
            id.push_str("und"); // need a language
        }
        // `_appendKeywords`
        let mut posix_variant = !t.variants.is_empty();
        let mut keywords: Vec<(String, String)> = Vec::new();
        for (key, value) in &t.extensions {
            if key == "u" {
                append_ldml_extension_as_keywords(value, &mut keywords, &mut posix_variant, package)?;
            } else if !add_extension(&mut keywords, key.clone(), value.clone()) {
                return None;
            }
        }
        if !t.privateuse.is_empty() && !add_extension(&mut keywords, "x".to_owned(), t.privateuse.clone()) {
            return None;
        }
        if posix_variant {
            id.push_str("_POSIX");
        }
        for (i, (key, value)) in keywords.iter().enumerate() {
            id.push(if i == 0 { '@' } else { ';' });
            id.push_str(key);
            id.push('=');
            id.push_str(value);
        }
    }
    if id.len() >= FULLNAME_CAPACITY {
        return None;
    }
    Some(id)
}
