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

/// `ULOC_KEYWORD_BUFFER_LEN`.
const KEYWORD_BUFFER_LEN: usize = 25;
/// `ULOC_MAX_NO_KEYWORDS`.
const MAX_NO_KEYWORDS: usize = 25;

/// `_getKeywords(localeID, '@', ..., valuesToo)`: the keywords after the `@`, keys
/// lowercased without spaces, values trimmed, the first of duplicate keys, sorted by key;
/// `None` if malformed.
fn get_keywords(keywords: &[u8]) -> Option<Vec<(String, String)>> {
    let mut list: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    loop {
        // skip leading spaces
        while at(keywords, pos) == b' ' {
            pos += 1;
        }
        if pos >= keywords.len() {
            break; // handle trailing "; "
        }
        if list.len() == MAX_NO_KEYWORDS {
            return None;
        }
        let rest = &keywords[pos..];
        let equal_sign = rest.iter().position(|&c| c == b'=')?;
        let semicolon = rest.iter().position(|&c| c == b';');
        // ';' before '=' is illegal
        if semicolon.is_some_and(|semicolon| semicolon < equal_sign) || equal_sign >= KEYWORD_BUFFER_LEN {
            return None;
        }
        let key: String = rest[..equal_sign]
            .iter()
            .filter(|&&c| c != b' ')
            .map(|&c| char::from(c.to_ascii_lowercase()))
            .collect();
        if key.is_empty() {
            return None;
        }
        let mut value_start = equal_sign + 1;
        while at(rest, value_start) == b' ' {
            value_start += 1;
        }
        let value_end = semicolon.unwrap_or(rest.len());
        if value_start >= value_end {
            return None; // Premature end or zero-length value
        }
        let mut value_end = value_end;
        while rest[value_end - 1] == b' ' {
            value_end -= 1;
        }
        let value = String::from_utf8_lossy(&rest[value_start..value_end]).into_owned();
        if list.iter().all(|(existing, _)| *existing != key) {
            list.push((key, value));
        }
        match semicolon {
            Some(semicolon) => pos += semicolon + 1,
            None => break,
        }
    }
    list.sort_by(|a, b| a.0.cmp(&b.0));
    Some(list)
}

/// A `Locale`: its full ID, base name and keywords.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    /// `getName()`.
    pub name: String,
    /// `getBaseName()`: the name without keywords.
    pub base_name: String,
    /// The keywords, sorted by their lowercase keys.
    pub keywords: Vec<(String, String)>,
}

impl Locale {
    /// `getKeywordValue(name)`: keys are case-insensitive.
    pub fn keyword_value(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.keywords
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// `uloc_getName`: language, script, country and variants separated by `_`, a POSIX
/// charset or variant, and the keywords; `None` if malformed.
fn get_name(id: &[u8], package: &CollationPackage) -> Option<(String, Vec<(String, String)>)> {
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
                // `_getVariant`: up to a terminator, uppercased, with `-` as `_`
                let mut i = pos + 1;
                while !is_terminator(at(id, i)) {
                    let c = at(id, i).to_ascii_uppercase();
                    name.push(char::from(if c == b'-' { b'_' } else { c }));
                    i += 1;
                }
                if i > pos + 1 {
                    pos = i;
                }
            }
        }
    }
    // Copy POSIX-style charset specifier, if any [mr.utf8]
    if at(id, pos) == b'.' {
        while pos < id.len() && id[pos] != b'@' {
            name.push(char::from(id[pos]));
            pos += 1;
        }
    }
    let mut keywords = Vec::new();
    if let Some(at_sign) = id[pos.min(id.len())..].iter().position(|&c| c == b'@') {
        let rest = &id[pos + at_sign..];
        let equal_sign = rest.iter().position(|&c| c == b'=');
        let semicolon = rest.iter().position(|&c| c == b';');
        match equal_sign {
            // Copy POSIX-style variant, if any [mr@FOO]
            None => name.extend(rest.iter().map(|&c| char::from(c))),
            Some(equal_sign) if semicolon.is_none_or(|semicolon| semicolon > equal_sign) => {
                keywords = get_keywords(&rest[1..])?;
                name.push('@');
                let pairs: Vec<String> = keywords
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                name.push_str(&pairs.join(";"));
            }
            Some(_) => {}
        }
    }
    Some((name, keywords))
}

/// `Locale(language, country)`, or `None` if the locale is bogus.
pub fn locale(language: &str, country: &str) -> Option<Locale> {
    let package = CollationPackage::get()?;
    let mut id = language.to_owned();
    if !country.is_empty() {
        id.push('_');
        id.push_str(country);
    }
    // a NUL ends the C string
    let id = id.split('\0').next().unwrap_or_default();
    let (name, keywords) = get_name(id.as_bytes(), package)?;
    // the language field must fit `Locale::language`
    let at_sign = name.find('@');
    let language_length = match name.find('_') {
        Some(separator) if at_sign.is_none_or(|at_sign| separator < at_sign) => separator,
        _ => name.find(['@', '.']).unwrap_or(name.len()),
    };
    if language_length >= LANG_CAPACITY {
        return None;
    }
    // `initBaseName`: keywords exist if there is an `=` after the `@`
    let base_name = match (at_sign, name.find('=')) {
        (Some(at_sign), Some(equal_sign)) if at_sign < equal_sign => name[..at_sign].to_owned(),
        _ => name.clone(),
    };
    Some(Locale {
        name,
        base_name,
        keywords,
    })
}
