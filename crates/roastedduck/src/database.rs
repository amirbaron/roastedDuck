use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_ast::{SetScope, SqlStatement, TransactionType};
use roastedduck_catalog::{Catalog, Transaction, TransactionManager};
use roastedduck_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use roastedduck_execution::operators::{PhysicalResultCollector, QueryResultMemoryType};
use roastedduck_execution::{
    PhysicalRef, PlanContext, QueryExecution, ScalarEvaluator, create_physical_plan_with,
};
use roastedduck_function::cast::{CastFunctionSet, ValueCast};
use roastedduck_optimizer::Optimizer;
use roastedduck_planner::{
    ClientContext, ColumnBindingResolver, LogicalKind, LogicalOperator, PlannedStatement, Planner,
    PreparedStatementData,
};
use roastedduck_types::{LogicalType, Value};

use crate::attached::{AttachOptions, AttachedDatabase, DatabaseManager};
use crate::settings::Settings;
use crate::transaction::TransactionContext;
use crate::{Config, MaterializedResult};

mod active_query;
mod appender;
mod prepared_statement;
mod registration;
mod statement_preprocessor;

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
    path: roastedduck_common::ByteString,
    config: Config,
    pub(crate) transactions: Arc<TransactionManager>,
    /// Builtin functions and system objects (`system.main`); read by the binder.
    pub(crate) system_catalog: Arc<Catalog>,
    /// The database opened at start-up and every attached one.
    pub(crate) databases: DatabaseManager,
    /// The name of the database opened at start-up (the default database), which
    /// `ALTER DATABASE ... SET ALIAS TO` renames.
    default_database: std::sync::RwLock<Option<roastedduck_common::ByteString>>,
    /// The cast functions (DuckDB's `DBConfig::GetCastFunctions`); extensions replace
    /// the set when they register casts.
    casts: std::sync::RwLock<Arc<CastFunctionSet>>,
    /// Whether registered casts depend on the connection's settings (the ICU casts): the
    /// connections then bind casts for themselves.
    client_casts: std::sync::atomic::AtomicBool,
    /// The options extensions added (DuckDB's `DBConfig::extension_parameters`).
    extension_options: std::sync::RwLock<Vec<roastedduck_function::ExtensionOption>>,
    /// The global values of the extension options (DuckDB's global user settings),
    /// starting at their defaults.
    extension_globals: Mutex<CaseInsensitiveMap<Value>>,
    /// Options given at start-up that no option knows yet (DuckDB's
    /// `DBConfigOptions::unrecognized_options`); an extension option of that name takes
    /// the value when its extension loads.
    unrecognized_options: Mutex<Vec<(String, Value)>>,
    /// Buffer manager and thread count used by every query.
    pub(crate) env: Arc<roastedduck_execution::ClientEnv>,
    /// Set by `PRAGMA disable_checkpoint_on_shutdown`.
    pub(crate) shutdown_checkpoint_disabled: std::sync::atomic::AtomicBool,
    /// A commit that grows the WAL beyond this many bytes checkpoints
    /// (`checkpoint_threshold`, DuckDB's `checkpoint_wal_size`).
    pub(crate) checkpoint_wal_size: std::sync::atomic::AtomicU64,
    /// How checkpoints compress segments (`force_compression`, a global setting): applied
    /// to the storage of every attached database.
    compression: std::sync::Mutex<roastedduck_storage::compression::CompressionConfig>,
    /// Whether the instance can still be used (`DatabaseInstance::GetValidChecker`).
    pub(crate) validity: Arc<crate::attached::ValidChecker>,
    /// `wal_autocheckpoint_entries`: a commit that leaves this many WAL entries
    /// checkpoints (0: disabled).
    pub(crate) wal_autocheckpoint_entries: std::sync::atomic::AtomicU64,
    /// How checkpoints reuse metadata (`experimental_metadata_reuse`,
    /// `force_column_metadata_reuse`; global settings): applied to the storage of every
    /// attached database.
    checkpoint_config: std::sync::Mutex<roastedduck_storage::storage_manager::CheckpointConfig>,
    /// How large appends are written to database files before they commit
    /// (`enable_optimistic_write`, `write_buffer_row_group_count`,
    /// `write_buffer_row_group_memory_limit`; global settings): applied to the storage of
    /// every attached database.
    optimistic_write: std::sync::Mutex<roastedduck_storage::OptimisticWriteConfig>,
    /// How checkpoints shred VARIANT columns (`variant_minimum_shredding_size`,
    /// `force_variant_shredding`; global settings): applied to the storage of every
    /// attached database.
    variant_shredding: std::sync::Mutex<roastedduck_storage::table::VariantShreddingConfig>,
    /// The secrets (DuckDB's `DBConfig::secret_manager`).
    pub(crate) secrets: Arc<crate::secrets::SecretManager>,
    /// The number of open connections (DuckDB's `ConnectionManager`).
    connections: Arc<std::sync::atomic::AtomicU64>,
    /// The object cache (DuckDB's `DatabaseInstance::object_cache`).
    object_cache: Arc<roastedduck_function::ObjectCache>,
    /// The cache of external files' ranges (DuckDB's `DatabaseInstance::external_file_cache`).
    external_file_cache: Arc<roastedduck_function::external_file_cache::ExternalFileCache>,
    /// The log manager (DuckDB's `DatabaseInstance::GetLogManager`).
    pub(crate) log_manager: Arc<crate::logging::LogManager>,
    /// The options set globally (DuckDB's `DBConfig` user settings), with a version
    /// that changes on every global `SET` / `RESET`; connections apply them unless they
    /// set the option themselves.
    global_settings: Mutex<GlobalSettings>,
    /// The statically linked extensions whose catalog entries are registered.
    loaded_extensions: Arc<Mutex<Vec<StaticExtension>>>,
    external_extensions: Arc<crate::external_extensions::ExternalExtensionState>,
    /// The replacement scans clients added (DuckDB's `DBConfig::replacement_scans` after
    /// the built-in ones).
    replacement_scans: std::sync::RwLock<Vec<roastedduck_planner::ReplacementScan>>,
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

impl roastedduck_planner::DatabaseState for InstanceState {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn autoload_extension(&self, name: &str) -> Result<bool> {
        let Some(extension) = StaticExtension::from_name(name) else {
            return Ok(false);
        };
        self.instance
            .upgrade()
            .ok_or_else(|| Error::internal("Extension autoload after database shutdown"))?
            .load_static_extension(extension)?;
        Ok(true)
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

/// `BindExtensionFunction`: load the provider, resolve using the declared placeholder
/// arguments, then delegate to the real overload's bind callback.
pub(crate) fn bind_extension_overload(
    function: &mut roastedduck_function::ScalarFunction,
    input: &roastedduck_function::BindInput<'_>,
    extension: &str,
) -> Result<Option<Arc<dyn roastedduck_function::FunctionData>>> {
    if !roastedduck_function::extension_entries::can_autoload_extension(extension) {
        return Err(Error::binder(roastedduck_common::byte_format!(
            "Trying to call function \"{}\" which is present in extension \"{extension}\" - but the extension is not loaded and could not be auto-loaded",
            function.name
        )));
    }
    let context = input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("Extension overload bound without a client context"))?;
    let instance = instance_state(context)?
        .instance()
        .upgrade()
        .ok_or_else(|| Error::internal("Extension overload bound after database shutdown"))?;
    if let Some(extension) = StaticExtension::from_name(extension) {
        instance.load_static_extension(extension)?;
    }
    let entry = instance
        .system_catalog
        .get_entry(
            &Transaction::system(),
            roastedduck_catalog::CatalogType::ScalarFunction,
            "main",
            &function.name,
        )
        .ok_or_else(|| Error::internal("Extension scalar entry disappeared during loading"))?;
    let roastedduck_catalog::EntryKind::ScalarFunction(functions) = &entry.kind else {
        return Err(Error::internal("Extension entry is not a scalar function"));
    };
    let index = roastedduck_planner::function_binder::FunctionBinder::new(context)
        .bind_function(&function.name, &functions.functions, &function.arguments)?
        .map_err(|error| Error::internal(error.raw_message()))?;
    let resolved = functions.functions[index].clone();
    if resolved
        .bind
        .as_ref()
        .zip(function.bind.as_ref())
        .is_some_and(|(left, right)| Arc::ptr_eq(left, right))
    {
        return Err(Error::internal(
            "Extension loading did not replace its scalar placeholder",
        ));
    }
    *function = resolved;
    function
        .bind
        .clone()
        .map_or(Ok(None), |bind| bind(function, input))
}

/// The object cache key of the database's loaded extensions, read by `duckdb_extensions()`.
const LOADED_EXTENSIONS_KEY: &str = "roastedduck.loaded_extensions";

/// The object cache key of the extensions the client loaded itself, read by
/// `duckdb_extensions()`.
pub(crate) const CLIENT_EXTENSIONS_KEY: &str = "roastedduck.client_extensions";

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
pub(crate) fn static_extension_loaded(cache: &roastedduck_function::ObjectCache, name: &str) -> bool {
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
    Tpcds,
    Autocomplete,
    Icu,
}

impl StaticExtension {
    const ALL: [Self; 6] = [
        Self::Json,
        Self::Icu,
        Self::Tpch,
        Self::Tpcds,
        Self::Autocomplete,
        Self::Parquet,
    ];

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "parquet" => Some(Self::Parquet),
            "json" => Some(Self::Json),
            roastedduck_tpch::EXTENSION_NAME => Some(Self::Tpch),
            roastedduck_tpcds::EXTENSION_NAME => Some(Self::Tpcds),
            roastedduck_autocomplete::EXTENSION_NAME => Some(Self::Autocomplete),
            roastedduck_icu::EXTENSION_NAME => Some(Self::Icu),
            _ => None,
        }
    }

    /// The pragmas the extension registers (`ExtensionLoader::RegisterFunction` of a
    /// `PragmaFunction`).
    fn pragma_functions(self) -> Vec<roastedduck_function::PragmaFunctionSet> {
        use roastedduck_function::{PragmaFunction, PragmaFunctionSet};
        let function = match self {
            Self::Parquet => PragmaFunction::call(
                "add_parquet_key",
                vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
            ),
            Self::Json => {
                PragmaFunction::call("json_execute_serialized_sql", vec![LogicalType::VARCHAR]).with_query()
            }
            Self::Tpch => PragmaFunction::call("tpch", vec![LogicalType::BIGINT]).with_query(),
            Self::Tpcds => PragmaFunction::call("tpcds", vec![LogicalType::BIGINT]).with_query(),
            Self::Autocomplete | Self::Icu => return Vec::new(),
        };
        vec![PragmaFunctionSet::single(function)]
    }

    /// Whether the extension has a pragma `name` that stands for a query.
    fn has_pragma(self, name: impl AsRef<[u8]>) -> bool {
        let name = name.as_ref();
        match self {
            Self::Tpch => name.eq_ignore_ascii_case(b"tpch"),
            Self::Tpcds => name.eq_ignore_ascii_case(b"tpcds"),
            Self::Parquet | Self::Json | Self::Autocomplete | Self::Icu => false,
        }
    }

    /// The SQL the extension's pragma `name` stands for.
    fn pragma_query(self, name: impl AsRef<[u8]>, parameters: &[Value]) -> Result<String> {
        let name = name.as_ref();
        match self {
            Self::Tpch => roastedduck_tpch::pragma_tpch_query(parameters),
            Self::Tpcds => roastedduck_tpcds::pragma_tpcds_query(parameters),
            Self::Parquet | Self::Json | Self::Autocomplete | Self::Icu => Err(Error::internal(
                roastedduck_common::byte_format!("no pragma {name}"),
            )),
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
                system.register_copy_function(roastedduck_parquet::copy::parquet_copy_function())?;
                for set in roastedduck_parquet::parquet_table_functions() {
                    system.register_table_functions(set)?;
                }
                for set in roastedduck_parquet::parquet_scalar_functions() {
                    system.register_scalar_functions(set)?;
                }
            }
            Self::Json => {
                for function in roastedduck_json::json_copy_functions() {
                    system.register_copy_function(function)?;
                }
                for set in roastedduck_json::json_scalar_functions() {
                    system.register_scalar_functions(set)?;
                }
                for set in roastedduck_json::json_table_functions() {
                    system.register_table_functions(set)?;
                }
                crate::system_functions::register_json_macros(system)?;
            }
            Self::Tpch => {
                for set in roastedduck_tpch::tpch_table_functions() {
                    system.register_table_functions(set)?;
                }
            }
            Self::Tpcds => {
                for set in roastedduck_tpcds::tpcds_table_functions() {
                    system.register_table_functions(set)?;
                }
            }
            Self::Autocomplete => {
                let loaded = Arc::clone(loaded);
                let pragma_names = Arc::new(move || pragma_function_names(&loaded));
                for set in roastedduck_autocomplete::autocomplete_table_functions(pragma_names) {
                    system.register_table_functions(set)?;
                }
            }
            Self::Icu => {
                use roastedduck_icu::IcuExtension;
                for set in IcuExtension::scalar_functions() {
                    system.register_scalar_functions_merged(set)?;
                }
                for set in IcuExtension::table_functions() {
                    system.register_table_functions_merged(set)?;
                }
                for name in roastedduck_icu::PATCHED_FUNCTIONS {
                    system.alter_builtin_scalar_functions(name, IcuExtension::patch_function)?;
                }
                for (name, collation) in IcuExtension::collations() {
                    system.register_collation(
                        &name,
                        roastedduck_catalog::CollationEntry {
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
    /// None records an explicit RESET, including a reset of constructor options.
    values: Vec<(&'static str, Option<Value>)>,
}

/// The default of `checkpoint_threshold` (16 MiB).
const DEFAULT_CHECKPOINT_WAL_SIZE: u64 = 16 * 1024 * 1024;

/// `ForceVariantShredding::SetGlobal`: the shredded type of a `force_variant_shredding`
/// value, a type name whose every level is wrapped in `STRUCT(typed_value,
/// untyped_value_index)`.
fn parse_force_variant_shredding(value: &Value) -> Result<LogicalType> {
    use roastedduck_types::LogicalTypeId as L;
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
        let columns = roastedduck_parser::parse_column_list(
            &format!("dummy {text}"),
            &roastedduck_parser::ParserOptions::default(),
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
            Some(roastedduck_types::TypeInfo::Unbound(unbound)) => unbound.default_bind()?,
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
    Ok(roastedduck_storage::statistics::wrap_shredded_levels(&bound))
}

/// The type checks of `ForceVariantShredding::SetGlobal`.
fn check_shredding_type(ty: &LogicalType) -> Result<()> {
    use roastedduck_types::LogicalTypeId as L;
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
fn is_force_compression(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    name.eq_ignore_ascii_case(b"force_compression")
}

/// `ForceCompressionSetting::OnSet`: the codec named by `value` (`none` / `auto`: none).
fn parse_force_compression(
    value: &roastedduck_types::Value,
) -> Result<roastedduck_storage::compression::CompressionType> {
    use roastedduck_storage::compression::CompressionType;
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
            roastedduck_common::string_util::candidates_error_message(&candidates, &name, "Candidates")
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
fn is_secret_setting(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    [
        "secret_directory",
        "allow_persistent_secrets",
        "default_secret_storage",
    ]
    .iter()
    .any(|setting| setting.as_bytes().eq_ignore_ascii_case(name))
}

/// DuckDB's default `temp_directory`: `<database>.tmp`, or `.tmp` in memory.
fn default_temp_directory(path: impl AsRef<[u8]>) -> roastedduck_common::ByteString {
    let path = path.as_ref();
    if path.is_empty() {
        ".tmp".into()
    } else {
        roastedduck_common::byte_format!("{path}.tmp")
    }
}

/// Configure the explicitly opened main file and temporary directory as allowed
/// paths when external access starts disabled (`DatabaseInstance::Configure`).
fn add_startup_access_paths(config: &mut Config, path: &[u8]) -> Result<()> {
    use roastedduck_function::cast::ValueCast;
    let external = config
        .options
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case("enable_external_access"))
        .map(|(_, value)| value.default_cast_as(&LogicalType::BOOLEAN))
        .transpose()?
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if external || path.is_empty() {
        return Ok(());
    }
    let paths = ["", ".wal", ".wal.checkpoint", ".wal.recovery"]
        .into_iter()
        .map(|suffix| {
            roastedduck_function::file_system::sanitize_allowed_path_bytes(&roastedduck_common::byte_format!(
                "{path}{suffix}"
            ))
        })
        .collect();
    let temp = config
        .options
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case("temp_directory"))
        .map_or_else(
            || Ok(default_temp_directory(path)),
            |(_, value)| {
                if value.is_null() {
                    Ok(roastedduck_common::ByteString::new())
                } else {
                    value.to_varchar_bytes()
                }
            },
        )?;
    add_allowed_config_paths(config, "allowed_paths", paths);
    if !temp.is_empty() {
        let mut temp = roastedduck_function::file_system::sanitize_allowed_path_bytes(&temp);
        if !temp.ends_with(b"/") {
            temp.push(b'/');
        }
        add_allowed_config_paths(config, "allowed_directories", vec![temp]);
    }
    Ok(())
}

fn add_allowed_config_paths(config: &mut Config, name: &str, paths: Vec<roastedduck_common::ByteString>) {
    let mut values = config
        .options
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .and_then(|(_, value)| value.children())
        .unwrap_or_default()
        .to_vec();
    for path in paths {
        let value = Value::bytes(LogicalType::VARCHAR, &path);
        if !values.contains(&value) {
            values.push(value);
        }
    }
    config.options.retain(|(key, _)| key != name);
    config
        .options
        .push((name.to_owned(), Value::list(LogicalType::VARCHAR, values)));
}

/// Whether `name` is an option an extension adds rather than a built-in option (DuckDB
/// keeps such start-up options as `unrecognized_options` until the extension loads).
fn is_extension_setting(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    crate::settings::canonical_option(name).is_none()
        && roastedduck_function::extension_entries::find_extension(
            name,
            roastedduck_function::extension_entries::EXTENSION_SETTINGS,
        )
        .is_some()
}

/// Whether `ty` is or contains a type of id `id`.
fn contains_type(ty: &LogicalType, id: roastedduck_types::LogicalTypeId) -> bool {
    use roastedduck_types::LogicalTypeId as L;
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
fn is_threads_setting(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    name.eq_ignore_ascii_case(b"threads") || name.eq_ignore_ascii_case(b"worker_threads")
}

/// Whether `name` is `checkpoint_threshold` or its alias `wal_autocheckpoint`.
fn is_checkpoint_threshold(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    name.eq_ignore_ascii_case(b"checkpoint_threshold") || name.eq_ignore_ascii_case(b"wal_autocheckpoint")
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
            .get::<roastedduck_function::file_system::DisabledFileSystems>(
                roastedduck_function::file_system::DISABLED_FILE_SYSTEMS_KEY,
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
                self.load_parquet_config()?;
                // `ParquetExtension::Load` registers the ZSTD file system.
                self.object_cache.put(
                    roastedduck_function::file_system::ZSTD_FILE_SYSTEM_KEY.to_owned(),
                    Arc::new(()),
                );
            }
            loaded.push(extension);
        }
        Ok(())
    }

    /// `ParquetExtension::LoadInternal`: real boolean extension settings, with
    /// session defaults for SET and global initial/default values.
    fn load_parquet_config(&self) -> Result<()> {
        for name in crate::settings::PARQUET_OPTION_NAMES {
            let definition = crate::default_settings::SETTINGS
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| Error::internal(format!("Missing Parquet setting metadata: {name}")))?;
            self.add_extension_option(roastedduck_function::ExtensionOption {
                name: name.to_owned(),
                description: definition.description.to_owned(),
                input_type: LogicalType::BOOLEAN,
                default_value: Value::boolean(name == "enable_geoparquet_conversion"),
                set_function: None,
                global_by_default: false,
            });
        }
        Ok(())
    }

    /// The configuration parts of `IcuExtension::Load`: the casts and the `TimeZone` and
    /// `Calendar` options.
    fn load_icu_config(&self) {
        let mut casts = (*self.casts()).clone();
        roastedduck_icu::IcuExtension::register_casts(&mut casts);
        *self.casts.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(casts);
        self.client_casts
            .store(true, std::sync::atomic::Ordering::Release);
        for option in roastedduck_icu::IcuExtension::options() {
            self.add_extension_option(option);
        }
    }

    /// The collations extensions registered in the system catalog, with whether they are
    /// combinable.
    pub(crate) fn extension_collations(&self) -> Vec<(roastedduck_common::ByteString, bool)> {
        let Some(schema) = self.system_catalog.schema(
            &roastedduck_catalog::Transaction::system(),
            roastedduck_catalog::DEFAULT_SCHEMA,
        ) else {
            return Vec::new();
        };
        let Some(schema) = schema.as_schema() else {
            return Vec::new();
        };
        schema
            .collations
            .scan(&roastedduck_catalog::Transaction::system())
            .iter()
            .filter_map(|entry| match &entry.kind {
                roastedduck_catalog::EntryKind::Collation(collation) => {
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
    fn add_extension_option(&self, option: roastedduck_function::ExtensionOption) {
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
    pub(crate) fn extension_option(
        &self,
        name: impl AsRef<[u8]>,
    ) -> Option<roastedduck_function::ExtensionOption> {
        let name = name.as_ref();
        self.extension_options
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|option| option.name.as_bytes().eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Every extension option with its global value.
    pub(crate) fn extension_option_values(&self) -> Vec<(roastedduck_function::ExtensionOption, Value)> {
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
        option: &roastedduck_function::ExtensionOption,
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
    fn extension_with_pragma(&self, name: impl AsRef<[u8]>) -> Option<StaticExtension> {
        let name = name.as_ref();
        self.loaded_extensions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .find(|extension| extension.has_pragma(name))
    }

    /// Whether a loaded extension has a pragma `name`.
    fn has_extension_pragma(&self, name: impl AsRef<[u8]>) -> bool {
        let name = name.as_ref();
        self.extension_with_pragma(name).is_some()
    }

    /// The SQL the pragma `name` of a loaded extension stands for (a
    /// `PragmaFunction::PragmaCall` with a query).
    fn extension_pragma_query(&self, name: impl AsRef<[u8]>, parameters: &[Value]) -> Result<String> {
        let name = name.as_ref();
        self.extension_with_pragma(name)
            .ok_or_else(|| {
                Error::internal(roastedduck_common::byte_format!(
                    "no extension has the pragma {name}"
                ))
            })?
            .pragma_query(name, parameters)
    }

    /// `SET threads` (`ThreadsSetting::SetGlobal`), or the default thread count for
    /// `RESET threads` (`None`).
    fn set_threads(&self, value: Option<&roastedduck_types::Value>) -> Result<()> {
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
            None => roastedduck_execution::default_threads(),
        };
        self.env.scheduler.set_threads(threads);
        Ok(())
    }

    /// `SET`/`RESET` (`None`) of the buffer manager's options: `memory_limit` (alias
    /// `max_memory`), `temp_directory`, `max_temp_directory_size`,
    /// `block_allocator_memory` and `enable_external_file_cache` (the buffer-managed file
    /// cache); other names are ignored.
    fn set_buffer_setting(&self, name: &str, value: Option<&roastedduck_types::Value>) -> Result<()> {
        let buffer_manager = &self.env.buffer_manager;
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "enable_external_file_cache" => {
                // `EnableExternalFileCacheSetting::OnSet`.
                let enable = match value {
                    Some(value) => ValueCast::default_cast_as(value, &LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(true),
                    None => true,
                };
                self.external_file_cache.set_enabled(enable);
                Ok(())
            }
            "memory_limit" | "max_memory" => {
                let limit = match value {
                    Some(value) => roastedduck_buffer::parse_memory_limit(&value.to_varchar()?)?,
                    None => roastedduck_buffer::default_memory_limit(),
                };
                buffer_manager.set_memory_limit(limit)
            }
            "temp_directory" => {
                if !roastedduck_function::file_system::external_access_enabled() {
                    return Err(Error::new(
                        roastedduck_common::ErrorKind::Permission,
                        "Modifying the temp_directory has been disabled by configuration",
                    ));
                }
                let directory = match value {
                    Some(value) if value.is_null() => roastedduck_common::ByteString::new(),
                    Some(value) => value.to_varchar_bytes()?,
                    None => default_temp_directory(&self.path),
                };
                buffer_manager.set_temporary_directory(&directory)
            }
            "max_temp_directory_size" => {
                let limit = value
                    .map(|value| roastedduck_buffer::parse_memory_limit(&value.to_varchar()?))
                    .transpose()?;
                buffer_manager.set_swap_limit(limit)
            }
            "temp_file_encryption" => {
                // `TempFileEncryptionSetting::OnSet` (`RESET` has no check): temporary
                // files keep their format.
                if value.is_some() && buffer_manager.has_files_in_temporary_directory() {
                    return Err(Error::new(
                        roastedduck_common::ErrorKind::Permission,
                        "Existing temporary files found: Modifying the temp_file_encryption setting while there \
                         are existing temporary files is disabled.",
                    ));
                }
                let enabled = match value {
                    Some(value) => ValueCast::default_cast_as(value, &LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                    None => false,
                };
                buffer_manager.set_temp_file_encryption(enabled);
                Ok(())
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
    fn set_variant_shredding_setting(
        &self,
        name: &str,
        value: Option<&roastedduck_types::Value>,
    ) -> Result<()> {
        use roastedduck_storage::table::VariantShreddingConfig;
        let update: Box<dyn FnOnce(&mut VariantShreddingConfig)> =
            if name.eq_ignore_ascii_case("variant_minimum_shredding_size") {
                let rows = match value {
                    Some(value) => value
                        .default_cast_as(&roastedduck_types::LogicalType::BIGINT)?
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
    fn variant_shredding_config(&self) -> roastedduck_storage::table::VariantShreddingConfig {
        self.variant_shredding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `SET` / `RESET` (`None`) of `wal_autocheckpoint_entries`; other names are ignored.
    fn set_wal_autocheckpoint_entries(
        &self,
        name: &str,
        value: Option<&roastedduck_types::Value>,
    ) -> Result<()> {
        if !name.eq_ignore_ascii_case("wal_autocheckpoint_entries") {
            return Ok(());
        }
        let entries = match value {
            Some(value) => {
                use roastedduck_function::cast::ValueCast;
                value
                    .default_cast_as(&roastedduck_types::LogicalType::UBIGINT)?
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
    fn set_checkpoint_threshold(&self, value: &roastedduck_types::Value) -> Result<()> {
        let limit = roastedduck_buffer::parse_memory_limit(&value.to_varchar()?)?;
        self.checkpoint_wal_size
            .store(limit, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// `SET force_compression` / `RESET force_compression` (`None`): stores the setting and
    /// hands it to the storage of every attached database.
    fn set_force_compression(&self, value: Option<&roastedduck_types::Value>) -> Result<()> {
        let forced = match value {
            Some(value) => parse_force_compression(value)?,
            None => roastedduck_storage::compression::CompressionType::Auto,
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
        value: Option<&roastedduck_types::Value>,
    ) -> Result<()> {
        use roastedduck_storage::compression::{CompressionConfig, CompressionType};
        if name.eq_ignore_ascii_case("force_bitpacking_mode") {
            let mode = match value {
                Some(value) => {
                    let text = value.to_varchar()?;
                    CompressionConfig::bitpacking_mode_from_name(&text).ok_or_else(|| {
                        roastedduck_function::logging::enum_error(
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
                        roastedduck_common::string_util::candidates_error_message(
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
    fn set_checkpoint_setting(&self, name: &str, value: Option<&roastedduck_types::Value>) -> Result<()> {
        use roastedduck_function::cast::ValueCast;
        use roastedduck_storage::storage_manager::{CheckpointAbort, CheckpointConfig};
        let lower = name.to_ascii_lowercase();
        let update: Box<dyn Fn(&mut CheckpointConfig)> = match (lower.as_str(), value) {
            ("experimental_metadata_reuse" | "force_column_metadata_reuse", value) => {
                let enabled = match value {
                    Some(value) => value
                        .default_cast_as(&roastedduck_types::LogicalType::BOOLEAN)?
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
                        .default_cast_as(&roastedduck_types::LogicalType::UBIGINT)?
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
                        .default_cast_as(&roastedduck_types::LogicalType::UBIGINT)?
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
                        .default_cast_as(&roastedduck_types::LogicalType::BIGINT)?
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
                    roastedduck_function::logging::enum_error(&text, "CheckpointAbort", &names)
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
    fn set_optimistic_write_setting(
        &self,
        name: &str,
        value: Option<&roastedduck_types::Value>,
    ) -> Result<()> {
        use roastedduck_function::cast::ValueCast;
        let lower = name.to_ascii_lowercase();
        let defaults = roastedduck_storage::OptimisticWriteConfig::default();
        let update: Box<dyn Fn(&mut roastedduck_storage::OptimisticWriteConfig)> = match lower.as_str() {
            "enable_optimistic_write" => {
                let enabled = match value {
                    Some(value) => value
                        .default_cast_as(&roastedduck_types::LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                    None => defaults.enabled,
                };
                Box::new(move |config| config.enabled = enabled)
            }
            "write_buffer_row_group_count" => {
                let count = match value {
                    Some(value) => value
                        .default_cast_as(&roastedduck_types::LogicalType::UBIGINT)?
                        .as_u64()
                        .unwrap_or(0),
                    None => defaults.row_group_count,
                };
                Box::new(move |config| config.row_group_count = count)
            }
            "write_buffer_row_group_memory_limit" => {
                let limit = value
                    .map(|value| roastedduck_buffer::parse_memory_limit(&value.to_varchar()?))
                    .transpose()?;
                Box::new(move |config| config.memory_limit = limit)
            }
            "geometry_minimum_shredding_size" => {
                let rows = match value {
                    Some(value) => value
                        .default_cast_as(&roastedduck_types::LogicalType::BIGINT)?
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
    fn optimistic_write_config(&self) -> roastedduck_storage::OptimisticWriteConfig {
        *self
            .optimistic_write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How checkpoints reuse metadata.
    fn checkpoint_config(&self) -> roastedduck_storage::storage_manager::CheckpointConfig {
        *self
            .checkpoint_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The compression settings checkpoints use.
    fn compression_config(&self) -> roastedduck_storage::compression::CompressionConfig {
        *self
            .compression
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `ALTER DATABASE` (`DatabaseManager::Alter`): `SET ALIAS TO` renames an attached
    /// database (`RenameDatabase`), and the default database follows its name.
    fn alter_database(&self, info: &roastedduck_ast::AlterInfo) -> Result<()> {
        let roastedduck_ast::AlterInfoKind::AlterDatabase(
            roastedduck_ast::AlterDatabaseInfo::RenameDatabase(rename),
        ) = &info.kind
        else {
            return Err(Error::internal("Unsupported ALTER DATABASE operation"));
        };
        let (name, new_name) = (&info.catalog, &rename.new_name);
        if !self.databases.rename(name, new_name)? {
            if info.if_not_found == roastedduck_ast::OnEntryNotFound::ThrowException {
                return Err(Error::binder(roastedduck_common::byte_format!(
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
    pub(crate) fn default_database_name(&self) -> roastedduck_common::ByteString {
        self.default_database
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| "memory".into())
    }

    /// The attached database called `name`.
    pub(crate) fn database(&self, name: impl AsRef<[u8]>) -> Result<Arc<AttachedDatabase>> {
        let name = name.as_ref();
        self.databases.get(name).ok_or_else(|| {
            Error::binder(roastedduck_common::byte_format!(
                "Catalog \"{name}\" does not exist!"
            ))
        })
    }

    /// The catalogs the binder sees: `default` first, the other attached databases,
    /// then `system`.
    pub(crate) fn catalogs(&self, default: &[u8]) -> Vec<Arc<Catalog>> {
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
    /// other transaction runs (DuckDB cannot vacuum then; roastedduck does not compress then
    /// either).
    fn auto_checkpoint(
        &self,
        name: &[u8],
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
                || self.transactions.retains_changes(u64::MAX, database.name())
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

    /// The checkpoint the commit of `transaction` runs instead of logging its `changes`
    /// (`DuckTransactionManager::CommitTransaction` with
    /// `skip_wal_write_due_to_checkpoint`): the commit checkpoints (`CanCheckpoint`: the
    /// WAL of the database it wrote to would grow beyond `checkpoint_threshold` or holds
    /// `wal_autocheckpoint_entries` entries, and the transaction gets the checkpoint lock
    /// exclusively) and its estimated size is at least `skip_wal_threshold`
    /// (`auto_checkpoint_skip_wal_threshold`). The commit then holds the WAL lock until
    /// the checkpoint ends, and a failing checkpoint fails the commit (`error.Merge`).
    /// Without, the commit is logged and [`DatabaseInstance::auto_checkpoint`] decides
    /// after it.
    fn skip_wal_checkpoint(
        &self,
        transaction: &Transaction,
        changes: &CommittedChanges,
        skip_checkpoint: bool,
        skip_wal_threshold: u64,
    ) -> Option<roastedduck_storage::transaction::CommitCheckpoint<'static>> {
        if skip_checkpoint || changes.properties.estimated_size < skip_wal_threshold {
            return None;
        }
        let database = self.databases.get(&changes.database)?;
        if roastedduck_catalog::checkpoint::has_pending_index_replays(&database.catalog) {
            return None;
        }
        let storage = database.storage.file()?;
        if storage.is_read_only() {
            return None;
        }
        let wal = storage.wal()?;
        {
            // `SingleFileStorageManager::AutomaticCheckpoint`; a database that does not
            // log commits (`NO_WAL_WRITES`) has nothing to skip.
            let guard = wal.lock();
            let threshold = self
                .checkpoint_wal_size
                .load(std::sync::atomic::Ordering::Relaxed);
            let entry_limit = self
                .wal_autocheckpoint_entries
                .load(std::sync::atomic::Ordering::Relaxed);
            if guard.no_writes()
                || (guard.size() + changes.properties.estimated_size <= threshold
                    && !(entry_limit > 0 && guard.entries() >= entry_limit))
            {
                return None;
            }
        }
        // Committed transactions that updated rows or changed the catalog hold the
        // checkpoint lock while older transactions need their changes
        // (`recently_committed_transactions`).
        if self
            .transactions
            .retains_changes(transaction.transaction_id, database.name())
        {
            return None;
        }
        let checkpoint = transaction.try_checkpoint_lock(database.catalog.locks())?;
        let transactions = Arc::clone(&self.transactions);
        Some(Box::new(move |wal| {
            database.checkpoint_after_commit(&transactions, checkpoint, wal)
        }))
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
    _metrics: roastedduck_common::profiling::ActiveMetricsGuard,
    _opener: roastedduck_function::file_system::OpenerGuard,
}

/// A database instance (DuckDB's `DuckDB`). Cheap to clone; clones share the instance.
#[derive(Clone, Debug)]
pub struct Database {
    instance: Arc<DatabaseInstance>,
}

/// DuckDB's default casts plus those of the statically loaded json extension.
fn json_casts() -> CastFunctionSet {
    let mut casts = CastFunctionSet::new();
    roastedduck_json::register_json_casts(&mut casts);
    casts
}

impl Database {
    /// Opens (or creates) the database at `path`; `""` or `":memory:"` is in-memory.
    pub fn open(path: impl AsRef<[u8]>, config: &Config) -> Result<Self> {
        Self::open_configured(path.as_ref(), config, &[], &[])
    }

    /// Reopens using the configuration and registered extension options retained
    /// from a database, independently from any connection's session overrides.
    pub fn open_from_snapshot(
        path: impl AsRef<[u8]>,
        snapshot: &crate::ConfigurationSnapshot,
    ) -> Result<Self> {
        let snapshot = snapshot.prepare()?;
        Self::open_configured(
            path.as_ref(),
            &snapshot.config,
            &snapshot.extensions,
            &snapshot.database_options,
        )
    }

    fn open_configured(
        path: &[u8],
        config: &Config,
        extensions: &[(roastedduck_function::ExtensionOption, Value)],
        database_options: &[(String, Value)],
    ) -> Result<Self> {
        config.check_recognized()?;
        let mut config = config.clone();
        config.options.extend_from_slice(database_options);
        let path = if path == b":memory:" {
            roastedduck_common::ByteString::new()
        } else {
            roastedduck_common::ByteString::from(path)
        };
        add_startup_access_paths(&mut config, &path)?;
        let config = &config;
        let system_catalog = Catalog::new("system")?;
        let functions = crate::function_registration::StartupFunctions::new()?;
        for set in roastedduck_functions::builtin_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::checkpoint::checkpoint_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::query_profiler::profiling_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::system_functions::catalog_table_functions() {
            system_catalog.register_table_functions(set)?;
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
        for set in roastedduck_arrow::arrow_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        system_catalog.register_copy_function(roastedduck_csv::copy_csv::csv_copy_function())?;
        system_catalog.register_copy_function(roastedduck_csv::copy_blob::blob_copy_function())?;
        for set in roastedduck_csv::multi_file::glob_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in roastedduck_csv::sniff_csv::sniff_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in roastedduck_csv::read_csv::read_csv_functions() {
            system_catalog.register_table_functions(set)?;
        }
        system_catalog.register_table_functions(crate::read_duckdb::read_duckdb_functions())?;
        crate::function_registration::register_builtin_collations(&system_catalog)?;
        crate::function_registration::register_extension_overloads(&system_catalog)?;
        let core_functions = functions.register_builtin(&system_catalog)?;
        crate::system_functions::register_default_views(&system_catalog)?;
        crate::system_functions::register_default_macros(&system_catalog)?;
        let buffer_manager =
            roastedduck_buffer::BufferManager::new(roastedduck_buffer::BufferManagerConfig {
                temp_directory: Some(roastedduck_common::native_file_path(&default_temp_directory(
                    &path,
                ))?),
                ..roastedduck_buffer::BufferManagerConfig::default()
            })?;
        let env = Arc::new(roastedduck_execution::ClientEnv::new(buffer_manager));
        let threads = env.threads();
        let log_manager = Arc::new(crate::logging::LogManager::default());
        // `enable_external_file_cache` among the options turns it off below.
        let external_file_cache =
            Arc::new(roastedduck_function::external_file_cache::ExternalFileCache::new(
                env.buffer_manager.clone(),
                true,
            ));
        let oid_allocator = system_catalog.oid_allocator();
        let instance = DatabaseInstance {
            path: path.clone(),
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            databases: DatabaseManager::new(Arc::clone(&log_manager), oid_allocator),
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
            optimistic_write: std::sync::Mutex::new(roastedduck_storage::OptimisticWriteConfig {
                threads: threads as u64,
                ..roastedduck_storage::OptimisticWriteConfig::default()
            }),
            variant_shredding: std::sync::Mutex::default(),
            validity: Arc::default(),
            wal_autocheckpoint_entries: std::sync::atomic::AtomicU64::new(0),
            secrets: Arc::default(),
            connections: Arc::default(),
            object_cache: Arc::default(),
            external_file_cache,
            log_manager,
            global_settings: Mutex::default(),
            loaded_extensions: Arc::default(),
            external_extensions: Arc::default(),
            replacement_scans: std::sync::RwLock::default(),
        };
        for (option, value) in extensions {
            instance.add_extension_option(option.clone());
            instance.set_extension_global(option, Some(value.clone()));
        }
        instance.object_cache.put(
            crate::external_extensions::EXTERNAL_EXTENSIONS_KEY.to_owned(),
            Arc::clone(&instance.external_extensions),
        );
        instance.object_cache.put(
            LOADED_EXTENSIONS_KEY.to_owned(),
            Arc::clone(&instance.loaded_extensions),
        );
        instance.object_cache.put(
            roastedduck_function::file_system::DISABLED_FILE_SYSTEMS_KEY.to_owned(),
            Arc::new(roastedduck_function::file_system::DisabledFileSystems::default()),
        );
        for (name, value) in &config.options {
            if is_secret_setting(name) {
                instance.secrets.set_option(name, Some(value))?;
            }
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
        database.load_extension_settings()?;
        {
            let mut connection = database.connect()?;
            connection.transactions.begin(false)?;
            let _scope = connection.query_scope();
            let options = crate::storage::startup_options(config);
            // A data file opens as an in-memory database with views on it, as with `ATTACH`
            // (`DatabaseManager::GetDatabaseType` resolving to `__open_file__`).
            let main = if crate::attached::is_open_file(&path)? {
                database.attach_open_file(
                    "",
                    &path,
                    &options,
                    roastedduck_ast::OnCreateConflict::ErrorOnConflict,
                )?
            } else {
                database.attach(
                    "",
                    &path,
                    &options,
                    roastedduck_ast::OnCreateConflict::ErrorOnConflict,
                )?
            };
            if let Some(main) = main {
                main.is_initial.store(true, std::sync::atomic::Ordering::Release);
                *database
                    .instance
                    .default_database
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((&*main.name()).into());
            }
            connection.transactions.commit(None)?;
        }
        let mut core_functions = Some(core_functions);
        if config.load_extensions {
            for extension in StaticExtension::ALL {
                // The frozen generated loader registers core_functions after
                // autocomplete and before parquet, including ICU's overloads first.
                if extension == StaticExtension::Parquet
                    && let Some(functions) = core_functions.take()
                {
                    functions.register(&database.instance.system_catalog)?;
                }
                database.instance.load_static_extension(extension)?;
            }
        }
        // Retain the SDK's existing always-available core function contract when
        // ordinary extension loading is disabled.
        if let Some(functions) = core_functions {
            functions.register(&database.instance.system_catalog)?;
        }
        Ok(database)
    }

    /// Validate configured extension options through the real connection callbacks
    /// before creating main storage (`DatabaseInstance::LoadExtensionSettings`).
    fn load_extension_settings(&self) -> Result<()> {
        use roastedduck_function::cast::ValueCast;

        let options: Vec<_> = self
            .instance
            .config
            .options
            .iter()
            .filter(|(name, _)| is_extension_setting(name) && self.instance.extension_option(name).is_none())
            .collect();
        if options.is_empty() {
            return Ok(());
        }
        let mut order = roastedduck_common::libcxx::UnorderedOrder::default();
        for (index, (name, _)) in options.iter().enumerate() {
            order.insert(roastedduck_common::libcxx::ci_hash(name), index, |a, b| {
                options[*a].0.eq_ignore_ascii_case(&options[*b].0)
            });
        }
        // Configure copy-assigns DBConfigOptions into the empty instance config.
        let configured = order.copy_assigned_to_empty();
        let autoload = self
            .instance
            .config
            .options
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("autoload_known_extensions"))
            .map_or(Ok(true), |(_, value)| {
                Ok::<_, Error>(
                    value
                        .default_cast_as(&LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                )
            })?;
        if !autoload {
            let names: Vec<_> = configured
                .order()
                .into_iter()
                .map(|index| options[configured.keys()[index]].0.as_str())
                .collect();
            return Err(Error::invalid_input(format!(
                "The following options were not recognized: {}",
                names.join(", ")
            )));
        }
        // LoadExtensionSettings copy-constructs the map it traverses, protecting
        // iteration from extensions removing recognized options during loading.
        let snapshot = configured.copy_constructed(|a, b| options[*a].0.eq_ignore_ascii_case(&options[*b].0));
        let mut connection = self.connect()?;
        connection.transactions.begin(false)?;
        for index in snapshot.order() {
            let (name, value) = options[snapshot.keys()[index]];
            let extension = roastedduck_function::extension_entries::find_extension(
                name,
                roastedduck_function::extension_entries::EXTENSION_SETTINGS,
            )
            .ok_or_else(|| Error::internal(format!("Missing extension setting provider: {name}")))?;
            let loaded = StaticExtension::from_name(extension)
                .is_some_and(|extension| self.instance.load_static_extension(extension).is_ok());
            if !loaded {
                return Err(Error::invalid_input(format!(
                    "To set the {name} setting, the {extension} extension needs to be loaded. But it could not be autoloaded."
                )));
            }
            connection.set_extension_variable(name, value, SetScope::Global)?;
        }
        connection.transactions.commit(None)?;
        Ok(())
    }

    /// Attaches the database at `path` as `name` and replays its WAL
    /// (`DatabaseManager::AttachDatabase`).
    pub(crate) fn attach(
        &self,
        name: impl AsRef<[u8]>,
        path: impl AsRef<[u8]>,
        options: &AttachOptions,
        on_conflict: roastedduck_ast::OnCreateConflict,
    ) -> Result<Option<Arc<AttachedDatabase>>> {
        let path = path.as_ref();
        let name = name.as_ref();
        // Remote files use filesystem URI canonicalization and automatic read-only
        // access, as DatabaseManager::AttachDatabase does before tracking paths.
        let mut options = options.clone();
        let remote = roastedduck_function::file_system::is_remote_file(path);
        if remote && options.access == crate::AccessMode::Automatic {
            options.access = crate::AccessMode::ReadOnly;
        }
        let canonical_path;
        let path = if path.is_empty() || path == b":memory:" {
            path
        } else {
            canonical_path = if remote {
                roastedduck_function::file_system::canonicalize_path_bytes(path)
            } else {
                roastedduck_function::file_system::canonicalize_local_path_bytes(path)
            };
            canonical_path.as_bytes()
        };
        if !path.is_empty() && path != b":memory:" {
            roastedduck_function::file_system::verify_can_access_file(path)?;
            if !options.read_only() {
                roastedduck_function::file_system::verify_can_write(path)?;
            }
        }
        // Existing files retain their storage version and block allocation size.
        if !path.is_empty()
            && path != b":memory:"
            && !roastedduck_storage::file_system::DatabaseFileSystem::for_path(path).exists(path)?
        {
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
        // The `VACUUM_REBUILD_INDEXES` option, else the setting the database started with.
        let vacuum_rebuild_indexes = match options.vacuum_rebuild_indexes {
            Some(threshold) => threshold,
            None => self.startup_vacuum_rebuild_indexes()?,
        };
        attached
            .storage
            .set_vacuum_rebuild_index_threshold(vacuum_rebuild_indexes, &attached.catalog.locks().vacuum);
        attached
            .storage
            .set_optimistic_write_config(self.instance.optimistic_write_config());
        attached
            .storage
            .set_variant_shredding_config(self.instance.variant_shredding_config());
        if let Err(error) = crate::wal::replay(self, &attached) {
            self.instance.databases.remove(attached.name());
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
        name: impl AsRef<[u8]>,
        path: impl AsRef<[u8]>,
        options: &AttachOptions,
        on_conflict: roastedduck_ast::OnCreateConflict,
    ) -> Result<Option<Arc<AttachedDatabase>>> {
        let path = path.as_ref();
        let name = name.as_ref();
        // Its path is canonical, as the database type is only known after
        // `RequiresTrackingAttaches` canonicalized it.
        let remote = roastedduck_function::file_system::is_remote_file(path);
        let path = &if remote {
            roastedduck_function::file_system::canonicalize_path_bytes(path)
        } else {
            roastedduck_function::file_system::canonicalize_local_path_bytes(path)
        };
        roastedduck_function::file_system::verify_can_access_file(path)?;
        let mut options = options.clone();
        if remote && options.access == crate::AccessMode::Automatic {
            options.access = crate::AccessMode::ReadOnly;
        }
        let options = &options;
        let name = if name.is_empty() {
            crate::storage::database_name(path)
        } else {
            roastedduck_common::ByteString::from(name)
        };
        let Some(attached) = self.instance.databases.attach(
            &name,
            b"",
            options,
            on_conflict,
            &self.instance.env.buffer_manager,
            &self.instance.validity,
        )?
        else {
            return Ok(None);
        };
        let sql = roastedduck_common::byte_format!(
            "SELECT * FROM {}",
            roastedduck_ast::keyword::sql_string_bytes(path)
        );
        let mut view_names = vec![roastedduck_common::ByteString::from("file")];
        if !name.eq_ignore_ascii_case("file") {
            view_names.push(name);
        }
        for view_name in view_names {
            let Some(roastedduck_ast::SqlStatementKind::Select(query)) =
                roastedduck_parser::parse_bytes(&sql)?
                    .pop()
                    .map(|statement| statement.kind)
            else {
                return Err(Error::internal("open file view is not a SELECT"));
            };
            let view = roastedduck_catalog::ViewEntry {
                query,
                aliases: Vec::new(),
                types: Vec::new(),
                names: Vec::new(),
                column_comments: roastedduck_common::OrderedMap::default(),
            };
            attached.catalog.create_entry_committed(
                roastedduck_catalog::DEFAULT_SCHEMA,
                attached
                    .catalog
                    .new_entry(view_name, roastedduck_catalog::EntryKind::View(view)),
            )?;
        }
        Ok(Some(attached))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open("", &Config::default())
    }

    /// The database file path, or `""` for in-memory databases.
    pub fn path(&self) -> &[u8] {
        &self.instance.path
    }

    /// The value a global `SET` gave option `name` (canonical name), if any (the
    /// instance's `DBConfig` options).
    /// `VacuumRebuildIndexesSetting`, which can only be set when the database starts.
    fn startup_vacuum_rebuild_indexes(&self) -> Result<u64> {
        use roastedduck_function::cast::ValueCast;
        let Some((_, value)) = self
            .instance
            .config
            .options
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("vacuum_rebuild_indexes"))
        else {
            return Ok(0);
        };
        Ok(value
            .default_cast_as(&LogicalType::UBIGINT)?
            .as_u64()
            .unwrap_or(0))
    }

    pub fn global_setting(&self, name: &str) -> Option<Value> {
        self.instance
            .global_settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values
            .iter()
            .find(|(option, _)| option.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.clone())
    }

    pub fn config(&self) -> &Config {
        &self.instance.config
    }

    /// The live global configuration, including registered option definitions.
    pub fn configuration_snapshot(&self) -> crate::ConfigurationSnapshot {
        let mut config = self.instance.config.clone();
        let mut database_options = Vec::new();
        config.options.retain(|(name, value)| {
            let Some(option) = crate::settings::canonical_option(name) else {
                return true;
            };
            if crate::config::retained_database_option(option) {
                database_options.push((option.to_owned(), value.clone()));
            }
            !crate::config::requires_database(option)
        });
        for (name, value) in &self
            .instance
            .global_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values
        {
            if crate::config::requires_database(name) {
                database_options.retain(|(existing, _)| existing != name);
                if crate::config::retained_database_option(name)
                    && let Some(value) = value
                {
                    database_options.push(((*name).to_owned(), value.clone()));
                }
                continue;
            }
            config
                .options
                .retain(|(existing, _)| crate::settings::canonical_option(existing) != Some(*name));
            if let Some(value) = value {
                config.options.push(((*name).to_owned(), value.clone()));
            }
        }
        let extensions = self.instance.extension_option_values();
        config.options.retain(|(name, _)| {
            !extensions
                .iter()
                .any(|(option, _)| option.name.eq_ignore_ascii_case(name))
        });
        config.unrecognized_options.retain(|(name, _)| {
            !extensions
                .iter()
                .any(|(option, _)| option.name.eq_ignore_ascii_case(name))
        });
        crate::ConfigurationSnapshot {
            config,
            extensions,
            database_options,
        }
    }

    pub fn connect(&self) -> Result<Connection> {
        let mut settings = Settings::default();
        for (name, value) in &self.instance.config.options {
            if is_extension_setting(name) {
                continue;
            }
            settings.set(name, value)?;
        }
        let function_client = Arc::new(roastedduck_function::FunctionClient::default());
        let transactions = TransactionContext::new(
            Arc::clone(&self.instance.transactions),
            Arc::clone(&function_client),
        );
        function_client.set_database(self.instance.default_database_name());
        self.instance
            .connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        function_client.set_connection_counter(Arc::clone(&self.instance.connections));
        function_client.set_object_cache(Arc::clone(&self.instance.object_cache));
        function_client.set_external_file_cache(Arc::clone(&self.instance.external_file_cache));
        // `ClientContext::ClientContext`: a connection-scoped logger.
        let logger = Arc::new(crate::logging::ConnectionLogger::new(
            Arc::clone(&self.instance.log_manager),
            crate::logging::LoggingContext::new(crate::logging::ContextScope::Connection),
        ));
        function_client
            .set_logger(Arc::clone(&logger) as Arc<dyn roastedduck_function::logging::ClientLogger>);
        if crate::platform::is_rosetta_translated() {
            use roastedduck_function::logging::{ClientLogger, LogLevel, LogScope};
            if logger.should_log(LogScope::Connection, "", LogLevel::Warning) {
                logger.write_log(
                    LogScope::Connection,
                    "",
                    LogLevel::Warning,
                    "OSX binary translation ('Rosetta') detected. Running DuckDB through Rosetta will \
                     cause a significant performance degradation. DuckDB is available natively on Apple \
                     silicon, please download an appropriate binary here: https://duckdb.org/install/",
                )?;
            }
        }
        // `ClientData::temporary_objects`: the connection's in-memory `temp` database.
        let temp_catalog = Arc::new(Catalog::with_oid_allocator(
            roastedduck_catalog::TEMP_CATALOG,
            self.instance.system_catalog.oid_allocator(),
        )?);
        temp_catalog.set_io(Arc::new(roastedduck_storage::TableIoManager::new(
            self.instance.env.buffer_manager.clone(),
            Arc::new(roastedduck_buffer::InMemoryBlockManager::default()),
        )))?;
        let mut connection = Connection {
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
        };
        connection.sync_global_settings();
        Ok(connection)
    }

    /// Adds a configuration option (`DBConfig::AddExtensionOption`, as
    /// `duckdb_register_config_option` does): returns `false`, adding nothing, if an option
    /// of that name exists (`HasExtensionOption`).
    pub fn add_extension_option(&self, option: roastedduck_function::ExtensionOption) -> bool {
        if self.instance.extension_option(&option.name).is_some() {
            return false;
        }
        self.instance.add_extension_option(option);
        true
    }

    /// Register native HTTP(S) and S3 transports. Full HTTPFS support, including
    /// credential refresh and `HuggingFace`, remains separate work.
    pub fn register_http_file_system(&self) -> Result<()> {
        roastedduck_function::registered_files::Registry::initialize(&self.instance.object_cache)
            .register(Arc::new(roastedduck_httpfs::HttpFileSystem))?;
        roastedduck_function::registered_files::Registry::initialize(&self.instance.object_cache)
            .register(Arc::new(roastedduck_httpfs::huggingface::HuggingFaceFileSystem))?;
        roastedduck_function::registered_files::Registry::initialize(&self.instance.object_cache).register(
            Arc::new(roastedduck_httpfs::s3::S3FileSystem::new(
                self.instance.env.buffer_manager.clone(),
            )),
        )?;
        self.instance.secrets.register_s3_types();
        self.instance.object_cache.put(
            roastedduck_httpfs::HTTP_SECRET_RESOLVER_KEY,
            Arc::new(roastedduck_httpfs::HttpSecretResolver(Arc::new(
                crate::secrets::HttpSecretLookup(Arc::clone(&self.instance.secrets)),
            ))),
        );
        self.instance.object_cache.put(
            roastedduck_httpfs::s3::SECRET_RESOLVER_KEY,
            Arc::new(roastedduck_httpfs::s3::SecretResolver(Arc::new(
                crate::secrets::S3SecretLookup(Arc::clone(&self.instance.secrets)),
            ))),
        );
        self.instance.object_cache.put(
            roastedduck_httpfs::huggingface::SECRET_RESOLVER_KEY,
            Arc::new(roastedduck_httpfs::huggingface::SecretResolver(Arc::new(
                crate::secrets::HuggingFaceSecretLookup(Arc::clone(&self.instance.secrets)),
            ))),
        );
        for option in roastedduck_httpfs::options()
            .into_iter()
            .chain(roastedduck_httpfs::s3::options())
        {
            self.add_extension_option(option);
        }
        // The pinned extension snapshots these environment settings at LOAD.
        // These are global values; keep the reported option defaults intact.
        for (name, variable) in [
            ("s3_region", "AWS_DEFAULT_REGION"),
            ("s3_region", "AWS_REGION"),
            ("s3_access_key_id", "AWS_ACCESS_KEY_ID"),
            ("s3_secret_access_key", "AWS_SECRET_ACCESS_KEY"),
            ("s3_session_token", "AWS_SESSION_TOKEN"),
            ("s3_endpoint", "DUCKDB_S3_ENDPOINT"),
            ("s3_use_ssl", "DUCKDB_S3_USE_SSL"),
            ("s3_kms_key_id", "DUCKDB_S3_KMS_KEY_ID"),
            ("s3_requester_pays", "DUCKDB_S3_REQUESTER_PAYS"),
        ] {
            if let Ok(text) = std::env::var(variable)
                && let Some(option) = self.instance.extension_option(name)
            {
                let value = Value::varchar(text).default_cast_as(&option.input_type)?;
                self.instance.set_extension_global(&option, Some(value));
            }
        }
        Ok(())
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
    pub fn set_log_level(&self, level: roastedduck_function::logging::LogLevel) {
        self.instance.log_manager.set_level(level);
    }

    /// The version of the library (`DuckDB::LibraryVersion`).
    pub fn library_version() -> &'static str {
        roastedduck_storage::storage_info::LIBRARY_VERSION
    }

    /// The commit the library was built from (`DuckDB::SourceID`).
    pub fn source_id() -> &'static str {
        roastedduck_storage::storage_info::SOURCE_ID
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
    pub fn add_replacement_scan(&self, scan: roastedduck_planner::ReplacementScan) {
        self.instance
            .replacement_scans
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scan);
    }

    pub(crate) fn invalidate_external_extension(&self, message: &[u8]) {
        self.instance.validity.invalidate(message);
    }

    /// Metadata and initialization synchronization for external extensions.
    pub fn external_extensions(&self) -> &Arc<crate::external_extensions::ExternalExtensionState> {
        &self.instance.external_extensions
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
            "icu" | "httpfs" | "fts" | "inet" | "excel" | "sqlsmith" | "spatial" | "delta" | "iceberg"
            | "aws" | "azure" | "motherduck" | "postgres_scanner" | "mysql_scanner" | "sqlite_scanner"
            | "vss" | "ui" | "encodings" | "ducklake" | "avro" | "demo_capi" => ExtensionLoad::NotAvailable,
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
    search_path: Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>,
    /// The entries as `CatalogSearchPath::GetSetPaths` keeps them for display: the
    /// catalog as given (or taken from the previous default), empty if none.
    search_path_set: Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>,
    prepared_statements: CaseInsensitiveMap<Arc<PreparedStatementData>, roastedduck_common::ByteString>,
    /// Connection state functions use (DuckDB keeps it in the `ClientContext`).
    function_client: Arc<roastedduck_function::FunctionClient>,
    /// Databases attached by the open explicit transaction; rolling it back detaches
    /// them (the `ATTACHED_DATABASE` undo entries).
    transaction_attaches: Vec<Arc<AttachedDatabase>>,
    /// The databases the current transaction uses (`MetaTransaction::UseDatabase`): a
    /// database detached meanwhile is closed when the transaction ends.
    used_databases: Vec<Arc<AttachedDatabase>>,
    /// `SET VARIABLE` values (DuckDB's `ClientConfig::user_variables`).
    user_variables: CaseInsensitiveMap<roastedduck_types::Value, roastedduck_common::ByteString>,
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

/// Connection configuration retained across a database restart. Transaction,
/// prepared-statement and active-query state are deliberately not retained.
#[derive(Clone, Debug)]
pub struct ClientSessionSnapshot {
    settings: Settings,
    local_overrides: std::collections::HashSet<&'static str>,
    search_path: Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>,
    extension_session: CaseInsensitiveMap<Value>,
    query_log_path: Option<String>,
}

/// What a transaction about to commit changed, for the automatic checkpoint after the
/// commit.
#[derive(Debug)]
struct CommittedChanges {
    /// The database it wrote to.
    database: roastedduck_common::ByteString,
    properties: roastedduck_storage::transaction::UndoProperties,
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
    pub fn cast_value(&self, value: &Value, target: &roastedduck_types::LogicalType) -> Result<Value> {
        use roastedduck_function::cast::ValueCast;
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let mut params = roastedduck_function::cast::CastParameters::new();
        value.cast_with(&self.casts(), target, &mut params)
    }

    /// Casts the first `count` rows of `source` to `target` with the casts of this
    /// database, including those of loaded extensions (DuckDB's
    /// `VectorOperations::Cast(ClientContext &, ...)`).
    pub fn cast_vector(
        &self,
        source: &roastedduck_types::Vector,
        target: &roastedduck_types::LogicalType,
        count: usize,
    ) -> Result<roastedduck_types::Vector> {
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let mut params = roastedduck_function::cast::CastParameters::new();
        roastedduck_function::cast::cast_vector(&self.casts(), source, target, count, &mut params)
    }

    /// Whether the connection commits every statement, being outside an explicit
    /// transaction (DuckDB's `Connection::IsAutoCommit`).
    pub fn is_auto_commit(&self) -> bool {
        self.transactions.is_auto_commit()
    }

    /// The current value of option `name` for this connection, or DuckDB's error for an
    /// unknown option (`ClientContext::TryGetCurrentSetting`).
    pub fn current_setting(&self, name: &str) -> Result<Value> {
        use roastedduck_function::SettingsSource;
        self.settings_snapshot().current_setting(name)
    }

    /// The default database and schema of the connection
    /// (`DatabaseManager::GetDefaultDatabase` and the default of its catalog search path).
    pub fn current_database_and_schema(
        &self,
    ) -> (roastedduck_common::ByteString, roastedduck_common::ByteString) {
        let schema = self.search_path.first().map_or_else(
            || roastedduck_catalog::DEFAULT_SCHEMA.into(),
            |(_, schema)| schema.clone(),
        );
        (self.default_catalog(), schema)
    }

    /// The bytes the latest profiled query read from database files
    /// (`QueryProfiler::GetBytesRead`).
    pub fn bytes_read(&self) -> u64 {
        self.profiler
            .bytes(roastedduck_common::profiling::MetricType::TotalBytesRead)
    }

    /// The bytes the latest profiled query wrote to database files
    /// (`QueryProfiler::GetBytesWritten`).
    pub fn bytes_written(&self) -> u64 {
        self.profiler
            .bytes(roastedduck_common::profiling::MetricType::TotalBytesWritten)
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
    pub fn has_extension_cast(&self, ty: &roastedduck_types::LogicalType) -> bool {
        self.database
            .instance()
            .client_casts
            .load(std::sync::atomic::Ordering::Acquire)
            && contains_type(ty, roastedduck_types::LogicalTypeId::TimestampTz)
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

    pub fn configuration_snapshot(&self) -> crate::ConfigurationSnapshot {
        self.database.configuration_snapshot()
    }

    pub fn session_snapshot(&self) -> ClientSessionSnapshot {
        ClientSessionSnapshot {
            settings: self.settings.clone(),
            local_overrides: self.local_overrides.clone(),
            search_path: self.search_path_set.clone(),
            extension_session: self.extension_session.clone(),
            query_log_path: self
                .log_query_writer
                .as_ref()
                .and_then(|_| self.current_setting("log_query_path").ok()?.to_varchar().ok()),
        }
    }

    /// Restores only client configuration; registered options and global values
    /// come from the separately restored database configuration.
    pub fn restore_session(&mut self, snapshot: ClientSessionSnapshot) -> Result<()> {
        self.cleanup_active_query();
        let inherited = self.settings.clone();
        self.settings = snapshot.settings;
        self.local_overrides = snapshot.local_overrides;
        self.extension_session = snapshot.extension_session;
        self.settings.extension_collations = self.database.instance.extension_collations();
        let mut names: Vec<_> = self.settings.assigned().map(|(name, _)| name).collect();
        names.extend(inherited.assigned().map(|(name, _)| name));
        names.sort_unstable();
        names.dedup();
        for name in names {
            if !self.local_overrides.contains(name) {
                if let Some((_, value)) = inherited.assigned().find(|(option, _)| *option == name) {
                    self.settings.set(name, value)?;
                } else {
                    self.settings.reset(name)?;
                }
            }
        }
        self.synced_globals = self
            .database
            .instance
            .global_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.transactions.begin(false)?;
        let paths = self.apply_search_path(snapshot.search_path, false, "SET search_path");
        if let Err(error) = paths {
            let _ = self.transactions.rollback();
            return Err(error);
        }
        self.transactions.commit(None)?;
        self.log_query_writer = snapshot
            .query_log_path
            .map(|path| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&path)
                    .map_err(|error| {
                        Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {error}"))
                    })
            })
            .transpose()?;
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        Ok(())
    }

    /// Enable profiling of subsequent queries (`Connection::EnableProfiling`).
    pub fn enable_profiling(&mut self) {
        self.settings.profiler.enable();
    }
    /// Disable profiling without discarding the last profile (`Connection::DisableProfiling`).
    pub fn disable_profiling(&mut self) {
        self.settings.profiler.disable();
    }
    /// Render the native query profile in the requested format without running a query.
    pub fn profiling_information(&self, format: &str) -> Result<roastedduck_common::ByteString> {
        let format = crate::query_profiler::ProfilerPrintFormat::from_name(format)
            .ok_or_else(|| Error::invalid_input(format!("Invalid ProfilerPrintFormat string: {format}")))?;
        self.profiler.information(&self.settings.profiler, format)
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
    pub fn client_properties(&self) -> roastedduck_arrow::ClientProperties {
        let settings = self.settings_snapshot();
        let setting =
            |name: &str| roastedduck_function::SettingsSource::current_setting(&settings, name).ok();
        let enabled = |name: &str| setting(name).and_then(|value| value.as_bool()).unwrap_or(false);
        let mut properties = roastedduck_arrow::ClientProperties::default();
        if let Some(time_zone) = setting("TimeZone").and_then(|value| value.to_varchar().ok()) {
            properties.time_zone = time_zone;
        }
        if enabled("arrow_large_buffer_size") {
            properties.arrow_offset_size = roastedduck_arrow::ArrowOffsetSize::Large;
        }
        properties.arrow_use_list_view = enabled("arrow_output_list_view");
        properties.arrow_lossless_conversion = enabled("arrow_lossless_conversion");
        properties.produce_arrow_string_view = enabled("produce_arrow_string_view");
        if let Some(version) = setting("arrow_output_version")
            .and_then(|value| value.to_varchar().ok())
            .and_then(|version| roastedduck_arrow::ArrowFormatVersion::from_setting(&version))
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
    ///
    /// A failure is processed (`ProcessError`: JSON under `errors_as_json`, else the
    /// location in the query) and thrown again, as `ClientContext::ParseStatementsInternal`
    /// does.
    pub fn extract_statements(&self, sql: &str) -> Result<Vec<SqlStatement>> {
        self.extract_statements_internal(sql)
            .map_err(|error| self.process_error(error, sql).rethrown())
    }

    fn extract_statements_internal(&self, sql: &str) -> Result<Vec<SqlStatement>> {
        use crate::settings::ParserOverride;
        let options = self.settings.parser_options();
        let mode = self.settings.parser_override();
        if mode != ParserOverride::Default && self.database.instance().has_parser_extension() {
            let stripped = roastedduck_parser::strip_unicode_spaces(sql);
            let query = stripped.as_deref().unwrap_or(sql);
            match roastedduck_autocomplete::parse_query(query, options) {
                Ok(statements) => return Ok(statements),
                Err(error) if mode == ParserOverride::Strict => return Err(error),
                // `FALLBACK`: the Postgres parser takes over.
                Err(_) => {}
            }
        }
        let error = match roastedduck_parser::parse_with_options(sql, &options) {
            Err(error)
                if error.subtype() == Some(roastedduck_parser::SYNTAX_ERROR)
                    && self.database.instance().has_parser_extension() =>
            {
                error
            }
            parsed => return parsed,
        };
        // With a parser extension loaded, a query that does not parse is parsed again a
        // statement at a time, for the extension to parse the one the grammar cannot. None
        // does here, so that statement fails with the error of the whole query; but a
        // statement before it is transformed, and fails with its own error if it cannot be.
        for statement in roastedduck_parser::split_queries(sql) {
            match roastedduck_parser::parse_bytes_with_options(&statement, &options) {
                Ok(_) => {}
                Err(failure) if failure.subtype() == Some(roastedduck_parser::SYNTAX_ERROR) => break,
                Err(failure) => return Err(failure),
            }
        }
        Err(error)
    }

    /// Native SQL ingress through the standard parser, with processed native errors.
    pub fn extract_statements_bytes(&self, sql: &[u8]) -> Result<Vec<SqlStatement>> {
        // The PEG override currently has a text ingress; preserve its behavior for
        // representable input and fail explicitly in strict mode until its byte port.
        if self.settings.parser_override() != crate::settings::ParserOverride::Default
            && self.database.instance().has_parser_extension()
        {
            if let Ok(text) = std::str::from_utf8(sql) {
                return self.extract_statements(text);
            }
            if self.settings.parser_override() == crate::settings::ParserOverride::Strict {
                return Err(Error::not_implemented(
                    "Native-byte PEG parser ingress is not implemented",
                ));
            }
        }
        let options = self.settings.parser_options();
        let parsed = roastedduck_parser::parse_bytes_with_options(sql, &options);
        let parsed = match parsed {
            Err(error)
                if error.subtype() == Some(roastedduck_parser::SYNTAX_ERROR)
                    && self.database.instance().has_parser_extension() =>
            {
                for statement in roastedduck_parser::split_queries_bytes(sql) {
                    match roastedduck_parser::parse_bytes_with_options(&statement, &options) {
                        Ok(_) => {}
                        Err(failure) if failure.subtype() == Some(roastedduck_parser::SYNTAX_ERROR) => break,
                        Err(failure) => return Err(self.process_error(failure, sql).rethrown()),
                    }
                }
                Err(error)
            }
            outcome => outcome,
        };
        parsed.map_err(|error| self.process_error(error, sql).rethrown())
    }

    /// Native equivalent of `ParseStatements`, preserving query bytes through pragmas.
    pub fn parse_statements_bytes(&mut self, sql: &[u8]) -> Result<Vec<SqlStatement>> {
        let statements = self.extract_statements_bytes(sql)?;
        self.preprocess_statements(sql, statements)
    }

    /// Preprocess owned statements through DuckDB's transaction-aware expansion.
    pub fn preprocess_statements(
        &mut self,
        sql: impl AsRef<[u8]>,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<SqlStatement>> {
        self.preprocess_parsed_statements(statements)
            .map_err(|error| self.process_error(error, sql.as_ref()).rethrown())
    }

    /// Parsed and preprocessed statements, including expanded PRAGMA bodies.
    pub fn parse_statements(&mut self, sql: &str) -> Result<Vec<SqlStatement>> {
        let statements = self.extract_statements(sql)?;
        self.preprocess_statements(sql, statements)
    }

    /// Runs every statement in `sql` and returns the result of the last one (DuckDB's
    /// `ClientContext::Query`).
    pub fn query(&mut self, sql: impl AsRef<[u8]>) -> Result<MaterializedResult> {
        self.sync_global_settings();
        let statements = self.parse_statements_bytes(sql.as_ref())?;
        self.execute_statements(statements)
    }

    /// [`Connection::query`] with the results' rows kept as `memory_type` says
    /// (`ClientContext::Query` with `QueryParameters::memory_type`): a buffer-managed
    /// result counts against the memory limit and spills like other operator data.
    pub fn query_with(
        &mut self,
        sql: &str,
        memory_type: QueryResultMemoryType,
    ) -> Result<MaterializedResult> {
        self.sync_global_settings();
        let statements = self.parse_statements(sql)?;
        let mut result = MaterializedResult::default();
        for statement in statements {
            let query = statement.query.clone();
            result = self.execute_statement_with(&query, statement, memory_type)?;
        }
        Ok(result)
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
    pub fn execute_statement(
        &mut self,
        query: impl AsRef<[u8]>,
        statement: SqlStatement,
    ) -> Result<MaterializedResult> {
        let query = query.as_ref();
        self.execute_statement_with(query, statement, QueryResultMemoryType::InMemory)
    }

    /// [`Connection::execute_statement`] with the result's rows kept as `memory_type`
    /// says (`Connection::Query(statement, memory_type)`).
    pub fn execute_statement_with(
        &mut self,
        query: impl AsRef<[u8]>,
        statement: SqlStatement,
        memory_type: QueryResultMemoryType,
    ) -> Result<MaterializedResult> {
        let query = query.as_ref();
        self.initial_cleanup();
        let statement_type = statement.statement_type();
        self.begin_query(query, &statement)?;
        let outcome = self.in_query_scope(|connection| {
            connection.begin_query_logging(query);
            connection
                .start_statement(statement, RunMode::Materialize(memory_type))
                .and_then(StatementRun::into_materialized)
        });
        self.end_query(query, outcome)
            .map(|result| result.with_statement_type(statement_type))
    }

    /// `ClientContext::BeginQueryInternal` up to the logging: nothing runs on an invalidated
    /// instance; the statement sees the latest global settings, and the profiler starts.
    pub(crate) fn begin_query(&mut self, query: impl AsRef<[u8]>, statement: &SqlStatement) -> Result<()> {
        let query = query.as_ref();
        if let Some(message) = self.database.instance().validity.invalidated() {
            return Err(invalidated_database_error(&message).with_query_context_bytes(query));
        }
        self.sync_global_settings();
        self.function_client.set_query(query);
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        // `PendingStatementOrPreparedStatement` starts the profiler first.
        let is_explain_analyze = matches!(
            &statement.kind,
            roastedduck_ast::SqlStatementKind::Explain(explain)
                if explain.explain_type == roastedduck_ast::ExplainType::ExplainAnalyze
        );
        self.profiler
            .start_query(&self.settings.profiler, query, is_explain_analyze);
        Ok(())
    }

    /// Activates the running query's thread-local context until the returned scope is
    /// dropped: the metrics its profiler records into, and the connection as the opener of
    /// the files it opens (they read its settings and log to its loggers).
    pub(crate) fn query_scope(&self) -> QueryScope {
        if let Some(context) = self
            .active_query
            .as_ref()
            .and_then(|query| query.callback_context.as_ref())
        {
            let context: Arc<dyn std::any::Any + Send + Sync> = Arc::clone(context) as _;
            self.function_client
                .set_transaction_context(Some(Arc::downgrade(&context)));
        }
        let metrics = self.profiler.active_metrics().cloned();
        QueryScope {
            _metrics: roastedduck_common::profiling::activate(metrics.as_ref()),
            _opener: roastedduck_function::file_system::activate_opener(Arc::clone(&self.function_client)),
        }
    }

    /// Runs `work` within the running query's [scope](Connection::query_scope).
    pub(crate) fn in_query_scope<T>(&mut self, work: impl FnOnce(&mut Self) -> T) -> T {
        let _scope = self.query_scope();
        work(self)
    }

    /// Binding outside query execution leaves an aborted synthetic transaction for
    /// its query owner to roll back, matching the client-context transaction helper.
    fn end_client_statement(&mut self, success: bool, invalidates: bool) -> Result<()> {
        self.transactions.end_statement(success, invalidates)
    }

    /// `ClientContext::EndQueryInternal`: applies the configuration changes functions made,
    /// ends the statement in its transaction (committing or rolling back an auto-commit
    /// one), writes the profile, checkpoints automatically and releases the databases the
    /// transaction used. Errors are put in the context of `query`.
    pub(crate) fn end_query(
        &mut self,
        query: impl AsRef<[u8]>,
        outcome: Result<MaterializedResult>,
    ) -> Result<MaterializedResult> {
        let query = query.as_ref();
        self.end_query_internal(query, outcome, true)
    }

    /// [`Connection::end_query`]; a failure invalidates an explicit transaction only if
    /// `invalidate_transaction` (a query whose result was closed unread does not).
    pub(crate) fn end_query_internal(
        &mut self,
        query: impl AsRef<[u8]>,
        outcome: Result<MaterializedResult>,
        invalidate_transaction: bool,
    ) -> Result<MaterializedResult> {
        let query = query.as_ref();
        let metrics = self.profiler.active_metrics().cloned();
        let metrics_guard = roastedduck_common::profiling::activate(metrics.as_ref());
        let _opener_guard =
            roastedduck_function::file_system::activate_opener(Arc::clone(&self.function_client));
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
            Some(message) => Err(Error::new_bytes(ErrorKind::Io, message)),
            None => outcome,
        };
        // An auto-commit that checkpoints instead of logging its changes; it is the
        // commit's only checkpoint.
        let checkpoint =
            if self.transactions.is_auto_commit() && self.transactions.has_current() && outcome.is_ok() {
                self.commit_checkpoint(committed.as_ref())
            } else {
                None
            };
        let committed = committed.filter(|_| checkpoint.is_none());
        let was_explicit = !self.transactions.is_auto_commit();
        let ended = self.transactions.end_statement_checkpointing(
            outcome.is_ok(),
            invalidates,
            crate::transaction::StatementEnd::Query {
                allow_invalidation: invalidate_transaction,
            },
            checkpoint,
        );
        if was_explicit && self.transactions.is_auto_commit() {
            self.finish_transaction_attaches(outcome.is_err());
        }
        // Between statements, the connection's functions (and C API clients reading its
        // options) see the settings the statement left.
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        // `EndQueryInternal`: the profile is written before the logger is refreshed.
        drop(metrics_guard);
        let profiled = self.profiler.end_query(
            &self.settings.profiler,
            self.logger.as_ref(),
            self.function_client.http_stats(),
        );
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

    /// The checkpoint the commit of the open transaction, which made `changes`, runs
    /// instead of logging them (see [`DatabaseInstance::skip_wal_checkpoint`]).
    fn commit_checkpoint(
        &mut self,
        changes: Option<&CommittedChanges>,
    ) -> Option<roastedduck_storage::transaction::CommitCheckpoint<'static>> {
        let changes = changes?;
        let current = self.transactions.current();
        self.database.instance().skip_wal_checkpoint(
            &current,
            changes,
            self.settings.skip_checkpoint_on_commit(),
            self.settings.auto_checkpoint_skip_wal_threshold(),
        )
    }

    /// Binds an ADD PRIMARY KEY WAL entry while restoring its native tree. Its
    /// index stays staged until the creating replay transaction commits.
    pub(crate) fn replay_primary_key(
        &mut self,
        info: roastedduck_ast::AlterInfo,
        stored: &roastedduck_storage::index::art::ArtStorageInfo,
    ) -> Result<(
        Arc<roastedduck_storage::DataTableInfo>,
        roastedduck_storage::index::IndexSlot,
    )> {
        let transaction = self.current_transaction();
        let client = self.client_context(Arc::clone(&transaction));
        let planned = Planner::new(Arc::clone(&client)).create_plan(SqlStatement::new(
            roastedduck_ast::SqlStatementKind::Alter(Box::new(info)),
        ))?;
        let roastedduck_planner::operator::LogicalKind::Alter(alter) = planned
            .plan
            .ok_or_else(|| Error::internal("primary-key WAL replay without logical plan"))?
            .kind
        else {
            return Err(Error::internal("primary-key WAL replay produced another plan"));
        };
        roastedduck_execution::operators::AlterEntryAction {
            catalog: client
                .catalog(&alter.catalog)
                .cloned()
                .ok_or_else(|| Error::internal("primary-key WAL replay catalog is missing"))?,
            schema: alter.schema,
            entry: alter.entry,
            action: alter.action,
            dependents_allowed: alter.dependents_allowed,
            info: alter.info,
        }
        .execute_replayed_primary_key(&transaction, stored)
    }

    /// Executes a checkpoint table function's request while its query metrics and
    /// transaction are still active. Prepared and SQL-text execution share this path.
    fn checkpoint_database(
        &mut self,
        database: Option<roastedduck_common::ByteString>,
        force: bool,
    ) -> Result<()> {
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
                    roastedduck_execution::operators::bind_table_indexes(&client, &table)?;
                }
            }
        }
        crate::storage::checkpoint(&attached, &transaction, started, force, &instance.transactions)?;
        Ok(())
    }

    /// Checkpoint effects belong to the executor task that called the function:
    /// observe their errors before telling a pending client the task is ready.
    /// Other callback changes keep their existing end-query ordering.
    fn apply_checkpoint_requests(&mut self) -> Result<()> {
        for request in self.function_client.take_client_requests() {
            match request {
                roastedduck_function::ClientRequest::Checkpoint { database, force } => {
                    self.checkpoint_database(database, force)?;
                }
                other => self.function_client.push_client_request(other),
            }
        }
        Ok(())
    }

    /// Applies the configuration changes functions made (`enable_profiling` /
    /// `disable_profiling`, `enable_peg_parser` / `disable_peg_parser`).
    fn apply_client_requests(&mut self, requests: Vec<roastedduck_function::ClientRequest>) -> Result<()> {
        use roastedduck_function::ClientRequest;
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
                ClientRequest::Checkpoint { database, force } => {
                    self.checkpoint_database(database, force)?;
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
                match value {
                    Some(value) => {
                        let _ = self.settings.set(name, value);
                    }
                    None => {
                        let _ = self.settings.reset(name);
                    }
                }
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
    fn set_extension_variable(
        &mut self,
        name: impl AsRef<[u8]>,
        value: &Value,
        scope: SetScope,
    ) -> Result<()> {
        let name = name.as_ref();
        let instance = self.database.instance();
        let option = instance.extension_option(name).ok_or_else(|| {
            Error::invalid_input(roastedduck_common::byte_format!(
                "Extension parameter {name} was not found after autoloading"
            ))
        })?;
        let mut target_value = self.cast_value(value, &option.input_type).map_err(|error| {
            Error::invalid_input(format!("Failed to cast value: {}", error.raw_message()))
        })?;
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
    fn reset_extension_variable(&mut self, name: impl AsRef<[u8]>, scope: SetScope) -> Result<()> {
        let name = name.as_ref();
        let instance = self.database.instance();
        let option = instance.extension_option(name).ok_or_else(|| {
            Error::invalid_input(roastedduck_common::byte_format!(
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
    fn record_setting(&mut self, name: impl AsRef<[u8]>, value: Option<&Value>, scope: SetScope) {
        let name = name.as_ref();
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
                match global_value {
                    Some(value) => {
                        let _ = self.settings.set(option, value);
                    }
                    None => {
                        let _ = self.settings.reset(option);
                    }
                }
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
        globals.values.push((option, value.cloned()));
        globals.version += 1;
        // This connection already applied the change.
        self.synced_globals = globals.clone();
    }

    /// `ClientContext::BeginQueryInternal`'s logging: flush, a logger for this query's
    /// connection, transaction and query, and the `QueryLog` entry.
    fn begin_query_logging(&mut self, query: impl AsRef<[u8]>) {
        use crate::logging::{ContextScope, LoggingContext};
        use roastedduck_function::logging::{ClientLogger, LogLevel, LogScope};
        let query = query.as_ref();
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
                .write_log_bytes(LogScope::Connection, "QueryLog", LogLevel::Info, query);
        }
    }

    /// `ClientContext::LogQueryInternal`: the statement and a newline, synced to disk.
    fn log_query(&mut self, query: impl AsRef<[u8]>) {
        use std::io::Write as _;
        let query = query.as_ref();
        if let Some(writer) = &mut self.log_query_writer {
            let _ = writer
                .write_all(query)
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.sync_data());
        }
    }

    /// `LogQueryPathSetting::OnSet`: an empty path stops logging, any other starts a new
    /// log file there.
    fn set_log_query_path(&mut self, value: Option<&roastedduck_types::Value>) -> Result<()> {
        let path = value
            .map(roastedduck_types::Value::to_varchar)
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
    pub fn logical_plan(
        &mut self,
        query: impl AsRef<[u8]>,
        statement: SqlStatement,
    ) -> Result<LogicalOperator> {
        let query = query.as_ref();
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let outcome = planner.create_plan(statement).and_then(|planned| {
            let mut plan = planned
                .plan
                .ok_or_else(|| Error::invalid_input("Not all parameters were bound"))?;
            ColumnBindingResolver::resolve(&mut plan)?;
            Ok(plan)
        });
        let ended = self.end_client_statement(outcome.is_ok(), false);
        outcome
            .and_then(|plan| ended.map(|()| plan))
            .map_err(|error| self.process_error(error, query))
    }

    /// `ClientContext::ProcessError`: the error as JSON under `errors_as_json`, else with
    /// the location in the query.
    fn process_error(&self, error: Error, query: impl AsRef<[u8]>) -> Error {
        let query = query.as_ref();
        if self.settings.errors_as_json() {
            error.into_json()
        } else {
            error.with_query_context_bytes(query)
        }
    }

    /// The transaction the next statement runs in, started if needed.
    pub(crate) fn current_transaction(&mut self) -> Arc<Transaction> {
        self.transactions.current()
    }

    /// The catalogs this connection sees: the attached databases with `default` first,
    /// its `temp` catalog, then `system`.
    fn catalogs(&self, default: &[u8]) -> Vec<Arc<Catalog>> {
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
        let has_default_database = instance
            .default_database
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        let context = Arc::new(ClientContext {
            catalogs: self.catalogs(&instance.default_database_name()),
            search_path: self.search_path.clone(),
            raw_search_path: self.search_path_set.clone(),
            has_default_database,
            view_search_path: Vec::new(),
            view_default_schema_precedence: false,
            transaction,
            casts: self.casts(),
            evaluator: Arc::new(ScalarEvaluator),
            config: {
                let optimizer = self.settings.optimizer_config();
                roastedduck_planner::PlannerConfig {
                    flip_right_joins: optimizer.enabled
                        && !optimizer
                            .disabled
                            .contains(&roastedduck_optimizer::OptimizerType::BuildSideProbeSide),
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
            }) as Arc<dyn roastedduck_planner::DatabaseState>),
        });
        let any: Arc<dyn std::any::Any + Send + Sync> = Arc::clone(&context) as _;
        self.function_client
            .set_transaction_context(Some(Arc::downgrade(&any)));
        context
    }

    /// `IMPORT DATABASE 'dir'` (DuckDB's `PragmaImportDatabase`): runs the directory's
    /// `schema.sql`, then its `load.sql` with the files taken from the directory.
    fn import_database(&mut self, parameters: &[roastedduck_types::Value]) -> Result<MaterializedResult> {
        let statements = self.import_database_statements(parameters)?;
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
    fn copy_database(&mut self, stmt: &roastedduck_ast::CopyDatabaseStatement) -> Result<MaterializedResult> {
        let instance = self.database.instance();
        let source = instance.database(&stmt.from_database)?;
        let target = instance.database(&stmt.to_database)?;
        if Arc::ptr_eq(&source.catalog, &target.catalog) {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Cannot copy from \"{}\" to \"{}\" - FROM and TO databases are the same",
                stmt.from_database,
                stmt.to_database
            )));
        }
        let target_name = roastedduck_common::ByteString::from(&*target.name());
        // Read the source in the open transaction, or in a snapshot of its own.
        let (snapshot, temporary) = if self.transactions.has_current() {
            (self.transactions.current(), false)
        } else {
            (Arc::new(instance.transactions.begin()), true)
        };
        // The statements to run; `queries` are SQL texts to run before `statements[i]`.
        let mut statements: Vec<SqlStatement> = Vec::new();
        let mut queries: Vec<(usize, roastedduck_common::ByteString)> = Vec::new();
        if stmt.copy_type == roastedduck_ast::CopyDatabaseType::CopySchema {
            for mut info in roastedduck_catalog::checkpoint::export_create_infos(&source.catalog, &snapshot) {
                info.catalog.clone_from(&target_name);
                info.on_conflict = if matches!(info.kind, roastedduck_ast::CreateInfoKind::Schema) {
                    roastedduck_ast::OnCreateConflict::IgnoreOnConflict
                } else {
                    roastedduck_ast::OnCreateConflict::ErrorOnConflict
                };
                // Schemas and sequences are created from their infos: their SQL names no
                // catalog, and a sequence's SQL loses its usage.
                if matches!(
                    info.kind,
                    roastedduck_ast::CreateInfoKind::Schema | roastedduck_ast::CreateInfoKind::Sequence(_)
                ) {
                    statements.push(SqlStatement::new(roastedduck_ast::SqlStatementKind::Create(
                        Box::new(info),
                    )));
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
                    if matches!(entry.kind, roastedduck_catalog::EntryKind::Table(_)) && !entry.internal {
                        tables.push((schema.name().to_owned(), entry));
                    }
                }
            }
            for (schema, entry) in roastedduck_catalog::checkpoint::reorder_table_entries(tables) {
                let roastedduck_catalog::EntryKind::Table(table) = &entry.kind else {
                    continue;
                };
                let columns: Vec<roastedduck_common::ByteString> = table
                    .physical_columns()
                    .map(|(_, _, column)| {
                        roastedduck_ast::keyword::write_optionally_quoted_bytes(&column.name)
                    })
                    .collect();
                let columns = roastedduck_common::ByteString::join(&columns, b", ");
                queries.push((
                    statements.len(),
                    roastedduck_common::byte_format!(
                        "INSERT INTO {} ({columns}) SELECT {columns} FROM {}",
                        roastedduck_ast::qualifier_to_string(&target_name, &schema, entry.name()),
                        roastedduck_ast::qualifier_to_string(source.name(), &schema, entry.name()),
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
                for parsed in roastedduck_parser::parse_bytes(&query)? {
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
    fn run_load(&mut self, info: &roastedduck_ast::LoadInfo) -> Result<()> {
        let config =
            crate::extensions::ExtensionConfig::from_settings(|name| self.current_setting(name).ok());
        if let Some(name) = crate::extensions::load_extension(info, &config, &self.database)?
            && let Some(extension) = StaticExtension::from_name(&name)
        {
            self.database.instance.load_static_extension(extension)?;
        }
        Ok(())
    }

    /// Load a native extension without closing an active client result.
    pub fn load_extension(&mut self, name: impl AsRef<[u8]>) -> Result<()> {
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let info = roastedduck_ast::LoadInfo {
            filename: name.as_ref().into(),
            load_type: roastedduck_ast::LoadType::Load,
            repository: roastedduck_common::ByteString::new(),
            version: roastedduck_common::ByteString::new(),
            repo_is_alias: false,
        };
        self.in_query_scope(|connection| connection.run_load(&info))
    }

    /// Loads the extension of the `ATTACH` storage type `db_type`
    /// (`DatabaseManager::GetDatabaseType`'s `LoadExternalExtension`).
    fn load_storage_extension(&mut self, db_type: &[u8]) -> Result<()> {
        self.run_load(&roastedduck_ast::LoadInfo {
            filename: db_type.into(),
            load_type: roastedduck_ast::LoadType::Load,
            repository: String::new().into(),
            version: String::new().into(),
            repo_is_alias: false,
        })
    }

    fn run_statement(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        self.start_statement(statement, RunMode::MATERIALIZE)
            .and_then(StatementRun::into_materialized)
    }

    /// Starts one parsed statement (DuckDB's `PendingStatementInternal`): statements the
    /// connection runs itself complete right away; a query's physical plan runs to
    /// completion or, in [`RunMode::Pending`], is left to the caller to execute.
    pub(crate) fn start_statement(&mut self, statement: SqlStatement, mode: RunMode) -> Result<StatementRun> {
        if let roastedduck_ast::SqlStatementKind::Multi(statements) = statement.kind {
            return self.run_multi_statement(statements).map(StatementRun::Done);
        }
        if let roastedduck_ast::SqlStatementKind::CopyDatabase(copy) = &statement.kind {
            return self.copy_database(copy).map(StatementRun::Done);
        }
        if let roastedduck_ast::SqlStatementKind::Load(info) = &statement.kind {
            // PhysicalLoad runs inside ExecutorTask in DuckDB, whose error catch
            // rebuilds the exception metadata before the client receives it.
            self.run_load(info).map_err(Error::caught)?;
            // `Binder::Bind(LoadStatement &)`: the statement returns nothing.
            return Ok(StatementRun::Done(
                MaterializedResult::new(vec!["Success".into()], vec![LogicalType::BOOLEAN], Vec::new())
                    .with_return_type(roastedduck_planner::StatementReturnType::Nothing),
            ));
        }
        // Pragmas that stand for a query run that query (DuckDB's `PragmaHandler`).
        if let roastedduck_ast::SqlStatementKind::Pragma(pragma) = &statement.kind
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
        if let roastedduck_ast::SqlStatementKind::Pragma(pragma) = &statement.kind {
            let constants: Option<Vec<roastedduck_types::Value>> = pragma
                .parameters
                .iter()
                .map(|parameter| match &parameter.kind {
                    roastedduck_ast::ParsedExpressionKind::Constant(constant) => {
                        Ok(Some(constant.value.clone()))
                    }
                    roastedduck_ast::ParsedExpressionKind::ColumnRef(column) => {
                        Ok(Some(roastedduck_types::Value::try_varchar(
                            &roastedduck_common::ByteString::join(&column.column_names, b"."),
                        )?))
                    }
                    _ => Ok(None),
                })
                .collect::<Result<Option<Vec<_>>>>()?;
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
        // `VACUUM tbl` compacts the table's indexes (`PhysicalVacuum::Finalize`).
        let vacuum_table = match &statement.kind {
            roastedduck_ast::SqlStatementKind::Vacuum(info) if info.has_table => info
                .table_ref
                .as_deref()
                .and_then(|table_ref| match &table_ref.kind {
                    roastedduck_ast::TableRefKind::BaseTable(base) => Some((**base).clone()),
                    _ => None,
                }),
            _ => None,
        };
        if vacuum_table.is_none() {
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
                roastedduck_catalog::CatalogType::Table,
                &base.table_name,
                true,
            )?;
            if let Some(found) = found
                && let roastedduck_catalog::EntryKind::Table(table) = &found.entry.kind
                && let Some(storage) = &table.storage
            {
                storage.vacuum_indexes()?;
            }
        }
        Ok(StatementRun::Done(result))
    }

    /// Runs the statements of the query a pragma stands for; the result is the last
    /// statement's.
    fn run_pragma_query(&mut self, query: &str) -> Result<MaterializedResult> {
        let mut result = MaterializedResult::default();
        for statement in roastedduck_parser::parse(query)? {
            result = self.run_statement(statement)?;
        }
        Ok(result)
    }

    /// The values of a `PRAGMA`'s parameters, bound and evaluated as constants
    /// (`Binder::BindPragma`).
    fn bind_pragma_parameters(&mut self, statement: SqlStatement) -> Result<Vec<Value>> {
        let roastedduck_ast::SqlStatementKind::Pragma(mut pragma) = statement.kind else {
            return Err(Error::internal("expected a PRAGMA"));
        };
        // PRAGMA expansion binds before ordinary statement startup, so it must
        // publish the current settings before catalog lookup can autoload.
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        Ok(planner.bind_pragma(&mut pragma)?.parameters)
    }

    /// Records that the transaction uses `database` (`MetaTransaction::UseDatabase`).
    fn use_database(&mut self, database: &Arc<AttachedDatabase>) {
        if !self.used_databases.iter().any(|used| Arc::ptr_eq(used, database)) {
            self.used_databases.push(Arc::clone(database));
        }
    }

    /// The reason a database the transaction used was invalidated, if one was.
    fn invalidated_used_database(&self) -> Option<roastedduck_common::ByteString> {
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
    fn detach(&mut self, info: &roastedduck_ast::DetachInfo) -> Result<()> {
        // `PhysicalDetach`: not while this transaction has work on the database.
        if !self.transactions.is_auto_commit()
            && self.transactions.current().has_accessed_database(&info.name)
        {
            return Err(Error::new_bytes(
                ErrorKind::Transaction,
                roastedduck_common::byte_format!(
                    "Cannot detach database \"{}\" because the current transaction has outstanding work on it - \
                     commit or rollback first",
                    info.name
                ),
            ));
        }
        if self.default_catalog().eq_ignore_ascii_case(&info.name) {
            return Err(Error::binder(roastedduck_common::byte_format!(
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
            None if info.if_not_found == roastedduck_ast::OnEntryNotFound::ReturnNull => Ok(()),
            None => Err(Error::binder(roastedduck_common::byte_format!(
                "Failed to detach database with name \"{}\": database not found",
                info.name
            ))),
        }
    }

    /// `SET schema` / `SET search_path` (`CatalogSearchPath::Set`): every entry must name
    /// an existing schema; a single name may be a database (its default schema).
    fn set_search_path(&mut self, name: impl AsRef<[u8]>, value: &roastedduck_types::Value) -> Result<()> {
        let name = name.as_ref();
        let set_schema = name.eq_ignore_ascii_case(b"schema");
        let set_name = if set_schema {
            "SET schema"
        } else {
            "SET search_path"
        };
        let text = value.to_varchar_bytes()?;
        let entries = parse_search_path(&text)?;
        if set_schema && entries.len() > 1 {
            // `CatalogSearchEntry::Parse` reads a single entry.
            return Err(Error::parser(roastedduck_common::byte_format!(
                "Failed to convert entry \"{text}\" to CatalogSearchEntry - expected a single entry"
            )));
        }
        if set_schema && entries.len() != 1 {
            return Err(Error::catalog(roastedduck_common::byte_format!(
                "{set_name} can set only 1 schema. This has {}",
                entries.len()
            )));
        }
        self.apply_search_path(entries, set_schema, set_name)
    }

    fn apply_search_path(
        &mut self,
        entries: Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>,
        set_schema: bool,
        set_name: &str,
    ) -> Result<()> {
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
                roastedduck_common::byte_format!("{catalog}.{schema}")
            };
            let not_found = || {
                Error::catalog(roastedduck_common::byte_format!(
                    "{set_name}: No catalog + schema named \"{display}\" found."
                ))
            };
            if catalog.is_empty() {
                // The `temp` and `system` catalogs alone mean their `main` schema.
                if schema.eq_ignore_ascii_case(roastedduck_catalog::TEMP_CATALOG)
                    || schema.eq_ignore_ascii_case("system")
                {
                    resolved.push((
                        schema.to_ascii_lowercase(),
                        roastedduck_catalog::DEFAULT_SCHEMA.into(),
                    ));
                    set_paths.push((schema.clone(), roastedduck_catalog::DEFAULT_SCHEMA.into()));
                    continue;
                }
                // A database name alone means its default schema.
                if let Some(database) = instance.databases.get(&schema)
                    && let Some(main) = database
                        .catalog
                        .schema(&transaction, roastedduck_catalog::DEFAULT_SCHEMA)
                {
                    resolved.push(((&*database.name()).into(), main.name().to_owned()));
                    set_paths.push((schema.clone(), main.name().to_owned()));
                    continue;
                }
                let default = self.default_catalog();
                let database = instance.databases.get(&default).ok_or_else(not_found)?;
                if let Some(found) = database.catalog.schema(&transaction, &schema) {
                    resolved.push(((&*database.name()).into(), found.name().to_owned()));
                } else {
                    // `Catalog::GetSchema` without a catalog also finds the system schemas.
                    let found = instance
                        .system_catalog
                        .schema(&transaction, &schema)
                        .ok_or_else(not_found)?;
                    resolved.push(("system".into(), found.name().to_owned()));
                }
                set_paths.push((previous_default.clone(), schema));
                continue;
            }
            let database = instance.databases.get(&catalog).ok_or_else(not_found)?;
            let found = database
                .catalog
                .schema(&transaction, &schema)
                .ok_or_else(not_found)?;
            resolved.push(((&*database.name()).into(), found.name().to_owned()));
            set_paths.push((catalog, schema));
        }
        if set_schema
            && let Some((catalog, _)) = resolved.first()
            && (catalog.eq_ignore_ascii_case("temp") || catalog.eq_ignore_ascii_case("system"))
        {
            return Err(Error::catalog(roastedduck_common::byte_format!(
                "{set_name} cannot be set to internal schema \"{catalog}\""
            )));
        }
        self.search_path = resolved;
        self.search_path_set = set_paths;
        Ok(())
    }

    /// The configuration functions see while the next statement runs.
    fn settings_snapshot(&self) -> crate::settings::SettingsSnapshot {
        // Read-only client APIs also observe global changes without first running
        // a statement that synchronizes the connection's planner settings.
        let mut settings = self.settings.clone();
        let globals = self
            .database
            .instance
            .global_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if globals.version != self.synced_globals.version {
            for (name, value) in &globals.values {
                if !self.local_overrides.contains(name) {
                    match value {
                        Some(value) => {
                            let _ = settings.set(name, value);
                        }
                        None => {
                            let _ = settings.reset(name);
                        }
                    }
                }
            }
        }
        drop(globals);
        // `CatalogSearchEntry::ListToString` of the set paths.
        let quoted = |part: &[u8]| {
            if part.iter().any(|byte| b".,\"".contains(byte)) {
                let mut output = roastedduck_common::ByteString::from(b"\"");
                for &byte in part {
                    if byte == b'"' {
                        output.push(byte);
                    }
                    output.push(byte);
                }
                output.push(b'"');
                output
            } else {
                part.into()
            }
        };
        let paths: Vec<_> = self
            .search_path_set
            .iter()
            .map(|(catalog, schema)| {
                if catalog.is_empty() {
                    quoted(schema)
                } else {
                    roastedduck_common::byte_format!("{}.{}", quoted(catalog), quoted(schema))
                }
            })
            .collect();
        let search_path = roastedduck_common::ByteString::join(&paths, b",");
        let schema = self.search_path.first().map_or_else(
            || roastedduck_catalog::DEFAULT_SCHEMA.into(),
            |(_, schema)| schema.clone(),
        );
        let buffer_manager = &self.database.instance().env.buffer_manager;
        crate::settings::SettingsSnapshot {
            extension_registry: Arc::downgrade(&self.database.instance),
            access_mode: self.database.instance().config.access_mode,
            logging: self.database.instance().log_manager.config(),
            settings,
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
    fn default_catalog(&self) -> roastedduck_common::ByteString {
        self.search_path.first().map_or_else(
            || self.database.instance().default_database_name(),
            |(catalog, _)| catalog.clone(),
        )
    }

    fn run_planned(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        self.start_planned(statement, RunMode::MATERIALIZE)
            .and_then(StatementRun::into_materialized)
    }

    /// Binds and plans `statement` (`CreatePreparedStatementInternal`) and starts its plan.
    fn start_planned(&mut self, statement: SqlStatement, mode: RunMode) -> Result<StatementRun> {
        use roastedduck_common::profiling::MetricType;
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
        names: Vec<roastedduck_common::ByteString>,
        types: Vec<LogicalType>,
        properties: &roastedduck_planner::StatementProperties,
        statement_type: roastedduck_ast::StatementType,
        mode: RunMode,
    ) -> Result<StatementRun> {
        self.register_modifications(&plan, &properties.modified_databases, statement_type)?;
        let mode = match mode {
            RunMode::Pending { stream, memory_type } => RunMode::Pending {
                stream: stream && properties.allow_stream_result,
                memory_type,
            },
            RunMode::Materialize(memory_type) => RunMode::Materialize(memory_type),
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
            .any(|statement| matches!(statement.kind, roastedduck_ast::SqlStatementKind::Select(_)));
        let transactional = statements.len() > 1 && !has_select;
        let wrap = transactional && self.transactions.is_auto_commit();
        // In an explicit transaction the body runs under `ALL_ERRORS_INVALIDATE_TRANSACTION`
        // (`StatementPreprocessor`'s `SET_INVALIDATION_POLICY`): any error aborts it.
        let all_errors_invalidate = transactional && !wrap;
        let run_sql = |connection: &mut Self, sql: &str| -> Result<MaterializedResult> {
            let mut result = MaterializedResult::default();
            for statement in roastedduck_parser::parse(sql)? {
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
        function_modified: &[roastedduck_common::ByteString],
        statement_type: roastedduck_ast::StatementType,
    ) -> Result<()> {
        let instance = self.database.instance();
        // DuckDB walks its `modified_databases` map, which lists the latest registration
        // first: the databases functions write to come before the statement's own target,
        // which its bind registered earlier.
        let mut modified: Vec<roastedduck_common::ByteString> =
            function_modified.iter().rev().cloned().collect();
        collect_modified_databases(plan, instance, &self.default_catalog(), &mut modified);
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
                return Err(Error::new_bytes(
                    ErrorKind::Transaction,
                    roastedduck_common::byte_format!(
                        "Cannot write to database \"{name}\" - transaction is launched in read-only mode"
                    ),
                ));
            }
            let Some(database) = instance.databases.get(&name) else {
                continue;
            };
            if database.read_only {
                let ty = statement_type.name();
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "Cannot execute statement of type \"{}\" on database \"{}\" which is attached in read-only mode!",
                    ty.strip_suffix("_STATEMENT").unwrap_or(ty),
                    database.name()
                )));
            }
            transaction.modify_database(
                database.name(),
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
        names: Vec<roastedduck_common::ByteString>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        self.start_plan(plan, names, types, RunMode::MATERIALIZE)
            .and_then(StatementRun::into_materialized)
    }

    /// Starts a bound plan: statements that act on the connection are executed here
    /// (DuckDB's `PhysicalTransaction`, `PhysicalSet`, `PhysicalPrepare`, ...); queries go
    /// through the physical planner and executor.
    fn start_plan(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<roastedduck_common::ByteString>,
        types: Vec<LogicalType>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        let nothing =
            || MaterializedResult::new(vec!["Success".into()], vec![LogicalType::BOOLEAN], Vec::new());
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
            LogicalKind::Load(_) => Some("LOAD"),
            LogicalKind::UpdateExtensions(_) => Some("UPDATE_EXTENSIONS"),
            LogicalKind::Set {
                scope: SetScope::Variable,
                ..
            } => None,
            LogicalKind::Set { .. } => Some("SET"),
            LogicalKind::Reset { .. } => Some("RESET"),
            LogicalKind::Prepare { .. } => Some("PREPARE"),
            LogicalKind::Deallocate { .. } => Some("DROP"),
            LogicalKind::CreateSecret(_) => Some("CREATE_SECRET"),
            LogicalKind::Drop(info) if info.catalog_type == roastedduck_ast::CatalogType::SecretEntry => {
                Some("DROP")
            }
            LogicalKind::Pragma { .. } => Some("PRAGMA"),
            _ => None,
        };
        if let Some(operator_type) = operator_type {
            self.profiler
                .initialize_statement(&self.settings.profiler, operator_type)?;
        }
        match &plan.kind {
            LogicalKind::Transaction { info } => {
                match info.transaction_type {
                    TransactionType::BeginTransaction => {
                        self.transactions.begin_with_options(
                            info.modifier == roastedduck_ast::TransactionModifierType::TransactionReadOnly,
                            info.invalidation_policy,
                            info.auto_rollback,
                        )?;
                        // `immediate_transaction_mode` starts the transaction in every valid
                        // database right away.
                        if roastedduck_function::SettingsSource::current_setting(
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
                                    transaction.access_database(database.name());
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
                            return Err(Error::new_bytes(ErrorKind::Io, message));
                        }
                        let rolls_back = self.transactions.is_invalidated();
                        let mut checkpoint = None;
                        if !rolls_back && self.transactions.has_current() {
                            let current = self.transactions.current();
                            let changes = CommittedChanges::of(&current);
                            checkpoint = self.commit_checkpoint(changes.as_ref());
                            self.explicit_commit = changes.filter(|_| checkpoint.is_none());
                        }
                        let result = self.transactions.commit(checkpoint);
                        self.finish_transaction_attaches(rolls_back || result.is_err());
                        result?;
                    }
                    TransactionType::Rollback => {
                        self.transactions.rollback()?;
                        self.finish_transaction_attaches(true);
                        if let Some(message) = self.invalidated_used_database() {
                            return Err(Error::new_bytes(ErrorKind::Io, message));
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
                // other type, whose storage roastedduck does not have.
                let mut path = info.path.as_bytes();
                let mut db_type = options.db_type.clone();
                if db_type.is_none()
                    && let Some(prefix) = crate::attached::extension_prefix(path)
                {
                    db_type = Some(prefix.to_ascii_lowercase().into());
                    path = &path[prefix.len() + 1..];
                }
                let duckdb_type = db_type.as_deref() == Some(b"duckdb".as_slice());
                if let Some(db_type) = db_type.filter(|db_type| db_type != "duckdb") {
                    self.load_storage_extension(&db_type)?;
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "Unrecognized storage type \"{db_type}\""
                    )));
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
            LogicalKind::Load(info) => {
                self.run_load(info).map_err(Error::caught)?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::UpdateExtensions(info) => {
                let config =
                    crate::extensions::ExtensionConfig::from_settings(|name| self.current_setting(name).ok());
                let allow_mismatch = self
                    .current_setting("allow_extensions_metadata_mismatch")?
                    .default_cast_as(&LogicalType::BOOLEAN)?
                    .as_bool()
                    .unwrap_or(false);
                let rows =
                    crate::extensions::update_extensions(info, &config, &self.database, allow_mismatch)?;
                let mut chunks = Vec::new();
                for batch in rows.chunks(roastedduck_types::STANDARD_VECTOR_SIZE) {
                    let mut chunk = roastedduck_types::DataChunk::with_types(&types, batch.len());
                    for (row, values) in batch.iter().enumerate() {
                        for (column, value) in values.iter().enumerate() {
                            chunk.column_mut(column).set_value(row, value)?;
                        }
                    }
                    chunk.set_len(batch.len());
                    chunks.push(chunk);
                }
                return Ok(StatementRun::Done(MaterializedResult::new(names, types, chunks)));
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
                let result = self.run_plan(
                    child,
                    vec![roastedduck_common::ByteString::new(); types.len()],
                    types,
                )?;
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
                let native_name = name;
                let name = self.settings.resolve_option(native_name)?;
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
                if name.eq_ignore_ascii_case("current_transaction_invalidation_policy") {
                    let text = value.to_varchar()?;
                    let policy = roastedduck_ast::TransactionInvalidationPolicy::from_name(&text)
                        .ok_or_else(|| {
                            crate::settings::enum_value_error(
                                &text,
                                "TransactionInvalidationPolicy",
                                &["STANDARD_POLICY", "ALL_ERRORS_INVALIDATE_TRANSACTION"],
                            )
                        })?;
                    self.transactions.set_invalidation_policy(policy);
                }
                self.database.instance().set_buffer_setting(name, Some(value))?;
                if crate::logging::is_logging_setting(name) {
                    crate::logging::apply_setting(&self.database.instance().log_manager, name, Some(value))?;
                }
                if name.eq_ignore_ascii_case("default_collation") {
                    self.settings.extension_collations = self.database.instance().extension_collations();
                }
                self.settings.set(name, value)?;
                let (option, set_value, set_scope) = (name, value.clone(), *scope);
                self.record_setting(option, Some(&set_value), set_scope);
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
                let native_name = name;
                let name = self.settings.resolve_option(native_name)?;
                if name.eq_ignore_ascii_case("current_transaction_invalidation_policy") {
                    self.transactions.set_invalidation_policy(
                        roastedduck_ast::TransactionInvalidationPolicy::StandardPolicy,
                    );
                }
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
                let (option, reset_scope) = (name, *scope);
                self.record_setting(option, None, reset_scope);
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
                let mut prepared = Arc::clone(prepared);
                self.prepare_aggregate_layouts(Arc::make_mut(&mut prepared))?;
                self.prepared_statements.insert(name.clone(), prepared);
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Deallocate { name } => {
                self.prepared_statements.remove(name);
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::CreateSecret(input) => {
                // `PhysicalCreateSecret`: one row with `true`.
                self.database
                    .instance()
                    .secrets
                    .create_secret(input, &self.transactions.current())?;
                let column = roastedduck_types::Vector::constant(&roastedduck_types::Value::boolean(true))?;
                return Ok(StatementRun::Done(MaterializedResult::new(
                    names,
                    types,
                    vec![roastedduck_types::DataChunk::new(vec![column], 1)],
                )));
            }
            LogicalKind::Drop(info) if info.catalog_type == roastedduck_ast::CatalogType::SecretEntry => {
                let (persist_type, storage) = match info.extra_drop_info.as_deref() {
                    Some(roastedduck_ast::ExtraDropInfo::Secret(extra)) => {
                        (extra.persist_mode, extra.secret_storage.clone())
                    }
                    None => (
                        roastedduck_ast::SecretPersistType::Default,
                        roastedduck_common::ByteString::new(),
                    ),
                };
                self.database.instance().secrets.drop_secret(
                    &info.name,
                    info.if_not_found,
                    persist_type,
                    &storage,
                    &self.transactions.current(),
                )?;
                return Ok(StatementRun::Done(nothing()));
            }
            LogicalKind::Pragma { name, parameters } => {
                let lower = name.to_ascii_lowercase();
                if lower == "add_parquet_key" {
                    // `ParquetCrypto::AddKey` (`PragmaCall` with two VARCHAR arguments).
                    let [key_name, key] = parameters.as_slice() else {
                        return Err(Error::binder(roastedduck_common::byte_format!(
                            "No function matches the given name and argument types '{name}({})'. You might need to add explicit type casts.",
                            parameters
                                .iter()
                                .map(|parameter| parameter.logical_type().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    };
                    roastedduck_parquet::crypto::add_key(
                        &self.database.instance().object_cache,
                        &key_name.to_varchar()?,
                        &key.to_varchar()?,
                    )?;
                    return Ok(StatementRun::Done(nothing()));
                }
                // `PragmaEnableProgressBar` and friends set the client's configuration.
                let progress_setting = match lower.as_bytes() {
                    b"enable_progress_bar" => Some(("enable_progress_bar", true)),
                    b"disable_progress_bar" => Some(("enable_progress_bar", false)),
                    b"enable_print_progress_bar" => Some(("enable_progress_bar_print", true)),
                    b"disable_print_progress_bar" => Some(("enable_progress_bar_print", false)),
                    _ => None,
                };
                if let Some((setting, enabled)) = progress_setting {
                    self.settings.set(setting, &Value::boolean(enabled))?;
                    return Ok(StatementRun::Done(nothing()));
                }
                if lower == "enable_optimizer" || lower == "disable_optimizer" {
                    self.settings.optimizer_disabled = lower == "disable_optimizer";
                }
                match lower.as_bytes() {
                    b"enable_profiling" | b"enable_profile" => self.settings.profiler.enable(),
                    b"disable_profiling" | b"disable_profile" => self.settings.profiler.disable(),
                    _ => {}
                }
                if lower == "disable_checkpoint_on_shutdown" || lower == "enable_checkpoint_on_shutdown" {
                    self.database.instance().shutdown_checkpoint_disabled.store(
                        lower.starts_with(b"disable"),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                if NO_OP_PRAGMAS
                    .iter()
                    .any(|pragma| pragma.as_bytes() == lower.as_bytes())
                {
                    return Ok(StatementRun::Done(nothing()));
                }
                return Err(Error::not_implemented(roastedduck_common::byte_format!(
                    "PRAGMA {name}"
                )));
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
            LogicalKind::Drop(info) if info.catalog_type == roastedduck_ast::CatalogType::SchemaEntry => {
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
                .unwrap_or_else(|| (self.default_catalog(), roastedduck_catalog::DEFAULT_SCHEMA.into()));
            if catalog == current_catalog && schema == current_schema {
                self.set_search_path(
                    "schema",
                    &roastedduck_types::Value::varchar(roastedduck_catalog::DEFAULT_SCHEMA),
                )?;
            }
        }
        Ok(StatementRun::Done(result))
    }

    /// Plans and runs a logical plan, collecting its result.
    fn execute_plan(
        &mut self,
        plan: LogicalOperator,
        names: Vec<roastedduck_common::ByteString>,
        types: Vec<LogicalType>,
    ) -> Result<MaterializedResult> {
        self.start_execution(plan, names, types, RunMode::MATERIALIZE)
            .and_then(StatementRun::into_materialized)
    }

    /// Optimizes a logical plan, plans it physically and initializes its executor
    /// (`PendingPreparedStatementInternal`); in [`RunMode::Materialize`] the plan also runs
    /// to completion.
    fn start_execution(
        &mut self,
        mut plan: LogicalOperator,
        names: Vec<roastedduck_common::ByteString>,
        types: Vec<LogicalType>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        use roastedduck_common::profiling::MetricType;
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
        let roastedduck_execution::PhysicalPlan {
            root: physical,
            estimates,
        } = physical?;
        let explain_analyze = physical.name() == "EXPLAIN_ANALYZE";
        let env = Arc::clone(&self.database.instance().env);
        let streaming = mode.streams();
        let root: PhysicalRef = if streaming {
            Arc::new(PhysicalResultCollector::streaming(
                physical,
                env.threads(),
                preserve_insertion_order,
                usize::try_from(self.settings.streaming_buffer_size()).unwrap_or(usize::MAX),
            ))
        } else {
            Arc::new(
                PhysicalResultCollector::for_plan(physical, env.threads(), preserve_insertion_order)
                    .with_memory_type(mode.memory_type()),
            )
        };
        // `Executor::Initialize`; EXPLAIN ANALYZE and profiled queries record what every
        // operator did.
        self.profiler
            .initialize(&self.settings.profiler, &root, &estimates)?;
        let profiler = (explain_analyze || self.profiler.is_running()).then(|| {
            Arc::new(
                roastedduck_execution::QueryProfiler::new()
                    .with_query_metrics(self.profiler.active_metrics().cloned())
                    .with_json_by_default(crate::query_profiler::QueryProfiler::prints_json(
                        &self.settings.profiler,
                    ))
                    .with_memory(env.buffer_manager.clone()),
            )
        });
        let options = roastedduck_execution::QueryOptions {
            profiler: profiler.clone(),
            preserve_insertion_order,
            client_interrupted: Some(self.client.interrupt_flag()),
            estimates,
            streaming_result: streaming,
            force_external: self.settings.force_external(),
        };
        let execution = match QueryExecution::new(root, env, &options) {
            Ok(execution) => execution,
            Err(error) => {
                if let Some(profiler) = &profiler {
                    self.profiler.flush(&self.settings.profiler, profiler)?;
                }
                return Err(error);
            }
        };
        let run = PlanExecution::new(execution, names, types, profiler, streaming);
        match mode {
            RunMode::Materialize(_) => self.finish_execution(run).map(StatementRun::Done),
            RunMode::Pending { .. } => Ok(StatementRun::Executing(Box::new(run))),
        }
    }

    /// Finish an exhausted streaming executor and flush its actual operator counters.
    pub(crate) fn finish_stream(&mut self, run: PlanExecution) -> Result<MaterializedResult> {
        let (execution, _, _, profiler, _) = run.into_parts();
        let state = execution.finish();
        if let Some(profiler) = &profiler {
            self.profiler.flush(&self.settings.profiler, profiler)?;
        }
        drop(state?);
        Ok(MaterializedResult::default())
    }

    /// Runs the remaining tasks of a plan and collects its result
    /// (`ClientContext::ExecutePendingQueryInternal`).
    pub(crate) fn finish_execution(&mut self, run: PlanExecution) -> Result<MaterializedResult> {
        let (execution, names, types, profiler, return_type) = run.into_parts();
        let state = execution.finish();
        if let Some(profiler) = &profiler {
            self.profiler.flush(&self.settings.profiler, profiler)?;
        }
        let rows = PhysicalResultCollector::take_rows(&state?)?;
        Ok(MaterializedResult::from_rows(names, types, rows).with_return_type(return_type))
    }
}

/// Gives every unseeded sample a seed from the connection's random engine, children
/// first, as DuckDB's physical planner does (`CreatePlan(LogicalSample &)`).
fn seed_samples(plan: &mut LogicalOperator, client: &roastedduck_function::FunctionClient) {
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
fn is_search_path_setting(name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    name.eq_ignore_ascii_case(b"schema") || name.eq_ignore_ascii_case(b"search_path")
}

/// `CatalogSearchEntry::ParseList`: comma-separated `[catalog.]schema` entries with
/// double-quoted identifiers.
fn parse_search_path(
    input: &[u8],
) -> Result<Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>> {
    let mut entries = Vec::new();
    let mut parts: Vec<roastedduck_common::ByteString> = Vec::new();
    let mut current = roastedduck_common::ByteString::new();
    let mut chars = input.iter().copied().peekable();
    let finish_entry = |parts: &mut Vec<roastedduck_common::ByteString>,
                        current: &mut roastedduck_common::ByteString,
                        entries: &mut Vec<(
        roastedduck_common::ByteString,
        roastedduck_common::ByteString,
    )>|
     -> Result<()> {
        parts.push(std::mem::take(current));
        let entry = match parts.len() {
            1 => (roastedduck_common::ByteString::new(), parts[0].clone()),
            2 => (parts[0].clone(), parts[1].clone()),
            _ => {
                return Err(Error::parser(roastedduck_common::byte_format!(
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
            b'"' => loop {
                match chars.next() {
                    Some(b'"') if chars.peek() == Some(&b'"') => {
                        chars.next();
                        current.push(b'"');
                    }
                    Some(b'"') => break,
                    Some(c) => current.push(c),
                    None => {
                        return Err(Error::parser(roastedduck_common::byte_format!(
                            "Unterminated quote in qualified name! (input: {input})"
                        )));
                    }
                }
            },
            b'.' => parts.push(std::mem::take(&mut current)),
            b',' => finish_entry(&mut parts, &mut current, &mut entries)?,
            c if c.is_ascii_whitespace() && current.is_empty() => {}
            c => current.push(c),
        }
    }
    finish_entry(&mut parts, &mut current, &mut entries)?;
    Ok(entries)
}

/// The databases a plan writes to: the catalog of DDL statements and the database of
/// the table of DML statements.
/// `ErrorManager::InvalidatedDatabase`: the error of using an invalidated instance.
pub(crate) fn invalidated_database_error(message: &[u8]) -> Error {
    const BASE: &str = "Failed: database has been invalidated because of a previous fatal error. The database must \
                        be restarted prior to being used again.\n";
    if message.windows(BASE.len()).any(|part| part == BASE.as_bytes()) {
        return Error::new_bytes(ErrorKind::Fatal, message.to_owned());
    }
    Error::new_bytes(
        ErrorKind::Fatal,
        roastedduck_common::byte_format!("{BASE}Original error: \"{message}\""),
    )
}

/// The databases whose tables a plan scans.
fn collect_read_databases(plan: &LogicalOperator, out: &mut Vec<roastedduck_common::ByteString>) {
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

/// The databases `plan` writes to; an empty catalog name (`CREATE SCHEMA` keeps the name
/// as written) is `default_catalog`.
fn collect_modified_databases(
    plan: &LogicalOperator,
    instance: &DatabaseInstance,
    default_catalog: &[u8],
    out: &mut Vec<roastedduck_common::ByteString>,
) {
    let mut push = |name: &[u8]| {
        if !name.is_empty() && !out.iter().any(|existing| existing.eq_ignore_ascii_case(name)) {
            out.push(name.into());
        }
    };
    let table_database =
        |entry: &roastedduck_catalog::CatalogEntry| -> Option<roastedduck_common::ByteString> {
            let roastedduck_catalog::EntryKind::Table(table) = &entry.kind else {
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
                .map(|database| (&*database.name()).into())
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
                    roastedduck_ast::MergeActionType::MergeUpdate
                        | roastedduck_ast::MergeActionType::MergeDelete
                        | roastedduck_ast::MergeActionType::MergeInsert
                )
            });
            if modifies && let Some(name) = table_database(&merge.table) {
                push(&name);
            }
        }
        LogicalKind::CreateTable(info) => push(&info.catalog),
        LogicalKind::Create { info, .. } if info.catalog.is_empty() => push(default_catalog),
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
        collect_modified_databases(child, instance, default_catalog, out);
    }
}

/// The database a table function call writes to, as its bind registered it (DuckDB's
/// `RegisterDBModify`, e.g. by `dbgen`).
fn table_function_modified_database(
    get: &roastedduck_planner::operator::LogicalGet,
) -> Option<roastedduck_common::ByteString> {
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
            .any(|action| action.action_type == roastedduck_ast::MergeActionType::MergeUpdate),
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
                roastedduck_ast::MergeActionType::MergeInsert | roastedduck_ast::MergeActionType::MergeDelete
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
