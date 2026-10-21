use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{SetScope, SqlStatement, TransactionType};
use rustdb_catalog::{Catalog, Transaction, TransactionManager};
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_execution::operators::PhysicalResultCollector;
use rustdb_execution::{
    PhysicalRef, PlanContext, QueryExecution, ScalarEvaluator, create_physical_plan_with,
};
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

mod active_query;
mod appender;
mod prepared_statement;
mod registration;

pub use active_query::{
    ClientHandle, PendingExecutionResult, PendingQuery, QueryId, QueryProgress, QueryResult,
    StreamQueryResult,
};
pub(crate) use active_query::{PlanExecution, RunMode, StatementRun};
pub use appender::{Appender, ColumnDescription, TableDescription};
pub use prepared_statement::PreparedStatement;

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
    /// The name of the database opened at start-up (the default database), which
    /// `ALTER DATABASE ... SET ALIAS TO` renames.
    default_database: std::sync::RwLock<Option<String>>,
    /// The cast functions (DuckDB's `DBConfig::GetCastFunctions`); extensions replace
    /// the set when they register casts.
    casts: std::sync::RwLock<Arc<CastFunctionSet>>,
    /// Whether registered casts depend on the connection's settings (the ICU casts): the
    /// connections then bind casts for themselves.
    client_casts: std::sync::atomic::AtomicBool,
    /// The options extensions added (DuckDB's `DBConfig::extension_parameters`).
    extension_options: std::sync::RwLock<Vec<rustdb_function::ExtensionOption>>,
    /// The global values of the extension options (DuckDB's global user settings),
    /// starting at their defaults.
    extension_globals: Mutex<CaseInsensitiveMap<Value>>,
    /// Options given at start-up that no option knows yet (DuckDB's
    /// `DBConfigOptions::unrecognized_options`); an extension option of that name takes
    /// the value when its extension loads.
    unrecognized_options: Mutex<Vec<(String, Value)>>,
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
    /// Whether the instance can still be used (`DatabaseInstance::GetValidChecker`).
    pub(crate) validity: Arc<crate::attached::ValidChecker>,
    /// `wal_autocheckpoint_entries`: a commit that leaves this many WAL entries
    /// checkpoints (0: disabled).
    pub(crate) wal_autocheckpoint_entries: std::sync::atomic::AtomicU64,
    /// How checkpoints reuse metadata (`experimental_metadata_reuse`,
    /// `force_column_metadata_reuse`; global settings): applied to the storage of every
    /// attached database.
    checkpoint_config: std::sync::Mutex<rustdb_storage::storage_manager::CheckpointConfig>,
    /// How large appends are written to database files before they commit
    /// (`enable_optimistic_write`, `write_buffer_row_group_count`,
    /// `write_buffer_row_group_memory_limit`; global settings): applied to the storage of
    /// every attached database.
    optimistic_write: std::sync::Mutex<rustdb_storage::OptimisticWriteConfig>,
    /// How checkpoints shred VARIANT columns (`variant_minimum_shredding_size`,
    /// `force_variant_shredding`; global settings): applied to the storage of every
    /// attached database.
    variant_shredding: std::sync::Mutex<rustdb_storage::table::VariantShreddingConfig>,
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
    loaded_extensions: Arc<Mutex<Vec<StaticExtension>>>,
    /// The replacement scans clients added (DuckDB's `DBConfig::replacement_scans` after
    /// the built-in ones).
    replacement_scans: std::sync::RwLock<Vec<rustdb_planner::ReplacementScan>>,
}

/// What the database's own functions reach through the client context (DuckDB's
/// `ClientContext::db`): the secret manager, and the instance for functions that attach
/// databases (`read_duckdb`). The instance is held weakly, as bind data can outlive the
/// statement (prepared statements).
#[derive(Debug)]
pub(crate) struct InstanceState {
    pub(crate) secrets: Arc<crate::secrets::SecretManager>,
    instance: std::sync::Weak<DatabaseInstance>,
}

impl InstanceState {
    /// The instance, held weakly.
    pub(crate) fn instance(&self) -> std::sync::Weak<DatabaseInstance> {
        self.instance.clone()
    }
}

impl rustdb_planner::DatabaseState for InstanceState {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The instance state of the database a call is bound in.
pub(crate) fn instance_state(context: &ClientContext) -> Result<&InstanceState> {
    context
        .database_state
        .as_ref()
        .and_then(|state| state.as_any().downcast_ref::<InstanceState>())
        .ok_or_else(|| Error::internal("function bound without a database instance"))
}

/// The object cache key of the database's loaded extensions, read by `duckdb_extensions()`.
const LOADED_EXTENSIONS_KEY: &str = "rustdb.loaded_extensions";

/// The object cache key of the extensions the client loaded itself, read by
/// `duckdb_extensions()`.
pub(crate) const CLIENT_EXTENSIONS_KEY: &str = "rustdb.client_extensions";

/// The name and description of each extension the client loaded itself.
pub(crate) type ClientExtensions = Mutex<Vec<(String, String)>>;

/// The memory of a database's buffer manager, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryStatus {
    /// `memory_limit`.
    pub memory_limit: u64,
    /// The memory in use.
    pub memory_usage: u64,
    /// The bytes in the temporary directory.
    pub swap_usage: u64,
    /// The limit of the temporary directory, once it is in use; `None` for no limit.
    pub swap_max: Option<u64>,
}

/// Whether the statically linked extension `name` is loaded (`ExtensionManager`).
/// Extensions linked without functions of their own are always loaded.
pub(crate) fn static_extension_loaded(cache: &rustdb_function::ObjectCache, name: &str) -> bool {
    let Some(extension) = StaticExtension::from_name(name) else {
        return true;
    };
    cache
        .get::<Mutex<Vec<StaticExtension>>>(LOADED_EXTENSIONS_KEY)
        .is_some_and(|loaded| {
            loaded
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&extension)
        })
}

/// An extension linked into the engine whose functions are registered on load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaticExtension {
    Parquet,
    Json,
    Tpch,
    Autocomplete,
    Icu,
}

impl StaticExtension {
    const ALL: [Self; 5] = [
        Self::Parquet,
        Self::Json,
        Self::Tpch,
        Self::Autocomplete,
        Self::Icu,
    ];

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "parquet" => Some(Self::Parquet),
            "json" => Some(Self::Json),
            rustdb_tpch::EXTENSION_NAME => Some(Self::Tpch),
            rustdb_autocomplete::EXTENSION_NAME => Some(Self::Autocomplete),
            rustdb_icu::EXTENSION_NAME => Some(Self::Icu),
            _ => None,
        }
    }

    /// The pragmas the extension registers (`ExtensionLoader::RegisterFunction` of a
    /// `PragmaFunction`).
    fn pragma_functions(self) -> Vec<rustdb_function::PragmaFunctionSet> {
        use rustdb_function::{PragmaFunction, PragmaFunctionSet};
        let function = match self {
            Self::Parquet => PragmaFunction::call(
                "add_parquet_key",
                vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
            ),
            Self::Json => PragmaFunction::call("json_execute_serialized_sql", vec![LogicalType::VARCHAR]),
            Self::Tpch => PragmaFunction::call("tpch", vec![LogicalType::BIGINT]),
            Self::Autocomplete | Self::Icu => return Vec::new(),
        };
        vec![PragmaFunctionSet::single(function)]
    }

    /// Whether the extension has a pragma `name` that stands for a query.
    fn has_pragma(self, name: &str) -> bool {
        match self {
            Self::Tpch => name.eq_ignore_ascii_case("tpch"),
            Self::Parquet | Self::Json | Self::Autocomplete | Self::Icu => false,
        }
    }

    /// The SQL the extension's pragma `name` stands for.
    fn pragma_query(self, name: &str, parameters: &[Value]) -> Result<String> {
        match self {
            Self::Tpch => rustdb_tpch::pragma_tpch_query(parameters),
            Self::Parquet | Self::Json | Self::Autocomplete | Self::Icu => {
                Err(Error::internal(format!("no pragma {name}")))
            }
        }
    }

    /// Registers the extension's functions, types and macros (its `Load`); `loaded` are
    /// the extensions loaded so far.
    fn register(self, system: &Catalog, loaded: &Arc<Mutex<Vec<Self>>>) -> Result<()> {
        for set in self.pragma_functions() {
            system.register_pragma_functions(set)?;
        }
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
            Self::Tpch => {
                for set in rustdb_tpch::tpch_table_functions() {
                    system.register_table_functions(set)?;
                }
            }
            Self::Autocomplete => {
                let loaded = Arc::clone(loaded);
                let pragma_names = Arc::new(move || pragma_function_names(&loaded));
                for set in rustdb_autocomplete::autocomplete_table_functions(pragma_names) {
                    system.register_table_functions(set)?;
                }
            }
            Self::Icu => {
                use rustdb_icu::IcuExtension;
                for set in IcuExtension::scalar_functions() {
                    system.register_scalar_functions_merged(set)?;
                }
                for set in IcuExtension::table_functions() {
                    system.register_table_functions_merged(set)?;
                }
                for name in rustdb_icu::PATCHED_FUNCTIONS {
                    system.alter_builtin_scalar_functions(name, IcuExtension::patch_function)?;
                }
                for (name, collation) in IcuExtension::collations() {
                    system.register_collation(
                        &name,
                        rustdb_catalog::CollationEntry {
                            function: collation,
                            combinable: false,
                            not_required_for_equality: false,
                        },
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// The names of the pragma functions: the builtin ones and those of the `loaded`
/// extensions (DuckDB's `PRAGMA_FUNCTION_ENTRY` catalog entries).
fn pragma_function_names(loaded: &Mutex<Vec<StaticExtension>>) -> Vec<String> {
    let loaded = loaded.lock().unwrap_or_else(PoisonError::into_inner);
    crate::system_functions::builtin_pragma_functions()
        .into_iter()
        .chain(loaded.iter().flat_map(|extension| extension.pragma_functions()))
        .map(|set| set.name.to_string())
        .collect()
}

/// The globally set options and their version (see `DatabaseInstance::global_settings`).
#[derive(Debug, Default, Clone)]
struct GlobalSettings {
    version: u64,
    values: Vec<(&'static str, Value)>,
}

/// The default of `checkpoint_threshold` (16 MiB).
const DEFAULT_CHECKPOINT_WAL_SIZE: u64 = 16 * 1024 * 1024;

/// `ForceVariantShredding::SetGlobal`: the shredded type of a `force_variant_shredding`
/// value, a type name whose every level is wrapped in `STRUCT(typed_value,
/// untyped_value_index)`.
fn parse_force_variant_shredding(value: &Value) -> Result<LogicalType> {
    use rustdb_types::LogicalTypeId as L;
    if value.logical_type().id() != L::Varchar {
        return Err(Error::invalid_input(format!(
            "The argument to 'force_variant_shredding' should be of type VARCHAR, not {}",
            value.logical_type()
        )));
    }
    let text = value.to_varchar()?;
    // `UnboundType::TryParseAndDefaultBind`.
    let parsed = if text.is_empty() {
        None
    } else {
        let columns = rustdb_parser::parse_column_list(
            &format!("dummy {text}"),
            &rustdb_parser::ParserOptions::default(),
        )
        .map_err(|_| Error::invalid_input(format!("Could not parse type string '{text}'")))?;
        columns
            .columns
            .into_iter()
            .next()
            .map(|column| column.logical_type)
    };
    let bound = match parsed {
        Some(ty) => match ty.info() {
            Some(rustdb_types::TypeInfo::Unbound(unbound)) => unbound.default_bind()?,
            _ => ty,
        },
        None => LogicalType::INVALID,
    };
    if matches!(bound.id(), L::Invalid | L::Unbound) {
        return Err(Error::invalid_input(format!(
            "Could not parse the argument '{text}' to 'force_variant_shredding' as a built in type"
        )));
    }
    check_shredding_type(&bound)?;
    Ok(rustdb_storage::statistics::wrap_shredded_levels(&bound))
}

/// The type checks of `ForceVariantShredding::SetGlobal`.
fn check_shredding_type(ty: &LogicalType) -> Result<()> {
    use rustdb_types::LogicalTypeId as L;
    if ty.is_nested() {
        if !matches!(ty.id(), L::Struct | L::List) {
            return Err(Error::invalid_input(format!(
                "Shredding can consist of the nested types LIST (for ARRAY Variant values) or STRUCT (for OBJECT \
                 Variant values), not {ty}"
            )));
        }
        if ty.id() == L::Struct && ty.is_unnamed_struct() {
            return Err(Error::invalid_input(
                "STRUCT types in the shredding can not be empty",
            ));
        }
        if let Some(child) = ty.child_type() {
            check_shredding_type(child)?;
        }
        for (_, field) in ty.struct_children().unwrap_or_default() {
            check_shredding_type(field)?;
        }
        return Ok(());
    }
    match ty.id() {
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::HugeInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::UHugeInt
        | L::Float
        | L::Double
        | L::Decimal
        | L::Date
        | L::Time
        | L::TimeTz
        | L::TimestampTz
        | L::Timestamp
        | L::TimestampSec
        | L::TimestampMs
        | L::TimestampNs
        | L::Blob
        | L::Varchar
        | L::Uuid
        | L::Bignum
        | L::TimeNs
        | L::Interval
        | L::Bit
        | L::Geometry => Ok(()),
        _ => Err(Error::invalid_input(format!(
            "Variants can not be shredded on type: {ty}"
        ))),
    }
}

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

/// Whether `name` is an option an extension adds rather than a built-in option (DuckDB
/// keeps such start-up options as `unrecognized_options` until the extension loads).
fn is_extension_setting(name: &str) -> bool {
    crate::settings::canonical_option(name).is_none()
        && rustdb_function::extension_entries::find_extension(
            name,
            rustdb_function::extension_entries::EXTENSION_SETTINGS,
        )
        .is_some()
}

/// Whether `ty` is or contains a type of id `id`.
fn contains_type(ty: &LogicalType, id: rustdb_types::LogicalTypeId) -> bool {
    use rustdb_types::LogicalTypeId as L;
    if ty.id() == id {
        return true;
    }
    match ty.id() {
        L::List => ty.child_type().is_some_and(|child| contains_type(child, id)),
        L::Array => ty
            .array_child()
            .is_some_and(|(child, _)| contains_type(child, id)),
        L::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| contains_type(key, id) || contains_type(value, id)),
        L::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| contains_type(child, id))),
        L::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| contains_type(member, id))),
        _ => false,
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
            let _ = database.close(checkpoint, crate::attached::CloseAction::TryCheckpoint);
        }
    }
}

impl DatabaseInstance {
    /// Registers a statically linked extension's catalog entries once.
    /// `DisabledFilesystemsSetting::SetGlobal` / `ResetGlobal`.
    fn set_disabled_file_systems(&self, names: &[&str]) -> Result<()> {
        self.object_cache
            .get::<rustdb_function::file_system::DisabledFileSystems>(
                rustdb_function::file_system::DISABLED_FILE_SYSTEMS_KEY,
            )
            .ok_or_else(|| Error::internal("database without a file system registry"))?
            .set(names)
    }

    fn load_static_extension(&self, extension: StaticExtension) -> Result<()> {
        let mut loaded = self
            .loaded_extensions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !loaded.contains(&extension) {
            extension.register(&self.system_catalog, &self.loaded_extensions)?;
            if extension == StaticExtension::Icu {
                self.load_icu_config();
            }
            if extension == StaticExtension::Parquet {
                // `ParquetExtension::Load` registers the ZSTD file system.
                self.object_cache.put(
                    rustdb_function::file_system::ZSTD_FILE_SYSTEM_KEY.to_owned(),
                    Arc::new(()),
                );
            }
            loaded.push(extension);
        }
        Ok(())
    }

    /// The configuration parts of `IcuExtension::Load`: the casts and the `TimeZone` and
    /// `Calendar` options.
    fn load_icu_config(&self) {
        let mut casts = (*self.casts()).clone();
        rustdb_icu::IcuExtension::register_casts(&mut casts);
        *self.casts.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(casts);
        self.client_casts
            .store(true, std::sync::atomic::Ordering::Release);
        for option in rustdb_icu::IcuExtension::options() {
            self.add_extension_option(option);
        }
    }

    /// The collations extensions registered in the system catalog, with whether they are
    /// combinable.
    pub(crate) fn extension_collations(&self) -> Vec<(String, bool)> {
        let Some(schema) = self.system_catalog.schema(
            &rustdb_catalog::Transaction::system(),
            rustdb_catalog::DEFAULT_SCHEMA,
        ) else {
            return Vec::new();
        };
        let Some(schema) = schema.as_schema() else {
            return Vec::new();
        };
        schema
            .collations
            .scan(&rustdb_catalog::Transaction::system())
            .iter()
            .filter_map(|entry| match &entry.kind {
                rustdb_catalog::EntryKind::Collation(collation) => {
                    Some((entry.name().to_owned(), collation.combinable))
                }
                _ => None,
            })
            .collect()
    }

    /// The cast functions.
    pub(crate) fn casts(&self) -> Arc<CastFunctionSet> {
        Arc::clone(&self.casts.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// `DBConfig::AddExtensionOption`: a start-up value given under the option's name, or
    /// else its default, becomes its global value.
    fn add_extension_option(&self, option: rustdb_function::ExtensionOption) {
        let mut globals = self
            .extension_globals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut unrecognized = self
            .unrecognized_options
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(position) = unrecognized
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(&option.name))
        {
            let (_, value) = unrecognized.remove(position);
            globals.insert(&option.name, value);
        }
        if !option.default_value.is_null() && globals.get(&option.name).is_none() {
            globals.insert(&option.name, option.default_value.clone());
        }
        let mut options = self
            .extension_options
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        options.retain(|existing| !existing.name.eq_ignore_ascii_case(&option.name));
        options.push(option);
    }

    /// The extension option called `name` (case-insensitive), if an extension added it
    /// (`DBConfig::TryGetExtensionOption`).
    pub(crate) fn extension_option(&self, name: &str) -> Option<rustdb_function::ExtensionOption> {
        self.extension_options
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|option| option.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Every extension option with its global value.
    pub(crate) fn extension_option_values(&self) -> Vec<(rustdb_function::ExtensionOption, Value)> {
        let globals = self
            .extension_globals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.extension_options
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|option| {
                let value = globals
                    .get(&option.name)
                    .cloned()
                    .unwrap_or_else(|| option.default_value.clone());
                (option.clone(), value)
            })
            .collect()
    }

    /// Sets (`Some`) or resets (`None`, to the default) the global value of extension
    /// option `option` (`DBConfig::SetOption` / `ResetOption`).
    pub(crate) fn set_extension_global(
        &self,
        option: &rustdb_function::ExtensionOption,
        value: Option<Value>,
    ) {
        let mut globals = self
            .extension_globals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match value {
            Some(value) => {
                globals.insert(&option.name, value);
            }
            None if !option.default_value.is_null() => {
                globals.insert(&option.name, option.default_value.clone());
            }
            None => {
                globals.remove(&option.name);
            }
        }
    }

    /// Keeps a start-up option no option knows (yet) for the extension that adds it.
    pub(crate) fn add_unrecognized_option(&self, name: &str, value: Value) {
        if let Some(option) = self.extension_option(name) {
            self.set_extension_global(&option, Some(value));
            return;
        }
        let mut unrecognized = self
            .unrecognized_options
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        unrecognized.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        unrecognized.push((name.to_owned(), value));
    }

    /// Whether a loaded extension registers a parser override (DuckDB's
    /// `ParserExtension::Register`; the autocomplete extension's PEG parser).
    fn has_parser_extension(&self) -> bool {
        self.loaded_extensions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&StaticExtension::Autocomplete)
    }

    /// The extension providing pragma `name`, if one is loaded.
    fn extension_with_pragma(&self, name: &str) -> Option<StaticExtension> {
        self.loaded_extensions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .find(|extension| extension.has_pragma(name))
    }

    /// Whether a loaded extension has a pragma `name`.
    fn has_extension_pragma(&self, name: &str) -> bool {
        self.extension_with_pragma(name).is_some()
    }

    /// The SQL the pragma `name` of a loaded extension stands for (a
    /// `PragmaFunction::PragmaCall` with a query).
    fn extension_pragma_query(&self, name: &str, parameters: &[Value]) -> Result<String> {
        self.extension_with_pragma(name)
            .ok_or_else(|| Error::internal(format!("no extension has the pragma {name}")))?
            .pragma_query(name, parameters)
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
    /// `max_memory`), `temp_directory`, `max_temp_directory_size` and
    /// `block_allocator_memory`; other names are ignored.
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
                if !rustdb_function::file_system::external_access_enabled() {
                    return Err(Error::new(
                        rustdb_common::ErrorKind::Permission,
                        "Modifying the temp_directory has been disabled by configuration",
                    ));
                }
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
            "block_allocator_memory" => {
                let size = match value {
                    Some(value) => crate::settings::block_allocator_size(value, buffer_manager.max_memory())?,
                    None => 0,
                };
                buffer_manager.pool().allocator().resize(size as usize)
            }
            _ => Ok(()),
        }
    }

    /// `SET` / `RESET` (`None`) of `variant_minimum_shredding_size` and
    /// `force_variant_shredding`; other names are ignored.
    fn set_variant_shredding_setting(&self, name: &str, value: Option<&rustdb_types::Value>) -> Result<()> {
        use rustdb_storage::table::VariantShreddingConfig;
        let update: Box<dyn FnOnce(&mut VariantShreddingConfig)> =
            if name.eq_ignore_ascii_case("variant_minimum_shredding_size") {
                let rows = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::BIGINT)?
                        .as_i64()
                        .unwrap_or(0),
                    None => VariantShreddingConfig::default().minimum_size,
                };
                Box::new(move |config| config.minimum_size = rows)
            } else if name.eq_ignore_ascii_case("force_variant_shredding") {
                let forced = value.map(parse_force_variant_shredding).transpose()?;
                Box::new(move |config| config.forced_type = forced)
            } else {
                return Ok(());
            };
        let config = {
            let mut config = self
                .variant_shredding
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            update(&mut config);
            config.clone()
        };
        for database in self.databases.all() {
            database.storage.set_variant_shredding_config(config.clone());
        }
        Ok(())
    }

    /// How checkpoints shred VARIANT columns.
    fn variant_shredding_config(&self) -> rustdb_storage::table::VariantShreddingConfig {
        self.variant_shredding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `SET` / `RESET` (`None`) of `wal_autocheckpoint_entries`; other names are ignored.
    fn set_wal_autocheckpoint_entries(&self, name: &str, value: Option<&rustdb_types::Value>) -> Result<()> {
        if !name.eq_ignore_ascii_case("wal_autocheckpoint_entries") {
            return Ok(());
        }
        let entries = match value {
            Some(value) => {
                use rustdb_function::cast::ValueCast;
                value
                    .default_cast_as(&rustdb_types::LogicalType::UBIGINT)?
                    .as_u64()
                    .unwrap_or(0)
            }
            None => 0,
        };
        self.wal_autocheckpoint_entries
            .store(entries, std::sync::atomic::Ordering::Relaxed);
        Ok(())
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
            database.storage.set_compression_config(config);
        }
        Ok(())
    }

    /// `SET` / `RESET` (`None`) of `disabled_compression_methods`
    /// (`DisabledCompressionMethodsSetting::SetGlobal`) and `force_bitpacking_mode`
    /// (`ForceBitpackingModeSetting::OnSet`); other names are ignored.
    fn set_disabled_compression_methods(
        &self,
        name: &str,
        value: Option<&rustdb_types::Value>,
    ) -> Result<()> {
        use rustdb_storage::compression::{CompressionConfig, CompressionType};
        if name.eq_ignore_ascii_case("force_bitpacking_mode") {
            let mode = match value {
                Some(value) => {
                    let text = value.to_varchar()?;
                    CompressionConfig::bitpacking_mode_from_name(&text).ok_or_else(|| {
                        rustdb_function::logging::enum_error(
                            &text,
                            "BitpackingMode",
                            &[
                                "INVALID",
                                "AUTO",
                                "CONSTANT",
                                "CONSTANT_DELTA",
                                "DELTA_FOR",
                                "FOR",
                            ],
                        )
                    })?
                }
                None => CompressionConfig::default().force_bitpacking_mode,
            };
            let config = {
                let mut config = self
                    .compression
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                config.force_bitpacking_mode = mode;
                *config
            };
            for database in self.databases.all() {
                database.storage.set_compression_config(config);
            }
            return Ok(());
        }
        if !name.eq_ignore_ascii_case("disabled_compression_methods") {
            return Ok(());
        }
        let mut methods = Vec::new();
        if let Some(value) = value {
            for entry in value.to_varchar()?.split(',') {
                let param = entry.trim().to_lowercase();
                if param.is_empty() {
                    continue;
                }
                if param == "none" {
                    methods.clear();
                    break;
                }
                let compression = CompressionType::from_enum_name(&param).ok_or_else(|| {
                    let candidates: Vec<String> = CompressionType::ALL
                        .iter()
                        .map(|compression| compression.enum_name().to_owned())
                        .collect();
                    Error::not_implemented(format!(
                        "Enum value: unrecognized value \"{param}\" for enum \"CompressionType\"\n{}",
                        rustdb_common::string_util::candidates_error_message(
                            &candidates,
                            &param,
                            "Candidates"
                        )
                    ))
                })?;
                if matches!(
                    compression,
                    CompressionType::Auto
                        | CompressionType::Constant
                        | CompressionType::Empty
                        | CompressionType::Uncompressed
                ) {
                    return Err(Error::invalid_input(format!(
                        "Compression method {param} cannot be disabled"
                    )));
                }
                methods.push(compression);
            }
        }
        let config = {
            let mut config = self
                .compression
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            config.set_disabled(&methods);
            *config
        };
        for database in self.databases.all() {
            database.storage.set_compression_config(config);
        }
        Ok(())
    }

    /// `SET` / `RESET` (`None`) of the settings checkpoints follow
    /// (`ExperimentalMetadataReuseSetting`, `ForceColumnMetadataReuseSetting`,
    /// `MaxVacuumTasksSetting`, `DebugCheckpointAbortSetting`); other names are ignored.
    fn set_checkpoint_setting(&self, name: &str, value: Option<&rustdb_types::Value>) -> Result<()> {
        use rustdb_function::cast::ValueCast;
        use rustdb_storage::storage_manager::{CheckpointAbort, CheckpointConfig};
        let lower = name.to_ascii_lowercase();
        let update: Box<dyn Fn(&mut CheckpointConfig)> = match (lower.as_str(), value) {
            ("experimental_metadata_reuse" | "force_column_metadata_reuse", value) => {
                let enabled = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                    None => lower == "experimental_metadata_reuse",
                };
                if lower == "experimental_metadata_reuse" {
                    Box::new(move |config| config.metadata_reuse = enabled)
                } else {
                    Box::new(move |config| config.force_column_metadata_reuse = enabled)
                }
            }
            ("max_vacuum_tasks", value) => {
                let tasks = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::UBIGINT)?
                        .as_u64()
                        .unwrap_or(0),
                    None => CheckpointConfig::default().max_vacuum_tasks,
                };
                Box::new(move |config| config.max_vacuum_tasks = tasks)
            }
            // `DebugCheckpointSleepMsSetting`.
            ("debug_checkpoint_sleep_ms", value) => {
                let sleep_ms = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::UBIGINT)?
                        .as_u64()
                        .unwrap_or(0),
                    None => CheckpointConfig::default().sleep_ms,
                };
                Box::new(move |config| config.sleep_ms = sleep_ms)
            }
            // `GeometryMinimumShreddingSize`.
            ("geometry_minimum_shredding_size", value) => {
                let rows = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::BIGINT)?
                        .as_i64()
                        .unwrap_or(0),
                    None => CheckpointConfig::default().geometry_minimum_shredding_size,
                };
                Box::new(move |config| config.geometry_minimum_shredding_size = rows)
            }
            // `DebugCheckpointAbortSetting::OnSet`.
            ("debug_checkpoint_abort", Some(value)) => {
                if value.is_null() {
                    return Err(Error::invalid_input(
                        "debug_checkpoint_abort setting cannot be NULL",
                    ));
                }
                let text = value.to_varchar()?;
                let abort = CheckpointAbort::from_name(&text).ok_or_else(|| {
                    let names: Vec<&str> = CheckpointAbort::ALL.iter().map(|(_, name)| *name).collect();
                    rustdb_function::logging::enum_error(&text, "CheckpointAbort", &names)
                })?;
                Box::new(move |config| config.abort = abort)
            }
            ("debug_checkpoint_abort", None) => Box::new(|config| config.abort = CheckpointAbort::NoAbort),
            _ => return Ok(()),
        };
        let config = {
            let mut config = self
                .checkpoint_config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            update(&mut config);
            *config
        };
        for database in self.databases.all() {
            database.storage.set_checkpoint_config(config);
        }
        Ok(())
    }

    /// `SET` / `RESET` (`None`) of the settings of optimistic writes
    /// (`EnableOptimisticWriteSetting`, `WriteBufferRowGroupCountSetting`,
    /// `WriteBufferRowGroupMemoryLimitSetting`) and of the `threads` and
    /// `geometry_minimum_shredding_size` they depend on; other names are ignored.
    fn set_optimistic_write_setting(&self, name: &str, value: Option<&rustdb_types::Value>) -> Result<()> {
        use rustdb_function::cast::ValueCast;
        let lower = name.to_ascii_lowercase();
        let defaults = rustdb_storage::OptimisticWriteConfig::default();
        let update: Box<dyn Fn(&mut rustdb_storage::OptimisticWriteConfig)> = match lower.as_str() {
            "enable_optimistic_write" => {
                let enabled = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                    None => defaults.enabled,
                };
                Box::new(move |config| config.enabled = enabled)
            }
            "write_buffer_row_group_count" => {
                let count = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::UBIGINT)?
                        .as_u64()
                        .unwrap_or(0),
                    None => defaults.row_group_count,
                };
                Box::new(move |config| config.row_group_count = count)
            }
            "write_buffer_row_group_memory_limit" => {
                let limit = value
                    .map(|value| rustdb_buffer::parse_memory_limit(&value.to_varchar()?))
                    .transpose()?;
                Box::new(move |config| config.memory_limit = limit)
            }
            "geometry_minimum_shredding_size" => {
                let rows = match value {
                    Some(value) => value
                        .default_cast_as(&rustdb_types::LogicalType::BIGINT)?
                        .as_i64()
                        .unwrap_or(0),
                    None => defaults.geometry_minimum_shredding_size,
                };
                Box::new(move |config| config.geometry_minimum_shredding_size = rows)
            }
            _ if is_threads_setting(name) => {
                let threads = self.env.threads() as u64;
                Box::new(move |config| config.threads = threads)
            }
            _ => return Ok(()),
        };
        let config = {
            let mut config = self
                .optimistic_write
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            update(&mut config);
            *config
        };
        for database in self.databases.all() {
            database.storage.set_optimistic_write_config(config);
        }
        Ok(())
    }

    /// How large appends are written to database files before they commit.
    fn optimistic_write_config(&self) -> rustdb_storage::OptimisticWriteConfig {
        *self
            .optimistic_write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How checkpoints reuse metadata.
    fn checkpoint_config(&self) -> rustdb_storage::storage_manager::CheckpointConfig {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The compression settings checkpoints use.
    fn compression_config(&self) -> rustdb_storage::compression::CompressionConfig {
        *self
            .compression
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `ALTER DATABASE` (`DatabaseManager::Alter`): `SET ALIAS TO` renames an attached
    /// database (`RenameDatabase`), and the default database follows its name.
    fn alter_database(&self, info: &rustdb_ast::AlterInfo) -> Result<()> {
        let rustdb_ast::AlterInfoKind::AlterDatabase(rustdb_ast::AlterDatabaseInfo::RenameDatabase(rename)) =
            &info.kind
        else {
            return Err(Error::internal("Unsupported ALTER DATABASE operation"));
        };
        let (name, new_name) = (&info.catalog, &rename.new_name);
        if !self.databases.rename(name, new_name)? {
            if info.if_not_found == rustdb_ast::OnEntryNotFound::ThrowException {
                return Err(Error::binder(format!(
                    "Failed to rename database \"{name}\": database not found"
                )));
            }
            return Ok(());
        }
        let mut default = self
            .default_database
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if default
            .as_deref()
            .is_some_and(|default| default.eq_ignore_ascii_case(name))
        {
            *default = Some(new_name.clone());
        }
        Ok(())
    }

    /// The name of the database opened at start-up.
    pub(crate) fn default_database_name(&self) -> String {
        self.default_database
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| "memory".to_owned())
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
            .filter(|database| database.name().eq_ignore_ascii_case(default))
            .map(|database| Arc::clone(&database.catalog))
            .collect();
        catalogs.extend(
            databases
                .iter()
                .filter(|database| !database.name().eq_ignore_ascii_case(default))
                .map(|database| Arc::clone(&database.catalog)),
        );
        catalogs.push(Arc::clone(&self.system_catalog));
        catalogs
    }

    /// Checkpoints the database `name` a commit wrote to when its WAL grew beyond
    /// `checkpoint_threshold`, unless other transactions are running
    /// (`DuckTransactionManager::CanCheckpoint`). Like DuckDB, the WAL size before the
    /// commit plus the commit's `estimated_size` is compared, which counts only the data
    /// pointers of optimistically written row groups (`AutomaticCheckpoint`).
    ///
    /// `blocks_concurrent` tells whether the committed transaction updated rows or
    /// dropped catalog entries: then it does not checkpoint while other transactions run
    /// (`GetCheckpointType`). A failing checkpoint reports that the commit succeeded
    /// (`BuildAutocheckpointError`).
    ///
    /// An in-memory database has no WAL: its WAL size grows by the `estimated_size` of
    /// each commit (with the index memory), and it checkpoints automatically only when no
    /// other transaction runs (DuckDB cannot vacuum then; rustdb does not compress then
    /// either).
    fn auto_checkpoint(
        &self,
        name: &str,
        blocks_concurrent: bool,
        estimated_size: u64,
        skip_checkpoint: bool,
    ) -> Result<()> {
        let threshold = self
            .checkpoint_wal_size
            .load(std::sync::atomic::Ordering::Relaxed);
        let Some(database) = self.databases.get(name) else {
            return Ok(());
        };
        let storage = match &database.storage {
            crate::attached::DatabaseStorage::File(storage) => storage,
            crate::attached::DatabaseStorage::InMemory(storage) => {
                storage.add_wal_size(estimated_size);
                if storage.wal_size() <= threshold || skip_checkpoint || self.transactions.active_count() > 0
                {
                    return Ok(());
                }
                // Nothing was logged: the commit itself failed (`error.Merge`).
                return database
                    .create_checkpoint(
                        crate::storage::CheckpointMode::Automatic,
                        Some(&self.transactions),
                    )
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::Transaction,
                            format!("Failed to commit: {}", error.raw_message()),
                        )
                    });
            }
        };
        let Some(wal) = storage.wal() else {
            return Ok(());
        };
        if skip_checkpoint {
            return Ok(());
        }
        // `SingleFileStorageManager::AutomaticCheckpoint`: the WAL size or entry count.
        let entry_limit = self
            .wal_autocheckpoint_entries
            .load(std::sync::atomic::Ordering::Relaxed);
        let (size, entries) = {
            let guard = wal.lock();
            (
                guard.size_before_last_commit(),
                guard.entries_before_last_commit(),
            )
        };
        if size + estimated_size <= threshold && !(entry_limit > 0 && entries >= entry_limit) {
            return Ok(());
        }
        let no_wal_writes = wal.lock().no_writes();
        if self.transactions.active_count() > 0 {
            // Other transactions may need older data: a concurrent checkpoint, unless this
            // transaction made changes they must not see; the checkpoint lock must be free,
            // also of committed changes older transactions still need.
            if blocks_concurrent
                || self.transactions.retains_changes(u64::MAX, &database.name())
                || database.catalog.locks().checkpoint.try_exclusive().is_none()
            {
                return Ok(());
            }
        }
        database
            .create_checkpoint(crate::storage::CheckpointMode::Automatic, Some(&self.transactions))
            .map_err(|error| {
                if no_wal_writes {
                    // Nothing was logged: the commit itself failed (`error.Merge`).
                    return Error::new(
                        ErrorKind::Transaction,
                        format!("Failed to commit: {}", error.raw_message()),
                    );
                }
                let recovery = if database.is_initial() {
                    "Reopen the database instance to recover."
                } else {
                    "Detach and reattach the database to recover."
                };
                Error::new(
                    error.kind(),
                    format!(
                        "Transaction COMMIT succeeded and is durable, but the autocheckpoint failed. {recovery} {}",
                        error.raw_message()
                    ),
                )
            })
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

/// The thread-local context of a running query (see [`Connection::query_scope`]).
pub(crate) struct QueryScope {
    _metrics: rustdb_common::profiling::ActiveMetricsGuard,
    _opener: rustdb_function::file_system::OpenerGuard,
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
        config.check_recognized()?;
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
        system_catalog.register_scalar_functions(crate::switch::switch_functions())?;
        for set in crate::checkpoint::checkpoint_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::query_profiler::profiling_functions() {
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
        for set in crate::system_functions::builtin_pragma_functions() {
            system_catalog.register_pragma_functions(set)?;
        }
        for set in rustdb_arrow::arrow_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        system_catalog.register_copy_function(rustdb_csv::copy_csv::csv_copy_function())?;
        system_catalog.register_copy_function(rustdb_csv::copy_blob::blob_copy_function())?;
        for set in rustdb_csv::multi_file::glob_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in rustdb_csv::sniff_csv::sniff_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in rustdb_csv::read_csv::read_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        system_catalog.register_table_functions(crate::read_duckdb::read_duckdb_functions())?;
        crate::system_functions::register_default_views(&system_catalog)?;
        crate::system_functions::register_default_macros(&system_catalog)?;
        let buffer_manager = rustdb_buffer::BufferManager::new(rustdb_buffer::BufferManagerConfig {
            temp_directory: Some(default_temp_directory(&path).into()),
            ..rustdb_buffer::BufferManagerConfig::default()
        })?;
        let env = Arc::new(rustdb_execution::ClientEnv::new(buffer_manager));
        let threads = env.threads();
        let log_manager = Arc::new(crate::logging::LogManager::default());
        let instance = DatabaseInstance {
            path: path.clone(),
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            databases: DatabaseManager::new(Arc::clone(&log_manager)),
            default_database: std::sync::RwLock::default(),
            casts: std::sync::RwLock::new(Arc::new(json_casts())),
            client_casts: std::sync::atomic::AtomicBool::new(false),
            extension_options: std::sync::RwLock::default(),
            extension_globals: Mutex::default(),
            unrecognized_options: Mutex::default(),
            env,
            shutdown_checkpoint_disabled: std::sync::atomic::AtomicBool::new(false),
            checkpoint_wal_size: std::sync::atomic::AtomicU64::new(DEFAULT_CHECKPOINT_WAL_SIZE),
            compression: std::sync::Mutex::default(),
            checkpoint_config: std::sync::Mutex::default(),
            optimistic_write: std::sync::Mutex::new(rustdb_storage::OptimisticWriteConfig {
                threads: threads as u64,
                ..rustdb_storage::OptimisticWriteConfig::default()
            }),
            variant_shredding: std::sync::Mutex::default(),
            validity: Arc::default(),
            wal_autocheckpoint_entries: std::sync::atomic::AtomicU64::new(0),
            secrets: Arc::default(),
            connections: Arc::default(),
            object_cache: Arc::default(),
            log_manager,
            global_settings: Mutex::default(),
            loaded_extensions: Arc::default(),
            replacement_scans: std::sync::RwLock::default(),
        };
        instance.object_cache.put(
            LOADED_EXTENSIONS_KEY.to_owned(),
            Arc::clone(&instance.loaded_extensions),
        );
        instance.object_cache.put(
            rustdb_function::file_system::DISABLED_FILE_SYSTEMS_KEY.to_owned(),
            Arc::new(rustdb_function::file_system::DisabledFileSystems::default()),
        );
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
            instance.set_checkpoint_setting(name, Some(value))?;
            instance.set_optimistic_write_setting(name, Some(value))?;
            instance.set_variant_shredding_setting(name, Some(value))?;
            instance.set_disabled_compression_methods(name, Some(value))?;
            instance.set_wal_autocheckpoint_entries(name, Some(value))?;
        }
        for (name, value) in &config.options {
            if is_extension_setting(name) {
                instance.add_unrecognized_option(name, value.clone());
            }
        }
        let database = Self {
            instance: Arc::new(instance),
        };
        let options = crate::storage::startup_options(config);
        // A data file opens as an in-memory database with views on it, as with `ATTACH`
        // (`DatabaseManager::GetDatabaseType` resolving to `__open_file__`).
        let main = if crate::attached::is_open_file(&path)? {
            database.attach_open_file("", &path, &options, rustdb_ast::OnCreateConflict::ErrorOnConflict)?
        } else {
            database.attach("", &path, &options, rustdb_ast::OnCreateConflict::ErrorOnConflict)?
        };
        if let Some(main) = main {
            main.is_initial.store(true, std::sync::atomic::Ordering::Release);
            *database
                .instance
                .default_database
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(main.name().to_string());
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
        // A database file is tracked by its canonical path
        // (`DatabaseManager::AttachDatabase` with `RequiresTrackingAttaches`).
        let canonical_path;
        let path = if path.is_empty() || path == ":memory:" {
            path
        } else {
            canonical_path = rustdb_function::file_system::canonicalize_local_path(path);
            canonical_path.as_str()
        };
        if !path.is_empty() && path != ":memory:" {
            rustdb_function::file_system::verify_can_access_file(path)?;
            if !options.read_only() {
                rustdb_function::file_system::verify_can_write(path)?;
            }
        }
        // A new database file gets the configured storage version and block size
        // (`serialization_compatibility`, `default_block_alloc_size`, also set by a
        // global `SET`); an existing file keeps its own.
        let mut options = options.clone();
        if !path.is_empty() && path != ":memory:" && !std::path::Path::new(path).exists() {
            if options.storage_version.is_none() {
                options.storage_version = self.global_setting("storage_compatibility_version");
            }
            if options.block_size.is_none() {
                options.block_size = self.global_setting("default_block_size");
            }
        }
        let options = &options;
        let Some(attached) = self.instance.databases.attach(
            name,
            path,
            options,
            on_conflict,
            &self.instance.env.buffer_manager,
            &self.instance.validity,
        )?
        else {
            return Ok(None);
        };
        attached
            .storage
            .set_compression_config(self.instance.compression_config());
        attached
            .storage
            .set_checkpoint_config(self.instance.checkpoint_config());
        attached
            .storage
            .set_optimistic_write_config(self.instance.optimistic_write_config());
        attached
            .storage
            .set_variant_shredding_config(self.instance.variant_shredding_config());
        if let Err(error) = crate::wal::replay(self, &attached) {
            self.instance.databases.remove(&attached.name());
            return Err(error);
        }
        // `RecoveryMode::NO_WAL_WRITES`: the WAL is replayed, but not written to again.
        if options.no_wal_writes
            && let Some(wal) = attached.storage.file().and_then(|storage| storage.wal())
        {
            wal.lock().set_no_writes();
        }
        Ok(Some(attached))
    }

    /// Attaches the Parquet, CSV or JSON file at `path` as an in-memory database whose
    /// `main` schema has the views `file` and `<name>` reading it
    /// (`OpenFileStorageAttach` with `OpenFileDefaultGenerator`).
    pub(crate) fn attach_open_file(
        &self,
        name: &str,
        path: &str,
        options: &AttachOptions,
        on_conflict: rustdb_ast::OnCreateConflict,
    ) -> Result<Option<Arc<AttachedDatabase>>> {
        // Its path is canonical, as the database type is only known after
        // `RequiresTrackingAttaches` canonicalized it.
        let path = &rustdb_function::file_system::canonicalize_local_path(path);
        rustdb_function::file_system::verify_can_access_file(path)?;
        let name = if name.is_empty() {
            crate::storage::database_name(path)
        } else {
            name.to_owned()
        };
        let Some(attached) = self.instance.databases.attach(
            &name,
            "",
            options,
            on_conflict,
            &self.instance.env.buffer_manager,
            &self.instance.validity,
        )?
        else {
            return Ok(None);
        };
        let sql = format!("SELECT * FROM '{}'", path.replace('\'', "''"));
        let mut view_names = vec!["file".to_owned()];
        if !name.eq_ignore_ascii_case("file") {
            view_names.push(name);
        }
        for view_name in view_names {
            let Some(rustdb_ast::SqlStatementKind::Select(query)) =
                rustdb_parser::parse(&sql)?.pop().map(|statement| statement.kind)
            else {
                return Err(Error::internal("open file view is not a SELECT"));
            };
            let view = rustdb_catalog::ViewEntry {
                query,
                aliases: Vec::new(),
                types: Vec::new(),
                names: Vec::new(),
                column_comments: rustdb_common::OrderedMap::default(),
            };
            attached.catalog.create_entry_committed(
                rustdb_catalog::DEFAULT_SCHEMA,
                rustdb_catalog::CatalogEntry::new(view_name, rustdb_catalog::EntryKind::View(view)),
            )?;
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

    /// The value a global `SET` gave option `name` (canonical name), if any (the
    /// instance's `DBConfig` options).
    pub fn global_setting(&self, name: &str) -> Option<Value> {
        self.instance
            .global_settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values
            .iter()
            .find(|(option, _)| option.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    pub fn config(&self) -> &Config {
        &self.instance.config
    }

    pub fn connect(&self) -> Result<Connection> {
        let mut settings = Settings::default();
        for (name, value) in &self.instance.config.options {
            if is_extension_setting(name) {
                continue;
            }
            settings.set(name, value)?;
        }
        let function_client = Arc::new(rustdb_function::FunctionClient::default());
        let transactions = TransactionContext::new(
            Arc::clone(&self.instance.transactions),
            Arc::clone(&function_client),
        );
        function_client.set_database(&self.instance.default_database_name());
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
            used_databases: Vec::new(),
            prepared_statements: CaseInsensitiveMap::default(),
            function_client,
            user_variables: CaseInsensitiveMap::default(),
            profiler: crate::query_profiler::QueryProfiler::default(),
            log_query_writer: None,
            extension_session: CaseInsensitiveMap::default(),
            bound_casts: Mutex::default(),
            explicit_commit: None,
            active_query: None,
            next_query_id: 0,
            client: Arc::default(),
        })
    }

    /// Adds a configuration option (`DBConfig::AddExtensionOption`, as
    /// `duckdb_register_config_option` does): returns `false`, adding nothing, if an option
    /// of that name exists (`HasExtensionOption`).
    pub fn add_extension_option(&self, option: rustdb_function::ExtensionOption) -> bool {
        if self.instance.extension_option(&option.name).is_some() {
            return false;
        }
        self.instance.add_extension_option(option);
        true
    }

    /// Runs queued tasks of the database's queries on the calling thread (DuckDB's
    /// `TaskScheduler::ExecuteTasks`): at most `max_tasks`, until none is queued or
    /// `marker` (if given) is cleared; returns how many ran.
    pub fn execute_tasks(&self, marker: Option<&std::sync::atomic::AtomicBool>, max_tasks: usize) -> usize {
        self.instance.env.scheduler.execute_tasks(marker, max_tasks)
    }

    /// Runs queued tasks of the database's queries on the calling thread until `marker`
    /// is cleared, waiting for tasks meanwhile (`TaskScheduler::ExecuteForever`).
    pub fn execute_tasks_forever(&self, marker: &std::sync::atomic::AtomicBool) {
        self.instance.env.scheduler.execute_forever(marker);
    }

    /// Wakes the threads in [`Database::execute_tasks_forever`] to see their cleared marker
    /// (`TaskScheduler::Signal`).
    pub fn signal_task_threads(&self) {
        self.instance.env.scheduler.signal();
    }

    /// Registers a log storage that `logging_storage` can then select by `name`
    /// (`LogManager::RegisterLogStorage`); returns `false`, registering nothing, if a
    /// storage of that name was registered before.
    pub fn register_log_storage(&self, name: &str, storage: Arc<dyn crate::logging::LogStorage>) -> bool {
        self.instance.log_manager.register_storage(name, storage)
    }

    /// Selects the log storage `name`, built in or registered (`LogManager::SetLogStorage`).
    pub fn set_log_storage(&self, name: &str) -> Result<()> {
        self.instance.log_manager.set_storage(name)
    }

    /// Turns logging on or off (`LogManager::SetEnableLogging`).
    pub fn set_enable_logging(&self, enabled: bool) {
        self.instance.log_manager.set_enabled(enabled);
    }

    /// Logs entries of `level` and above (`LogManager::SetLogLevel`).
    pub fn set_log_level(&self, level: rustdb_function::logging::LogLevel) {
        self.instance.log_manager.set_level(level);
    }

    /// The version of the library (`DuckDB::LibraryVersion`).
    pub fn library_version() -> &'static str {
        rustdb_storage::storage_info::LIBRARY_VERSION
    }

    /// The commit the library was built from (`DuckDB::SourceID`).
    pub fn source_id() -> &'static str {
        rustdb_storage::storage_info::SOURCE_ID
    }

    /// The codename of the library's release (`DuckDB::ReleaseCodename`).
    pub fn release_codename() -> &'static str {
        let version = Self::library_version();
        if version.contains("-dev") {
            "Development Version"
        } else if version.starts_with("v1.2.") {
            "Histrionicus"
        } else if version.starts_with("v1.3.") {
            "Ossivalis"
        } else if version.starts_with("v1.4.") {
            "Andium"
        } else if version.starts_with("v1.5.") {
            "Variegata"
        } else {
            "Unknown Version"
        }
    }

    /// Records an extension the client linked and loaded itself, such as the shell's
    /// (`DatabaseInstance::SetExtensionLoaded` after `DuckDB::LoadStaticExtension`):
    /// `duckdb_extensions()` lists it as loaded and statically linked.
    pub fn set_extension_loaded(&self, name: &str, description: &str) {
        let extensions = self.client_extensions();
        let mut extensions = extensions.lock().unwrap_or_else(PoisonError::into_inner);
        if !extensions.iter().any(|(existing, _)| existing == name) {
            extensions.push((name.to_owned(), description.to_owned()));
        }
    }

    /// The extensions the client loaded, as [`Database::set_extension_loaded`] records them.
    fn client_extensions(&self) -> Arc<ClientExtensions> {
        self.instance
            .object_cache
            .get::<ClientExtensions>(CLIENT_EXTENSIONS_KEY)
            .unwrap_or_else(|| {
                let extensions = Arc::new(ClientExtensions::default());
                self.instance
                    .object_cache
                    .put(CLIENT_EXTENSIONS_KEY.to_owned(), Arc::clone(&extensions));
                extensions
            })
    }

    /// The memory the database's buffer manager uses and may use
    /// (`BufferManager::GetUsedMemory`, `GetUsedSwap`, `GetMaxSwap` and the memory limit).
    pub fn memory_status(&self) -> MemoryStatus {
        let buffer_manager = &self.instance.env.buffer_manager;
        MemoryStatus {
            memory_limit: buffer_manager.max_memory(),
            memory_usage: buffer_manager.used_memory(),
            swap_usage: buffer_manager.used_swap(),
            swap_max: buffer_manager.max_swap(),
        }
    }

    /// Adds a replacement scan, tried after the built-in ones and those added before
    /// (`DBConfig::replacement_scans.push_back`).
    pub fn add_replacement_scan(&self, scan: rustdb_planner::ReplacementScan) {
        self.instance
            .replacement_scans
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scan);
    }

    /// Loads a (statically linked) extension, like `LOAD name`.
    pub fn load_extension(&self, name: &str) -> Result<ExtensionLoad> {
        if let Some(extension) = StaticExtension::from_name(name) {
            self.instance.load_static_extension(extension)?;
            return Ok(ExtensionLoad::Loaded);
        }
        Ok(match name {
            // The core function library is part of the system catalog, loaded at start-up
            // as the reference build's statically linked `core_functions` is.
            "core_functions" => ExtensionLoad::Loaded,
            "icu" | "tpcds" | "httpfs" | "fts" | "inet" | "excel" | "sqlsmith" | "spatial" | "delta"
            | "iceberg" | "aws" | "azure" | "motherduck" | "postgres_scanner" | "mysql_scanner"
            | "sqlite_scanner" | "vss" | "ui" | "encodings" | "ducklake" | "avro" | "demo_capi" => {
                ExtensionLoad::NotAvailable
            }
            _ => ExtensionLoad::Unknown,
        })
    }

    pub(crate) fn instance(&self) -> &DatabaseInstance {
        &self.instance
    }

    /// The database of an instance still open.
    pub(crate) fn from_instance(instance: &std::sync::Weak<DatabaseInstance>) -> Option<Self> {
        instance.upgrade().map(|instance| Self { instance })
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
    /// The databases the current transaction uses (`MetaTransaction::UseDatabase`): a
    /// database detached meanwhile is closed when the transaction ends.
    used_databases: Vec<Arc<AttachedDatabase>>,
    /// `SET VARIABLE` values (DuckDB's `ClientConfig::user_variables`).
    user_variables: CaseInsensitiveMap<rustdb_types::Value>,
    /// The connection's loggers.
    logger: Arc<crate::logging::ConnectionLogger>,
    /// The profiler of the connection's statements (DuckDB's `ClientData::profiler`).
    profiler: crate::query_profiler::QueryProfiler,
    /// The file `log_query_path` names, which every statement is written to (DuckDB's
    /// `ClientData::log_query_writer`).
    log_query_writer: Option<std::fs::File>,
    /// The session values of extension options (DuckDB's `ClientConfig::user_settings`
    /// for extension parameters).
    extension_session: CaseInsensitiveMap<Value>,
    /// The database's casts bound for this connection, with the set they were made from
    /// (only when extension casts depend on the connection's settings).
    bound_casts: Mutex<Option<(Arc<CastFunctionSet>, Arc<CastFunctionSet>)>>,
    /// The query a pending or streaming result belongs to (DuckDB's `active_query`).
    active_query: Option<active_query::ActiveQuery>,
    /// The id of the last query made active.
    next_query_id: u64,
    /// The interrupt flag and progress other threads may use (DuckDB's
    /// `ClientContext::interrupted` and `query_progress`).
    client: Arc<ClientHandle>,
    /// The database an explicit `COMMIT` of the statement committed changes to, what
    /// they were and the memory of the indexes over appended rows, for the automatic
    /// checkpoint at the end of the statement.
    explicit_commit: Option<CommittedChanges>,
}

/// What a transaction about to commit changed, for the automatic checkpoint after the
/// commit.
#[derive(Debug)]
struct CommittedChanges {
    /// The database it wrote to.
    database: String,
    properties: rustdb_storage::transaction::UndoProperties,
}

impl CommittedChanges {
    /// The changes of `transaction`, if it wrote to a database.
    fn of(transaction: &Transaction) -> Option<Self> {
        transaction.modified_database().map(|database| Self {
            database,
            properties: transaction.undo_properties(),
        })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // `ClientContext::Destroy`: a query still running is ended first.
        self.cleanup_active_query();
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
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let mut params = rustdb_function::cast::CastParameters::new();
        value.cast_with(&self.casts(), target, &mut params)
    }

    /// Casts the first `count` rows of `source` to `target` with the casts of this
    /// database, including those of loaded extensions (DuckDB's
    /// `VectorOperations::Cast(ClientContext &, ...)`).
    pub fn cast_vector(
        &self,
        source: &rustdb_types::Vector,
        target: &rustdb_types::LogicalType,
        count: usize,
    ) -> Result<rustdb_types::Vector> {
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let mut params = rustdb_function::cast::CastParameters::new();
        rustdb_function::cast::cast_vector(&self.casts(), source, target, count, &mut params)
    }

    /// Whether the connection commits every statement, being outside an explicit
    /// transaction (DuckDB's `Connection::IsAutoCommit`).
    pub fn is_auto_commit(&self) -> bool {
        self.transactions.is_auto_commit()
    }

    /// The current value of option `name` for this connection, or DuckDB's error for an
    /// unknown option (`ClientContext::TryGetCurrentSetting`).
    pub fn current_setting(&self, name: &str) -> Result<Value> {
        use rustdb_function::SettingsSource;
        self.settings_snapshot().current_setting(name)
    }

    /// The default database and schema of the connection
    /// (`DatabaseManager::GetDefaultDatabase` and the default of its catalog search path).
    pub fn current_database_and_schema(&self) -> (String, String) {
        let schema = self.search_path.first().map_or_else(
            || rustdb_catalog::DEFAULT_SCHEMA.to_owned(),
            |(_, schema)| schema.clone(),
        );
        (self.default_catalog(), schema)
    }

    /// The bytes the latest profiled query read from database files
    /// (`QueryProfiler::GetBytesRead`).
    pub fn bytes_read(&self) -> u64 {
        self.profiler
            .bytes(rustdb_common::profiling::MetricType::TotalBytesRead)
    }

    /// The bytes the latest profiled query wrote to database files
    /// (`QueryProfiler::GetBytesWritten`).
    pub fn bytes_written(&self) -> u64 {
        self.profiler
            .bytes(rustdb_common::profiling::MetricType::TotalBytesWritten)
    }

    /// The database's casts as this connection binds them (DuckDB's
    /// `CastFunctionSet::Get(context)` with the context in `BindCastInput`): when
    /// extension casts read the connection's settings, they are bound for it.
    fn casts(&self) -> Arc<CastFunctionSet> {
        let instance = self.database.instance();
        let casts = instance.casts();
        if !instance.client_casts.load(std::sync::atomic::Ordering::Acquire) {
            return casts;
        }
        let mut bound = self.bound_casts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((base, bound)) = bound.as_ref()
            && Arc::ptr_eq(base, &casts)
        {
            return Arc::clone(bound);
        }
        let client_casts = Arc::new(casts.with_client(Arc::clone(&self.function_client)));
        *bound = Some((casts, Arc::clone(&client_casts)));
        client_casts
    }

    /// Whether the cast of `ty` to VARCHAR depends on the connection (an extension
    /// registered casts for it): TIMESTAMP WITH TIME ZONE with ICU loaded.
    pub fn has_extension_cast(&self, ty: &rustdb_types::LogicalType) -> bool {
        self.database
            .instance()
            .client_casts
            .load(std::sync::atomic::Ordering::Acquire)
            && contains_type(ty, rustdb_types::LogicalTypeId::TimestampTz)
    }

    /// The options set on this connection (and not reset since), by canonical name,
    /// like the values DuckDB's `RestartCommand` carries over to the restarted database.
    pub fn assigned_settings(&self) -> Vec<(String, Value)> {
        let mut assigned: Vec<(String, Value)> = self
            .settings
            .assigned()
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect();
        for (option, _) in self.database.instance().extension_option_values() {
            if let Some(value) = self.extension_session.get(&option.name) {
                assigned.push((option.name.clone(), value.clone()));
            }
        }
        assigned
    }

    /// The profile of the latest query (`Connection::GetProfilingTree`): `None` if
    /// profiling is off or the query was not profiled.
    pub fn profiling_tree(&self) -> Option<Arc<crate::query_profiler::ProfilingTree>> {
        if !self.settings.profiler.enable_profiler {
            return None;
        }
        self.profiler.tree().cloned()
    }

    /// The settings results are converted for clients with, such as their Arrow form
    /// (DuckDB's `ClientContext::GetClientProperties`).
    pub fn client_properties(&self) -> rustdb_arrow::ClientProperties {
        let settings = self.settings_snapshot();
        let setting = |name: &str| rustdb_function::SettingsSource::current_setting(&settings, name).ok();
        let enabled = |name: &str| setting(name).and_then(|value| value.as_bool()).unwrap_or(false);
        let mut properties = rustdb_arrow::ClientProperties::default();
        if let Some(time_zone) = setting("TimeZone").and_then(|value| value.to_varchar().ok()) {
            properties.time_zone = time_zone;
        }
        if enabled("arrow_large_buffer_size") {
            properties.arrow_offset_size = rustdb_arrow::ArrowOffsetSize::Large;
        }
        properties.arrow_use_list_view = enabled("arrow_output_list_view");
        properties.arrow_lossless_conversion = enabled("arrow_lossless_conversion");
        properties.produce_arrow_string_view = enabled("produce_arrow_string_view");
        if let Some(version) = setting("arrow_output_version")
            .and_then(|value| value.to_varchar().ok())
            .and_then(|version| rustdb_arrow::ArrowFormatVersion::from_setting(&version))
        {
            properties.arrow_output_version = version;
        }
        properties
    }

    /// The connection's id (DuckDB's `ClientContext::GetConnectionId`).
    pub fn connection_id(&self) -> u64 {
        self.function_client.connection_id()
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Parses `sql` into its statements (DuckDB's `ClientContext::ExtractStatements` and
    /// `Parser::ParseQuery`): with `allow_parser_override_extension` set and the
    /// autocomplete extension loaded, its PEG parser goes first.
    pub fn extract_statements(&self, sql: &str) -> Result<Vec<SqlStatement>> {
        use crate::settings::ParserOverride;
        let options = self.settings.parser_options();
        let mode = self.settings.parser_override();
        if mode != ParserOverride::Default && self.database.instance().has_parser_extension() {
            let stripped = rustdb_parser::strip_unicode_spaces(sql);
            let query = stripped.as_deref().unwrap_or(sql);
            match rustdb_autocomplete::parse_query(query, options) {
                Ok(statements) => return Ok(statements),
                Err(error) if mode == ParserOverride::Strict => return Err(error.with_query_context(sql)),
                // `FALLBACK`: the Postgres parser takes over.
                Err(_) => {}
            }
        }
        rustdb_parser::parse_with_options(sql, &options).map_err(|error| error.with_query_context(sql))
    }

    /// The statements of `sql` as DuckDB's `ClientContext::ParseStatements` hands them out
    /// (its `StatementPreprocessor`): every PRAGMA is bound, which fails for one the catalog
    /// does not have, and a pragma that stands for a query is replaced by the statements of
    /// that query (`TryReparsePragma`).
    pub fn parse_statements(&mut self, sql: &str) -> Result<Vec<SqlStatement>> {
        let statements = self.extract_statements(sql)?;
        if !statements
            .iter()
            .any(|statement| matches!(statement.kind, rustdb_ast::SqlStatementKind::Pragma(_)))
        {
            return Ok(statements);
        }
        let mut result = Vec::with_capacity(statements.len());
        for statement in statements {
            let rustdb_ast::SqlStatementKind::Pragma(pragma) = &statement.kind else {
                result.push(statement);
                continue;
            };
            let name = pragma.name.clone();
            let bound = self.bind_pragma_parameters(statement.clone());
            let ended = self.transactions.end_statement(bound.is_ok(), false);
            let parameters = bound
                .and_then(|parameters| ended.map(|()| parameters))
                .map_err(|error| self.process_error(error, sql))?;
            let instance = self.database.instance();
            let query = if instance.has_extension_pragma(&name) {
                Some(instance.extension_pragma_query(&name, &parameters)?)
            } else {
                crate::system_functions::pragma_query(&name, &parameters)?
            };
            let Some(query) = query else {
                result.push(statement);
                continue;
            };
            let mut reparsed = rustdb_parser::parse(&query)?;
            if reparsed.len() <= 1 {
                result.append(&mut reparsed);
                continue;
            }
            // Several statements run as one (`GetTransactionHandling`): in an implicit
            // transaction outside an explicit one, under `ALL_ERRORS_INVALIDATE_TRANSACTION`
            // inside one; the multi-statement run decides which when it executes.
            result.push(SqlStatement {
                kind: rustdb_ast::SqlStatementKind::Multi(reparsed),
                ..statement
            });
        }
        Ok(result)
    }

    /// Runs every statement in `sql` and returns the result of the last one (DuckDB's
    /// `ClientContext::Query`).
    pub fn query(&mut self, sql: &str) -> Result<MaterializedResult> {
        self.sync_global_settings();
        let statements = self.parse_statements(sql)?;
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
        self.initial_cleanup();
        let statement_type = statement.statement_type();
        self.begin_query(query, &statement)?;
        let outcome = self.in_query_scope(|connection| {
            connection.begin_query_logging(query);
            connection
                .start_statement(statement, RunMode::Materialize)
                .and_then(StatementRun::into_materialized)
        });
        self.end_query(query, outcome)
            .map(|result| result.with_statement_type(statement_type))
    }

    /// `ClientContext::BeginQueryInternal` up to the logging: nothing runs on an invalidated
    /// instance; the statement sees the latest global settings, and the profiler starts.
    pub(crate) fn begin_query(&mut self, query: &str, statement: &SqlStatement) -> Result<()> {
        if let Some(message) = self.database.instance().validity.invalidated() {
            return Err(invalidated_database_error(&message).with_query_context(query));
        }
        self.sync_global_settings();
        self.function_client.set_query(query);
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        // `PendingStatementOrPreparedStatement` starts the profiler first.
        let is_explain_analyze = matches!(
            &statement.kind,
            rustdb_ast::SqlStatementKind::Explain(explain)
                if explain.explain_type == rustdb_ast::ExplainType::ExplainAnalyze
        );
        self.profiler
            .start_query(&self.settings.profiler, query, is_explain_analyze);
        Ok(())
    }

    /// Activates the running query's thread-local context until the returned scope is
    /// dropped: the metrics its profiler records into, and the connection as the opener of
    /// the files it opens (they read its settings and log to its loggers).
    pub(crate) fn query_scope(&self) -> QueryScope {
        let metrics = self.profiler.active_metrics().cloned();
        QueryScope {
            _metrics: rustdb_common::profiling::activate(metrics.as_ref()),
            _opener: rustdb_function::file_system::activate_opener(Arc::clone(&self.function_client)),
        }
    }

    /// Runs `work` within the running query's [scope](Connection::query_scope).
    pub(crate) fn in_query_scope<T>(&mut self, work: impl FnOnce(&mut Self) -> T) -> T {
        let _scope = self.query_scope();
        work(self)
    }

    /// `ClientContext::EndQueryInternal`: applies the configuration changes functions made,
    /// ends the statement in its transaction (committing or rolling back an auto-commit
    /// one), writes the profile, checkpoints automatically and releases the databases the
    /// transaction used. Errors are put in the context of `query`.
    pub(crate) fn end_query(
        &mut self,
        query: &str,
        outcome: Result<MaterializedResult>,
    ) -> Result<MaterializedResult> {
        self.end_query_internal(query, outcome, true)
    }

    /// [`Connection::end_query`]; a failure invalidates an explicit transaction only if
    /// `invalidate_transaction` (a query whose result was closed unread does not).
    pub(crate) fn end_query_internal(
        &mut self,
        query: &str,
        outcome: Result<MaterializedResult>,
        invalidate_transaction: bool,
    ) -> Result<MaterializedResult> {
        let metrics = self.profiler.active_metrics().cloned();
        let metrics_guard = rustdb_common::profiling::activate(metrics.as_ref());
        let _opener_guard = rustdb_function::file_system::activate_opener(Arc::clone(&self.function_client));
        // Functions that changed the connection's configuration did so while running.
        let requests = self.function_client.take_client_requests();
        let outcome = outcome.and_then(|result| self.apply_client_requests(requests).map(|()| result));
        let invalidates = invalidate_transaction
            && outcome
                .as_ref()
                .is_err_and(|error| invalidates_transaction(error.kind()));
        // The database the transaction wrote to, which its commit may checkpoint.
        let committed = if self.transactions.has_current() {
            let current = self.transactions.current();
            CommittedChanges::of(&current)
        } else {
            self.explicit_commit.take()
        };
        // A transaction that used an invalidated database cannot commit
        // (`MetaTransaction::Commit`); it is rolled back.
        let invalid_use = if self.transactions.is_auto_commit() && outcome.is_ok() {
            self.used_databases
                .iter()
                .find_map(|database| database.validity.invalidated())
        } else {
            None
        };
        let outcome = match invalid_use {
            Some(message) => Err(Error::new(ErrorKind::Io, message)),
            None => outcome,
        };
        let ended = self.transactions.end_statement(outcome.is_ok(), invalidates);
        // Between statements, the connection's functions (and C API clients reading its
        // options) see the settings the statement left.
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        // `EndQueryInternal`: the profile is written before the logger is refreshed.
        drop(metrics_guard);
        let profiled = self
            .profiler
            .end_query(&self.settings.profiler, self.logger.as_ref());
        self.end_query_logging();
        let mut result = outcome.and_then(|result| ended.map(|()| result));
        if result.is_ok()
            && let Err(error) = profiled
        {
            result = Err(error);
        }
        if result.is_ok()
            && !self.transactions.has_current()
            && let Some(committed) = committed
            && let Err(error) = self.database.instance().auto_checkpoint(
                &committed.database,
                committed.properties.has_updates || committed.properties.has_dropped_entries,
                committed.properties.estimated_size,
                self.settings.skip_checkpoint_on_commit(),
            )
        {
            result = Err(error);
        }
        if !self.transactions.has_current()
            && let Err(error) = self.release_databases()
            && result.is_ok()
        {
            result = Err(error);
        }
        result.map_err(|error| self.process_error(error, query))
    }

    /// Applies the configuration changes functions made (`enable_profiling` /
    /// `disable_profiling`, `enable_peg_parser` / `disable_peg_parser`).
    fn apply_client_requests(&mut self, requests: Vec<rustdb_function::ClientRequest>) -> Result<()> {
        use rustdb_function::ClientRequest;
        for request in requests {
            match request {
                ClientRequest::EnableProfiling {
                    format,
                    coverage,
                    save_location,
                    mode,
                    metrics,
                } => {
                    let disabled = self.settings.optimizer_config().disabled;
                    let options = crate::query_profiler::ProfilingOptions {
                        format: format.as_deref(),
                        coverage: coverage.as_deref(),
                        save_location: save_location.as_deref(),
                        mode: mode.as_deref(),
                        metrics: metrics.as_ref(),
                    };
                    self.settings
                        .profiler
                        .apply_enable_profiling(&options, &disabled)?;
                }
                ClientRequest::DisableProfiling => self.settings.profiler.apply_disable_profiling(),
                ClientRequest::SetGlobalOption { name, value } => {
                    self.settings.set(&name, &value)?;
                    self.record_setting(&name, Some(&value), SetScope::Global);
                }
            }
        }
        Ok(())
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

    /// `PhysicalSet::SetExtensionVariable`: the option's callback checks the value, which
    /// is then set for the session or globally (without a scope, as the option's default
    /// scope says).
    fn set_extension_variable(&mut self, name: &str, value: &Value, scope: SetScope) -> Result<()> {
        let instance = self.database.instance();
        let option = instance.extension_option(name).ok_or_else(|| {
            Error::invalid_input(format!(
                "Extension parameter {name} was not found after autoloading"
            ))
        })?;
        let mut target_value = self.cast_value(value, &option.input_type)?;
        if let Some(set_function) = &option.set_function {
            set_function(&mut target_value)?;
        }
        if scope == SetScope::Global || (scope == SetScope::Automatic && option.global_by_default) {
            instance.set_extension_global(&option, Some(target_value));
        } else {
            self.extension_session.insert(&option.name, target_value);
        }
        Ok(())
    }

    /// `PhysicalReset::ResetExtensionVariable`: the option's callback checks the default,
    /// which then becomes the session's or the global value (without a scope, as the
    /// option's default scope says).
    fn reset_extension_variable(&mut self, name: &str, scope: SetScope) -> Result<()> {
        let instance = self.database.instance();
        let option = instance.extension_option(name).ok_or_else(|| {
            Error::invalid_input(format!(
                "Extension parameter {name} was not found after autoloading"
            ))
        })?;
        if let Some(set_function) = &option.set_function {
            let mut default_value = option.default_value.clone();
            set_function(&mut default_value)?;
        }
        if scope == SetScope::Global || (scope == SetScope::Automatic && option.global_by_default) {
            instance.set_extension_global(&option, None);
        } else {
            self.extension_session
                .insert(&option.name, option.default_value.clone());
        }
        Ok(())
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
        self.log_query(query);
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

    /// `ClientContext::LogQueryInternal`: the statement and a newline, synced to disk.
    fn log_query(&mut self, query: &str) {
        use std::io::Write as _;
        if let Some(writer) = &mut self.log_query_writer {
            let _ = writer
                .write_all(query.as_bytes())
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.sync_data());
        }
    }

    /// `LogQueryPathSetting::OnSet`: an empty path stops logging, any other starts a new
    /// log file there.
    fn set_log_query_path(&mut self, value: Option<&rustdb_types::Value>) -> Result<()> {
        let path = value
            .map(rustdb_types::Value::to_varchar)
            .transpose()?
            .unwrap_or_default();
        self.log_query_writer = if path.is_empty() {
            None
        } else {
            Some(std::fs::File::create(&path).map_err(|error| {
                Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {error}"))
            })?)
        };
        Ok(())
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
            .map_err(|error| self.process_error(error, query))
    }

    /// `ClientContext::ProcessError`: the error as JSON under `errors_as_json`, else with
    /// the location in the query.
    fn process_error(&self, error: Error, query: &str) -> Error {
        if self.settings.errors_as_json() {
            error.into_json()
        } else {
            error.with_query_context(query)
        }
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
        self.function_client
            .set_temp_catalog(Arc::new(crate::temp_catalog::StatementTempCatalog {
                catalog: Arc::clone(&self.temp_catalog),
                transaction: Arc::clone(&transaction),
            }));
        let instance = self.database.instance();
        let context = Arc::new(ClientContext {
            catalogs: self.catalogs(&instance.default_database_name()),
            search_path: self.search_path.clone(),
            view_search_path: Vec::new(),
            transaction,
            casts: self.casts(),
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
            replacement_scans: instance
                .replacement_scans
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            dependency_recorder: Arc::default(),
            database_state: Some(Arc::new(InstanceState {
                secrets: Arc::clone(&instance.secrets),
                instance: Arc::downgrade(&self.database.instance),
            }) as Arc<dyn rustdb_planner::DatabaseState>),
        });
        let any: Arc<dyn std::any::Any + Send + Sync> = Arc::clone(&context) as _;
        self.function_client
            .set_transaction_context(Some(Arc::downgrade(&any)));
        context
    }

    /// `IMPORT DATABASE 'dir'` (DuckDB's `PragmaImportDatabase`): runs the directory's
    /// `schema.sql`, then its `load.sql` with the files taken from the directory.
    fn import_database(&mut self, parameters: &[rustdb_types::Value]) -> Result<MaterializedResult> {
        let directory = parameters
            .first()
            .map(rustdb_types::Value::to_varchar)
            .transpose()?
            .unwrap_or_default();
        rustdb_function::file_system::verify_can_access_directory(&directory)?;
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
        let target_name = target.name().to_string();
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
                        rustdb_ast::qualifier_to_string(&source.name(), &schema, entry.name()),
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

    /// `LOAD` / `INSTALL` (`PhysicalLoad`).
    fn run_load(&mut self, info: &rustdb_ast::LoadInfo) -> Result<()> {
        let config =
            crate::extensions::ExtensionConfig::from_settings(|name| self.database.global_setting(name));
        if let Some(name) = crate::extensions::load_extension(info, &config)?
            && let Some(extension) = StaticExtension::from_name(&name)
        {
            self.database.instance.load_static_extension(extension)?;
        }
        Ok(())
    }

    /// Loads the extension of the `ATTACH` storage type `db_type`
    /// (`DatabaseManager::GetDatabaseType`'s `LoadExternalExtension`).
    fn load_storage_extension(&mut self, db_type: &str) -> Result<()> {
        self.run_load(&rustdb_ast::LoadInfo {
            filename: db_type.to_owned(),
            load_type: rustdb_ast::LoadType::Load,
            repository: String::new(),
            version: String::new(),
            repo_is_alias: false,
        })
    }

    fn run_statement(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        self.start_statement(statement, RunMode::Materialize)
            .and_then(StatementRun::into_materialized)
    }

    /// Starts one parsed statement (DuckDB's `PendingStatementInternal`): statements the
    /// connection runs itself complete right away; a query's physical plan runs to
    /// completion or, in [`RunMode::Pending`], is left to the caller to execute.
    pub(crate) fn start_statement(&mut self, statement: SqlStatement, mode: RunMode) -> Result<StatementRun> {
        if let rustdb_ast::SqlStatementKind::Multi(statements) = statement.kind {
            return self.run_multi_statement(statements).map(StatementRun::Done);
        }
        if let rustdb_ast::SqlStatementKind::CopyDatabase(copy) = &statement.kind {
            return self.copy_database(copy).map(StatementRun::Done);
        }
        if let rustdb_ast::SqlStatementKind::Load(info) = &statement.kind {
            self.run_load(info)?;
            return Ok(StatementRun::Done(MaterializedResult::default()));
        }
        // Pragmas that stand for a query run that query (DuckDB's `PragmaHandler`).
        if let rustdb_ast::SqlStatementKind::Pragma(pragma) = &statement.kind
            && self.database.instance.has_extension_pragma(&pragma.name)
        {
            // An extension's pragma binds its parameters like any pragma.
            let name = pragma.name.clone();
            let parameters = self.bind_pragma_parameters(statement)?;
            let query = self
                .database
                .instance
                .extension_pragma_query(&name, &parameters)?;
            return self.run_pragma_query(&query).map(StatementRun::Done);
        }
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
                return self.import_database(constants).map(StatementRun::Done);
            }
            let query = match &constants {
                Some(constants) => crate::system_functions::pragma_query(&pragma.name, constants)?,
                None => None,
            };
            if let Some(query) = query {
                return self.run_pragma_query(&query).map(StatementRun::Done);
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
        // `VACUUM tbl` compacts the table's indexes (`PhysicalVacuum::Finalize`).
        let vacuum_table = match &statement.kind {
            rustdb_ast::SqlStatementKind::Vacuum(info) if info.has_table => info
                .table_ref
                .as_deref()
                .and_then(|table_ref| match &table_ref.kind {
                    rustdb_ast::TableRefKind::BaseTable(base) => Some((**base).clone()),
                    _ => None,
                }),
            _ => None,
        };
        if checkpoint.is_none() && vacuum_table.is_none() {
            return self.start_planned(statement, mode);
        }
        // The connection finishes these statements itself once their plan ran.
        let result = self.run_planned(statement)?;
        if let Some(base) = vacuum_table {
            let transaction = self.transactions.current();
            let client = self.client_context(transaction);
            let found = client.get_entry(
                &base.catalog_name,
                &base.schema_name,
                rustdb_catalog::CatalogType::Table,
                &base.table_name,
                true,
            )?;
            if let Some(found) = found
                && let rustdb_catalog::EntryKind::Table(table) = &found.entry.kind
                && let Some(storage) = &table.storage
            {
                storage.vacuum_indexes()?;
            }
        }
        if let Some(force) = checkpoint {
            let instance = self.database.instance();
            let name = database.unwrap_or_else(|| self.default_catalog());
            let attached = instance.database(&name)?;
            let started = !self.transactions.is_auto_commit()
                && self
                    .used_databases
                    .iter()
                    .any(|database| Arc::ptr_eq(database, &attached));
            let transaction = self.transactions.current();
            // Explicit checkpoints bind the indexes first, so the written indexes hold
            // the changes replayed from the WAL (`SingleFileCheckpointWriter::WriteTable`
            // with a bind connection).
            if attached.storage.file().is_some() {
                let client = self.client_context(Arc::clone(&transaction));
                let system = Transaction::system();
                for schema in attached.catalog.schemas().scan(&system) {
                    let Some(schema_entry) = schema.as_schema() else {
                        continue;
                    };
                    for table in schema_entry.tables.scan(&system) {
                        rustdb_execution::operators::bind_table_indexes(&client, &table)?;
                    }
                }
            }
            crate::storage::checkpoint(&attached, &transaction, started, force, &instance.transactions)?;
        }
        Ok(StatementRun::Done(result))
    }

    /// Runs the statements of the query a pragma stands for; the result is the last
    /// statement's.
    fn run_pragma_query(&mut self, query: &str) -> Result<MaterializedResult> {
        let mut result = MaterializedResult::default();
        for statement in rustdb_parser::parse(query)? {
            result = self.run_statement(statement)?;
        }
        Ok(result)
    }

    /// The values of a `PRAGMA`'s parameters, bound and evaluated as constants
    /// (`Binder::BindPragma`).
    fn bind_pragma_parameters(&mut self, statement: SqlStatement) -> Result<Vec<Value>> {
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        match planner.create_plan(statement)?.plan.map(|plan| plan.kind) {
            Some(LogicalKind::Pragma { parameters, .. }) => Ok(parameters),
            _ => Err(Error::internal("a PRAGMA bound to another statement")),
        }
    }

    /// Records that the transaction uses `database` (`MetaTransaction::UseDatabase`).
    fn use_database(&mut self, database: &Arc<AttachedDatabase>) {
        if !self.used_databases.iter().any(|used| Arc::ptr_eq(used, database)) {
            self.used_databases.push(Arc::clone(database));
        }
    }

    /// The reason a database the transaction used was invalidated, if one was.
    fn invalidated_used_database(&self) -> Option<String> {
        self.used_databases
            .iter()
            .find_map(|database| database.validity.invalidated())
    }

    /// Lets go of the databases the finished transaction used; one detached meanwhile is
    /// closed now that no transaction uses it (`MetaTransaction::Finalize`).
    fn release_databases(&mut self) -> Result<()> {
        let checkpoint = self.database.instance().checkpoint_on_shutdown();
        let mut result = Ok(());
        for database in std::mem::take(&mut self.used_databases) {
            if let Err(error) = AttachedDatabase::close_if_last_reference(&database, checkpoint)
                && result.is_ok()
            {
                result = Err(error);
            }
        }
        result
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
            instance
                .databases
                .detach_if_current(&attached, instance.checkpoint_on_shutdown());
        }
    }

    /// `DETACH name` (`DatabaseManager::DetachDatabase`).
    fn detach(&mut self, info: &rustdb_ast::DetachInfo) -> Result<()> {
        // `PhysicalDetach`: not while this transaction has work on the database.
        if !self.transactions.is_auto_commit()
            && self.transactions.current().has_accessed_database(&info.name)
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
        match instance
            .databases
            .detach(&info.name, instance.checkpoint_on_shutdown())
        {
            Some(result) => result,
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
                // The `temp` and `system` catalogs alone mean their `main` schema.
                if schema.eq_ignore_ascii_case(rustdb_catalog::TEMP_CATALOG)
                    || schema.eq_ignore_ascii_case("system")
                {
                    resolved.push((schema.to_lowercase(), rustdb_catalog::DEFAULT_SCHEMA.to_owned()));
                    set_paths.push((schema.clone(), rustdb_catalog::DEFAULT_SCHEMA.to_owned()));
                    continue;
                }
                // A database name alone means its default schema.
                if let Some(database) = instance.databases.get(&schema)
                    && let Some(main) = database
                        .catalog
                        .schema(&transaction, rustdb_catalog::DEFAULT_SCHEMA)
                {
                    resolved.push((database.name().to_string(), main.name().to_owned()));
                    set_paths.push((schema.clone(), main.name().to_owned()));
                    continue;
                }
                let default = self.default_catalog();
                let database = instance.databases.get(&default).ok_or_else(not_found)?;
                if let Some(found) = database.catalog.schema(&transaction, &schema) {
                    resolved.push((database.name().to_string(), found.name().to_owned()));
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
            resolved.push((database.name().to_string(), found.name().to_owned()));
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
            block_allocator_memory: buffer_manager.pool().allocator().physical_memory_size() as u64,
            threads: self.database.instance().env.threads() as u64,
            extensions: self
                .database
                .instance()
                .extension_option_values()
                .into_iter()
                .map(
                    |(option, global)| match self.extension_session.get(&option.name) {
                        Some(value) => {
                            let value = value.clone();
                            (option, value, true)
                        }
                        None => (option, global, false),
                    },
                )
                .collect(),
        }
    }

    /// The name of the default database of this connection.
    fn default_catalog(&self) -> String {
        self.search_path.first().map_or_else(
            || self.database.instance().default_database_name(),
            |(catalog, _)| catalog.clone(),
        )
    }

    fn run_planned(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        self.start_planned(statement, RunMode::Materialize)
            .and_then(StatementRun::into_materialized)
    }

    /// Binds and plans `statement` (`CreatePreparedStatementInternal`) and starts its plan.
    fn start_planned(&mut self, statement: SqlStatement, mode: RunMode) -> Result<StatementRun> {
        use rustdb_common::profiling::MetricType;
        let statement_type = statement.kind.statement_type();
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        // `CreatePreparedStatementInternal`: the planner's phase, of which binding is
        // all here.
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::Planner);
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PlannerBinding);
        let planned = planner.create_plan(statement);
        self.profiler.end_phase(&self.settings.profiler);
        self.profiler.end_phase(&self.settings.profiler);
        let planned = planned?;
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
        self.start_bound_plan(plan, names, types, &properties, statement_type, mode)
    }

    /// Starts the bound plan of a statement (`PendingPreparedStatementInternal`): the
    /// databases it writes join the transaction, and the result streams only if both the
    /// caller and the statement allow it.
    pub(crate) fn start_bound_plan(
        &mut self,
        plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
        properties: &rustdb_planner::StatementProperties,
        statement_type: rustdb_ast::StatementType,
        mode: RunMode,
    ) -> Result<StatementRun> {
        self.register_modifications(&plan, &properties.modified_databases, statement_type)?;
        let mode = match mode {
            RunMode::Pending { stream } => RunMode::Pending {
                stream: stream && properties.allow_stream_result,
            },
            RunMode::Materialize => RunMode::Materialize,
        };
        Ok(self
            .start_plan(plan, names, types, mode)?
            .with_return_type(properties.return_type))
    }

    /// The statements a statement was rewritten into (DuckDB's `UnpackMultiStatement`):
    /// outside a transaction, several statements without a SELECT run in one transaction
    /// that a failure rolls back. The result is the last statement's.
    fn run_multi_statement(&mut self, statements: Vec<SqlStatement>) -> Result<MaterializedResult> {
        let has_select = statements
            .iter()
            .any(|statement| matches!(statement.kind, rustdb_ast::SqlStatementKind::Select(_)));
        let transactional = statements.len() > 1 && !has_select;
        let wrap = transactional && self.transactions.is_auto_commit();
        // In an explicit transaction the body runs under `ALL_ERRORS_INVALIDATE_TRANSACTION`
        // (`StatementPreprocessor`'s `SET_INVALIDATION_POLICY`): any error aborts it.
        let all_errors_invalidate = transactional && !wrap;
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
            // Each statement runs as its own query (`PendingQuery` with its text).
            if !statement.query.is_empty() {
                self.function_client.set_query(&statement.query);
            }
            match self.run_statement(statement) {
                Ok(statement_result) => result = statement_result,
                Err(error) => {
                    if wrap && !self.transactions.is_auto_commit() {
                        let _ = run_sql(self, "ROLLBACK");
                    }
                    if all_errors_invalidate {
                        self.transactions.invalidate();
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
        function_modified: &[String],
        statement_type: rustdb_ast::StatementType,
    ) -> Result<()> {
        let instance = self.database.instance();
        // DuckDB walks its `modified_databases` map, which lists the latest registration
        // first: the databases functions write to come before the statement's own target,
        // which its bind registered earlier.
        let mut modified: Vec<String> = function_modified.iter().rev().cloned().collect();
        collect_modified_databases(plan, instance, &mut modified);
        // The databases the statement reads or writes join the transaction
        // (`MetaTransaction::GetTransaction`), unless they were invalidated.
        let mut used = modified.clone();
        collect_read_databases(plan, &mut used);
        let used: Vec<Arc<AttachedDatabase>> = used
            .iter()
            .filter_map(|name| instance.databases.get(name))
            .collect();
        for database in &used {
            database.check_valid()?;
            self.use_database(database);
        }
        let instance = self.database.instance();
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
                    database.name()
                )));
            }
            transaction.modify_database(
                &database.name(),
                database.catalog.wal(),
                database.catalog.locks(),
                needs_checkpoint_lock(plan),
                needs_vacuum_lock(plan),
            )?;
        }
        Ok(())
    }

    /// Runs a bound plan to completion (see [`Connection::start_plan`]).
    fn run_plan(
        &mut self,
        plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        self.start_plan(plan, names, types, RunMode::Materialize)
            .and_then(StatementRun::into_materialized)
    }

    /// Starts a bound plan: statements that act on the connection are executed here
    /// (DuckDB's `PhysicalTransaction`, `PhysicalSet`, `PhysicalPrepare`, ...); queries go
    /// through the physical planner and executor.
    fn start_plan(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        let nothing =
            || MaterializedResult::new(vec!["Success".to_owned()], vec![LogicalType::BOOLEAN], Vec::new());
        // `PhysicalSet` / `PhysicalReset`: `DBConfig::CheckLock`, then the checks of options
        // that cannot change while the database runs.
        match &plan.kind {
            LogicalKind::Set { name, value, scope } if *scope != SetScope::Variable => {
                self.settings.check_lock(name)?;
                Settings::check_session_scope(name, *scope)?;
                Settings::check_scope(name, *scope, false)?;
                Settings::check_running_set(name, value)?;
            }
            LogicalKind::Reset { name, scope } if *scope != SetScope::Variable => {
                self.settings.check_lock(name)?;
                Settings::check_session_scope(name, *scope)?;
                Settings::check_scope(name, *scope, true)?;
                Settings::check_running_reset(name)?;
            }
            _ => {}
        }
        // Statements the connection runs itself are profiled as their physical operator
        // under the result collector.
        let operator_type = match &plan.kind {
            LogicalKind::Transaction { .. } => Some("TRANSACTION"),
            LogicalKind::Attach(_) => Some("ATTACH"),
            LogicalKind::Detach(_) => Some("DETACH"),
            LogicalKind::Set {
                scope: SetScope::Variable,
                ..
            } => None,
            LogicalKind::Set { .. } => Some("SET"),
            LogicalKind::Reset { .. } => Some("RESET"),
            LogicalKind::Prepare { .. } => Some("PREPARE"),
            LogicalKind::Deallocate { .. } => Some("DROP"),
            LogicalKind::CreateSecret(_) => Some("CREATE_SECRET"),
            LogicalKind::Drop(info) if info.catalog_type == rustdb_ast::CatalogType::SecretEntry => {
                Some("DROP")
            }
            LogicalKind::Pragma { .. } => Some("PRAGMA"),
            _ => None,
        };
        if let Some(operator_type) = operator_type {
            self.profiler
                .initialize_statement(&self.settings.profiler, operator_type);
        }
        match &plan.kind {
            LogicalKind::Transaction { info } => {
                match info.transaction_type {
                    TransactionType::BeginTransaction => {
                        self.transactions.begin(
                            info.modifier == rustdb_ast::TransactionModifierType::TransactionReadOnly,
                        )?;
                        // `immediate_transaction_mode` starts the transaction in every valid
                        // database right away.
                        if rustdb_function::SettingsSource::current_setting(
                            &self.settings_snapshot(),
                            "immediate_transaction_mode",
                        )
                        .ok()
                        .and_then(|value| value.as_bool())
                            == Some(true)
                        {
                            let transaction = self.transactions.current();
                            for database in self.database.instance().databases.all() {
                                if database.validity.invalidated().is_none() {
                                    transaction.access_database(&database.name());
                                    self.use_database(&database);
                                }
                            }
                        }
                    }
                    TransactionType::Commit => {
                        // A transaction that used an invalidated database is rolled back
                        // (`MetaTransaction::Commit`), as is an aborted one.
                        if let Some(message) = self.invalidated_used_database() {
                            self.transactions.rollback()?;
                            self.finish_transaction_attaches(true);
                            return Err(Error::new(ErrorKind::Io, message));
                        }
                        let rolls_back = self.transactions.is_invalidated();
                        if !rolls_back && self.transactions.has_current() {
                            let current = self.transactions.current();
                            self.explicit_commit = CommittedChanges::of(&current);
                        }
                        let result = self.transactions.commit();
                        self.finish_transaction_attaches(rolls_back || result.is_err());
                        result?;
                    }
                    TransactionType::Rollback => {
                        self.transactions.rollback()?;
                        self.finish_transaction_attaches(true);
                        if let Some(message) = self.invalidated_used_database() {
                            return Err(Error::new(ErrorKind::Io, message));
                        }
                    }
                    TransactionType::Invalid => {
                        return Err(Error::not_implemented("Unrecognized transaction type!"));
                    }
                }
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Attach(info) => {
                let options = AttachOptions::from_info(info, crate::AccessMode::Automatic)?;
                // `PhysicalAttach::GetDataInternal`: a `type:` prefix names the type when
                // the TYPE option does not; `DatabaseManager::GetDatabaseType` treats
                // `duckdb` as no type, checks the file's magic bytes without one (data
                // files are attached as views on them) and loads the extension of any
                // other type, whose storage rustdb does not have.
                let mut path = info.path.as_str();
                let mut db_type = options.db_type.clone();
                if db_type.is_none()
                    && let Some(prefix) = crate::attached::extension_prefix(path)
                {
                    db_type = Some(prefix.to_lowercase());
                    path = &path[prefix.len() + 1..];
                }
                let duckdb_type = db_type.as_deref() == Some("duckdb");
                if let Some(db_type) = db_type.filter(|db_type| db_type != "duckdb") {
                    self.load_storage_extension(&db_type)?;
                    return Err(Error::binder(format!("Unrecognized storage type \"{db_type}\"")));
                }
                let attached = if !duckdb_type && crate::attached::is_open_file(path)? {
                    self.database
                        .attach_open_file(&info.name, path, &options, info.on_conflict)?
                } else {
                    self.database
                        .attach(&info.name, path, &options, info.on_conflict)?
                };
                if let Some(attached) = attached
                    && !self.transactions.is_auto_commit()
                {
                    self.transaction_attaches.push(attached);
                }
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Detach(info) => {
                self.detach(info)?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::AlterDatabase(info) => {
                self.database.instance().alter_database(info)?;
                return Ok(StatementRun::Done(nothing()));
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
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Reset {
                name,
                scope: SetScope::Variable,
            } => {
                self.user_variables.remove(name);
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Set { name, value, .. } if is_search_path_setting(name) => {
                self.set_search_path(name, value)?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Reset { name, .. } if is_search_path_setting(name) => {
                self.search_path.clear();
                self.search_path_set.clear();
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Set { name, value, scope }
                if crate::settings::canonical_option(name).is_none()
                    && self.database.instance().extension_option(name).is_some() =>
            {
                let (name, value, scope) = (name.clone(), value.clone(), *scope);
                self.set_extension_variable(&name, &value, scope)?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Reset { name, scope }
                if crate::settings::canonical_option(name).is_none()
                    && self.database.instance().extension_option(name).is_some() =>
            {
                let (name, scope) = (name.clone(), *scope);
                self.reset_extension_variable(&name, scope)?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Set { name, .. } | LogicalKind::Reset { name, .. }
                if crate::settings::canonical_option(name).is_none()
                    && !is_extension_setting(name)
                    && !self.database.instance().extension_option_values().is_empty() =>
            {
                // `Catalog::UnrecognizedConfigurationError` suggests the extensions' options too.
                let extension_names: Vec<String> = self
                    .database
                    .instance()
                    .extension_option_values()
                    .into_iter()
                    .map(|(option, _)| option.name)
                    .collect();
                return Err(crate::settings::unrecognized_with(name, &extension_names, |_| {
                    false
                }));
            }
            LogicalKind::Set { name, value, scope } => {
                if is_secret_setting(name) {
                    self.database.instance().secrets.set_option(name, Some(value))?;
                }
                if is_threads_setting(name) {
                    self.database.instance().set_threads(Some(value))?;
                }
                if name.eq_ignore_ascii_case("log_query_path") {
                    self.set_log_query_path(Some(value))?;
                }
                if name.eq_ignore_ascii_case("disabled_filesystems") {
                    let list = value.to_varchar()?;
                    self.database
                        .instance()
                        .set_disabled_file_systems(&list.split(',').collect::<Vec<_>>())?;
                }
                self.database.instance().set_buffer_setting(name, Some(value))?;
                if crate::logging::is_logging_setting(name) {
                    crate::logging::apply_setting(&self.database.instance().log_manager, name, Some(value))?;
                }
                if name.eq_ignore_ascii_case("default_collation") {
                    self.settings.extension_collations = self.database.instance().extension_collations();
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
                self.database
                    .instance()
                    .set_checkpoint_setting(name, Some(value))?;
                self.database
                    .instance()
                    .set_optimistic_write_setting(name, Some(value))?;
                self.database
                    .instance()
                    .set_variant_shredding_setting(name, Some(value))?;
                self.database
                    .instance()
                    .set_disabled_compression_methods(name, Some(value))?;
                self.database
                    .instance()
                    .set_wal_autocheckpoint_entries(name, Some(value))?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Reset { name, scope } => {
                if is_secret_setting(name) {
                    self.database.instance().secrets.set_option(name, None)?;
                }
                if crate::logging::is_logging_setting(name) {
                    crate::logging::apply_setting(&self.database.instance().log_manager, name, None)?;
                }
                if name.eq_ignore_ascii_case("disabled_filesystems") {
                    self.database.instance().set_disabled_file_systems(&[])?;
                }
                self.settings.reset(name)?;
                if name.eq_ignore_ascii_case("log_query_path") {
                    self.set_log_query_path(None)?;
                }
                if is_threads_setting(name) {
                    self.database.instance().set_threads(None)?;
                }
                self.database.instance().set_buffer_setting(name, None)?;
                let (option, reset_scope) = (name.clone(), *scope);
                self.record_setting(&option, None, reset_scope);
                if is_force_compression(name) {
                    self.database.instance().set_force_compression(None)?;
                }
                self.database.instance().set_checkpoint_setting(name, None)?;
                self.database
                    .instance()
                    .set_optimistic_write_setting(name, None)?;
                self.database
                    .instance()
                    .set_variant_shredding_setting(name, None)?;
                self.database
                    .instance()
                    .set_disabled_compression_methods(name, None)?;
                self.database
                    .instance()
                    .set_wal_autocheckpoint_entries(name, None)?;
                if is_checkpoint_threshold(name) {
                    self.database
                        .instance()
                        .checkpoint_wal_size
                        .store(DEFAULT_CHECKPOINT_WAL_SIZE, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Prepare { name, prepared } => {
                self.prepared_statements
                    .insert(name.clone(), Arc::clone(prepared));
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Deallocate { name } => {
                self.prepared_statements.remove(name);
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::CreateSecret(input) => {
                // `PhysicalCreateSecret`: one row with `true`.
                self.database.instance().secrets.create_secret(input)?;
                let column = rustdb_types::Vector::constant(&rustdb_types::Value::boolean(true))?;
                return Ok(StatementRun::Done(MaterializedResult::new(
                    names,
                    types,
                    vec![rustdb_types::DataChunk::new(vec![column], 1)],
                )));
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
                return Ok(StatementRun::Done(nothing()));
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
                    return Ok(StatementRun::Done(nothing()));
                }
                // `PragmaEnableProgressBar` and friends set the client's configuration.
                let progress_setting = match lower.as_str() {
                    "enable_progress_bar" => Some(("enable_progress_bar", true)),
                    "disable_progress_bar" => Some(("enable_progress_bar", false)),
                    "enable_print_progress_bar" => Some(("enable_progress_bar_print", true)),
                    "disable_print_progress_bar" => Some(("enable_progress_bar_print", false)),
                    _ => None,
                };
                if let Some((setting, enabled)) = progress_setting {
                    self.settings.set(setting, &Value::boolean(enabled))?;
                    return Ok(StatementRun::Done(nothing()));
                }
                if lower == "enable_optimizer" || lower == "disable_optimizer" {
                    self.settings.optimizer_disabled = lower == "disable_optimizer";
                }
                match lower.as_str() {
                    "enable_profiling" | "enable_profile" => self.settings.profiler.enable(),
                    "disable_profiling" | "disable_profile" => self.settings.profiler.disable(),
                    _ => {}
                }
                if lower == "disable_checkpoint_on_shutdown" || lower == "enable_checkpoint_on_shutdown" {
                    self.database
                        .instance()
                        .shutdown_checkpoint_disabled
                        .store(lower.starts_with("disable"), std::sync::atomic::Ordering::Relaxed);
                }
                if NO_OP_PRAGMAS.contains(&name.to_ascii_lowercase().as_str()) {
                    return Ok(StatementRun::Done(nothing()));
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
        let Some((catalog, schema)) = dropped_schema else {
            return self.start_execution(plan, names, types, mode);
        };
        let result = self.execute_plan(plan, names, types)?;
        {
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
        Ok(StatementRun::Done(result))
    }

    /// Plans and runs a logical plan, collecting its result.
    fn execute_plan(
        &mut self,
        plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        self.start_execution(plan, names, types, RunMode::Materialize)
            .and_then(StatementRun::into_materialized)
    }

    /// Optimizes a logical plan, plans it physically and initializes its executor
    /// (`PendingPreparedStatementInternal`); in [`RunMode::Materialize`] the plan also runs
    /// to completion.
    fn start_execution(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<String>,
        types: Vec<LogicalType>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        use rustdb_common::profiling::MetricType;
        let transaction = self.transactions.current();
        let client = self.client_context(transaction);
        let optimizer_config = self.settings.optimizer_config();
        let optimizer_client = optimizer_config.enabled.then(|| Arc::clone(&client));
        let preserve_insertion_order = optimizer_config.preserve_insertion_order;
        let plan_client = Arc::clone(&client);
        let optimize = optimizer_config.enabled;
        if optimize {
            self.profiler
                .start_phase(&self.settings.profiler, MetricType::AllOptimizers);
        }
        let mut optimizer = Optimizer::new(client, optimizer_config);
        let optimized = optimizer.optimize(plan);
        if optimize {
            for (pass, time) in optimizer.timings() {
                self.profiler
                    .add_optimizer_timing(&self.settings.profiler, *pass, time.as_secs_f64());
            }
            self.profiler.end_phase(&self.settings.profiler);
        }
        plan = optimized?;
        // `PhysicalPlanGenerator::Plan`: the operators' types are already resolved.
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PhysicalPlanner);
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PhysicalPlannerResolveTypes);
        self.profiler.end_phase(&self.settings.profiler);
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PhysicalPlannerColumnBinding);
        let resolved = ColumnBindingResolver::resolve(&mut plan);
        self.profiler.end_phase(&self.settings.profiler);
        if let Err(error) = resolved {
            self.profiler.end_phase(&self.settings.profiler);
            return Err(error);
        }
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
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PhysicalPlannerCreatePlan);
        let physical = create_physical_plan_with(plan, &context);
        self.profiler.end_phase(&self.settings.profiler);
        self.profiler.end_phase(&self.settings.profiler);
        let rustdb_execution::PhysicalPlan {
            root: physical,
            estimates,
        } = physical?;
        let explain_analyze = physical.name() == "EXPLAIN_ANALYZE";
        let env = Arc::clone(&self.database.instance().env);
        let streaming = mode == (RunMode::Pending { stream: true });
        let root: PhysicalRef = if streaming {
            Arc::new(PhysicalResultCollector::streaming(
                physical,
                preserve_insertion_order,
            ))
        } else {
            Arc::new(PhysicalResultCollector::for_plan(
                physical,
                env.threads(),
                preserve_insertion_order,
            ))
        };
        // `Executor::Initialize`; EXPLAIN ANALYZE and profiled queries record what every
        // operator did.
        self.profiler.initialize(&self.settings.profiler, &root);
        let profiler = (explain_analyze || self.profiler.is_running()).then(|| {
            Arc::new(
                rustdb_execution::QueryProfiler::new()
                    .with_query_metrics(self.profiler.active_metrics().cloned())
                    .with_json_by_default(crate::query_profiler::QueryProfiler::prints_json(
                        &self.settings.profiler,
                    ))
                    .with_memory(env.buffer_manager.clone()),
            )
        });
        let options = rustdb_execution::QueryOptions {
            profiler: profiler.clone(),
            preserve_insertion_order,
            client_interrupted: Some(self.client.interrupt_flag()),
            estimates,
        };
        let execution = match QueryExecution::new(root, env, &options) {
            Ok(execution) => execution,
            Err(error) => {
                if let Some(profiler) = &profiler {
                    self.profiler.flush(&self.settings.profiler, profiler);
                }
                return Err(error);
            }
        };
        let run = PlanExecution::new(execution, names, types, profiler, streaming);
        match mode {
            RunMode::Materialize => self.finish_execution(run).map(StatementRun::Done),
            RunMode::Pending { .. } => Ok(StatementRun::Executing(Box::new(run))),
        }
    }

    /// Runs the remaining tasks of a plan and collects its result
    /// (`ClientContext::ExecutePendingQueryInternal`).
    pub(crate) fn finish_execution(&mut self, run: PlanExecution) -> Result<MaterializedResult> {
        let (execution, names, types, profiler, return_type) = run.into_parts();
        let state = execution.finish();
        if let Some(profiler) = &profiler {
            self.profiler.flush(&self.settings.profiler, profiler);
        }
        let chunks = PhysicalResultCollector::take_chunks(&state?)?;
        Ok(MaterializedResult::new(names, types, chunks).with_return_type(return_type))
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
/// `ErrorManager::InvalidatedDatabase`: the error of using an invalidated instance.
pub(crate) fn invalidated_database_error(message: &str) -> Error {
    const BASE: &str = "Failed: database has been invalidated because of a previous fatal error. The database must \
                        be restarted prior to being used again.\n";
    if message.contains(BASE) {
        return Error::new(ErrorKind::Fatal, message.to_owned());
    }
    Error::new(ErrorKind::Fatal, format!("{BASE}Original error: \"{message}\""))
}

/// The databases whose tables a plan scans.
fn collect_read_databases(plan: &LogicalOperator, out: &mut Vec<String>) {
    if let LogicalKind::Get(get) = &plan.kind
        && get.table.is_some()
        && let Some((catalog, _)) = &get.table_schema
        && !catalog.is_empty()
        && !out.iter().any(|existing| existing.eq_ignore_ascii_case(catalog))
    {
        out.push(catalog.clone());
    }
    for child in &plan.children {
        collect_read_databases(child, out);
    }
}

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
            .map(|database| database.name().to_string())
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
        LogicalKind::Get(get) => {
            if let Some(name) = table_function_modified_database(get) {
                push(&name);
            }
        }
        _ => {}
    }
    for child in &plan.children {
        collect_modified_databases(child, instance, out);
    }
}

/// The database a table function call writes to, as its bind registered it (DuckDB's
/// `RegisterDBModify`, e.g. by `dbgen`).
fn table_function_modified_database(get: &rustdb_planner::operator::LogicalGet) -> Option<String> {
    get.bind_data.as_deref()?.modified_database()
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
        // A table function that writes creates catalog entries (`CREATE_CATALOG_ENTRY`).
        LogicalKind::Get(get) => table_function_modified_database(get).is_some(),
        LogicalKind::MergeInto(merge) => merge
            .actions
            .values()
            .flatten()
            .any(|action| action.action_type == rustdb_ast::MergeActionType::MergeUpdate),
        _ => false,
    };
    own || plan.children.iter().any(needs_checkpoint_lock)
}

/// Whether a plan inserts or deletes rows, which holds the vacuum lock shared
/// (`DuckTransaction::SetModifications` with `InsertData` / `DeleteData`).
fn needs_vacuum_lock(plan: &LogicalOperator) -> bool {
    let own = match &plan.kind {
        LogicalKind::Insert(_) | LogicalKind::Delete(_) => true,
        LogicalKind::MergeInto(merge) => merge.actions.values().flatten().any(|action| {
            matches!(
                action.action_type,
                rustdb_ast::MergeActionType::MergeInsert | rustdb_ast::MergeActionType::MergeDelete
            )
        }),
        _ => false,
    };
    own || plan.children.iter().any(needs_vacuum_lock)
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
