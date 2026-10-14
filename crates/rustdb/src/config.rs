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
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub access_mode: AccessMode,
    /// Settings applied at start-up, by name, exactly as `SET name = value` would.
    pub options: Vec<(String, Value)>,
}

impl Config {
    #[must_use]
    pub fn with_option(mut self, name: impl Into<String>, value: Value) -> Self {
        self.options.push((name.into(), value));
        self
    }
}
