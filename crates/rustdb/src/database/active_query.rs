//! Queries the caller executes step by step, and streaming results (DuckDB's
//! `PendingQueryResult`, `StreamQueryResult` and the `ActiveQueryContext` of the
//! `ClientContext`).
//!
//! A connection has at most one active query. [`Connection::pending_query`] starts a
//! statement and leaves its physical plan to the caller: [`Connection::execute_task`] works
//! on it a task at a time, [`Connection::execute_pending`] runs it to completion and returns
//! its result, which may be a [`StreamQueryResult`] whose rows are produced as
//! [`Connection::fetch`] asks for them. Starting another query closes the active one; its
//! pending or streaming result then reports that it was closed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{SqlStatement, StatementType};
use rustdb_common::{Error, Result};
use rustdb_execution::operators::{PhysicalResultCollector, QueryResultMemoryType};
use rustdb_execution::{ExecutionStep, QueryExecution, QueryProfiler};
use rustdb_planner::StatementReturnType;
use rustdb_types::{DataChunk, LogicalType};

use crate::{Connection, MaterializedResult};

/// How the plan of a statement is run (DuckDB's `QueryResultOutputType`, and whether the
/// caller drives the execution).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Run to completion, materializing the result as `memory_type` says
    /// (`Connection::Query`).
    Materialize(QueryResultMemoryType),
    /// Initialize the executor and return; the caller runs the tasks (`PendingQuery`).
    /// With `stream`, the result is handed out while the query runs; otherwise it is
    /// materialized as `memory_type` says.
    Pending {
        stream: bool,
        memory_type: QueryResultMemoryType,
    },
}

impl RunMode {
    /// Run to completion with the result on the heap (`Connection::Query(string)`).
    pub(crate) const MATERIALIZE: Self = Self::Materialize(QueryResultMemoryType::InMemory);

    /// Where a materialized result keeps its rows.
    pub(crate) fn memory_type(self) -> QueryResultMemoryType {
        match self {
            Self::Materialize(memory_type) | Self::Pending { memory_type, .. } => memory_type,
        }
    }

    /// Whether the result streams.
    pub(crate) fn streams(self) -> bool {
        matches!(self, Self::Pending { stream: true, .. })
    }
}

/// A started statement.
#[derive(Debug)]
pub(crate) enum StatementRun {
    /// The statement completed while it was started: statements the connection runs
    /// itself, and every statement run in [`RunMode::Materialize`].
    Done(MaterializedResult),
    /// The statement's physical plan runs in an executor.
    Executing(Box<PlanExecution>),
}

impl StatementRun {
    /// The result of a statement run in [`RunMode::Materialize`].
    pub(crate) fn into_materialized(self) -> Result<MaterializedResult> {
        match self {
            Self::Done(result) => Ok(result),
            Self::Executing(_) => Err(Error::internal("a materialized statement left its plan running")),
        }
    }

    /// Records what the statement returns.
    #[must_use]
    pub(crate) fn with_return_type(self, return_type: StatementReturnType) -> Self {
        match self {
            Self::Done(result) => Self::Done(result.with_return_type(return_type)),
            Self::Executing(mut run) => {
                run.return_type = return_type;
                Self::Executing(run)
            }
        }
    }
}

/// A physical plan being executed for a statement (DuckDB's `ActiveQueryContext::executor`
/// with the prepared statement's names and types).
#[derive(Debug)]
pub(crate) struct PlanExecution {
    execution: QueryExecution,
    names: Vec<String>,
    types: Vec<LogicalType>,
    return_type: StatementReturnType,
    /// Records what every operator did (EXPLAIN ANALYZE, profiling).
    profiler: Option<Arc<QueryProfiler>>,
    /// Whether the root is a streaming collector.
    streaming: bool,
    /// Chunks taken from the streaming collector and not handed out yet.
    buffered: std::collections::VecDeque<DataChunk>,
    /// Whether the plan ran to completion.
    finished: bool,
}

impl PlanExecution {
    pub(crate) fn new(
        execution: QueryExecution,
        names: Vec<String>,
        types: Vec<LogicalType>,
        profiler: Option<Arc<QueryProfiler>>,
        streaming: bool,
    ) -> Self {
        Self {
            execution,
            names,
            types,
            return_type: StatementReturnType::QueryResult,
            profiler,
            streaming,
            buffered: std::collections::VecDeque::new(),
            finished: false,
        }
    }

    /// The executor, names, types, profiler and return type, to finish a materialized run.
    pub(crate) fn into_parts(
        self,
    ) -> (
        QueryExecution,
        Vec<String>,
        Vec<LogicalType>,
        Option<Arc<QueryProfiler>>,
        StatementReturnType,
    ) {
        (
            self.execution,
            self.names,
            self.types,
            self.profiler,
            self.return_type,
        )
    }

    /// Moves the chunks the streaming collector received into the buffer.
    fn take_collected(&mut self) -> Result<()> {
        if let Some(chunks) = self
            .execution
            .with_root_sink_state(PhysicalResultCollector::take_chunks)
        {
            self.buffered.extend(chunks?);
        }
        Ok(())
    }

    /// Runs the query's tasks until its streaming result is ready (DuckDB's
    /// `PendingQueryResult::ExecuteInternal`): the query finished, or the collector holds
    /// `buffer_size` bytes, where DuckDB's `PhysicalBufferedCollector` blocks. An error in
    /// the first rows thus fails the query before the caller reads any of them.
    fn fill_buffer(&mut self, buffer_size: u64) -> Result<()> {
        loop {
            self.take_collected()?;
            let buffered: usize = self.buffered.iter().map(DataChunk::allocation_size).sum();
            if self.finished || buffered as u64 >= buffer_size {
                return Ok(());
            }
            match self.execution.execute_task()? {
                ExecutionStep::Finished => self.finished = true,
                ExecutionStep::NotReady => {}
                ExecutionStep::NoTasksAvailable => self.execution.wait_for_task(),
            }
        }
    }

    /// The next chunk of a streaming result, running the query's tasks until the
    /// collector has one; `None` once the query finished and every chunk was handed out.
    fn next_chunk(&mut self) -> Result<Option<DataChunk>> {
        loop {
            self.take_collected()?;
            if let Some(chunk) = self.buffered.pop_front() {
                return Ok(Some(chunk));
            }
            if self.finished {
                return Ok(None);
            }
            match self.execution.execute_task()? {
                ExecutionStep::Finished => self.finished = true,
                ExecutionStep::NotReady => {}
                ExecutionStep::NoTasksAvailable => self.execution.wait_for_task(),
            }
        }
    }
}

/// What a step of a pending query reports (DuckDB's `PendingExecutionResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingExecutionResult {
    /// The result can be fetched (`RESULT_READY`).
    ResultReady,
    /// The query ran to completion (`EXECUTION_FINISHED`), as a check without working on
    /// it reports.
    ExecutionFinished,
    /// More work remains (`RESULT_NOT_READY`).
    ResultNotReady,
    /// The remaining tasks run on other threads (`NO_TASKS_AVAILABLE`).
    NoTasksAvailable,
}

/// The progress of a connection's running query (DuckDB's `QueryProgress`): a percentage
/// of -1 when nothing runs or the progress is unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueryProgress {
    pub percentage: f64,
    pub rows_processed: u64,
    pub total_rows_to_process: u64,
}

impl QueryProgress {
    /// No query, or no progress known (`QueryProgress::Initialize`).
    const NONE: Self = Self {
        percentage: -1.0,
        rows_processed: 0,
        total_rows_to_process: 0,
    };
    /// A query that just started with the progress bar enabled (`QueryProgress::Restart`).
    const STARTED: Self = Self {
        percentage: 0.0,
        rows_processed: 0,
        total_rows_to_process: 0,
    };
}

/// What other threads may use of a connection while it runs a query: interrupting it and
/// reading its progress (DuckDB's `ClientContext::Interrupt` and `GetQueryProgress`).
#[derive(Debug)]
pub struct ClientHandle {
    interrupted: Arc<AtomicBool>,
    progress: Mutex<QueryProgress>,
}

impl Default for ClientHandle {
    fn default() -> Self {
        Self {
            interrupted: Arc::default(),
            progress: Mutex::new(QueryProgress::NONE),
        }
    }
}

impl ClientHandle {
    /// Interrupts the running query, which then fails with `INTERRUPT Error: Interrupted!`
    /// (`ClientContext::Interrupt`).
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    /// Whether the running query was interrupted (`ClientContext::IsInterrupted`).
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    /// The progress of the running query (`ClientContext::GetQueryProgress`).
    pub fn query_progress(&self) -> QueryProgress {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn interrupt_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupted)
    }

    fn set_progress(&self, progress: QueryProgress) {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = progress;
    }
}

/// A started query whose result is not fetched yet (DuckDB's `PendingQueryResult`). It
/// refers to the connection's active query; once another query starts, it is closed.
#[derive(Clone, Debug)]
pub struct PendingQuery {
    id: u64,
    names: Vec<String>,
    types: Vec<LogicalType>,
    statement_type: StatementType,
}

impl PendingQuery {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn statement_type(&self) -> StatementType {
        self.statement_type
    }
}

/// A result whose rows are produced while they are fetched (DuckDB's `StreamQueryResult`).
/// It refers to the connection's active query; once another query starts, it is closed.
#[derive(Clone, Debug)]
pub struct StreamQueryResult {
    id: u64,
    names: Vec<String>,
    types: Vec<LogicalType>,
    return_type: StatementReturnType,
    statement_type: StatementType,
}

impl StreamQueryResult {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn return_type(&self) -> StatementReturnType {
        self.return_type
    }

    pub fn statement_type(&self) -> StatementType {
        self.statement_type
    }
}

/// The result of a query (DuckDB's `QueryResult` subclasses).
#[derive(Debug)]
pub enum QueryResult {
    Materialized(MaterializedResult),
    Stream(StreamQueryResult),
}

/// The statement a pending or streaming result belongs to (DuckDB's
/// `ActiveQueryContext`).
#[derive(Debug)]
pub(crate) struct ActiveQuery {
    id: u64,
    query: String,
    statement_type: StatementType,
    state: ActiveState,
    /// Whether the progress of the query is tracked (`enable_progress_bar`).
    progress_bar: bool,
}

#[derive(Debug)]
enum ActiveState {
    /// The plan runs; the query has not ended yet.
    Running(Box<PlanExecution>),
    /// The query ended with this outcome (statements that completed when they started,
    /// and queries that failed).
    Ended(Result<MaterializedResult>),
}

/// "Attempting to execute an unsuccessful or closed pending query result".
fn closed_pending_error() -> Error {
    Error::invalid_input("Attempting to execute an unsuccessful or closed pending query result")
}

/// "Attempting to fetch from an unsuccessful or closed streaming query result".
fn closed_stream_error() -> Error {
    Error::invalid_input("Attempting to fetch from an unsuccessful or closed streaming query result")
}

impl Connection {
    /// The handle other threads use to interrupt this connection's query or read its
    /// progress.
    pub fn client_handle(&self) -> Arc<ClientHandle> {
        Arc::clone(&self.client)
    }

    /// The connection's state as its functions see it (the parts of DuckDB's
    /// `ClientContext` that function callbacks reach), usable from any thread.
    pub fn function_client(&self) -> Arc<rustdb_function::FunctionClient> {
        Arc::clone(&self.function_client)
    }

    /// Closes the active query, if any, and clears the interrupt flag; every query starts
    /// with this (`ClientContext::InitialCleanup`).
    pub(crate) fn initial_cleanup(&mut self) {
        self.cleanup_active_query();
        self.client.interrupted.store(false, Ordering::Release);
    }

    /// Ends the active query, if one is running: its results are closed (DuckDB's
    /// `CleanupInternal`).
    pub(crate) fn cleanup_active_query(&mut self) {
        let Some(active) = self.active_query.take() else {
            return;
        };
        if let ActiveState::Running(run) = active.state {
            // The executor is cancelled before the query ends, unsuccessfully, in its
            // transaction (`CleanupInternal` without a result).
            drop(run);
            let closed = Err(Error::invalid_input("query result was closed"));
            let _ = self.end_query_internal(&active.query, closed, false);
        }
        self.client.set_progress(QueryProgress::NONE);
    }

    /// Starts `statement` of `query` without running its plan (DuckDB's
    /// `ClientContext::PendingQuery`); the result streams if `allow_stream` and the
    /// statement allows it.
    pub fn pending_query(
        &mut self,
        query: &str,
        statement: SqlStatement,
        allow_stream: bool,
    ) -> Result<PendingQuery> {
        self.pending_query_with(query, statement, allow_stream, QueryResultMemoryType::InMemory)
    }

    /// [`Connection::pending_query`] whose result, if materialized, keeps its rows as
    /// `memory_type` says (`QueryParameters::memory_type`).
    pub fn pending_query_with(
        &mut self,
        query: &str,
        statement: SqlStatement,
        allow_stream: bool,
        memory_type: QueryResultMemoryType,
    ) -> Result<PendingQuery> {
        self.initial_cleanup();
        let statement_type = statement.statement_type();
        self.begin_query(query, &statement)?;
        let started = self.in_query_scope(|connection| {
            connection.begin_query_logging(query);
            connection.start_statement(
                statement,
                RunMode::Pending {
                    stream: allow_stream,
                    memory_type,
                },
            )
        });
        self.activate(query, statement_type, started)
    }

    /// Makes a started statement the active query and returns its pending result; a
    /// statement that failed or completed when it started ends right away.
    pub(crate) fn activate(
        &mut self,
        query: &str,
        statement_type: StatementType,
        started: Result<StatementRun>,
    ) -> Result<PendingQuery> {
        let (state, names, types) = match started {
            Ok(StatementRun::Executing(run)) => {
                let (names, types) = (run.names.clone(), run.types.clone());
                (ActiveState::Running(run), names, types)
            }
            Ok(StatementRun::Done(result)) => {
                let result = self.end_query(query, Ok(result))?;
                let (names, types) = (result.names().to_vec(), result.types().to_vec());
                (ActiveState::Ended(Ok(result)), names, types)
            }
            Err(error) => return self.end_query(query, Err(error)).map(|_| unreachable!()),
        };
        self.next_query_id += 1;
        let progress_bar = matches!(state, ActiveState::Running(_)) && self.settings.enable_progress_bar();
        if progress_bar {
            self.client.set_progress(QueryProgress::STARTED);
        }
        self.active_query = Some(ActiveQuery {
            id: self.next_query_id,
            query: query.to_owned(),
            statement_type,
            state,
            progress_bar,
        });
        Ok(PendingQuery {
            id: self.next_query_id,
            names,
            types,
            statement_type,
        })
    }

    /// The active query with id `id`, if it is still open.
    fn active(&mut self, id: u64) -> Option<&mut ActiveQuery> {
        self.active_query.as_mut().filter(|active| active.id == id)
    }

    /// Ends the active query with the failure `error`: the executor is cancelled and the
    /// query ends in its transaction. Returns the error as the client sees it.
    fn fail_active_query(&mut self, error: Error) -> Error {
        let Some(active) = self.active_query.take() else {
            return error;
        };
        drop(active.state);
        let error = match self.end_query(&active.query, Err(error)) {
            Ok(_) => Error::internal("a failed query ended successfully"),
            Err(error) => error,
        };
        self.client.set_progress(QueryProgress::NONE);
        error
    }

    /// Works on the pending query for a while (DuckDB's `PendingQueryResult::ExecuteTask`).
    pub fn execute_task(&mut self, pending: &PendingQuery) -> Result<PendingExecutionResult> {
        let scope = self.query_scope();
        let active = self.active(pending.id).ok_or_else(closed_pending_error)?;
        let progress_bar = active.progress_bar;
        let run = match &mut active.state {
            ActiveState::Ended(Ok(_)) => return Ok(PendingExecutionResult::ResultReady),
            ActiveState::Ended(Err(error)) => return Err(error.clone()),
            ActiveState::Running(run) => run,
        };
        if run.finished {
            return Ok(PendingExecutionResult::ResultReady);
        }
        let step = run.execution.execute_task();
        if step == Ok(ExecutionStep::Finished) {
            run.finished = true;
        }
        drop(scope);
        match step {
            Ok(step) => {
                if progress_bar {
                    self.update_progress();
                }
                Ok(match step {
                    ExecutionStep::Finished => PendingExecutionResult::ResultReady,
                    ExecutionStep::NotReady => PendingExecutionResult::ResultNotReady,
                    ExecutionStep::NoTasksAvailable => PendingExecutionResult::NoTasksAvailable,
                })
            }
            Err(error) => Err(self.fail_active_query(error)),
        }
    }

    /// Whether the active query's execution finished, completing or failing; `false`
    /// without an active query (`ClientContext::ExecutionIsFinished`).
    pub fn execution_is_finished(&self) -> bool {
        self.active_query
            .as_ref()
            .is_some_and(|active| match &active.state {
                ActiveState::Ended(_) => true,
                ActiveState::Running(run) => run.finished || run.execution.execution_is_finished(),
            })
    }

    /// The state of the pending query without working on it (DuckDB's
    /// `PendingQueryResult::CheckPulse`): the caller's tasks wait to be run.
    pub fn check_pending(&mut self, pending: &PendingQuery) -> Result<PendingExecutionResult> {
        let active = self.active(pending.id).ok_or_else(closed_pending_error)?;
        match &active.state {
            ActiveState::Ended(Ok(_)) => Ok(PendingExecutionResult::ExecutionFinished),
            ActiveState::Ended(Err(error)) => Err(error.clone()),
            ActiveState::Running(run) if run.finished => Ok(PendingExecutionResult::ExecutionFinished),
            ActiveState::Running(_) => Ok(PendingExecutionResult::NoTasksAvailable),
        }
    }

    /// Runs the pending query to completion and returns its result (DuckDB's
    /// `PendingQueryResult::Execute`): materialized, or a [`StreamQueryResult`] if the query
    /// streams its result.
    pub fn execute_pending(&mut self, pending: &PendingQuery) -> Result<QueryResult> {
        let active = self.active(pending.id).ok_or_else(closed_pending_error)?;
        if let ActiveState::Ended(outcome) = &active.state {
            let outcome = outcome.clone();
            self.active_query = None;
            return outcome.map(QueryResult::Materialized);
        }
        let streaming = matches!(&active.state, ActiveState::Running(run) if run.streaming);
        if streaming {
            let buffer_size = self.settings.streaming_buffer_size();
            let scope = self.query_scope();
            let active = self.active(pending.id).ok_or_else(closed_pending_error)?;
            let ActiveState::Running(run) = &mut active.state else {
                unreachable!("checked to be running above")
            };
            let filled = run.fill_buffer(buffer_size);
            drop(scope);
            if let Err(error) = filled {
                return Err(self.fail_active_query(error));
            }
            let active = self.active(pending.id).ok_or_else(closed_pending_error)?;
            let ActiveState::Running(run) = &active.state else {
                unreachable!("checked to be running above")
            };
            return Ok(QueryResult::Stream(StreamQueryResult {
                id: pending.id,
                names: run.names.clone(),
                types: run.types.clone(),
                return_type: run.return_type,
                statement_type: active.statement_type,
            }));
        }
        let Some(active) = self.active_query.take() else {
            return Err(closed_pending_error());
        };
        let ActiveState::Running(run) = active.state else {
            unreachable!("checked to be running above")
        };
        let outcome = self.in_query_scope(|connection| connection.finish_execution(*run));
        self.client.set_progress(QueryProgress::NONE);
        self.end_query(&active.query, outcome)
            .map(|result| QueryResult::Materialized(result.with_statement_type(active.statement_type)))
    }

    /// The next chunk of a streaming result, `None` once it is exhausted (DuckDB's
    /// `StreamQueryResult::Fetch`). The query ends when its last chunk was fetched.
    pub fn fetch(&mut self, stream: &StreamQueryResult) -> Result<Option<DataChunk>> {
        let scope = self.query_scope();
        let active = self.active(stream.id).ok_or_else(closed_stream_error)?;
        let ActiveState::Running(run) = &mut active.state else {
            return Err(closed_stream_error());
        };
        let fetched = run.next_chunk();
        drop(scope);
        match fetched {
            Ok(Some(chunk)) => Ok(Some(chunk)),
            Ok(None) => {
                // The stream is exhausted: the query ends (`StreamQueryResult::FetchInternal`).
                let Some(active) = self.active_query.take() else {
                    return Ok(None);
                };
                drop(active.state);
                self.client.set_progress(QueryProgress::NONE);
                self.end_query(&active.query, Ok(MaterializedResult::default()))?;
                Ok(None)
            }
            Err(error) => Err(self.fail_active_query(error)),
        }
    }

    /// Fetches the remaining rows of a streaming result into a materialized one
    /// (`StreamQueryResult::Materialize`).
    pub fn materialize(&mut self, stream: &StreamQueryResult) -> Result<MaterializedResult> {
        let mut chunks = Vec::new();
        while let Some(chunk) = self.fetch(stream)? {
            chunks.push(chunk);
        }
        Ok(
            MaterializedResult::new(stream.names.clone(), stream.types.clone(), chunks)
                .with_return_type(stream.return_type)
                .with_statement_type(stream.statement_type),
        )
    }

    /// Closes a pending or streaming result: if its query is still the active one, it
    /// ends (`PendingQueryResult::Close`, `StreamQueryResult::Close`).
    pub fn close_query(&mut self, id: QueryId) {
        if self.active_query.as_ref().is_some_and(|active| active.id == id.0) {
            self.cleanup_active_query();
        }
    }

    /// Updates the progress of the active query after a step (`ProgressBar::Update`).
    fn update_progress(&mut self) {
        let Some(ActiveQuery {
            state: ActiveState::Running(run),
            ..
        }) = &self.active_query
        else {
            return;
        };
        let Some(progress) = run.execution.progress() else {
            return;
        };
        let current = self.client.query_progress();
        let percentage = progress.percentage();
        if percentage > current.percentage {
            self.client.set_progress(QueryProgress {
                percentage,
                rows_processed: progress.done as u64,
                total_rows_to_process: progress.total as u64,
            });
        }
    }
}

/// Identifies the query a pending or streaming result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryId(u64);

impl PendingQuery {
    pub fn id(&self) -> QueryId {
        QueryId(self.id)
    }
}

impl StreamQueryResult {
    pub fn id(&self) -> QueryId {
        QueryId(self.id)
    }
}
