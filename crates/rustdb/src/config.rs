use rustdb_common::{Error, Result};
use rustdb_function::extension_entries::{EXTENSION_SETTINGS, find_extension};
use rustdb_types::Value;

use crate::default_settings::SETTINGS;

/// How a database file is opened (DuckDB's `AccessMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessMode {
    #[default]
    Automatic,
    ReadOnly,
    ReadWrite,
}

impl AccessMode {
    /// `AccessModeSetting::SetGlobal`: `automatic`, `read_only` or `read_write`, in any
    /// case.
    fn parse(value: &Value) -> Result<Self> {
        let parameter = value.to_varchar()?.to_lowercase();
        match parameter.as_str() {
            "automatic" => Ok(Self::Automatic),
            "read_only" => Ok(Self::ReadOnly),
            "read_write" => Ok(Self::ReadWrite),
            _ => Err(Error::invalid_input(format!(
                "Unrecognized parameter for option ACCESS_MODE \"{parameter}\". Expected READ_ONLY or READ_WRITE."
            ))),
        }
    }
}

/// Database start-up configuration (DuckDB's `DBConfig`).
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub access_mode: AccessMode,
    /// Settings applied at start-up, by name, exactly as `SET name = value` would.
    pub options: Vec<(String, Value)>,
    /// Whether the statically linked extensions (json, parquet) are loaded at start-up
    /// (DuckDB's `DBConfig::options.load_extensions`); otherwise `LOAD` loads them.
    pub load_extensions: bool,
    /// Options set by a name no option or known extension has
    /// (`DBConfigOptions::unrecognized_options`): opening the database fails on them.
    pub unrecognized_options: Vec<(String, Value)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            access_mode: AccessMode::default(),
            options: Vec::new(),
            load_extensions: true,
            unrecognized_options: Vec::new(),
        }
    }
}

impl Config {
    #[must_use]
    pub fn with_option(mut self, name: impl Into<String>, value: Value) -> Self {
        self.options.push((name.into(), value));
        self
    }

    /// Sets option `name` (DuckDB's `DBConfig::SetOptionByName`): the value must suit the
    /// option. A name that neither an option nor a known extension has is kept as
    /// unrecognized, and the database fails to open.
    pub fn set_option_by_name(&mut self, name: &str, value: Value) -> Result<()> {
        match crate::settings::canonical_option(name) {
            Some("access_mode") => self.access_mode = AccessMode::parse(&value)?,
            Some(option) => {
                crate::settings::check_value(option, &value)?;
                self.options
                    .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                self.options.push((name.to_owned(), value));
            }
            None if find_extension(name, EXTENSION_SETTINGS).is_some() => {
                self.options.push((name.to_owned(), value));
            }
            None => {
                self.unrecognized_options
                    .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                self.unrecognized_options.push((name.to_owned(), value));
            }
        }
        Ok(())
    }

    /// The error for the unrecognized options, if any (`ThrowExtensionSetUnrecognizedOptions`).
    pub(crate) fn check_recognized(&self) -> Result<()> {
        if self.unrecognized_options.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = self
            .unrecognized_options
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        Err(Error::invalid_input(format!(
            "The following options were not recognized: {}",
            names.join(", ")
        )))
    }
}

/// The options a client can set before opening a database, as DuckDB's C API enumerates
/// them (`duckdb_get_config_flag`): DuckDB's own options, then their aliases (in alias
/// order, described like their option), then the options of the known extensions
/// (described by the extension's name).
pub fn config_flags() -> Vec<(&'static str, &'static str)> {
    let is_alias = |name: &str| SETTINGS.iter().any(|setting| setting.aliases.contains(&name));
    let mut flags: Vec<(&'static str, &'static str)> = SETTINGS
        .iter()
        .filter(|setting| {
            !is_alias(setting.name) && find_extension(setting.name, EXTENSION_SETTINGS).is_none()
        })
        .map(|setting| (setting.name, setting.description))
        .collect();
    let mut aliases: Vec<(&'static str, &'static str)> = SETTINGS
        .iter()
        .flat_map(|setting| setting.aliases.iter().map(|&alias| (alias, setting.description)))
        .collect();
    aliases.sort_unstable_by_key(|&(alias, _)| alias);
    flags.extend(aliases);
    flags.extend(EXTENSION_SETTINGS.iter().copied());
    flags
}
