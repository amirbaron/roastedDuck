use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{SqlStatement, TransactionType};
use rustdb_catalog::{Catalog, Transaction, TransactionManager};
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_execution::operators::{CollectedResult, PhysicalResultCollector};
use rustdb_execution::{Executor, PhysicalRef, PlanContext, ScalarEvaluator, create_physical_plan_with};
use rustdb_function::cast::CastFunctionSet;
use rustdb_planner::{
    ClientContext, ColumnBindingResolver, LogicalKind, LogicalOperator, PlannedStatement, Planner,
    PreparedStatementData,
};
use rustdb_types::LogicalType;

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

    /// Checkpoints databases whose WAL a commit grew beyond `checkpoint_threshold`,
    /// unless other transactions are running (`DuckTransactionManager::CanCheckpoint`).
    fn auto_checkpoint(&self) -> Result<()> {
        let threshold = self
            .checkpoint_wal_size
            .load(std::sync::atomic::Ordering::Relaxed);
        for database in self.databases.all() {
            let Some(storage) = &database.storage else {
                continue;
            };
            let Some(wal) = storage.wal() else {
                continue;
            };
            if wal.size() <= threshold || self.transactions.active_count() > 0 {
                continue;
            }
            crate::storage::create_checkpoint(
                &database.catalog,
                storage,
                crate::storage::CheckpointMode::Automatic,
                Some(&self.transactions),
            )?;
        }
        Ok(())
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
        crate::system_functions::register_default_views(&system_catalog)?;
        let env = rustdb_execution::ClientEnv::standalone()?;
        let instance = DatabaseInstance {
            path: path.clone(),
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            databases: DatabaseManager::default(),
            default_database: std::sync::OnceLock::new(),
            casts: Arc::new(CastFunctionSet::new()),
            env,
            shutdown_checkpoint_disabled: std::sync::atomic::AtomicBool::new(false),
            checkpoint_wal_size: std::sync::atomic::AtomicU64::new(DEFAULT_CHECKPOINT_WAL_SIZE),
            compression: std::sync::Mutex::default(),
        };
        for (name, value) in &config.options {
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
        Ok(Connection {
            database: self.clone(),
            transactions,
            settings,
            search_path: Vec::new(),
            transaction_attaches: Vec::new(),
            prepared_statements: CaseInsensitiveMap::default(),
            function_client: Arc::default(),
        })
    }

    /// Loads a (statically linked) extension, like `LOAD name`.
    pub fn load_extension(&self, name: &str) -> ExtensionLoad {
        match name {
            "core_functions" | "parquet" | "json" | "icu" | "tpch" | "tpcds" | "autocomplete" | "httpfs"
            | "fts" | "inet" | "excel" | "sqlsmith" | "spatial" | "delta" | "iceberg" | "aws" | "azure"
            | "motherduck" | "postgres_scanner" | "mysql_scanner" | "sqlite_scanner" | "vss" | "ui"
            | "encodings" | "ducklake" | "avro" | "demo_capi" => ExtensionLoad::NotAvailable,
            _ => ExtensionLoad::Unknown,
        }
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
    transactions: TransactionContext,
    settings: Settings,
    /// The `(catalog, schema)` entries set by `USE` / `SET search_path` (DuckDB's
    /// `ClientData::catalog_search_path`).
    search_path: Vec<(String, String)>,
    prepared_statements: CaseInsensitiveMap<Arc<PreparedStatementData>>,
    /// Connection state functions use (DuckDB keeps it in the `ClientContext`).
    function_client: Arc<rustdb_function::FunctionClient>,
    /// Databases attached by the open explicit transaction; rolling it back detaches
    /// them (the `ATTACHED_DATABASE` undo entries).
    transaction_attaches: Vec<Arc<AttachedDatabase>>,
}

impl Connection {
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Parses `sql` into its statements (DuckDB's `ClientContext::ExtractStatements`).
    pub fn extract_statements(&self, sql: &str) -> Result<Vec<SqlStatement>> {
        rustdb_parser::parse(sql).map_err(|error| error.with_query_context(sql))
    }

    /// Runs every statement in `sql` and returns the result of the last one (DuckDB's
    /// `ClientContext::Query`).
    pub fn query(&mut self, sql: &str) -> Result<MaterializedResult> {
        let statements = rustdb_parser::parse(sql).map_err(|error| error.with_query_context(sql))?;
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
        let outcome = self.run_statement(statement);
        let invalidates = outcome
            .as_ref()
            .is_err_and(|error| invalidates_transaction(error.kind()));
        let ended = self.transactions.end_statement(outcome.is_ok(), invalidates);
        let mut result = outcome.and_then(|result| ended.map(|()| result));
        if result.is_ok()
            && !self.transactions.has_current()
            && let Err(error) = self.database.instance().auto_checkpoint()
        {
            result = Err(error);
        }
        result.map_err(|error| error.with_query_context(query))
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

    /// The planner's view of this connection, in `transaction`.
    fn client_context(&self, transaction: Arc<Transaction>) -> Arc<ClientContext> {
        let instance = self.database.instance();
        Arc::new(ClientContext {
            catalogs: instance.catalogs(instance.default_database_name()),
            search_path: self.search_path.clone(),
            transaction,
            casts: Arc::clone(&instance.casts),
            evaluator: Arc::new(ScalarEvaluator),
            config: self.settings.planner.clone(),
            prepared_statements: self.prepared_statements.clone(),
            function_client: Arc::clone(&self.function_client),
        })
    }

    fn run_statement(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
        if let rustdb_ast::SqlStatementKind::Multi(statements) = statement.kind {
            return self.run_multi_statement(statements);
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
            let current = instance.databases.get(&attached.name);
            if current.is_some_and(|current| Arc::ptr_eq(&current, &attached))
                && let Some(database) = instance.databases.remove(&attached.name)
            {
                database.close(true);
            }
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
        match instance.databases.remove(&info.name) {
            Some(database) => {
                database.close(true);
                Ok(())
            }
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
        let entries = parse_search_path(&value.to_varchar()?)?;
        if set_schema && entries.len() != 1 {
            return Err(Error::catalog(format!(
                "{set_name} can set only 1 schema. This has {}",
                entries.len()
            )));
        }
        let instance = self.database.instance();
        let transaction = self.transactions.current();
        let mut resolved = Vec::with_capacity(entries.len());
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
                    continue;
                }
                let default = self.default_catalog();
                let database = instance.databases.get(&default).ok_or_else(not_found)?;
                let found = database
                    .catalog
                    .schema(&transaction, &schema)
                    .ok_or_else(not_found)?;
                resolved.push((database.name.clone(), found.name().to_owned()));
                continue;
            }
            let database = instance.databases.get(&catalog).ok_or_else(not_found)?;
            let found = database
                .catalog
                .schema(&transaction, &schema)
                .ok_or_else(not_found)?;
            resolved.push((database.name.clone(), found.name().to_owned()));
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
        Ok(())
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
            LogicalKind::Set { name, value, .. } if is_search_path_setting(name) => {
                self.set_search_path(name, value)?;
                return Ok(nothing());
            }
            LogicalKind::Reset { name, .. } if is_search_path_setting(name) => {
                self.search_path.clear();
                return Ok(nothing());
            }
            LogicalKind::Set { name, value, .. } => {
                self.settings.set(name, value)?;
                if is_checkpoint_threshold(name) {
                    self.database.instance().set_checkpoint_threshold(value)?;
                }
                if is_force_compression(name) {
                    self.database.instance().set_force_compression(Some(value))?;
                }
                return Ok(nothing());
            }
            LogicalKind::Reset { name, .. } => {
                self.settings.reset(name)?;
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
            LogicalKind::Pragma { name, .. } => {
                let lower = name.to_ascii_lowercase();
                if lower == "disable_checkpoint_on_shutdown" || lower == "enable_checkpoint_on_shutdown" {
                    self.database
                        .instance()
                        .shutdown_checkpoint_disabled
                        .store(lower.starts_with("disable"), std::sync::atomic::Ordering::Relaxed);
                }
                return if NO_OP_PRAGMAS.contains(&name.to_ascii_lowercase().as_str()) {
                    Ok(nothing())
                } else {
                    Err(Error::not_implemented(format!("PRAGMA {name}")))
                };
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
        ColumnBindingResolver::resolve(&mut plan)?;
        let instance = self.database.instance();
        let context = PlanContext {
            transaction: self.transactions.current(),
            catalogs: instance.catalogs(&self.default_catalog()),
        };
        let physical = create_physical_plan_with(plan, &context)?;
        let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(physical));
        let state = Executor::execute(&root, Arc::clone(&self.database.instance().env))?;
        let collected = state
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("result collector state has an unexpected type"))?;
        let chunks = std::mem::take(&mut collected.lock().unwrap_or_else(PoisonError::into_inner).chunks);
        Ok(MaterializedResult::new(names, types, chunks))
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
