//! The part of ICU's `Locale` (`locid.cpp`) and `uloc_getName` (`uloc.cpp`) a collator
//! needs: the locale ID of `Locale(language, country)`, which picks the collation bundle.
//! Keywords (`@collation=...`) and BCP 47 extensions are not supported.

use super::collation_package::CollationPackage;

/// `ULOC_LANG_CAPACITY`.
const LANG_CAPACITY: usize = 12;

/// `_isIDSeparator`.
fn is_id_separator(c: u8) -> bool {
    c == b'_' || c == b'-'
}

/// `_isTerminator`.
fn is_terminator(c: u8) -> bool {
    c == 0 || c == b'.' || c == b'@'
}

/// The byte at `i`, or the terminating 0.
fn at(id: &[u8], i: usize) -> u8 {
    id.get(i).copied().unwrap_or(0)
}

/// `ulocimp_getLanguage`: the language and the position after it.
fn get_language(id: &[u8], package: &CollationPackage) -> (String, usize) {
    let mut pos = 0;
    if id.eq_ignore_ascii_case(b"root") {
        pos = 4;
    } else if id.len() >= 3 && id[..3].eq_ignore_ascii_case(b"und") && {
        let c = at(id, 3);
        c == 0 || c == b'-' || c == b'_' || c == b'@'
    } {
        pos = 3;
    }
    let mut language = String::new();
    // if it starts with i- or x- then copy that prefix
    if matches!(at(id, pos), b'x' | b'X' | b'i' | b'I') && is_id_separator(at(id, pos + 1)) {
        language.push(char::from(at(id, pos).to_ascii_lowercase()));
        language.push('-');
        pos += 2;
    }
    // copy the language as far as possible
    while !is_terminator(at(id, pos)) && !is_id_separator(at(id, pos)) {
        language.push(char::from(at(id, pos).to_ascii_lowercase()));
        pos += 1;
    }
    if language.len() == 3
        && let Some(two_letter) = package.languages.get(language.as_str())
    {
        // convert 3 character code to 2 character code if possible
        language = (*two_letter).to_owned();
    }
    (language, pos)
}

/// `ulocimp_getScript`: a four-letter script, titlecased.
fn get_script(id: &[u8], pos: usize) -> Option<(String, usize)> {
    let mut length = 0;
    while !is_terminator(at(id, pos + length))
        && !is_id_separator(at(id, pos + length))
        && at(id, pos + length).is_ascii_alphabetic()
    {
        length += 1;
    }
    (length == 4).then(|| {
        let script = id[pos..pos + 4]
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                char::from(if i == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                })
            })
            .collect();
        (script, pos + 4)
    })
}

/// `ulocimp_getCountry`: a two- or three-character country, uppercased.
fn get_country(id: &[u8], pos: usize, package: &CollationPackage) -> Option<(String, usize)> {
    let mut length = 0;
    while !is_terminator(at(id, pos + length)) && !is_id_separator(at(id, pos + length)) {
        length += 1;
    }
    if length != 2 && length != 3 {
        return None;
    }
    let country: String = id[pos..pos + length]
        .iter()
        .map(|&c| char::from(c.to_ascii_uppercase()))
        .collect();
    // convert 3 character code to 2 character code if possible
    let country = match package.countries.get(country.as_str()) {
        Some(two_letter) if length == 3 => (*two_letter).to_owned(),
        _ => country,
    };
    Some((country, pos + length))
}

/// `uloc_getName` without keywords: language, script, country and variants, separated
/// by `_`.
fn get_name(id: &[u8], package: &CollationPackage) -> String {
    let (mut name, mut pos) = get_language(id, package);
    if is_id_separator(at(id, pos)) {
        name.push('_');
        if let Some((script, end)) = get_script(id, pos + 1) {
            name.push_str(&script);
            pos = end;
            if is_id_separator(at(id, pos)) {
                name.push('_');
            }
        }
        if is_id_separator(at(id, pos)) {
            let country = get_country(id, pos + 1, package);
            if let Some((country, end)) = &country {
                name.push_str(country);
                pos = *end;
            }
            if is_id_separator(at(id, pos)) {
                if !is_id_separator(at(id, pos + 1)) {
                    name.push('_');
                }
                // `_getVariant`: the rest, uppercased, with `-` as `_`
                let mut i = pos + 1;
                while !is_terminator(at(id, i)) {
                    let c = at(id, i).to_ascii_uppercase();
                    name.push(char::from(if c == b'-' { b'_' } else { c }));
                    i += 1;
                }
            }
        }
    }
    name
}

/// The locale ID (`getName()`) of `Locale(language, country)`, or `None` if the locale is
/// bogus.
pub fn locale_name(language: &str, country: &str) -> Option<String> {
    let package = CollationPackage::get()?;
    let mut id = language.to_owned();
    if !country.is_empty() {
        id.push('_');
        id.push_str(country);
    }
    // a NUL ends the C string
    let id = id.split('\0').next().unwrap_or_default();
    let name = get_name(id.as_bytes(), package);
    // the language field must fit `Locale::language`
    let language_length = name.find('_').unwrap_or(name.len());
    (language_length < LANG_CAPACITY).then_some(name)
}
