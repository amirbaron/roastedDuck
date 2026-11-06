//! The engine abstraction the runner drives.
//!
//! An [`Engine`] is a small "driver" over opaque handle types: the runner owns configurations,
//! databases, connections and results, and asks the engine to act on them. The operations mirror
//! the engine calls in DuckDB's own runner, so the same runner code drives both the Rust engine
//! ([`rust::RustEngine`]) and the C++ oracle ([`cpp::CppEngine`]).

pub mod cpp;
pub mod rust;

/// Outcome of loading an extension (`ExtensionLoadResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionLoad {
    Loaded,
    /// Not a known extension name.
    Unknown,
    /// A known extension that is not available in this build.
    NotLoaded,
}

/// How the runner compares a result column (see `TestResultHelper::CompareValues`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnClass {
    /// Integers, floating point and decimals: compared through a cast to the column type.
    Numeric,
    /// `BOOLEAN`: `true`/`1` and `false`/`0` are interchangeable.
    Boolean,
    /// Everything else: compared as strings only.
    Other,
}

/// Settings of the runner's configuration that `require` inspects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequireSetting {
    DefaultBlockSize,
    AutoloadKnownExtensions,
    AllowUnsignedExtensions,
    VacuumRebuildIndexes,
}

/// A materialized query result as seen by the runner.
pub trait QueryResult: Send {
    /// The full error message (`"<Kind> Error: ..."`), or `None` on success.
    fn error(&self) -> Option<&str>;
    fn column_count(&self) -> usize;
    fn row_count(&self) -> usize;
    fn column_name(&self, column: usize) -> &[u8];
    /// The column type as DuckDB prints it (e.g. `DECIMAL(18,3)`).
    fn column_type(&self, column: usize) -> String;
    fn column_class(&self, column: usize) -> ColumnClass;
}

/// A database engine.
///
/// Handles are `Send + Sync` because concurrent loops run one connection per thread while value
/// conversion uses the main connection from all of them, exactly like DuckDB's runner.
pub trait Engine: Sync {
    /// The runner's database configuration (`DBConfig`).
    type Config: Send + Sync;
    type Database: Send + Sync;
    type Connection: Send + Sync;
    type Result: QueryResult;
    /// Client state carried across `restart`.
    type Session;

    /// A short engine name for reports.
    fn name(&self) -> &'static str;

    /// What `__BUILD_DIRECTORY__` expands to.
    fn build_directory(&self) -> &str;

    /// The default test configuration (`GetTestConfig()` plus the runner's non-setting options).
    fn test_config(&self) -> Result<Self::Config, String>;

    /// `DBConfig::SetOptionByName(name, Value(value))`.
    fn set_option(&self, config: &mut Self::Config, name: &str, value: &str) -> Result<(), String>;

    /// `load ... readonly|readwrite`: sets the access mode (and temporary directory use).
    fn set_read_only(&self, config: &mut Self::Config, read_only: bool);

    /// `load <path> <rw> <version>`: the storage compatibility version for new databases.
    fn set_storage_version(&self, config: &mut Self::Config, version: &str) -> Result<(), String>;

    /// `load` without a version keeps the storage version of the database that is open.
    fn inherit_storage_version(&self, config: &mut Self::Config, database: &Self::Database);

    /// Reads a setting from the configuration (booleans are 0/1).
    fn setting(&self, config: &Self::Config, setting: RequireSetting) -> u64;

    /// Opens the database at `path` (`""` is in-memory).
    fn open(&self, path: &str, config: &Self::Config) -> Result<Self::Database, String>;

    /// `ExtensionHelper::LoadExtension`; `Err` carries the exception message.
    fn load_extension(&self, database: &Self::Database, name: &str) -> Result<ExtensionLoad, String>;

    fn connect(&self, database: &Self::Database) -> Result<Self::Connection, String>;

    /// Client options every runner connection uses (progress bar enabled but not printed).
    fn configure_test_client(&self, connection: &mut Self::Connection);

    /// Runs `sql` and materializes the result; errors are reported inside the result.
    fn query(&self, connection: &mut Self::Connection, sql: &str) -> Self::Result;

    /// Converts every value of `result` to its sqllogictest string form
    /// (`TestResultHelper::DuckDBConvertResult`), row-major, casting with `context`.
    fn render(
        &self,
        context: &Self::Connection,
        result: &mut Self::Result,
        original_sqlite_test: bool,
    ) -> Result<Vec<String>, String>;

    /// Numeric value comparison: both strings are cast to the type of `column` and compared with
    /// DuckDB's `Value::ValuesAreEqual` (approximate for `FLOAT`/`DOUBLE`).
    fn values_equal(
        &self,
        context: &Self::Connection,
        result: &Self::Result,
        column: usize,
        actual: &str,
        expected: &str,
    ) -> bool;

    /// `TestResultHelper::LoadResultFromFile`: reads a `|`-separated file with a header row as
    /// VARCHAR columns named `names`. Returns the values (row-major) and the column count, or the
    /// complete error message.
    fn load_result_file(
        &self,
        context: &Self::Connection,
        path: &str,
        names: &[String],
    ) -> Result<(Vec<String>, usize), String>;

    /// `RestartCommand` part one: copies the running database's options into `config` and
    /// captures the main connection's client state.
    fn save_session(&self, main: &Self::Connection, config: &mut Self::Config) -> Self::Session;

    /// `RestartCommand` part two: re-applies the captured client state to the new connection.
    fn restore_session(&self, main: &mut Self::Connection, session: Self::Session) -> Result<(), String>;
}
