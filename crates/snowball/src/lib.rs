//! Pure Rust Snowball matching DuckDB 1.5.6's pinned stemming algorithms.
//!
//! Algorithms are generated from verified upstream Snowball definitions. Runtime
//! strings are bytes so embedded NUL and Snowball's UTF-8 traversal are preserved.
//! No C compiler, C runtime library, or external stemmer is used by this crate.

mod algorithms;
mod runtime;

use roastedduck_common::{Error, Result, byte_format};
use runtime::SnowballEnv;

/// Canonical names returned by the pinned Snowball library, in its original order.
pub const SUPPORTED_STEMMERS: &[&str] = &[
    "arabic",
    "basque",
    "catalan",
    "danish",
    "dutch",
    "english",
    "finnish",
    "french",
    "german",
    "greek",
    "hindi",
    "hungarian",
    "indonesian",
    "irish",
    "italian",
    "lithuanian",
    "nepali",
    "norwegian",
    "porter",
    "portuguese",
    "romanian",
    "russian",
    "serbian",
    "spanish",
    "swedish",
    "tamil",
    "turkish",
];

/// Accepted aliases and their canonical algorithm names.
pub const ALIASES: &[(&str, &str)] = &[
    ("ar", "arabic"),
    ("ara", "arabic"),
    ("arabic", "arabic"),
    ("baq", "basque"),
    ("basque", "basque"),
    ("ca", "catalan"),
    ("cat", "catalan"),
    ("catalan", "catalan"),
    ("da", "danish"),
    ("dan", "danish"),
    ("danish", "danish"),
    ("de", "german"),
    ("deu", "german"),
    ("dut", "dutch"),
    ("dutch", "dutch"),
    ("el", "greek"),
    ("ell", "greek"),
    ("en", "english"),
    ("eng", "english"),
    ("english", "english"),
    ("es", "spanish"),
    ("esl", "spanish"),
    ("eu", "basque"),
    ("eus", "basque"),
    ("fi", "finnish"),
    ("fin", "finnish"),
    ("finnish", "finnish"),
    ("fr", "french"),
    ("fra", "french"),
    ("fre", "french"),
    ("french", "french"),
    ("ga", "irish"),
    ("ger", "german"),
    ("german", "german"),
    ("gle", "irish"),
    ("gre", "greek"),
    ("greek", "greek"),
    ("hi", "hindi"),
    ("hin", "hindi"),
    ("hindi", "hindi"),
    ("hu", "hungarian"),
    ("hun", "hungarian"),
    ("hungarian", "hungarian"),
    ("id", "indonesian"),
    ("ind", "indonesian"),
    ("indonesian", "indonesian"),
    ("irish", "irish"),
    ("it", "italian"),
    ("ita", "italian"),
    ("italian", "italian"),
    ("lit", "lithuanian"),
    ("lithuanian", "lithuanian"),
    ("lt", "lithuanian"),
    ("ne", "nepali"),
    ("nep", "nepali"),
    ("nepali", "nepali"),
    ("nl", "dutch"),
    ("nld", "dutch"),
    ("no", "norwegian"),
    ("nor", "norwegian"),
    ("norwegian", "norwegian"),
    ("por", "portuguese"),
    ("porter", "porter"),
    ("portuguese", "portuguese"),
    ("pt", "portuguese"),
    ("ro", "romanian"),
    ("romanian", "romanian"),
    ("ron", "romanian"),
    ("ru", "russian"),
    ("rum", "romanian"),
    ("rus", "russian"),
    ("russian", "russian"),
    ("serbian", "serbian"),
    ("spa", "spanish"),
    ("spanish", "spanish"),
    ("sr", "serbian"),
    ("srp", "serbian"),
    ("sv", "swedish"),
    ("swe", "swedish"),
    ("swedish", "swedish"),
    ("ta", "tamil"),
    ("tam", "tamil"),
    ("tamil", "tamil"),
    ("tr", "turkish"),
    ("tur", "turkish"),
    ("turkish", "turkish"),
];

/// Stem one byte string using DuckDB 1.5.6's algorithm and alias semantics.
///
/// SQL NULL propagation belongs to the scalar-function caller. `none` is the
/// identity algorithm. Other names follow Snowball's case-sensitive C-string
/// lookup, including termination at the first NUL byte.
pub fn stem(input: &[u8], algorithm: &[u8]) -> Result<Vec<u8>> {
    if algorithm == b"none" {
        return Ok(input.to_vec());
    }
    let name = algorithm.split(|&b| b == 0).next().unwrap_or_default();
    let canonical = ALIASES.iter().find(|(alias, _)| alias.as_bytes() == name)
        .map(|(_, canonical)| *canonical)
        .ok_or_else(|| Error::invalid_input(byte_format!(
            "Unrecognized stemmer '{algorithm}'. Supported stemmers are: ['{}'], or use 'none' for no stemming",
            SUPPORTED_STEMMERS.join("', '")
        )))?;
    if input.len() > i32::MAX as usize {
        return Err(Error::invalid_input(
            "Snowball input exceeds the supported byte length",
        ));
    }
    let mut env = SnowballEnv::new(input);
    match canonical {
        "arabic" => {
            algorithms::arabic_stemmer::stem(&mut env);
        }
        "basque" => {
            algorithms::basque_stemmer::stem(&mut env);
        }
        "catalan" => {
            algorithms::catalan_stemmer::stem(&mut env);
        }
        "danish" => {
            algorithms::danish_stemmer::stem(&mut env);
        }
        "dutch" => {
            algorithms::dutch_stemmer::stem(&mut env);
        }
        "english" => {
            algorithms::english_stemmer::stem(&mut env);
        }
        "finnish" => {
            algorithms::finnish_stemmer::stem(&mut env);
        }
        "french" => {
            algorithms::french_stemmer::stem(&mut env);
        }
        "german" => {
            algorithms::german_stemmer::stem(&mut env);
        }
        "greek" => {
            algorithms::greek_stemmer::stem(&mut env);
        }
        "hindi" => {
            algorithms::hindi_stemmer::stem(&mut env);
        }
        "hungarian" => {
            algorithms::hungarian_stemmer::stem(&mut env);
        }
        "indonesian" => {
            algorithms::indonesian_stemmer::stem(&mut env);
        }
        "irish" => {
            algorithms::irish_stemmer::stem(&mut env);
        }
        "italian" => {
            algorithms::italian_stemmer::stem(&mut env);
        }
        "lithuanian" => {
            algorithms::lithuanian_stemmer::stem(&mut env);
        }
        "nepali" => {
            algorithms::nepali_stemmer::stem(&mut env);
        }
        "norwegian" => {
            algorithms::norwegian_stemmer::stem(&mut env);
        }
        "porter" => {
            algorithms::porter_stemmer::stem(&mut env);
        }
        "portuguese" => {
            algorithms::portuguese_stemmer::stem(&mut env);
        }
        "romanian" => {
            algorithms::romanian_stemmer::stem(&mut env);
        }
        "russian" => {
            algorithms::russian_stemmer::stem(&mut env);
        }
        "serbian" => {
            algorithms::serbian_stemmer::stem(&mut env);
        }
        "spanish" => {
            algorithms::spanish_stemmer::stem(&mut env);
        }
        "swedish" => {
            algorithms::swedish_stemmer::stem(&mut env);
        }
        "tamil" => {
            algorithms::tamil_stemmer::stem(&mut env);
        }
        "turkish" => {
            algorithms::turkish_stemmer::stem(&mut env);
        }
        _ => return Err(Error::internal("registered Snowball algorithm is missing")),
    }
    Ok(env.current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_and_identity_boundaries() -> Result<()> {
        assert_eq!(stem(b"information", b"porter")?, b"inform");
        assert_eq!(
            stem(b"information information", b"porter")?,
            b"information inform"
        );
        assert_eq!(stem(b"information ", b"porter")?, b"information ");
        assert_eq!(stem(b"", b"porter")?, b"");
        assert_eq!(stem("🦆".as_bytes(), b"porter")?, "🦆".as_bytes());
        assert_eq!(stem(b"\x00\xffinformation", b"none")?, b"\x00\xffinformation");
        assert_eq!(stem(b"information", b"porter\x00suffix")?, b"inform");
        assert!(stem(b"information", b"none\x00suffix").is_err());
        assert!(stem(b"information", b"PORTER").is_err());
        for unavailable in [b"german2".as_slice(), b"lovins", b"kraaij_pohlmann"] {
            assert!(stem(b"information", unavailable).is_err());
        }
        Ok(())
    }

    #[test]
    fn aliases_and_arbitrary_bytes_do_not_require_string_boundaries() -> Result<()> {
        for (alias, canonical) in ALIASES {
            for input in [
                b"\x00information".as_slice(),
                b"\xff\x80\xc0\x80information",
                "привет😀".as_bytes(),
                b"",
            ] {
                assert_eq!(stem(input, alias.as_bytes())?, stem(input, canonical.as_bytes())?);
            }
        }
        Ok(())
    }
}
