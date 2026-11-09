//! Statements prepared for repeated execution with parameter values (DuckDB's
//! `PreparedStatement` and `ClientContext::Prepare` / `PendingQuery` of a prepared
//! statement).

use std::collections::BTreeSet;
use std::sync::Arc;

use roastedduck_ast::{SqlStatement, StatementType};
use roastedduck_common::{CaseInsensitiveMap, Error, Result};
use roastedduck_execution::operators::QueryResultMemoryType;
use roastedduck_planner::expression::BoundParameterData;
use roastedduck_planner::{Planner, PreparedStatementData, StatementReturnType};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

use super::active_query::{PendingQuery, QueryResult, RunMode, StatementRun};
use crate::Connection;
use crate::transaction::TransactionContext;

/// A prepared statement (DuckDB's `PreparedStatement`): its parameters, result columns
/// and the statement to bind again with the values of each execution.
#[derive(Clone, Debug)]
pub struct PreparedStatement {
    query: roastedduck_common::ByteString,
    data: Arc<PreparedStatementData>,
}

impl PreparedStatement {
    /// The statement's text.
    pub fn query(&self) -> &[u8] {
        &self.query
    }

    /// The parameters with their 1-based indexes, in DuckDB's order (`named_param_map`):
    /// `?` and `$n` parameters are named by their index, `$name` ones by their name.
    pub fn named_parameters(&self) -> impl Iterator<Item = (&roastedduck_common::ByteString, u64)> {
        self.data
            .unbound_statement
            .named_param_map
            .iter()
            .map(|(name, index)| (name, *index))
    }

    /// The number of parameters.
    pub fn parameter_count(&self) -> usize {
        self.data.unbound_statement.named_param_map.len()
    }

    /// The type the parameter `identifier` was bound to, if it could be resolved
    /// (`PreparedStatementData::TryGetType`).
    pub fn parameter_type(&self, identifier: impl AsRef<[u8]>) -> Option<LogicalType> {
        self.data.parameter_type(identifier)
    }

    /// The names of the result columns.
    pub fn names(&self) -> &[roastedduck_common::ByteString] {
        &self.data.names
    }

    /// The types of the result columns.
    pub fn types(&self) -> &[LogicalType] {
        &self.data.types
    }

    pub fn statement_type(&self) -> StatementType {
        self.data.statement_type
    }

    /// What executing the statement returns.
    pub fn return_type(&self) -> StatementReturnType {
        self.data.properties.return_type
    }
}

impl Connection {
    /// Test a relational table lookup without executing a query or closing a stream.
    /// Views use the Python client's query/replacement-scan fallback instead.
    pub fn relation_is_base_table(&mut self, catalog: &[u8], schema: &[u8], name: &[u8]) -> Result<bool> {
        if self.transactions.is_invalidated() {
            return Err(TransactionContext::aborted_error());
        }
        let owns_transaction = !self.transactions.has_current();
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let context = self.client_context(transaction);
        let outcome = self.in_query_scope(|_| {
            context
                .get_entry(
                    catalog,
                    schema,
                    roastedduck_catalog::CatalogType::Table,
                    name,
                    true,
                )
                .map(|entry| {
                    entry.is_some_and(|entry| {
                        matches!(&entry.entry.kind, roastedduck_catalog::EntryKind::Table(_))
                    })
                })
        });
        let invalidates = outcome
            .as_ref()
            .is_err_and(|error| super::invalidates_transaction(error.kind()));
        let ended = if owns_transaction || !self.transactions.is_auto_commit() {
            self.end_client_statement(outcome.is_ok(), invalidates)
        } else {
            Ok(())
        };
        outcome.and_then(|found| ended.map(|()| found))
    }

    /// Resolve a type during a callback on this connection without borrowing its
    /// already executing mutable connection again. Uses the active transaction's
    /// immutable binder context; `None` means the caller is outside this query.
    pub fn type_from_current_query(connection_id: u64, name: &[u8]) -> Option<Result<LogicalType>> {
        let client = roastedduck_function::file_system::opener()?;
        if client.connection_id() != connection_id {
            return None;
        }
        let context = client
            .transaction_context()
            .and_then(|context| context.downcast::<roastedduck_planner::ClientContext>().ok());
        Some(
            context
                .ok_or_else(|| Error::internal("Missing active type lookup context"))
                .and_then(|context| {
                    roastedduck_csv::read_csv::transform_bytes_to_logical_type(&context, name)
                }),
        )
    }
    /// Bind relational metadata in the current transaction without closing an open
    /// result (`ClientContext::TryBindRelation`). This does not execute the relation.
    pub fn relation_columns(
        &mut self,
        statement: &mut SqlStatement,
    ) -> Result<(Vec<roastedduck_common::ByteString>, Vec<LogicalType>)> {
        if self.transactions.is_invalidated() {
            return Err(TransactionContext::aborted_error());
        }
        let owns_transaction = !self.transactions.has_current();
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let outcome = self.in_query_scope(|_| {
            planner
                .create_relation_plan(statement)
                .map(|plan| (plan.names, plan.types))
        });
        let invalidates = outcome
            .as_ref()
            .is_err_and(|error| super::invalidates_transaction(error.kind()));
        let ended = if owns_transaction || !self.transactions.is_auto_commit() {
            self.end_client_statement(outcome.is_ok(), invalidates)
        } else {
            Ok(())
        };
        outcome.and_then(|columns| ended.map(|()| columns))
    }

    /// Resolve a SQL type name without preparing a query or closing an active result
    /// (`TransformStringToLogicalType` inside `RunFunctionInTransaction`).
    pub fn type_from_name(&mut self, name: &str) -> Result<LogicalType> {
        self.type_from_name_bytes(name.as_bytes())
    }

    /// Type-name lookup preserving native identifier bytes.
    pub fn type_from_name_bytes(&mut self, name: &[u8]) -> Result<LogicalType> {
        if self.transactions.is_invalidated() {
            return Err(TransactionContext::aborted_error());
        }
        let owns_transaction = !self.transactions.has_current();
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let context = self.client_context(transaction);
        let outcome = self
            .in_query_scope(|_| roastedduck_csv::read_csv::transform_bytes_to_logical_type(&context, name));
        let invalidates = outcome
            .as_ref()
            .is_err_and(|e| super::invalidates_transaction(e.kind()));
        let ended = if owns_transaction || !self.transactions.is_auto_commit() {
            self.end_client_statement(outcome.is_ok(), invalidates)
        } else {
            Ok(())
        };
        outcome.and_then(|kind| ended.map(|()| kind))
    }

    /// Resolves the native CSV dialect/schema once for a client relation, retaining
    /// the caller's active stream and explicit transaction during metadata binding.
    pub fn csv_relation_options(
        &mut self,
        paths: &[roastedduck_common::ByteString],
        parameters: &[(roastedduck_common::ByteString, Value)],
    ) -> Result<roastedduck_csv::read_csv::CsvRelationOptions> {
        if self.transactions.is_invalidated() {
            return Err(TransactionContext::aborted_error());
        }
        let owns_transaction = !self.transactions.has_current();
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let context = self.client_context(transaction);
        let outcome =
            self.in_query_scope(|_| roastedduck_csv::read_csv::relation_options(&context, paths, parameters));
        let invalidates = outcome
            .as_ref()
            .is_err_and(|e| super::invalidates_transaction(e.kind()));
        let ended = if owns_transaction || !self.transactions.is_auto_commit() {
            self.end_client_statement(outcome.is_ok(), invalidates)
        } else {
            Ok(())
        };
        outcome.and_then(|value| ended.map(|()| value))
    }

    /// Prepares the single statement in `query` (DuckDB's `ClientContext::Prepare`).
    pub fn prepare(&mut self, query: impl AsRef<[u8]>) -> Result<PreparedStatement> {
        let query = query.as_ref();
        self.initial_cleanup();
        let mut statements = self.parse_statements_bytes(query)?;
        if statements.len() > 1 {
            return Err(Error::invalid_input(
                "Cannot prepare multiple statements at once!",
            ));
        }
        let statement = statements
            .pop()
            .ok_or_else(|| Error::invalid_input("No statement to prepare!"))?;
        self.prepare_statement(statement)
            .map_err(|error| self.process_error(error, query))
    }

    /// The tables the single statement in `query` uses, by name or by qualified name
    /// (DuckDB's `ClientContext::GetTableNames`): the statement is bound without being
    /// planned, so tables need not exist.
    pub fn table_names(
        &mut self,
        query: impl AsRef<[u8]>,
        qualified: bool,
    ) -> Result<BTreeSet<roastedduck_common::ByteString>> {
        let mut statements = self.parse_statements_bytes(query.as_ref())?;
        if statements.len() != 1 {
            return Err(Error::invalid_input("Expected a single statement"));
        }
        let statement = statements.remove(0);
        if self.transactions.is_invalidated() {
            return Err(TransactionContext::aborted_error());
        }
        let owns_transaction = !self.transactions.has_current();
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let names = self.in_query_scope(|_| planner.table_names(statement, qualified));
        let ended = if owns_transaction || !self.transactions.is_auto_commit() {
            self.end_client_statement(names.is_ok(), false)
        } else {
            Ok(())
        };
        names.and_then(|names| ended.map(|()| names))
    }

    /// Prepares a parsed statement (DuckDB's `ClientContext::PrepareInternal`): it is bound
    /// in the current transaction, or in one of its own outside a transaction
    /// (`RunFunctionInTransactionInternal`).
    pub fn prepare_statement(&mut self, statement: SqlStatement) -> Result<PreparedStatement> {
        self.initial_cleanup();
        self.sync_global_settings();
        let query = statement.query.clone();
        self.function_client.set_query(&query);
        self.function_client
            .set_settings(Arc::new(self.settings_snapshot()));
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let prepared = self.in_query_scope(|connection| -> Result<PreparedStatementData> {
            let mut prepared = planner.prepare(statement)?;
            connection.prepare_aggregate_layouts(&mut prepared)?;
            Ok(prepared)
        });
        let invalidates = prepared
            .as_ref()
            .is_err_and(|error| super::invalidates_transaction(error.kind()));
        let ended = self.end_client_statement(prepared.is_ok(), invalidates);
        let prepared = prepared.and_then(|prepared| ended.map(|()| prepared));
        match prepared {
            Ok(data) => Ok(PreparedStatement {
                query,
                data: Arc::new(data),
            }),
            Err(error) => {
                // A statement that fails to prepare leaves no profile behind.
                self.profiler.reset();
                Err(self.process_error(error, &query))
            }
        }
    }

    /// State-size callbacks are physical-planning metadata, not aggregate execution.
    /// Resolve them after optimization, without initializing states or running a plan.
    pub(super) fn prepare_aggregate_layouts(&mut self, prepared: &mut PreparedStatementData) -> Result<()> {
        // Preparing SQL PREPARE also prepares its contained statement. Optimize that
        // statement separately before resolving its aggregate metadata.
        fn nested(
            connection: &mut Connection,
            plan: &mut roastedduck_planner::LogicalOperator,
        ) -> Result<()> {
            for child in &mut plan.children {
                nested(connection, child)?;
            }
            if let roastedduck_planner::LogicalKind::Prepare { prepared, .. } = &mut plan.kind {
                connection.prepare_aggregate_layouts(Arc::make_mut(prepared))?;
            }
            Ok(())
        }
        let Some(plan) = prepared.preparation_plan.take() else {
            return Ok(());
        };
        if !prepared.properties.bound_all_parameters {
            return Ok(());
        }
        let transaction = self.transactions.current();
        let client = self.client_context(transaction);
        let mut optimizer = roastedduck_optimizer::Optimizer::new(client, self.settings.optimizer_config());
        let mut plan = optimizer.optimize(plan)?;
        nested(self, &mut plan)?;
        roastedduck_execution::verify_aggregate_state_layouts(&mut plan)
    }

    /// Starts the prepared statement with `values` for its parameters, by identifier,
    /// without running its plan (DuckDB's `PreparedStatement::PendingQuery`); the result
    /// streams if `allow_stream` and the statement allows it.
    pub fn pending_prepared<K: AsRef<[u8]>>(
        &mut self,
        prepared: &PreparedStatement,
        values: &CaseInsensitiveMap<Value, K>,
        allow_stream: bool,
    ) -> Result<PendingQuery> {
        roastedduck_planner::verify_parameters(values, &prepared.data.unbound_statement.named_param_map)?;
        self.initial_cleanup();
        let query = prepared.query.clone();
        self.begin_query(&query, &prepared.data.unbound_statement)?;
        let started = self.in_query_scope(|connection| {
            connection.begin_query_logging(&query);
            connection.start_prepared(
                &prepared.data,
                values,
                RunMode::Pending {
                    stream: allow_stream,
                    memory_type: QueryResultMemoryType::InMemory,
                },
            )
        });
        self.activate(&query, prepared.statement_type(), started)
    }

    /// Runs the prepared statement with `values` (DuckDB's `PreparedStatement::Execute`).
    pub fn execute_prepared<K: AsRef<[u8]>>(
        &mut self,
        prepared: &PreparedStatement,
        values: &CaseInsensitiveMap<Value, K>,
        allow_stream: bool,
    ) -> Result<QueryResult> {
        let pending = self.pending_prepared(prepared, values, allow_stream)?;
        self.execute_pending(&pending)
    }

    /// Binds the prepared statement again with the values as constants and starts its
    /// plan (`RebindPreparedStatement`, then `PendingPreparedStatementInternal`). A value
    /// is typed like DuckDB's `BoundParameterData(Value)`: strings as string literals.
    fn start_prepared<K: AsRef<[u8]>>(
        &mut self,
        prepared: &PreparedStatementData,
        values: &CaseInsensitiveMap<Value, K>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        use roastedduck_common::profiling::MetricType;
        prepared.check_parameter_count(values.len())?;
        let mut parameters = CaseInsensitiveMap::default();
        for (identifier, value) in values.iter_keys() {
            parameters.insert(
                roastedduck_common::ByteString::from(identifier.as_ref()),
                parameter_data(value.clone()),
            );
        }
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::Planner);
        self.profiler
            .start_phase(&self.settings.profiler, MetricType::PlannerBinding);
        let rebound = planner.prepare_with_values(prepared.unbound_statement.clone(), parameters);
        self.profiler.end_phase(&self.settings.profiler);
        self.profiler.end_phase(&self.settings.profiler);
        let (rebound, plan) = rebound?;
        // `CheckIfPreparedStatementIsExecutable`
        if self.transactions.is_invalidated() && rebound.properties.requires_valid_transaction {
            return Err(TransactionContext::aborted_error());
        }
        self.start_bound_plan(
            plan,
            rebound.names,
            rebound.types,
            &rebound.properties,
            rebound.statement_type,
            mode,
        )
    }
}

/// `BoundParameterData(Value)`: the value's own type, except that a VARCHAR without a
/// collation is a string literal, so that it still casts implicitly.
fn parameter_data(value: Value) -> BoundParameterData {
    if value.logical_type().id() == LogicalTypeId::Varchar
        && roastedduck_planner::expression_binder::collation(value.logical_type()).is_some()
    {
        return BoundParameterData {
            return_type: value.logical_type().clone(),
            value,
        };
    }
    BoundParameterData::from_value(value)
}
