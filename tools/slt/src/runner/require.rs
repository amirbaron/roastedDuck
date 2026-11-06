//! `require` (port of `SQLLogicTestRunner::CheckRequire`).

use super::Runner;
use crate::backend::{Engine, ExtensionLoad, RequireSetting};
use crate::outcome::{Failure, TestResult};
use crate::parser::Parser;
use crate::sys;

/// `STANDARD_VECTOR_SIZE` of the engines under test.
const STANDARD_VECTOR_SIZE: u64 = 2048;

/// Extensions DuckDB can autoload (`AUTOLOADABLE_EXTENSIONS` in `extension_entries.hpp`).
const AUTOLOADABLE_EXTENSIONS: [&str; 26] = [
    "autocomplete",
    "avro",
    "aws",
    "azure",
    "core_functions",
    "delta",
    "ducklake",
    "encodings",
    "excel",
    "fts",
    "httpfs",
    "iceberg",
    "icu",
    "inet",
    "json",
    "motherduck",
    "mysql_scanner",
    "parquet",
    "postgres_scanner",
    "quack",
    "sqlite_scanner",
    "sqlsmith",
    "tpcds",
    "tpch",
    "ui",
    "unity_catalog",
];

/// Whether a requirement is met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequireResult {
    Present,
    Missing,
}

impl RequireResult {
    fn from_present(present: bool) -> Self {
        if present { Self::Present } else { Self::Missing }
    }
}

/// Properties of the platform the oracle was built for (compile-time checks in DuckDB).
fn platform_requirement(param: &str) -> Option<bool> {
    let mingw = cfg!(all(windows, target_env = "gnu"));
    Some(match param {
        "notmusl" => !cfg!(target_env = "musl"),
        "notmingw" => !mingw,
        "mingw" => mingw,
        "notwindows" => !cfg!(windows),
        "windows" => cfg!(windows),
        // `LDBL_MANT_DIG >= 54`: x86 extended precision or 128-bit long double (non-Apple ARM).
        "longdouble" => {
            !cfg!(windows)
                && (cfg!(any(target_arch = "x86", target_arch = "x86_64"))
                    || (cfg!(target_arch = "aarch64") && !cfg!(target_vendor = "apple")))
        }
        "64bit" => cfg!(target_pointer_width = "64"),
        // Build flavours of the reference build: no force-storage, no sanitizers, inlined strings,
        // no alternative verify, default storage version, no vector verification.
        "noforcestorage"
        | "no_force_storage"
        | "nothreadsan"
        | "strinline"
        | "no_alternative_verify"
        | "no_latest_storage"
        | "no_vector_verification" => true,
        _ => return None,
    })
}

/// `ParseExplanation`: a double-quoted explanation spanning several parameters.
fn parse_explanation(parser: &Parser, params: &[String]) -> TestResult<String> {
    let mut words = params.iter();
    let first = words.next().map_or("", String::as_str);
    let Some(rest) = first.strip_prefix('"') else {
        return Err(parser.fail("Quoted parameter should start with double quotes"));
    };
    let mut explanation = rest.to_owned();
    for word in words {
        explanation.push(' ');
        explanation.push_str(word);
        if explanation.ends_with('"') {
            explanation.pop();
            break;
        }
    }
    Ok(explanation)
}

/// `std::stoi` + `NumericCast<idx_t>` for `require <size> <n>`.
fn parse_size(text: &str) -> TestResult<u64> {
    crate::text::parse_leading_int(text)
        .filter(|value| i32::try_from(*value).is_ok())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| Failure::new(format!("stoi: invalid size '{text}'")))
}

impl<E: Engine> Runner<'_, E> {
    pub(super) fn check_require(&mut self, parser: &Parser, params: &[String]) -> TestResult<RequireResult> {
        let Some(first) = params.first() else {
            return Err(parser.fail("require requires a single parameter"));
        };
        let param = first.to_ascii_lowercase();
        if let Some(present) = platform_requirement(&param) {
            return Ok(RequireResult::from_present(present));
        }
        let second = || {
            params
                .get(1)
                .filter(|_| params.len() == 2)
                .ok_or_else(|| parser.fail(&format!("require {param} requires a parameter")))
        };
        match param.as_str() {
            "ram" => {
                let required = sys::parse_memory_limit(second()?)
                    .map_err(|error| Failure::new(format!("Parser Error: {error}")))?;
                return Ok(RequireResult::from_present(
                    sys::available_memory().is_some_and(|available| available >= required),
                ));
            }
            "disk_space" => {
                let required = sys::parse_memory_limit(second()?)
                    .map_err(|error| Failure::new(format!("Parser Error: {error}")))?;
                return Ok(RequireResult::from_present(
                    sys::available_disk_space(".").is_some_and(|available| available >= required),
                ));
            }
            "vector_size" => {
                return Ok(RequireResult::from_present(
                    STANDARD_VECTOR_SIZE >= parse_size(second()?)?,
                ));
            }
            "exact_vector_size" => {
                return Ok(RequireResult::from_present(
                    STANDARD_VECTOR_SIZE == parse_size(second()?)?,
                ));
            }
            "block_size" => {
                let required = parse_size(second()?)?;
                let block_size = self
                    .engine
                    .setting(&self.config, RequireSetting::DefaultBlockSize);
                return Ok(RequireResult::from_present(block_size == required));
            }
            "skip_reload" => {
                self.skip_reload = true;
                return Ok(RequireResult::Present);
            }
            "no_extension_autoloading" => {
                if params.len() < 2 {
                    return Err(parser.fail("require no_extension_autoloading needs an explanation string"));
                }
                let explanation = parse_explanation(parser, &params[1..])?;
                if !explanation.starts_with("EXPECTED") && !explanation.starts_with("FIXME") {
                    return Err(parser.fail(
                        "require no_extension_autoloading explanation string should begin with either 'EXPECTED' \
                         or FIXME'",
                    ));
                }
                let autoload = self
                    .engine
                    .setting(&self.config, RequireSetting::AutoloadKnownExtensions);
                return Ok(RequireResult::from_present(autoload == 0));
            }
            "allow_unsigned_extensions" => {
                let allowed = self
                    .engine
                    .setting(&self.config, RequireSetting::AllowUnsignedExtensions);
                return Ok(RequireResult::from_present(allowed != 0));
            }
            "vacuum_rebuild_indexes" => {
                let threshold = self
                    .engine
                    .setting(&self.config, RequireSetting::VacuumRebuildIndexes);
                return Ok(RequireResult::from_present(threshold > 0));
            }
            _ => {}
        }
        self.require_extension(parser, &param, first)
    }

    /// The extension part of `CheckRequire`, for the NONE autoloading test mode.
    fn require_extension(
        &mut self,
        parser: &Parser,
        name: &str,
        original: &str,
    ) -> TestResult<RequireResult> {
        if self
            .engine
            .setting(&self.config, RequireSetting::AutoloadKnownExtensions)
            != 0
        {
            // Autoloading is on: autoloadable extensions are assumed present, others cannot be
            // installed in this test mode.
            return Ok(RequireResult::from_present(
                AUTOLOADABLE_EXTENSIONS.contains(&name),
            ));
        }
        match self.load_extension(name) {
            Ok(ExtensionLoad::Loaded) => {
                self.extensions.insert(name.to_owned());
                Ok(RequireResult::Present)
            }
            Ok(ExtensionLoad::Unknown) => Err(parser.fail(&format!("unknown extension type: {original}"))),
            Ok(ExtensionLoad::NotLoaded) => Ok(RequireResult::Missing),
            Err(error) => Err(parser.fail(&format!("extension '{name}' load threw an exception: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn explanations() {
        let parser = Parser::from_text("", "t.test");
        let explanation =
            parse_explanation(&parser, &strings(&["\"EXPECTED", "this", "fails\"", "extra"])).unwrap();
        assert_eq!(explanation, "EXPECTED this fails");
        // A single quoted word keeps its closing quote, like DuckDB.
        assert_eq!(
            parse_explanation(&parser, &strings(&["\"FIXME\""])).unwrap(),
            "FIXME\""
        );
        assert!(parse_explanation(&parser, &strings(&["EXPECTED"])).is_err());
    }

    #[test]
    fn platform_flags() {
        assert_eq!(
            platform_requirement("64bit"),
            Some(cfg!(target_pointer_width = "64"))
        );
        assert_eq!(platform_requirement("notwindows"), Some(!cfg!(windows)));
        assert_eq!(platform_requirement("parquet"), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2048").unwrap(), 2048);
        assert!(parse_size("-1").is_err());
        assert!(parse_size("x").is_err());
    }
}
