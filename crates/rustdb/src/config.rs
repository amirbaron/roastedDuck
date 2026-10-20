use rustdb_types::Value;

/// How a database file is opened (DuckDB's `AccessMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessMode {
    #[default]
    Automatic,
    ReadOnly,
    ReadWrite,
}

/// Database start-up configuration (DuckDB's `DBConfig`).
#[derive(Clone, Debug)]
pub struct Config {
    pub access_mode: AccessMode,
    /// Settings applied at start-up, by name, exactly as `SET name = value` would.
    pub options: Vec<(String, Value)>,
    /// Whether the statically linked extensions (json, parquet) are loaded at start-up
    /// (DuckDB's `DBConfig::options.load_extensions`); otherwise `LOAD` loads them.
    pub load_extensions: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            access_mode: AccessMode::default(),
            options: Vec::new(),
            load_extensions: true,
        }
    }
}

impl Config {
    #[must_use]
    pub fn with_option(mut self, name: impl Into<String>, value: Value) -> Self {
        self.options.push((name.into(), value));
        self
    }
}
