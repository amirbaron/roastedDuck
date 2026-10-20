use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{SetScope, SqlStatement, TransactionType};
use rustdb_catalog::{Catalog, Transaction, TransactionManager};
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_execution::operators::{CollectedResult, PhysicalResultCollector};
use rustdb_execution::{Executor, PhysicalRef, PlanContext, ScalarEvaluator, create_physical_plan_with};
use rustdb_function::cast::{CastFunctionSet, ValueCast};
use rustdb_optimizer::Optimizer;
use rustdb_planner::{
    ClientContext, ColumnBindingResolver, LogicalKind, LogicalOperator, PlannedStatement, Planner,
    PreparedStatementData,
};
use rustdb_types::{LogicalType, Value};

use crate::attached::{AttachOptions, AttachedDatabase, DatabaseManager};
use crate::settings::Settings;
use crate::transaction::TransactionContext;
use crate::{Config, MaterializedResult};

/// Outcome of loading an extension by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionLoad {
    Loaded,
    /// A known DuckDB extension that this build does not provide.
    NotAvailable,
    /// Not a known extension name.
    Unknown,
}

/// Shared state of one database (DuckDB's `DatabaseInstance`).
#[derive(Debug)]
pub(crate) struct DatabaseInstance {
    path: String,
    config: Config,
    pub(crate) transactions: Arc<TransactionManager>,
    /// Builtin functions and system objects (`system.main`); read by the binder.
    pub(crate) system_catalog: Arc<Catalog>,
    /// The database opened at start-up and every attached one.
    pub(crate) databases: DatabaseManager,
    /// The name of the database opened at start-up (the default database).
    default_database: std::sync::OnceLock<String>,
    /// The cast functions (DuckDB's `DBConfig::GetCastFunctions`).
    pub(crate) casts: Arc<CastFunctionSet>,
    /// Buffer manager and thread count used by every query.
    pub(crate) env: Arc<rustdb_execution::ClientEnv>,
    /// Set by `PRAGMA disable_checkpoint_on_shutdown`.
    pub(crate) shutdown_checkpoint_disabled: std::sync::atomic::AtomicBool,
    /// A commit that grows the WAL beyond this many bytes checkpoints
    /// (`checkpoint_threshold`, DuckDB's `checkpoint_wal_size`).
    pub(crate) checkpoint_wal_size: std::sync::atomic::AtomicU64,
    /// How checkpoints compress segments (`force_compression`, a global setting): applied
    /// to the storage of every attached database.
    compression: std::sync::Mutex<rustdb_storage::compression::CompressionConfig>,
    /// The secrets (DuckDB's `DBConfig::secret_manager`).
    pub(crate) secrets: Arc<crate::secrets::SecretManager>,
    /// The number of open connections (DuckDB's `ConnectionManager`).
    connections: Arc<std::sync::atomic::AtomicU64>,
    /// The object cache (DuckDB's `DatabaseInstance::object_cache`).
    object_cache: Arc<rustdb_function::ObjectCache>,
    /// The log manager (DuckDB's `DatabaseInstance::GetLogManager`).
    pub(crate) log_manager: Arc<crate::logging::LogManager>,
    /// The options set globally (DuckDB's `DBConfig` user settings), with a version
    /// that changes on every global `SET` / `RESET`; connections apply them unless they
    /// set the option themselves.
    global_settings: Mutex<GlobalSettings>,
    /// The statically linked extensions whose catalog entries are registered.
    loaded_extensions: Mutex<Vec<StaticExtension>>,
}

/// An extension linked into the engine whose functions are registered on load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaticExtension {
    Parquet,
    Json,
}

impl StaticExtension {
    const ALL: [Self; 2] = [Self::Parquet, Self::Json];

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "parquet" => Some(Self::Parquet),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// Registers the extension's functions, types and macros (its `Load`).
    fn register(self, system: &Catalog) -> Result<()> {
        match self {
            Self::Parquet => {
                system.register_copy_function(rustdb_parquet::copy::parquet_copy_function())?;
                for set in rustdb_parquet::parquet_table_functions() {
                    system.register_table_functions(set)?;
                }
                for set in rustdb_parquet::parquet_scalar_functions() {
                    system.register_scalar_functions(set)?;
                }
            }
            Self::Json => {
                for function in rustdb_json::json_copy_functions() {
                    system.register_copy_function(function)?;
                }
                for set in rustdb_json::json_scalar_functions() {
                    system.register_scalar_functions(set)?;
                }
                for set in rustdb_json::json_table_functions() {
                    system.register_table_functions(set)?;
                }
                crate::system_functions::register_json_macros(system)?;
            }
        }
        Ok(())
    }
}

/// The globally set options and their version (see `DatabaseInstance::global_settings`).
#[derive(Debug, Default, Clone)]
struct GlobalSettings {
    version: u64,
    values: Vec<(&'static str, Value)>,
}

/// The default of `checkpoint_threshold` (16 MiB).
const DEFAULT_CHECKPOINT_WAL_SIZE: u64 = 16 * 1024 * 1024;

/// Whether `name` is `force_compression`.
fn is_force_compression(name: &str) -> bool {
    name.eq_ignore_ascii_case("force_compression")
}

/// `ForceCompressionSetting::OnSet`: the codec named by `value` (`none` / `auto`: none).
fn parse_force_compression(
    value: &rustdb_types::Value,
) -> Result<rustdb_storage::compression::CompressionType> {
    use rustdb_storage::compression::CompressionType;
    let name = value.to_varchar()?.to_lowercase();
    if name == "none" || name == "auto" {
        return Ok(CompressionType::Auto);
    }
    let compression = CompressionType::from_enum_name(&name).ok_or_else(|| {
        let candidates: Vec<String> = CompressionType::ALL
            .iter()
            .map(|compression| compression.enum_name().to_owned())
            .collect();
        Error::not_implemented(format!(
            "Enum value: unrecognized value \"{name}\" for enum \"CompressionType\"\n{}",
            rustdb_common::string_util::candidates_error_message(&candidates, &name, "Candidates")
        ))
    })?;
    match compression {
        CompressionType::Constant | CompressionType::Empty => Err(Error::parser(
            "auto / constant cannot be used for force_compression",
        )),
        // `CompressionTypeIsAvailable`: deprecated codecs can no longer be written.
        CompressionType::Chimp | CompressionType::Patas => Err(Error::parser(format!(
            "Attempted to force a deprecated compression type ({compression})"
        ))),
        other => Ok(other),
    }
}

/// Whether `name` is a setting of the secret manager.
fn is_secret_setting(name: &str) -> bool {
    [
        "secret_directory",
        "allow_persistent_secrets",
        "default_secret_storage",
    ]
    .iter()
    .any(|setting| setting.eq_ignore_ascii_case(name))
}

/// DuckDB's default `temp_directory`: `<database>.tmp`, or `.tmp` in memory.
fn default_temp_directory(path: &str) -> String {
    if path.is_empty() {
        ".tmp".to_owned()
    } else {
        format!("{path}.tmp")
    }
}

/// Whether `name` is `threads` or its alias `worker_threads`.
fn is_threads_setting(name: &str) -> bool {
    name.eq_ignore_ascii_case("threads") || name.eq_ignore_ascii_case("worker_threads")
}

/// Whether `name` is `checkpoint_threshold` or its alias `wal_autocheckpoint`.
fn is_checkpoint_threshold(name: &str) -> bool {
    name.eq_ignore_ascii_case("checkpoint_threshold") || name.eq_ignore_ascii_case("wal_autocheckpoint")
}

impl Drop for DatabaseInstance {
    /// Writes a checkpoint of every database file when the last handle goes away
    /// (`checkpoint_on_shutdown`).
    fn drop(&mut self) {
        let checkpoint = self.checkpoint_on_shutdown();
        for database in self.databases.all() {
            database.close(checkpoint);
        }
    }
}

impl DatabaseInstance {
    /// Registers a statically linked extension's catalog entries once.
    fn load_static_extension(&self, extension: StaticExtension) -> Result<()> {
        let mut loaded = self.loaded_extensions.lock().unwrap_or_else(PoisonError::into_inner);
        if !loaded.contains(&extension) {
            extension.register(&self.system_catalog)?;
            loaded.push(extension);
        }
        Ok(())
    }

    /// `SET threads` (`ThreadsSetting::SetGlobal`), or the default thread count for
    /// `RESET threads` (`None`).
    fn set_threads(&self, value: Option<&rustdb_types::Value>) -> Result<()> {
        let threads = match value {
            Some(value) => {
                let threads = ValueCast::default_cast_as(value, &LogicalType::BIGINT)?
                    .as_i128()
                    .unwrap_or(0);
                if threads < 1 {
                    return Err(Error::new(ErrorKind::Syntax, "Must have at least 1 thread!"));
                }
                usize::try_from(threads).unwrap_or(usize::MAX)
            }
            None => rustdb_execution::default_threads(),
        };
        self.env.scheduler.set_threads(threads);
        Ok(())
    }

    /// `SET`/`RESET` (`None`) of the buffer manager's options: `memory_limit` (alias
    /// `max_memory`), `temp_directory` and `max_temp_directory_size`; other names are
    /// ignored.
    fn set_buffer_setting(&self, name: &str, value: Option<&rustdb_types::Value>) -> Result<()> {
        let buffer_manager = &self.env.buffer_manager;
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "memory_limit" | "max_memory" => {
                let limit = match value {
                    Some(value) => rustdb_buffer::parse_memory_limit(&value.to_varchar()?)?,
                    None => rustdb_buffer::default_memory_limit(),
                };
                buffer_manager.set_memory_limit(limit)
            }
            "temp_directory" => {
                let directory = match value {
                    Some(value) => value.to_varchar()?,
                    None => default_temp_directory(&self.path),
                };
                buffer_manager.set_temporary_directory(&directory)
            }
            "max_temp_directory_size" => {
                let limit = value
                    .map(|value| rustdb_buffer::parse_memory_limit(&value.to_varchar()?))
                    .transpose()?;
                buffer_manager.set_swap_limit(limit)
            }
            _ => Ok(()),
        }
    }

    /// `SET checkpoint_threshold` (`CheckpointThresholdSetting::SetGlobal`).
    fn set_checkpoint_threshold(&self, value: &rustdb_types::Value) -> Result<()> {
        let limit = rustdb_buffer::parse_memory_limit(&value.to_varchar()?)?;
        self.checkpoint_wal_size
            .store(limit, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// `SET force_compression` / `RESET force_compression` (`None`): stores the setting and
    /// hands it to the storage of every attached database.
    fn set_force_compression(&self, value: Option<&rustdb_types::Value>) -> Result<()> {
        let forced = match value {
            Some(value) => parse_force_compression(value)?,
            None => rustdb_storage::compression::CompressionType::Auto,
        };
        let config = {
            let mut config = self
                .compression
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            config.force_compression = forced;
            *config
        };
        for database in self.databases.all() {
            if let Some(storage) = &database.storage {
                storage.set_compression_config(config);
            }
        }
        Ok(())
    }

    /// The compression settings checkpoints use.
    fn compression_config(&self) -> rustdb_storage::compression::CompressionConfig {
        *self
            .compression
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The name of the database opened at start-up.
    pub(crate) fn default_database_name(&self) -> &str {
        self.default_database.get().map_or("memory", String::as_str)
    }

    /// The attached database called `name`.
    pub(crate) fn database(&self, name: &str) -> Result<Arc<AttachedDatabase>> {
        self.databases
            .get(name)
            .ok_or_else(|| Error::binder(format!("Catalog \"{name}\" does not exist!")))
    }

    /// The catalogs the binder sees: `default` first, the other attached databases,
    /// then `system`.
    pub(crate) fn catalogs(&self, default: &str) -> Vec<Arc<Catalog>> {
        let databases = self.databases.all();
        let mut catalogs: Vec<Arc<Catalog>> = databases
            .iter()
            .filter(|database| database.name.eq_ignore_ascii_case(default))
            .map(|database| Arc::clone(&database.catalog))
            .collect();
        catalogs.extend(
            databases
                .iter()
                .filter(|database| !database.name.eq_ignore_ascii_case(default))
                .map(|database| Arc::clone(&database.catalog)),
        );
        catalogs.push(Arc::clone(&self.system_catalog));
        catalogs
    }

    /// Checkpoints the database `name` a commit wrote to when its WAL grew beyond
    /// `checkpoint_threshold`, unless other transactions are running
    /// (`DuckTransactionManager::CanCheckpoint`).
    fn auto_checkpoint(&self, name: &str) -> Result<()> {
        let threshold = self
            .checkpoint_wal_size
            .load(std::sync::atomic::Ordering::Relaxed);
        let Some(database) = self.databases.get(name) else {
            return Ok(());
        };
        let Some(storage) = &database.storage else {
            return Ok(());
        };
        let Some(wal) = storage.wal() else {
            return Ok(());
        };
        if wal.size() <= threshold || self.transactions.active_count() > 0 {
            return Ok(());
        }
        crate::storage::create_checkpoint(
            &database.catalog,
            storage,
            crate::storage::CheckpointMode::Automatic,
            Some(&self.transactions),
        )
    }

    /// Whether `checkpoint_on_shutdown` is on (`PRAGMA disable_checkpoint_on_shutdown`).
    fn checkpoint_on_shutdown(&self) -> bool {
        !self.config.options.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("checkpoint_on_shutdown") && value.as_bool() == Some(false)
        }) && !self
            .shutdown_checkpoint_disabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// A database instance (DuckDB's `DuckDB`). Cheap to clone; clones share the instance.
#[derive(Clone, Debug)]
pub struct Database {
    instance: Arc<DatabaseInstance>,
}

/// DuckDB's default casts plus those of the statically loaded json extension.
fn json_casts() -> CastFunctionSet {
    let mut casts = CastFunctionSet::new();
    rustdb_json::register_json_casts(&mut casts);
    casts
}

impl Database {
    /// Opens (or creates) the database at `path`; `""` or `":memory:"` is in-memory.
    pub fn open(path: &str, config: &Config) -> Result<Self> {
        let path = if path == ":memory:" {
            String::new()
        } else {
            path.to_owned()
        };
        let system_catalog = Catalog::new("system")?;
        for set in rustdb_functions::builtin_scalar_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        for set in rustdb_functions::builtin_aggregate_functions()? {
            system_catalog.register_aggregate_functions(set)?;
        }
        for set in rustdb_functions::builtin_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::sequence::sequence_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        for set in crate::checkpoint::checkpoint_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::system_functions::catalog_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::system_functions::context_scalar_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        for set in crate::system_functions::pragma_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::log_functions::log_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::read_file::read_file_functions() {
            system_catalog.register_table_functions(set)?;
        }
        system_catalog.register_copy_function(rustdb_csv::copy_csv::csv_copy_function())?;
        for set in rustdb_csv::multi_file::glob_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in rustdb_csv::sniff_csv::sniff_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in rustdb_csv::read_csv::read_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        crate::system_functions::register_default_views(&system_catalog)?;
        crate::system_functions::register_default_macros(&system_catalog)?;
        let buffer_manager = rustdb_buffer::BufferManager::new(rustdb_buffer::BufferManagerConfig {
            temp_directory: Some(default_temp_directory(&path).into()),
            ..rustdb_buffer::BufferManagerConfig::default()
        })?;
        let env = Arc::new(rustdb_execution::ClientEnv::new(buffer_manager));
        let instance = DatabaseInstance {
            path: path.clone(),
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            databases: DatabaseManager::default(),
            default_database: std::sync::OnceLock::new(),
            casts: Arc::new(json_casts()),
            env,
            shutdown_checkpoint_disabled: std::sync::atomic::AtomicBool::new(false),
            checkpoint_wal_size: std::sync::atomic::AtomicU64::new(DEFAULT_CHECKPOINT_WAL_SIZE),
            compression: std::sync::Mutex::default(),
            secrets: Arc::default(),
            connections: Arc::default(),
            object_cache: Arc::default(),
            log_manager: Arc::default(),
            global_settings: Mutex::default(),
            loaded_extensions: Mutex::default(),
        };
        if config.load_extensions {
            for extension in StaticExtension::ALL {
                instance.load_static_extension(extension)?;
            }
        }
        for (name, value) in &config.options {
            if is_threads_setting(name) {
                instance.set_threads(Some(value))?;
            }
            instance.set_buffer_setting(name, Some(value))?;
            if is_checkpoint_threshold(name) {
                instance.set_checkpoint_threshold(value)?;
            }
            if is_force_compression(name) {
                instance.set_force_compression(Some(value))?;
            }
        }
        let database = Self {
            instance: Arc::new(instance),
        };
        let options = crate::storage::startup_options(config);
        let main = database.attach("", &path, &options, rustdb_ast::OnCreateConflict::ErrorOnConflict)?;
        if let Some(main) = main {
            let _ = database.instance.default_database.set(main.name.clone());
        }
        Ok(database)
    }

    /// Attaches the database at `path` as `name` and replays its WAL
    /// (`DatabaseManager::AttachDatabase`).
    pub(crate) fn attach(
        &self,
        name: &str,
        path: &str,
        options: &AttachOptions,
        on_conflict: rustdb_ast::OnCreateConflict,
    ) -> Result<Option<Arc<AttachedDatabase>>> {
        let Some(attached) = self.instance.databases.attach(
            name,
            path,
            options,
            on_conflict,
            &self.instance.env.buffer_manager,
        )?
        else {
            return Ok(None);
        };
        if let Some(storage) = &attached.storage {
            storage.set_compression_config(self.instance.compression_config());
        }
        if let Err(error) = crate::wal::replay(self, &attached) {
            self.instance.databases.remove(&attached.name);
            return Err(error);
        }
        Ok(Some(attached))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open("", &Config::default())
    }

    /// The database file path, or `""` for in-memory databases.
    pub fn path(&self) -> &str {
        &self.instance.path
    }

    pub fn config(&self) -> &Config {
        &self.instance.config
    }

    pub fn connect(&self) -> Result<Connection> {
        let transactions = TransactionContext::new(Arc::clone(&self.instance.transactions));
        let mut settings = Settings::default();
        for (name, value) in &self.instance.config.options {
            settings.set(name, value)?;
        }
        let function_client = Arc::new(rustdb_function::FunctionClient::default());
        function_client.set_database(self.instance.default_database_name());
        self.instance
            .connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        function_client.set_connection_counter(Arc::clone(&self.instance.connections));
        function_client.set_object_cache(Arc::clone(&self.instance.object_cache));
        // `ClientContext::ClientContext`: a connection-scoped logger.
        let logger = Arc::new(crate::logging::ConnectionLogger::new(
            Arc::clone(&self.instance.log_manager),
            crate::logging::LoggingContext::new(crate::logging::ContextScope::Connection),
        ));
        function_client.set_logger(Arc::clone(&logger) as Arc<dyn rustdb_function::logging::ClientLogger>);
        // `ClientData::temporary_objects`: the connection's in-memory `temp` database.
        let temp_catalog = Arc::new(Catalog::new(rustdb_catalog::TEMP_CATALOG)?);
        temp_catalog.set_io(Arc::new(rustdb_storage::TableIoManager::new(
            self.instance.env.buffer_manager.clone(),
            Arc::new(rustdb_buffer::InMemoryBlockManager::default()),
        )))?;
        Ok(Connection {
            synced_globals: GlobalSettings::default(),
            local_overrides: std::collections::HashSet::new(),
            temp_catalog,
            logger,
            database: self.clone(),
            transactions,
            settings,
            search_path: Vec::new(),
            search_path_set: Vec::new(),
            transaction_attaches: Vec::new(),
            prepared_statements: CaseInsensitiveMap::default(),
            function_client,
            user_variables: CaseInsensitiveMap::default(),
        })
    }

    /// Loads a (statically linked) extension, like `LOAD name`.
    pub fn load_extension(&self, name: &str) -> Result<ExtensionLoad> {
        if let Some(extension) = StaticExtension::from_name(name) {
            self.instance.load_static_extension(extension)?;
            return Ok(ExtensionLoad::Loaded);
        }
        Ok(match name {
            "core_functions" | "icu" | "tpch" | "tpcds" | "autocomplete" | "httpfs" | "fts" | "inet"
            | "excel" | "sqlsmith" | "spatial" | "delta" | "iceberg" | "aws" | "azure" | "motherduck"
            | "postgres_scanner" | "mysql_scanner" | "sqlite_scanner" | "vss" | "ui" | "encodings"
            | "ducklake" | "avro" | "demo_capi" => ExtensionLoad::NotAvailable,
            _ => ExtensionLoad::Unknown,
        })
    }

    pub(crate) fn instance(&self) -> &DatabaseInstance {
        &self.instance
    }
}

/// A connection to a database (DuckDB's `Connection` and `ClientContext`). Each
/// connection is used by one thread at a time; open one connection per thread for
/// concurrency.
#[derive(Debug)]
pub struct Connection {
    database: Database,
    /// The global options as last applied to `settings`.
    synced_globals: GlobalSettings,
    /// Options this connection set for itself (`SET SESSION`, or options of local scope):
    /// global values do not override them.
    local_overrides: std::collections::HashSet<&'static str>,
    /// The connection's temporary objects (DuckDB's `ClientData::temporary_objects`, the
    /// `temp` catalog).
    temp_catalog: Arc<Catalog>,
    transactions: TransactionContext,
    settings: Settings,
    /// The `(catalog, schema)` entries set by `USE` / `SET search_path` (DuckDB's
    /// `ClientData::catalog_search_path`).
    search_path: Vec<(String, String)>,
    /// The entries as `CatalogSearchPath::GetSetPaths` keeps them for display: the
    /// catalog as given (or taken from the previous default), empty if none.
    search_path_set: Vec<(String, String)>,
    prepared_statements: CaseInsensitiveMap<Arc<PreparedStatementData>>,
    /// Connection state functions use (DuckDB keeps it in the `ClientContext`).
    function_client: Arc<rustdb_function::FunctionClient>,
    /// Databases attached by the open explicit transaction; rolling it back detaches
    /// them (the `ATTACHED_DATABASE` undo entries).
    transaction_attaches: Vec<Arc<AttachedDatabase>>,
    /// `SET VARIABLE` values (DuckDB's `ClientConfig::user_variables`).
    user_variables: CaseInsensitiveMap<rustdb_types::Value>,
    /// The connection's loggers.
    logger: Arc<crate::logging::ConnectionLogger>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.database
            .instance
            .connections
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Connection {
    /// Casts `value` with the casts of this database, including those of loaded extensions
    /// (DuckDB's `Value::CastAs(ClientContext &, ...)`).
    pub fn cast_value(&self, value: &Value, target: &rustdb_types::LogicalType) -> Result<Value> {
        use rustdb_function::cast::ValueCast;
        let mut params = rustdb_function::cast::CastParameters::new();
        value.cast_with(&self.database.instance.casts, target, &mut params)
    }

    /// The options set on this connection (and not reset since), by canonical name,
    /// like the values DuckDB's `RestartCommand` carries over to the restarted database.
    pub fn assigned_settings(&self) -> Vec<(String, Value)> {
        self.settings
            .assigned()
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect()
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Parses `sql` into its statements (DuckDB's `ClientContext::ExtractStatements`).
    pub fn extract_statements(&self, sql: &str) -> Result<Vec<SqlStatement>> {
        rustdb_parser::parse_with_options(sql, &self.settings.parser_options())
            .map_err(|error| error.with_query_context(sql))
    }

    /// Runs every statement in `sql` and returns the result of the last one (DuckDB's
    /// `ClientContext::Query`).
    pub fn query(&mut self, sql: &str) -> Result<MaterializedResult> {
        self.sync_global_settings();
        let statements = self.extract_statements(sql)?;
        self.execute_statements(statements)
    }

    /// Runs parsed statements and returns the result of the last one; errors are put in
    /// the context of their statement's text.
    pub fn execute_statements(&mut self, statements: Vec<SqlStatement>) -> Result<MaterializedResult> {
        let mut result = MaterializedResult::default();
        for statement in statements {
            let query = statement.query.clone();
            result = self.execute_statement(&query, statement)?;
        }
        Ok(result)
    }

    /// Runs one parsed statement of `query` (DuckDB's `PendingStatementInternal` and
    /// `ExecutePendingQueryInternal`): bind and plan, resolve column bindings, build the
    /// physical plan and run it, all inside the connection's transaction.
    pub fn execute_statement(&mut self, query: &str, statement: SqlStatement) -> Result<MaterializedResult> {
        self.sync_global_settings();
        self.function_client.set_query(query);
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        self.begin_query_logging(query);
        let outcome = self.run_statement(statement);
        let invalidates = outcome
            .as_ref()
            .is_err_and(|error| invalidates_transaction(error.kind()));
        // The database the transaction wrote to, which its commit may checkpoint.
        let modified = if self.transactions.has_current() {
            self.transactions.current().modified_database()
        } else {
            None
        };
        let ended = self.transactions.end_statement(outcome.is_ok(), invalidates);
        self.end_query_logging();
        let mut result = outcome.and_then(|result| ended.map(|()| result));
        if result.is_ok()
            && !self.transactions.has_current()
            && let Some(modified) = modified
            && let Err(error) = self.database.instance().auto_checkpoint(&modified)
        {
            result = Err(error);
        }
        result.map_err(|error| error.with_query_context(query))
    }

    /// Applies the options another connection set globally since the last statement
    /// (DuckDB reads them from the shared `DBConfig`); options this connection set itself
    /// keep their value.
    fn sync_global_settings(&mut self) {
        let current = self
            .database
            .instance()
            .global_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if current.version == self.synced_globals.version {
            return;
        }
        for (name, value) in &current.values {
            let unchanged = self
                .synced_globals
                .values
                .iter()
                .any(|(synced, synced_value)| synced == name && synced_value == value);
            if !unchanged && !self.local_overrides.contains(name) {
                let _ = self.settings.set(name, value);
            }
        }
        for (name, _) in &self.synced_globals.values {
            let removed = !current
                .values
                .iter()
                .any(|(current_name, _)| current_name == name);
            if removed && !self.local_overrides.contains(name) {
                let _ = self.settings.reset(name);
            }
        }
        self.synced_globals = current;
    }

    /// Records a `SET` / `RESET` (`value` `None`) of option `name` with `scope`: a global
    /// one is shared with the other connections, a local one overrides the global value.
    fn record_setting(&mut self, name: &str, value: Option<&Value>, scope: SetScope) {
        let Some(option) = crate::settings::canonical_option(name) else {
            return;
        };
        let global = match scope {
            SetScope::Global => true,
            SetScope::Session | SetScope::Local => false,
            _ => crate::settings::is_global_option(option),
        };
        if !global {
            if value.is_some() {
                self.local_overrides.insert(option);
                return;
            }
            self.local_overrides.remove(option);
            // The global value applies again.
            if let Some((_, global_value)) = self
                .synced_globals
                .values
                .iter()
                .find(|(name, _)| *name == option)
            {
                let _ = self.settings.set(option, &global_value.clone());
            }
            return;
        }
        self.local_overrides.remove(option);
        let mut globals = self
            .database
            .instance()
            .global_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        globals.values.retain(|(name, _)| *name != option);
        if let Some(value) = value {
            globals.values.push((option, value.clone()));
        }
        globals.version += 1;
        // This connection already applied the change.
        self.synced_globals = globals.clone();
    }

    /// `ClientContext::BeginQueryInternal`'s logging: flush, a logger for this query's
    /// connection, transaction and query, and the `QueryLog` entry.
    fn begin_query_logging(&mut self, query: &str) {
        use crate::logging::{ContextScope, LoggingContext};
        use rustdb_function::logging::{ClientLogger, LogLevel, LogScope};
        let manager = Arc::clone(&self.logger.manager);
        let _ = manager.flush();
        // Like DuckDB, the query's transaction starts first when the query is logged.
        let transaction_id = if manager.config().enabled || self.transactions.has_current() {
            let transaction = self.transactions.current();
            self.function_client
                .observe_transaction(transaction.transaction_id);
            Some(transaction.transaction_id)
        } else {
            None
        };
        self.logger.refresh(LoggingContext {
            scope: ContextScope::Connection,
            connection_id: Some(self.function_client.connection_id()),
            transaction_id,
            query_id: Some(self.function_client.query_id()),
        });
        if self
            .logger
            .should_log(LogScope::Connection, "QueryLog", LogLevel::Info)
        {
            let _ = self
                .logger
                .write_log(LogScope::Connection, "QueryLog", LogLevel::Info, query);
        }
    }

    /// `ClientContext::EndQueryInternal`'s logging: flush and a connection-scoped logger.
    fn end_query_logging(&self) {
        use crate::logging::{ContextScope, LoggingContext};
        let _ = self.logger.manager.flush();
        self.logger.refresh(LoggingContext {
            connection_id: Some(self.function_client.connection_id()),
            ..LoggingContext::new(ContextScope::Connection)
        });
    }

    /// The logical plan of a parsed statement of `query`, with column references
    /// resolved, as `json_serialize_plan` (without optimization) sees it. A development
    /// aid for comparing plans with DuckDB's.
    pub fn logical_plan(&mut self, query: &str, statement: SqlStatement) -> Result<LogicalOperator> {
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let outcome = planner.create_plan(statement).and_then(|planned| {
            let mut plan = planned
                .plan
                .ok_or_else(|| Error::invalid_input("Not all parameters were bound"))?;
            ColumnBindingResolver::resolve(&mut plan)?;
            Ok(plan)
        });
        let ended = self.transactions.end_statement(outcome.is_ok(), false);
        outcome
            .and_then(|plan| ended.map(|()| plan))
            .map_err(|error| error.with_query_context(query))
    }

    /// The transaction the next statement runs in, started if needed.
    pub(crate) fn current_transaction(&mut self) -> Arc<Transaction> {
        self.transactions.current()
    }

    /// The catalogs this connection sees: the attached databases with `default` first,
    /// its `temp` catalog, then `system`.
    fn catalogs(&self, default: &str) -> Vec<Arc<Catalog>> {
        let mut catalogs = self.database.instance().catalogs(default);
        let system = catalogs.len().saturating_sub(1);
        catalogs.insert(system, Arc::clone(&self.temp_catalog));
        catalogs
    }

    /// The planner's view of this connection, in `transaction`.
    fn client_context(&self, transaction: Arc<Transaction>) -> Arc<ClientContext> {
        self.function_client
            .observe_transaction(transaction.transaction_id);
        let instance = self.database.instance();
        Arc::new(ClientContext {
            catalogs: self.catalogs(instance.default_database_name()),
            search_path: self.search_path.clone(),
            view_search_path: Vec::new(),
            transaction,
            casts: Arc::clone(&instance.casts),
            evaluator: Arc::new(ScalarEvaluator),
            config: {
                let optimizer = self.settings.optimizer_config();
                rustdb_planner::PlannerConfig {
                    flip_right_joins: optimizer.enabled
                        && !optimizer
                            .disabled
                            .contains(&rustdb_optimizer::OptimizerType::BuildSideProbeSide),
                    ..self.settings.planner.clone()
                }
            },
            prepared_statements: self.prepared_statements.clone(),
            function_client: Arc::clone(&self.function_client),
            user_variables: self.user_variables.clone(),
            dependency_recorder: Arc::default(),
            database_state: Some(Arc::clone(&instance.secrets) as Arc<dyn rustdb_planner::DatabaseState>),
        })
    }

    /// `IMPORT DATABASE 'dir'` (DuckDB's `PragmaImportDatabase`): runs the directory's
    /// `schema.sql`, then its `load.sql` with the files taken from the directory.
    fn import_database(&mut self, parameters: &[rustdb_types::Value]) -> Result<MaterializedResult> {
        let directory = parameters
            .first()
            .map(rustdb_types::Value::to_varchar)
            .transpose()?
            .unwrap_or_default();
        let join = |name: &str| {
            if directory.ends_with('/') {
                format!("{directory}{name}")
            } else {
                format!("{directory}/{name}")
            }
        };
        let mut statements = Vec::new();
        for file in ["schema.sql", "load.sql"] {
            let path = join(file);
            let query = std::fs::read_to_string(&path).map_err(|error| {
                let reason = if error.kind() == std::io::ErrorKind::NotFound {
                    "No such file or directory".to_owned()
                } else {
                    error.to_string()
                };
                Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {reason}"))
            })?;
            for mut statement in rustdb_parser::parse(&query)? {
                if file == "load.sql"
                    && let rustdb_ast::SqlStatementKind::Copy(info) = &mut statement.kind
                {
                    let name = info
                        .file_path
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    info.file_path = join(&name);
                }
                statements.push(statement);
            }
        }
        let mut result = MaterializedResult::default();
        for statement in statements {
            result = self.run_statement(statement)?;
        }
        Ok(result)
    }

    /// `COPY FROM DATABASE source TO target (SCHEMA | DATA)` (DuckDB's
    /// `Binder::Bind(CopyDatabaseStatement &)`): the source's entries are created in the
    /// target (in export order), or every source table's rows are inserted into the
    /// target's table of the same name.
    fn copy_database(&mut self, stmt: &rustdb_ast::CopyDatabaseStatement) -> Result<MaterializedResult> {
        let instance = self.database.instance();
        let source = instance.database(&stmt.from_database)?;
        let target = instance.database(&stmt.to_database)?;
        if Arc::ptr_eq(&source.catalog, &target.catalog) {
            return Err(Error::binder(format!(
                "Cannot copy from \"{}\" to \"{}\" - FROM and TO databases are the same",
                stmt.from_database, stmt.to_database
            )));
        }
        let target_name = target.name.clone();
        // Read the source in the open transaction, or in a snapshot of its own.
        let (snapshot, temporary) = if self.transactions.has_current() {
            (self.transactions.current(), false)
        } else {
            (Arc::new(instance.transactions.begin()), true)
        };
        // The statements to run; `queries` are SQL texts to run before `statements[i]`.
        let mut statements: Vec<SqlStatement> = Vec::new();
        let mut queries: Vec<(usize, String)> = Vec::new();
        if stmt.copy_type == rustdb_ast::CopyDatabaseType::CopySchema {
            for mut info in rustdb_catalog::checkpoint::export_create_infos(&source.catalog, &snapshot) {
                info.catalog.clone_from(&target_name);
                info.on_conflict = if matches!(info.kind, rustdb_ast::CreateInfoKind::Schema) {
                    rustdb_ast::OnCreateConflict::IgnoreOnConflict
                } else {
                    rustdb_ast::OnCreateConflict::ErrorOnConflict
                };
                // Schemas and sequences are created from their infos: their SQL names no
                // catalog, and a sequence's SQL loses its usage.
                if matches!(
                    info.kind,
                    rustdb_ast::CreateInfoKind::Schema | rustdb_ast::CreateInfoKind::Sequence(_)
                ) {
                    statements.push(SqlStatement::new(rustdb_ast::SqlStatementKind::Create(Box::new(
                        info,
                    ))));
                } else if let Some(sql) = info.to_sql() {
                    queries.push((statements.len(), sql));
                }
            }
        } else {
            let mut tables = Vec::new();
            for schema in source.catalog.schemas().scan(&snapshot) {
                let Some(contents) = schema.as_schema() else {
                    continue;
                };
                for entry in contents.tables.scan(&snapshot) {
                    if matches!(entry.kind, rustdb_catalog::EntryKind::Table(_)) && !entry.internal {
                        tables.push((schema.name().to_owned(), entry));
                    }
                }
            }
            for (schema, entry) in rustdb_catalog::checkpoint::reorder_table_entries(tables) {
                let rustdb_catalog::EntryKind::Table(table) = &entry.kind else {
                    continue;
                };
                let columns: Vec<String> = table
                    .physical_columns()
                    .map(|(_, _, column)| rustdb_ast::keyword::write_optionally_quoted(&column.name))
                    .collect();
                let columns = columns.join(", ");
                queries.push((
                    statements.len(),
                    format!(
                        "INSERT INTO {} ({columns}) SELECT {columns} FROM {}",
                        rustdb_ast::qualifier_to_string(&target_name, &schema, entry.name()),
                        rustdb_ast::qualifier_to_string(&source.name, &schema, entry.name()),
                    ),
                ));
            }
        }
        if temporary {
            instance.transactions.rollback(&snapshot);
        }
        let mut queries = queries.into_iter().peekable();
        for (index, statement) in statements
            .into_iter()
            .map(Some)
            .chain(std::iter::once(None))
            .enumerate()
        {
            while let Some((_, query)) = queries.next_if(|(before, _)| *before == index) {
                for parsed in rustdb_parser::parse(&query)? {
                    self.run_statement(parsed)?;
                }
            }
            if let Some(statement) = statement {
                self.run_statement(statement)?;
            }
        }
        Ok(MaterializedResult::default())
    }

    fn run_statement(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        if let rustdb_ast::SqlStatementKind::Multi(statements) = statement.kind {
            return self.run_multi_statement(statements);
        }
        if let rustdb_ast::SqlStatementKind::CopyDatabase(copy) = &statement.kind {
            return self.copy_database(copy);
        }
        if let rustdb_ast::SqlStatementKind::Load(info) = &statement.kind {
            if let Some(name) = crate::extensions::load_extension(info)?
                && let Some(extension) = StaticExtension::from_name(&name)
            {
                self.database.instance.load_static_extension(extension)?;
            }
            return Ok(MaterializedResult::default());
        }
        // Pragmas that stand for a query run that query (DuckDB's `PragmaHandler`).
        let statement = statement;
        if let rustdb_ast::SqlStatementKind::Pragma(pragma) = &statement.kind {
            let constants: Option<Vec<rustdb_types::Value>> = pragma
                .parameters
                .iter()
                .map(|parameter| match &parameter.kind {
                    rustdb_ast::ParsedExpressionKind::Constant(constant) => Some(constant.value.clone()),
                    rustdb_ast::ParsedExpressionKind::ColumnRef(column) => {
                        Some(rustdb_types::Value::varchar(column.column_names.join(".")))
                    }
                    _ => None,
                })
                .collect();
            if let Some(constants) = &constants
                && pragma.name.eq_ignore_ascii_case("import_database")
            {
                return self.import_database(constants);
            }
            if let Some(constants) = constants
                && let Some(query) = crate::system_functions::pragma_query(&pragma.name, &constants)?
            {
                let mut result = MaterializedResult::default();
                for statement in rustdb_parser::parse(&query)? {
                    result = self.run_statement(statement)?;
                }
                return Ok(result);
            }
        }
        // `CHECKPOINT` is `CALL checkpoint()`: the call is bound and run like any table
        // function, then the connection writes the checkpoint.
        let checkpoint = match &statement.kind {
            rustdb_ast::SqlStatementKind::Call(call) => match &call.function.kind {
                rustdb_ast::ParsedExpressionKind::Function(function)
                    if function.function_name.eq_ignore_ascii_case("checkpoint")
                        || function.function_name.eq_ignore_ascii_case("force_checkpoint") =>
                {
                    Some(function.function_name.eq_ignore_ascii_case("force_checkpoint"))
                }
                _ => None,
            },
            _ => None,
        };
        let database = checkpoint.and_then(|_| match &statement.kind {
            rustdb_ast::SqlStatementKind::Call(call) => match &call.function.kind {
                rustdb_ast::ParsedExpressionKind::Function(function) => {
                    function.children.first().and_then(|name| match &name.kind {
                        rustdb_ast::ParsedExpressionKind::Constant(constant) => {
                            constant.value.to_varchar().ok()
                        }
                        _ => None,
                    })
                }
                _ => None,
            },
            _ => None,
        });
        let result = self.run_planned(statement)?;
        if let Some(force) = checkpoint {
            let instance = self.database.instance();
            let name = database.unwrap_or_else(|| self.default_catalog());
            let attached = instance.database(&name)?;
            let transaction = self.transactions.current();
            crate::storage::checkpoint(
                &attached.catalog,
                attached.storage.as_ref(),
                &transaction,
                force,
                &instance.transactions,
            )?;
        }
        Ok(result)
    }

    /// Ends the explicit transaction's attaches: kept on commit, detached again when the
    /// transaction rolled back (`RollbackState` of `ATTACHED_DATABASE`).
    fn finish_transaction_attaches(&mut self, rolled_back: bool) {
        let attaches = std::mem::take(&mut self.transaction_attaches);
        if !rolled_back {
            return;
        }
        let instance = self.database.instance();
        for attached in attaches {
            instance.databases.detach_if_current(&attached);
        }
    }

    /// `DETACH name` (`DatabaseManager::DetachDatabase`).
    fn detach(&mut self, info: &rustdb_ast::DetachInfo) -> Result<()> {
        // `PhysicalDetach`: not while this transaction has work on the database.
        if !self.transactions.is_auto_commit()
            && self
                .transactions
                .current()
                .modified_database()
                .is_some_and(|name| name.eq_ignore_ascii_case(&info.name))
        {
            return Err(Error::new(
                ErrorKind::Transaction,
                format!(
                    "Cannot detach database \"{}\" because the current transaction has outstanding work on it - \
                     commit or rollback first",
                    info.name
                ),
            ));
        }
        if self.default_catalog().eq_ignore_ascii_case(&info.name) {
            return Err(Error::binder(format!(
                "Cannot detach database \"{}\" because it is the default database. Select a different database \
                 using `USE` to allow detaching this database",
                info.name
            )));
        }
        let instance = self.database.instance();
        match instance.databases.detach(&info.name) {
            Some(_) => Ok(()),
            None if info.if_not_found == rustdb_ast::OnEntryNotFound::ReturnNull => Ok(()),
            None => Err(Error::binder(format!(
                "Failed to detach database with name \"{}\": database not found",
                info.name
            ))),
        }
    }

    /// `SET schema` / `SET search_path` (`CatalogSearchPath::Set`): every entry must name
    /// an existing schema; a single name may be a database (its default schema).
    fn set_search_path(&mut self, name: &str, value: &rustdb_types::Value) -> Result<()> {
        let set_schema = name.eq_ignore_ascii_case("schema");
        let set_name = if set_schema {
            "SET schema"
        } else {
            "SET search_path"
        };
        let text = value.to_varchar()?;
        let entries = parse_search_path(&text)?;
        if set_schema && entries.len() > 1 {
            // `CatalogSearchEntry::Parse` reads a single entry.
            return Err(Error::parser(format!(
                "Failed to convert entry \"{text}\" to CatalogSearchEntry - expected a single entry"
            )));
        }
        if set_schema && entries.len() != 1 {
            return Err(Error::catalog(format!(
                "{set_name} can set only 1 schema. This has {}",
                entries.len()
            )));
        }
        let instance = self.database.instance();
        let transaction = self.transactions.current();
        let mut resolved = Vec::with_capacity(entries.len());
        let mut set_paths = Vec::with_capacity(entries.len());
        // `GetDefault().catalog`: the catalog of the first entry set before.
        let previous_default = self
            .search_path_set
            .first()
            .map(|(catalog, _)| catalog.clone())
            .unwrap_or_default();
        for (catalog, schema) in entries {
            let display = if catalog.is_empty() {
                schema.clone()
            } else {
                format!("{catalog}.{schema}")
            };
            let not_found = || {
                Error::catalog(format!(
                    "{set_name}: No catalog + schema named \"{display}\" found."
                ))
            };
            if catalog.is_empty() {
                // A database name alone means its default schema.
                if let Some(database) = instance.databases.get(&schema)
                    && let Some(main) = database
                        .catalog
                        .schema(&transaction, rustdb_catalog::DEFAULT_SCHEMA)
                {
                    resolved.push((database.name.clone(), main.name().to_owned()));
                    set_paths.push((schema.clone(), main.name().to_owned()));
                    continue;
                }
                let default = self.default_catalog();
                let database = instance.databases.get(&default).ok_or_else(not_found)?;
                if let Some(found) = database.catalog.schema(&transaction, &schema) {
                    resolved.push((database.name.clone(), found.name().to_owned()));
                } else {
                    // `Catalog::GetSchema` without a catalog also finds the system schemas.
                    let found = instance
                        .system_catalog
                        .schema(&transaction, &schema)
                        .ok_or_else(not_found)?;
                    resolved.push(("system".to_owned(), found.name().to_owned()));
                }
                set_paths.push((previous_default.clone(), schema));
                continue;
            }
            let database = instance.databases.get(&catalog).ok_or_else(not_found)?;
            let found = database
                .catalog
                .schema(&transaction, &schema)
                .ok_or_else(not_found)?;
            resolved.push((database.name.clone(), found.name().to_owned()));
            set_paths.push((catalog, schema));
        }
        if set_schema
            && let Some((catalog, _)) = resolved.first()
            && (catalog.eq_ignore_ascii_case("temp") || catalog.eq_ignore_ascii_case("system"))
        {
            return Err(Error::catalog(format!(
                "{set_name} cannot be set to internal schema \"{catalog}\""
            )));
        }
        self.search_path = resolved;
        self.search_path_set = set_paths;
        Ok(())
    }

    /// The configuration functions see while the next statement runs.
    fn settings_snapshot(&self) -> crate::settings::SettingsSnapshot {
        // `CatalogSearchEntry::ListToString` of the set paths.
        let quoted = |part: &str| {
            if part.contains(['.', ',', '"']) {
                format!("\"{}\"", part.replace('"', "\"\""))
            } else {
                part.to_owned()
            }
        };
        let search_path = self
            .search_path_set
            .iter()
            .map(|(catalog, schema)| {
                if catalog.is_empty() {
                    quoted(schema)
                } else {
                    format!("{}.{}", quoted(catalog), quoted(schema))
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let schema = self.search_path.first().map_or_else(
            || rustdb_catalog::DEFAULT_SCHEMA.to_owned(),
            |(_, schema)| schema.clone(),
        );
        let buffer_manager = &self.database.instance().env.buffer_manager;
        crate::settings::SettingsSnapshot {
            logging: self.database.instance().log_manager.config(),
            settings: self.settings.clone(),
            search_path,
            schema,
            memory_limit: buffer_manager.max_memory(),
            temp_directory: buffer_manager.temporary_directory(),
            max_swap: buffer_manager.max_swap(),
            threads: self.database.instance().env.threads() as u64,
        }
    }

    /// The name of the default database of this connection.
    fn default_catalog(&self) -> String {
        self.search_path.first().map_or_else(
            || self.database.instance().default_database_name().to_owned(),
            |(catalog, _)| catalog.clone(),
        )
    }

    fn run_planned(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        let statement_type = statement.kind.statement_type();
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let planned = planner.create_plan(statement)?;
        if self.transactions.is_invalidated() && planned.properties.requires_valid_transaction {
            return Err(TransactionContext::aborted_error());
        }
        let parameter_count = planned.properties.parameter_count;
        if parameter_count > 0 {
            return Err(Error::invalid_input(format!(
                "Expected {parameter_count} parameters, but none were supplied"
            )));
        }
        let PlannedStatement {
            plan,
            names,
            types,
            properties,
            ..
        } = planned;
        let plan = match plan {
            Some(plan) if properties.bound_all_parameters => plan,
            _ => return Err(Error::invalid_input("Not all parameters were bound")),
        };
        let return_type = properties.return_type;
        self.register_modifications(&plan, statement_type)?;
        self.run_plan(plan, names, types)
            .map(|result| result.with_return_type(return_type))
    }

    /// The statements a statement was rewritten into (DuckDB's `UnpackMultiStatement`):
    /// outside a transaction, several statements without a SELECT run in one transaction
    /// that a failure rolls back. The result is the last statement's.
    fn run_multi_statement(&mut self, statements: Vec<SqlStatement>) -> Result<MaterializedResult> {
        let has_select = statements
            .iter()
            .any(|statement| matches!(statement.kind, rustdb_ast::SqlStatementKind::Select(_)));
        let wrap = statements.len() > 1 && !has_select && self.transactions.is_auto_commit();
        let run_sql = |connection: &mut Self, sql: &str| -> Result<MaterializedResult> {
            let mut result = MaterializedResult::default();
            for statement in rustdb_parser::parse(sql)? {
                result = connection.run_statement(statement)?;
            }
            Ok(result)
        };
        if wrap {
            run_sql(self, "BEGIN TRANSACTION")?;
        }
        let mut result = MaterializedResult::default();
        for statement in statements {
            match self.run_statement(statement) {
                Ok(statement_result) => result = statement_result,
                Err(error) => {
                    if wrap && !self.transactions.is_auto_commit() {
                        let _ = run_sql(self, "ROLLBACK");
                    }
                    return Err(error);
                }
            }
        }
        if wrap {
            run_sql(self, "COMMIT")?;
        }
        Ok(result)
    }

    /// Records which database the statement writes to in the transaction and rejects
    /// writes to read-only databases (`MetaTransaction::ModifyDatabase` and the check in
    /// `ClientContext::CreatePreparedStatementInternal`).
    fn register_modifications(
        &mut self,
        plan: &LogicalOperator,
        statement_type: rustdb_ast::StatementType,
    ) -> Result<()> {
        let instance = self.database.instance();
        let mut modified: Vec<String> = Vec::new();
        collect_modified_databases(plan, instance, &mut modified);
        if modified.is_empty() {
            return Ok(());
        }
        let transaction = self.transactions.current();
        for name in modified {
            if name.eq_ignore_ascii_case("system") || name.eq_ignore_ascii_case("temp") {
                continue;
            }
            if self.transactions.is_read_only() {
                return Err(Error::new(
                    ErrorKind::Transaction,
                    format!(
                        "Cannot write to database \"{name}\" - transaction is launched in read-only mode"
                    ),
                ));
            }
            let Some(database) = instance.databases.get(&name) else {
                continue;
            };
            if database.read_only {
                let ty = statement_type.name();
                return Err(Error::invalid_input(format!(
                    "Cannot execute statement of type \"{}\" on database \"{}\" which is attached in read-only mode!",
                    ty.strip_suffix("_STATEMENT").unwrap_or(ty),
                    database.name
                )));
            }
            transaction.modify_database(
                &database.name,
                database.catalog.wal(),
                needs_checkpoint_lock(plan),
            )?;
        }
        Ok(())
    }

    /// Runs a bound plan: statements that act on the connection are executed here
    /// (DuckDB's `PhysicalTransaction`, `PhysicalSet`, `PhysicalPrepare`, ...); queries go
    /// through the physical planner and executor.
    fn run_plan(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        let nothing =
            || MaterializedResult::new(vec!["Success".to_owned()], vec![LogicalType::BOOLEAN], Vec::new());
        // `PhysicalSet` / `PhysicalReset`: `DBConfig::CheckLock`, then the checks of options
        // that cannot change while the database runs.
        match &plan.kind {
            LogicalKind::Set { name, value, scope } if *scope != SetScope::Variable => {
                self.settings.check_lock(name)?;
                Settings::check_running_set(name, value)?;
            }
            LogicalKind::Reset { name, scope } if *scope != SetScope::Variable => {
                self.settings.check_lock(name)?;
                Settings::check_running_reset(name)?;
            }
            _ => {}
        }
        match &plan.kind {
            LogicalKind::Transaction { info } => {
                match info.transaction_type {
                    TransactionType::BeginTransaction => self
                        .transactions
                        .begin(info.modifier == rustdb_ast::TransactionModifierType::TransactionReadOnly)?,
                    TransactionType::Commit => {
                        // An aborted transaction's COMMIT rolls it back.
                        let rolls_back = self.transactions.is_invalidated();
                        let result = self.transactions.commit();
                        self.finish_transaction_attaches(rolls_back || result.is_err());
                        result?;
                    }
                    TransactionType::Rollback => {
                        self.transactions.rollback()?;
                        self.finish_transaction_attaches(true);
                    }
                    TransactionType::Invalid => {
                        return Err(Error::not_implemented("Unrecognized transaction type!"));
                    }
                }
                return Ok(nothing());
            }
            LogicalKind::Attach(info) => {
                let options = AttachOptions::from_info(info, crate::AccessMode::Automatic)?;
                let attached = self
                    .database
                    .attach(&info.name, &info.path, &options, info.on_conflict)?;
                if let Some(attached) = attached
                    && !self.transactions.is_auto_commit()
                {
                    self.transaction_attaches.push(attached);
                }
                return Ok(nothing());
            }
            LogicalKind::Detach(info) => {
                self.detach(info)?;
                return Ok(nothing());
            }
            LogicalKind::Set {
                name,
                scope: SetScope::Variable,
                ..
            } => {
                // `SET VARIABLE`: the value comes from running the child query
                // (DuckDB's `PhysicalSetVariable`).
                let name = name.clone();
                let child = plan
                    .children
                    .pop()
                    .ok_or_else(|| Error::internal("SET VARIABLE without a value plan"))?;
                let types = child.types.clone();
                let result = self.run_plan(child, vec![String::new(); types.len()], types)?;
                match result.row_count() {
                    0 => {}
                    1 => {
                        self.user_variables.insert(&name, result.value(0, 0));
                    }
                    _ => {
                        return Err(Error::invalid_input(
                            "PhysicalSetVariable can only handle a single value",
                        ));
                    }
                }
                return Ok(nothing());
            }
            LogicalKind::Reset {
                name,
                scope: SetScope::Variable,
            } => {
                self.user_variables.remove(name);
                return Ok(nothing());
            }
            LogicalKind::Set { name, value, .. } if is_search_path_setting(name) => {
                self.set_search_path(name, value)?;
                return Ok(nothing());
            }
            LogicalKind::Reset { name, .. } if is_search_path_setting(name) => {
                self.search_path.clear();
                self.search_path_set.clear();
                return Ok(nothing());
            }
            LogicalKind::Set { name, value, scope } => {
                if is_secret_setting(name) {
                    self.database.instance().secrets.set_option(name, Some(value))?;
                }
                if is_threads_setting(name) {
                    self.database.instance().set_threads(Some(value))?;
                }
                self.database.instance().set_buffer_setting(name, Some(value))?;
                if crate::logging::is_logging_setting(name) {
                    crate::logging::apply_setting(&self.database.instance().log_manager, name, Some(value))?;
                }
                self.settings.set(name, value)?;
                let (option, set_value, set_scope) = (name.clone(), value.clone(), *scope);
                self.record_setting(&option, Some(&set_value), set_scope);
                if is_checkpoint_threshold(name) {
                    self.database.instance().set_checkpoint_threshold(value)?;
                }
                if is_force_compression(name) {
                    self.database.instance().set_force_compression(Some(value))?;
                }
                return Ok(nothing());
            }
            LogicalKind::Reset { name, scope } => {
                if is_secret_setting(name) {
                    self.database.instance().secrets.set_option(name, None)?;
                }
                if crate::logging::is_logging_setting(name) {
                    crate::logging::apply_setting(&self.database.instance().log_manager, name, None)?;
                }
                self.settings.reset(name)?;
                if is_threads_setting(name) {
                    self.database.instance().set_threads(None)?;
                }
                self.database.instance().set_buffer_setting(name, None)?;
                let (option, reset_scope) = (name.clone(), *scope);
                self.record_setting(&option, None, reset_scope);
                if is_force_compression(name) {
                    self.database.instance().set_force_compression(None)?;
                }
                if is_checkpoint_threshold(name) {
                    self.database
                        .instance()
                        .checkpoint_wal_size
                        .store(DEFAULT_CHECKPOINT_WAL_SIZE, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(nothing());
            }
            LogicalKind::Prepare { name, prepared } => {
                self.prepared_statements
                    .insert(name.clone(), Arc::clone(prepared));
                return Ok(nothing());
            }
            LogicalKind::Deallocate { name } => {
                self.prepared_statements.remove(name);
                return Ok(nothing());
            }
            LogicalKind::CreateSecret(input) => {
                // `PhysicalCreateSecret`: one row with `true`.
                self.database.instance().secrets.create_secret(input)?;
                let column = rustdb_types::Vector::constant(&rustdb_types::Value::boolean(true))?;
                return Ok(MaterializedResult::new(
                    names,
                    types,
                    vec![rustdb_types::DataChunk::new(vec![column], 1)],
                ));
            }
            LogicalKind::Drop(info) if info.catalog_type == rustdb_ast::CatalogType::SecretEntry => {
                let (persist_type, storage) = match info.extra_drop_info.as_deref() {
                    Some(rustdb_ast::ExtraDropInfo::Secret(extra)) => {
                        (extra.persist_mode, extra.secret_storage.clone())
                    }
                    None => (rustdb_ast::SecretPersistType::Default, String::new()),
                };
                self.database.instance().secrets.drop_secret(
                    &info.name,
                    info.if_not_found,
                    persist_type,
                    &storage,
                )?;
                return Ok(nothing());
            }
            LogicalKind::Pragma { name, parameters } => {
                let lower = name.to_ascii_lowercase();
                if lower == "add_parquet_key" {
                    // `ParquetCrypto::AddKey` (`PragmaCall` with two VARCHAR arguments).
                    let [key_name, key] = parameters.as_slice() else {
                        return Err(Error::binder(format!(
                            "No function matches the given name and argument types '{name}({})'. You might need to add explicit type casts.",
                            parameters
                                .iter()
                                .map(|parameter| parameter.logical_type().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    };
                    rustdb_parquet::crypto::add_key(
                        &self.database.instance().object_cache,
                        &key_name.to_varchar()?,
                        &key.to_varchar()?,
                    )?;
                    return Ok(nothing());
                }
                if lower == "enable_optimizer" || lower == "disable_optimizer" {
                    self.settings.optimizer_disabled = lower == "disable_optimizer";
                }
                if lower == "disable_checkpoint_on_shutdown" || lower == "enable_checkpoint_on_shutdown" {
                    self.database
                        .instance()
                        .shutdown_checkpoint_disabled
                        .store(lower.starts_with("disable"), std::sync::atomic::Ordering::Relaxed);
                }
                if NO_OP_PRAGMAS.contains(&name.to_ascii_lowercase().as_str()) {
                    return Ok(nothing());
                }
                // `Binder::BindPragma`: a table function of the name is suggested.
                let transaction = self.transactions.current();
                if self
                    .database
                    .instance()
                    .system_catalog
                    .get_entry(
                        &transaction,
                        rustdb_catalog::CatalogType::TableFunction,
                        "main",
                        name,
                    )
                    .is_some()
                {
                    return Err(Error::catalog(format!(
                        "Pragma Function with name {name} does not exist, but a table function with the same name \
                         exists, try `CALL {name}(...)`"
                    )));
                }
                return Err(Error::not_implemented(format!("PRAGMA {name}")));
            }
            LogicalKind::Execute { .. } => {
                // The prepared statement was planned again with the values: run that plan.
                plan = plan
                    .children
                    .pop()
                    .ok_or_else(|| Error::internal("EXECUTE without a plan"))?;
            }
            _ => {}
        }
        // `PhysicalDrop`: dropping the current schema resets the schema to `main`.
        let dropped_schema = match &plan.kind {
            LogicalKind::Drop(info) if info.catalog_type == rustdb_ast::CatalogType::SchemaEntry => {
                Some((info.catalog.clone(), info.name.clone()))
            }
            _ => None,
        };
        let result = self.execute_plan(plan, names, types)?;
        if let Some((catalog, schema)) = dropped_schema {
            let (current_catalog, current_schema) = self
                .search_path
                .first()
                .cloned()
                .unwrap_or_else(|| (self.default_catalog(), rustdb_catalog::DEFAULT_SCHEMA.to_owned()));
            if catalog == current_catalog && schema == current_schema {
                self.set_search_path(
                    "schema",
                    &rustdb_types::Value::varchar(rustdb_catalog::DEFAULT_SCHEMA),
                )?;
            }
        }
        Ok(result)
    }

    /// Plans and runs a logical plan, collecting its result.
    fn execute_plan(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        let transaction = self.transactions.current();
        let client = self.client_context(transaction);
        let optimizer_config = self.settings.optimizer_config();
        let optimizer_client = optimizer_config.enabled.then(|| Arc::clone(&client));
        let preserve_insertion_order = optimizer_config.preserve_insertion_order;
        let plan_client = Arc::clone(&client);
        plan = Optimizer::new(client, optimizer_config).optimize(plan)?;
        ColumnBindingResolver::resolve(&mut plan)?;
        seed_samples(&mut plan, &self.function_client);
        let context = PlanContext {
            transaction: self.transactions.current(),
            catalogs: self.catalogs(&self.default_catalog()),
            join_settings: self.settings.join_plan_settings(),
            explain_output: self.settings.explain_output(),
            query: self.function_client.query(),
            optimizer_client,
            threads: self.database.instance().env.threads(),
            preserve_insertion_order,
            index_scan: self.settings.index_scan_thresholds(),
            client: plan_client,
        };
        let physical = create_physical_plan_with(plan, &context)?;
        // EXPLAIN ANALYZE profiles the run.
        let profiler =
            (physical.name() == "EXPLAIN_ANALYZE").then(|| Arc::new(rustdb_execution::QueryProfiler::new()));
        let env = Arc::clone(&self.database.instance().env);
        let root: PhysicalRef = Arc::new(PhysicalResultCollector::for_plan(
            physical,
            env.threads(),
            preserve_insertion_order,
        ));
        let options = rustdb_execution::QueryOptions {
            profiler,
            preserve_insertion_order,
        };
        let state = Executor::execute_with(&root, env, &options)?;
        let collected = state
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("result collector state has an unexpected type"))?;
        let chunks = std::mem::take(&mut collected.lock().unwrap_or_else(PoisonError::into_inner).chunks);
        Ok(MaterializedResult::new(names, types, chunks))
    }
}

/// Gives every unseeded sample a seed from the connection's random engine, children
/// first, as DuckDB's physical planner does (`CreatePlan(LogicalSample &)`).
fn seed_samples(plan: &mut LogicalOperator, client: &rustdb_function::FunctionClient) {
    for child in &mut plan.children {
        seed_samples(child, client);
    }
    if let LogicalKind::Sample { sample_options } = &mut plan.kind
        && sample_options.seed.is_none()
    {
        sample_options.seed = Some(u64::from(client.random_engine().next_random_integer()));
    }
}

/// Whether `name` is `schema` or `search_path`, which set the catalog search path.
fn is_search_path_setting(name: &str) -> bool {
    name.eq_ignore_ascii_case("schema") || name.eq_ignore_ascii_case("search_path")
}

/// `CatalogSearchEntry::ParseList`: comma-separated `[catalog.]schema` entries with
/// double-quoted identifiers.
fn parse_search_path(input: &str) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let finish_entry = |parts: &mut Vec<String>,
                        current: &mut String,
                        entries: &mut Vec<(String, String)>|
     -> Result<()> {
        parts.push(std::mem::take(current));
        let entry = match parts.len() {
            1 => (String::new(), parts[0].clone()),
            2 => (parts[0].clone(), parts[1].clone()),
            _ => {
                return Err(Error::parser(format!(
                    "Too many dots - expected [schema] or [catalog.schema] for search path entry \"{input}\""
                )));
            }
        };
        parts.clear();
        if !entry.1.is_empty() {
            entries.push(entry);
        }
        Ok(())
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        current.push('"');
                    }
                    Some('"') => break,
                    Some(c) => current.push(c),
                    None => {
                        return Err(Error::parser(format!(
                            "Unterminated quote in qualified name! (input: {input})"
                        )));
                    }
                }
            },
            '.' => parts.push(std::mem::take(&mut current)),
            ',' => finish_entry(&mut parts, &mut current, &mut entries)?,
            c if c.is_whitespace() && current.is_empty() => {}
            c => current.push(c),
        }
    }
    finish_entry(&mut parts, &mut current, &mut entries)?;
    Ok(entries)
}

/// The databases a plan writes to: the catalog of DDL statements and the database of
/// the table of DML statements.
fn collect_modified_databases(plan: &LogicalOperator, instance: &DatabaseInstance, out: &mut Vec<String>) {
    let mut push = |name: &str| {
        if !name.is_empty() && !out.iter().any(|existing| existing.eq_ignore_ascii_case(name)) {
            out.push(name.to_owned());
        }
    };
    let table_database = |entry: &rustdb_catalog::CatalogEntry| -> Option<String> {
        let rustdb_catalog::EntryKind::Table(table) = &entry.kind else {
            return None;
        };
        let storage = table.storage.as_ref()?;
        instance
            .databases
            .all()
            .into_iter()
            .find(|database| {
                database
                    .catalog
                    .io()
                    .is_ok_and(|io| Arc::ptr_eq(io, storage.io()))
            })
            .map(|database| database.name.clone())
    };
    match &plan.kind {
        LogicalKind::Insert(insert) => {
            if let Some(name) = table_database(&insert.table) {
                push(&name);
            }
        }
        LogicalKind::Delete(delete) => {
            if let Some(name) = table_database(&delete.table) {
                push(&name);
            }
        }
        LogicalKind::Update(update) => {
            if let Some(name) = table_database(&update.table) {
                push(&name);
            }
        }
        LogicalKind::MergeInto(merge) => {
            // Only UPDATE, DELETE and INSERT actions modify the table.
            let modifies = merge.actions.values().flatten().any(|action| {
                matches!(
                    action.action_type,
                    rustdb_ast::MergeActionType::MergeUpdate
                        | rustdb_ast::MergeActionType::MergeDelete
                        | rustdb_ast::MergeActionType::MergeInsert
                )
            });
            if modifies && let Some(name) = table_database(&merge.table) {
                push(&name);
            }
        }
        LogicalKind::CreateTable(info) => push(&info.catalog),
        LogicalKind::Create { info, .. } => push(&info.catalog),
        LogicalKind::CreateIndex(index) => {
            if let Some(name) = table_database(&index.table) {
                push(&name);
            }
        }
        LogicalKind::Alter(alter) => push(&alter.catalog),
        LogicalKind::Drop(info) => push(&info.catalog),
        _ => {}
    }
    for child in &plan.children {
        collect_modified_databases(child, instance, out);
    }
}

/// Whether a plan updates table data or changes the catalog, which keeps checkpoints from
/// replacing the table data until the transaction ends (DuckDB's `SetModifications`);
/// inserts and deletes do not.
fn needs_checkpoint_lock(plan: &LogicalOperator) -> bool {
    let own = match &plan.kind {
        LogicalKind::Update(_)
        | LogicalKind::CreateTable(_)
        | LogicalKind::Create { .. }
        | LogicalKind::CreateIndex(_)
        | LogicalKind::Alter(_)
        | LogicalKind::Drop(_) => true,
        LogicalKind::MergeInto(merge) => merge
            .actions
            .values()
            .flatten()
            .any(|action| action.action_type == rustdb_ast::MergeActionType::MergeUpdate),
        _ => false,
    };
    own || plan.children.iter().any(needs_checkpoint_lock)
}

/// Pragmas without a result that only tune testing or profiling behaviour.
const NO_OP_PRAGMAS: &[&str] = &[
    "enable_verification",
    "disable_verification",
    "verify_external",
    "disable_verify_external",
    "verify_serializer",
    "disable_verify_serializer",
    "verify_parallelism",
    "disable_verify_parallelism",
    "verify_fetch_row",
    "disable_verify_fetch_row",
    "enable_profiling",
    "enable_profile",
    "disable_profiling",
    "disable_profile",
    "enable_progress_bar",
    "disable_progress_bar",
    "enable_print_progress_bar",
    "disable_print_progress_bar",
    "enable_object_cache",
    "disable_object_cache",
    "enable_optimizer",
    "disable_optimizer",
    "enable_checkpoint_on_shutdown",
    "disable_checkpoint_on_shutdown",
    // Nothing to checkpoint while databases live in memory.
    "checkpoint",
    "force_checkpoint",
];

/// Whether an error aborts an explicit transaction (DuckDB's
/// `Exception::InvalidatesTransaction`).
const fn invalidates_transaction(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Binder
            | ErrorKind::Catalog
            | ErrorKind::Connection
            | ErrorKind::ParameterNotAllowed
            | ErrorKind::Parser
            | ErrorKind::Permission
    )
}
