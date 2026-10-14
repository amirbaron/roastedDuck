//! DuckDB's serialization spec format (`src/include/duckdb/storage/serialization/*.json`),
//! as consumed by `scripts/generate_serialization.py`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::Result;

/// One class entry of a spec file. Every key is declared (`deny_unknown_fields`) so that
/// new spec features are noticed; C++-only keys (`includes`, `pointer_type`, ...) are
/// parsed but have no Rust meaning.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct SpecClass {
    pub class: String,
    /// Present on hierarchy roots: the member whose value selects the subclass.
    pub class_type: Option<String>,
    pub base: Option<String>,
    /// The `class_type` value(s) of the base that select this subclass.
    #[serde(rename = "enum")]
    pub enum_values: Option<OneOrMany>,
    #[serde(default)]
    pub members: Vec<SpecMember>,
    pub pointer_type: Option<String>,
    pub constructor: Option<Vec<String>>,
    pub constructor_method: Option<String>,
    #[serde(default)]
    pub custom_implementation: bool,
    pub custom_switch_code: Option<String>,
    pub return_type: Option<String>,
    #[serde(default)]
    pub set_parameters: Vec<String>,
    #[serde(default)]
    pub includes: Vec<String>,
    pub finalize_deserialization: Option<Vec<String>>,
}

/// A string or a list of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value.clone()],
            Self::Many(values) => values.clone(),
        }
    }
}

/// One property of a class.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct SpecMember {
    pub id: u16,
    pub name: String,
    #[serde(rename = "type")]
    pub cpp_type: String,
    pub property: Option<String>,
    pub serialize_property: Option<String>,
    pub deserialize_property: Option<String>,
    pub base: Option<String>,
    pub default: Option<serde_json::Value>,
    pub status: Option<String>,
    pub version: Option<String>,
}

/// Whether a member is still written (`status` in the spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberStatus {
    /// Written and read.
    Existing,
    /// Read for backwards compatibility, no longer written.
    ReadOnly,
    /// Skipped when read, never written.
    Deleted,
}

impl SpecMember {
    pub fn status(&self) -> Result<MemberStatus> {
        match self.status.as_deref() {
            None | Some("existing") => Ok(MemberStatus::Existing),
            Some("read_only") => Ok(MemberStatus::ReadOnly),
            Some("deleted") => Ok(MemberStatus::Deleted),
            Some(other) => Err(format!("invalid status '{other}' on member '{}'", self.name).into()),
        }
    }
}

/// Reads one spec file.
pub fn read_spec_file(path: &Path) -> Result<Vec<SpecClass>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()).into())
}

/// `src/storage/version_map.json`: release names → serialization versions.
#[derive(Debug, Clone)]
pub struct VersionMap {
    /// In file order, ending with `latest`.
    pub serialization: Vec<(String, u64)>,
}

impl VersionMap {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        let values = json
            .pointer("/serialization/values")
            .and_then(serde_json::Value::as_object)
            .ok_or("version_map.json: missing serialization.values")?;
        let serialization = values
            .iter()
            .map(|(name, version)| {
                version
                    .as_u64()
                    .map(|v| (name.clone(), v))
                    .ok_or_else(|| format!("version_map.json: bad version for {name}"))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if serialization.last().map(|(name, _)| name.as_str()) != Some("latest") {
            return Err("version_map.json: serialization versions must end in 'latest'".into());
        }
        Ok(Self { serialization })
    }

    /// The serialization version a spec `version` string maps to, following
    /// `lookup_serialization_version`: unknown (unreleased) versions map to `latest`.
    pub fn lookup(&self, version: &str) -> u64 {
        let map: BTreeMap<&str, u64> = self.serialization.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        map.get(version)
            .or_else(|| map.get("latest"))
            .copied()
            .unwrap_or(1)
    }
}
