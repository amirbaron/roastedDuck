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

/// A database configuration together with the extension option registry and its
/// global values (`DBConfig::options` and `DBConfig::user_settings`).
#[derive(Clone, Debug)]
pub struct ConfigurationSnapshot {
    pub(crate) config: Config,
    pub(crate) extensions: Vec<(rustdb_function::ExtensionOption, Value)>,
    /// Already validated `DBConfig` option state, not fresh setter invocations.
    pub(crate) database_options: Vec<(String, Value)>,
}

impl From<Config> for ConfigurationSnapshot {
    fn from(config: Config) -> Self {
        Self {
            config,
            extensions: Vec::new(),
            database_options: Vec::new(),
        }
    }
}

impl ConfigurationSnapshot {
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Values assigned to already registered options do not become unknown startup
    /// parameters and do not require an extension to be autoloaded.
    pub(crate) fn prepare(&self) -> Result<Self> {
        use rustdb_function::cast::ValueCast;

        let mut result = self.clone();
        let mut apply = |name: &str, value: &Value| -> Result<bool> {
            if crate::settings::canonical_option(name).is_some() {
                return Ok(false);
            }
            let Some((option, current)) = result
                .extensions
                .iter_mut()
                .find(|(option, _)| option.name.eq_ignore_ascii_case(name))
            else {
                return Ok(false);
            };
            *current = value.default_cast_as(&option.input_type).map_err(|error| {
                Error::invalid_input(format!("Failed to cast value: {}", error.raw_message()))
            })?;
            Ok(true)
        };
        let mut options = Vec::new();
        for (name, value) in &self.config.options {
            if !apply(name, value)? {
                options.push((name.clone(), value.clone()));
            }
        }
        let mut unknown = Vec::new();
        for (name, value) in &self.config.unrecognized_options {
            if !apply(name, value)? {
                unknown.push((name.clone(), value.clone()));
            }
        }
        result.config.options = options;
        result.config.unrecognized_options = unknown;
        Ok(result)
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
        let option = crate::settings::canonical_option(name);
        if let Some(option) = option {
            check_startup_assignment(option)?;
        }
        // config.cpp's DUCKDB_LOCAL custom options have no SetGlobal callback.
        // This is distinct from a generic setting whose default scope is LOCAL:
        // DBConfig can still set those generic defaults before a connection exists.
        if let Some(option) = option
            && matches!(
                option,
                "custom_profiling_settings"
                    | "debug_force_external"
                    | "enable_caching_operators"
                    | "enable_http_logging"
                    | "enable_profiling"
                    | "enable_progress_bar"
                    | "enable_progress_bar_print"
                    | "http_logging_output"
                    | "profile_output"
                    | "profiling_coverage"
                    | "profiling_mode"
                    | "progress_bar_time"
                    | "schema"
                    | "search_path"
                    | "streaming_buffer_size"
            )
        {
            return Err(Error::invalid_input(format!(
                "Could not set option \"{option}\" as a global option"
            )));
        }
        match option {
            Some("access_mode") => self.access_mode = AccessMode::parse(&value)?,
            Some(option) => {
                crate::settings::check_value(option, &value)?;
                self.options
                    .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                self.options.push((name.to_owned(), value));
            }
            None if find_extension(name, EXTENSION_SETTINGS).is_some() => {
                if let Some((_, existing)) = self
                    .options
                    .iter_mut()
                    .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
                {
                    *existing = value;
                } else {
                    self.options.push((name.to_owned(), value));
                }
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
        for (name, _) in &self.options {
            if let Some(option) = crate::settings::canonical_option(name) {
                check_startup_assignment(option)?;
            }
        }
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

/// Custom setters that need an initialized DatabaseInstance/FileSystem.
pub(crate) fn requires_database(option: &str) -> bool {
    matches!(
        option,
        "disabled_filesystems"
            | "enable_logging"
            | "logging_mode"
            | "logging_level"
            | "logging_storage"
            | "enabled_log_types"
            | "disabled_log_types"
            | "allowed_paths"
            | "allowed_directories"
            | "current_transaction_invalidation_policy"
    )
}

/// Validated values that survive a `DBConfig` copy without replaying setter callbacks;
/// filesystem registries and the log manager belong to the old instance instead.
pub(crate) fn retained_database_option(option: &str) -> bool {
    matches!(
        option,
        "allowed_paths" | "allowed_directories" | "current_transaction_invalidation_policy"
    )
}

fn check_startup_assignment(option: &str) -> Result<()> {
    if option == "current_transaction_invalidation_policy" {
        return Err(Error::invalid_input(
            "current_transaction_invalidaton_policy can only be set when there is an active client context",
        ));
    }
    if requires_database(option) {
        Err(Error::invalid_input(format!(
            "Cannot change/set {option} before the database is started"
        )))
    } else {
        Ok(())
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
