//! Statements prepared for repeated execution with parameter values (DuckDB's
//! `PreparedStatement` and `ClientContext::Prepare` / `PendingQuery` of a prepared
//! statement).

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_ast::{SqlStatement, StatementType};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_execution::operators::QueryResultMemoryType;
use rustdb_planner::expression::BoundParameterData;
use rustdb_planner::{Planner, PreparedStatementData, StatementReturnType};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::active_query::{PendingQuery, QueryResult, RunMode, StatementRun};
use crate::Connection;
use crate::transaction::TransactionContext;

/// A prepared statement (DuckDB's `PreparedStatement`): its parameters, result columns
/// and the statement to bind again with the values of each execution.
#[derive(Clone, Debug)]
pub struct PreparedStatement {
    query: String,
    data: Arc<PreparedStatementData>,
}

impl PreparedStatement {
    /// The statement's text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The parameters with their 1-based indexes, in DuckDB's order (`named_param_map`):
    /// `?` and `$n` parameters are named by their index, `$name` ones by their name.
    pub fn named_parameters(&self) -> impl Iterator<Item = (&str, u64)> {
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
    pub fn parameter_type(&self, identifier: &str) -> Option<LogicalType> {
        self.data.parameter_type(identifier)
    }

    /// The names of the result columns.
    pub fn names(&self) -> &[String] {
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
    /// Prepares the single statement in `query` (DuckDB's `ClientContext::Prepare`).
    pub fn prepare(&mut self, query: &str) -> Result<PreparedStatement> {
        self.initial_cleanup();
        let mut statements = self.parse_statements(query)?;
        if statements.len() > 1 {
            return Err(Error::invalid_input(
                "Cannot prepare multiple statements at once!",
            ));
        }
        let statement = statements
            .pop()
            .ok_or_else(|| Error::invalid_input("No statement to prepare!"))?;
        self.prepare_statement(statement)
            .map_err(|error| error.with_query_context(query))
    }

    /// The tables the single statement in `query` uses, by name or by qualified name
    /// (DuckDB's `ClientContext::GetTableNames`): the statement is bound without being
    /// planned, so tables need not exist.
    pub fn table_names(&mut self, query: &str, qualified: bool) -> Result<BTreeSet<String>> {
        let mut statements = self.extract_statements(query)?;
        if statements.len() != 1 {
            return Err(Error::invalid_input("Expected a single statement"));
        }
        let statement = statements.remove(0);
        let transaction = self.transactions.current();
        let planner = Planner::new(self.client_context(transaction));
        let names = planner.table_names(statement, qualified);
        let ended = self.transactions.end_statement(names.is_ok(), false);
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
        let prepared = self.in_query_scope(|_| planner.prepare(statement));
        let invalidates = prepared
            .as_ref()
            .is_err_and(|error| super::invalidates_transaction(error.kind()));
        let ended = self.transactions.end_statement(prepared.is_ok(), invalidates);
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

    /// Starts the prepared statement with `values` for its parameters, by identifier,
    /// without running its plan (DuckDB's `PreparedStatement::PendingQuery`); the result
    /// streams if `allow_stream` and the statement allows it.
    pub fn pending_prepared(
        &mut self,
        prepared: &PreparedStatement,
        values: &CaseInsensitiveMap<Value>,
        allow_stream: bool,
    ) -> Result<PendingQuery> {
        rustdb_planner::verify_parameters(values, &prepared.data.unbound_statement.named_param_map)?;
        self.initial_cleanup();
        let query = prepared.query.clone();
        self.begin_query(&query, &prepared.data.unbound_statement)?;
        let started = self.in_query_scope(|connection| {
            connection.begin_query_logging(&query);
            connection.start_prepared(&prepared.data, values, RunMode::Pending {
                stream: allow_stream,
                memory_type: QueryResultMemoryType::InMemory,
            })
        });
        self.activate(&query, prepared.statement_type(), started)
    }

    /// Runs the prepared statement with `values` (DuckDB's `PreparedStatement::Execute`).
    pub fn execute_prepared(
        &mut self,
        prepared: &PreparedStatement,
        values: &CaseInsensitiveMap<Value>,
        allow_stream: bool,
    ) -> Result<QueryResult> {
        let pending = self.pending_prepared(prepared, values, allow_stream)?;
        self.execute_pending(&pending)
    }

    /// Binds the prepared statement again with the values as constants and starts its
    /// plan (`RebindPreparedStatement`, then `PendingPreparedStatementInternal`). A value
    /// is typed like DuckDB's `BoundParameterData(Value)`: strings as string literals.
    fn start_prepared(
        &mut self,
        prepared: &PreparedStatementData,
        values: &CaseInsensitiveMap<Value>,
        mode: RunMode,
    ) -> Result<StatementRun> {
        use rustdb_common::profiling::MetricType;
        prepared.check_parameter_count(values.len())?;
        let mut parameters = CaseInsensitiveMap::default();
        for (identifier, value) in values.iter() {
            parameters.insert(identifier.to_owned(), parameter_data(value.clone()));
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
        && rustdb_planner::expression_binder::collation(value.logical_type()).is_some()
    {
        return BoundParameterData {
            return_type: value.logical_type().clone(),
            value,
        };
    }
    BoundParameterData::from_value(value)
}
