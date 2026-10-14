//! Serialization options and the storage-compatibility target.

use std::borrow::Cow;

use rustdb_common::{Error, Result};

use crate::generated::version_map::SERIALIZATION_VERSIONS;

/// The serialization format version a writer targets (DuckDB's `SerializationCompatibility`).
///
/// Properties added in a later version than the target are skipped, and some types fall
/// back to their legacy encoding, so that older DuckDB releases can read the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializationCompatibility {
    /// The targeted DuckDB release (e.g. `"v0.10.2"`), as used in error messages.
    pub duckdb_version: Cow<'static, str>,
    /// The serialization format version of that release.
    pub serialization_version: u64,
}

impl SerializationCompatibility {
    /// The version DuckDB targets unless configured otherwise (`v0.10.2`).
    pub const DEFAULT_VERSION_NAME: &'static str = "v0.10.2";

    /// Looks up a release name from `src/storage/version_map.json` (or `"latest"`).
    pub fn from_version_name(name: &str) -> Result<Self> {
        let Some(&(found, version)) = SERIALIZATION_VERSIONS.iter().find(|(n, _)| *n == name) else {
            let candidates: Vec<&str> = SERIALIZATION_VERSIONS.iter().map(|(n, _)| *n).collect();
            return Err(Error::invalid_input(format!(
                "The version string '{name}' is not a known DuckDB version, valid options are: {}",
                candidates.join(", ")
            )));
        };
        Ok(Self {
            duckdb_version: Cow::Borrowed(found),
            serialization_version: version,
        })
    }

    /// Targets a raw serialization version (DuckDB's `FromIndex`, used for attached databases).
    pub const fn from_index(serialization_version: u64) -> Self {
        Self {
            duckdb_version: Cow::Borrowed(""),
            serialization_version,
        }
    }

    /// The newest format (DuckDB's `SerializationCompatibility::Latest()`).
    pub fn latest() -> Self {
        Self::named("latest")
    }

    /// Whether a property introduced in `property_version` may be written.
    pub const fn supports(&self, property_version: u64) -> bool {
        property_version <= self.serialization_version
    }

    fn named(name: &'static str) -> Self {
        let version = SERIALIZATION_VERSIONS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(1, |(_, v)| *v);
        Self {
            duckdb_version: Cow::Borrowed(name),
            serialization_version: version,
        }
    }
}

impl Default for SerializationCompatibility {
    fn default() -> Self {
        Self::named(Self::DEFAULT_VERSION_NAME)
    }
}

/// Options shared by all serializers (DuckDB's `SerializationOptions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SerializationOptions {
    /// Write enums by name instead of by number (always on for JSON, off for binary).
    pub serialize_enum_as_string: bool,
    /// Write properties even when they hold their default value.
    pub serialize_default_values: bool,
    /// The format version to target.
    pub compatibility: SerializationCompatibility,
}

impl SerializationOptions {
    /// Whether a property introduced in serialization version `version` may be written
    /// (DuckDB's `Serializer::ShouldSerialize`).
    pub const fn should_serialize(&self, version: u64) -> bool {
        self.compatibility.supports(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_resolve() {
        assert_eq!(SerializationCompatibility::default().serialization_version, 1);
        assert_eq!(SerializationCompatibility::latest().serialization_version, 7);
        let v = SerializationCompatibility::from_version_name("v1.2.0").unwrap();
        assert_eq!(v.serialization_version, 4);
        let err = SerializationCompatibility::from_version_name("v9").unwrap_err();
        assert!(
            err.raw_message()
                .starts_with("The version string 'v9' is not a known DuckDB version")
        );
    }
}
