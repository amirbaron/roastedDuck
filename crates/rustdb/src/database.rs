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
    /// The database's own catalog (`memory` for in-memory databases).
    pub(crate) catalog: Arc<Catalog>,
    /// The cast functions (DuckDB's `DBConfig::GetCastFunctions`).
    pub(crate) casts: Arc<CastFunctionSet>,
    /// Buffer manager and thread count used by every query.
    pub(crate) env: Arc<rustdb_execution::ClientEnv>,
    /// Where tables keep their data (in memory for now).
    pub(crate) io: Arc<rustdb_storage::TableIoManager>,
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
        if !path.is_empty() {
            return Err(Error::not_implemented(
                "persistent databases are not supported yet",
            ));
        }
        let system_catalog = Catalog::new("system")?;
        for set in rustdb_functions::builtin_scalar_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        for set in rustdb_functions::builtin_table_functions() {
            system_catalog.register_table_functions(set)?;
        }
        for set in crate::sequence::sequence_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        for set in crate::checkpoint::checkpoint_functions(&["memory".to_owned(), "system".to_owned()]) {
            system_catalog.register_table_functions(set)?;
        }
        let env = rustdb_execution::ClientEnv::standalone()?;
        let io = Arc::new(rustdb_storage::TableIoManager::new(
            env.buffer_manager.clone(),
            Arc::new(rustdb_buffer::InMemoryBlockManager::default()),
        ));
        let instance = DatabaseInstance {
            path,
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            catalog: Arc::new(Catalog::new("memory")?),
            casts: Arc::new(CastFunctionSet::new()),
            env,
            io,
        };
        Ok(Self {
            instance: Arc::new(instance),
        })
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
            prepared_statements: CaseInsensitiveMap::default(),
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
    prepared_statements: CaseInsensitiveMap<Arc<PreparedStatementData>>,
}

impl Connection {
    pub fn database(&self) -> &Database {
        &self.database
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
        let result = outcome.and_then(|result| ended.map(|()| result));
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

    /// The planner's view of this connection, in `transaction`.
    fn client_context(&self, transaction: Arc<Transaction>) -> Arc<ClientContext> {
        let instance = self.database.instance();
        Arc::new(ClientContext {
            catalogs: vec![
                Arc::clone(&instance.catalog),
                Arc::clone(&instance.system_catalog),
            ],
            transaction,
            casts: Arc::clone(&instance.casts),
            evaluator: Arc::new(ScalarEvaluator),
            config: self.settings.planner.clone(),
            prepared_statements: self.prepared_statements.clone(),
        })
    }

    fn run_statement(&mut self, statement: SqlStatement) -> Result<MaterializedResult> {
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
        self.run_plan(plan, names, types)
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
                    TransactionType::BeginTransaction => self.transactions.begin()?,
                    TransactionType::Commit => self.transactions.commit()?,
                    TransactionType::Rollback => self.transactions.rollback()?,
                    TransactionType::Invalid => {
                        return Err(Error::not_implemented("Unrecognized transaction type!"));
                    }
                }
                return Ok(nothing());
            }
            LogicalKind::Set { name, value, .. } => {
                self.settings.set(name, value)?;
                return Ok(nothing());
            }
            LogicalKind::Reset { name, .. } => {
                self.settings.reset(name)?;
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
            catalogs: vec![
                Arc::clone(&instance.catalog),
                Arc::clone(&instance.system_catalog),
            ],
            io: Arc::clone(&instance.io),
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
