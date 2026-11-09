//! Drive native pending tasks while honoring Python's main-thread signal protocol.
use pyo3::Python;
use roastedduck::{Connection, MaterializedResult, PendingExecutionResult, PendingQuery, QueryResult};
use roastedduck_common::{Error, ErrorKind, Result};

fn check_signals(py: Python<'_>) -> pyo3::PyResult<()> {
    py.check_signals()
}

pub(super) trait PythonExecution {
    fn execute_python_statement(
        &mut self,
        query: impl AsRef<[u8]>,
        statement: roastedduck_ast::SqlStatement,
    ) -> Result<MaterializedResult>;
    fn execute_python_prepared<K: AsRef<[u8]>>(
        &mut self,
        prepared: &roastedduck::PreparedStatement,
        values: &roastedduck_common::CaseInsensitiveMap<roastedduck_types::Value, K>,
        allow_stream: bool,
    ) -> Result<QueryResult>;
}
impl PythonExecution for Connection {
    fn execute_python_statement(
        &mut self,
        query: impl AsRef<[u8]>,
        statement: roastedduck_ast::SqlStatement,
    ) -> Result<MaterializedResult> {
        let pending = self.pending_query(query, statement, false)?;
        match complete(self, &pending)? {
            QueryResult::Materialized(result) => Ok(result),
            QueryResult::Stream(_) => Err(Error::internal(
                "Python statement unexpectedly returned a streaming result",
            )),
        }
    }
    fn execute_python_prepared<K: AsRef<[u8]>>(
        &mut self,
        prepared: &roastedduck::PreparedStatement,
        values: &roastedduck_common::CaseInsensitiveMap<roastedduck_types::Value, K>,
        allow_stream: bool,
    ) -> Result<QueryResult> {
        let pending = self.pending_prepared(prepared, values, allow_stream)?;
        complete(self, &pending)
    }
}

pub(super) fn complete(connection: &mut Connection, pending: &PendingQuery) -> Result<QueryResult> {
    loop {
        let state = connection.execute_task(pending)?;
        if matches!(
            state,
            PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished
        ) {
            return connection.execute_pending(pending);
        }
        if Python::attach(check_signals).is_err() {
            connection.close_query(pending.id());
            return Err(Error::new(ErrorKind::Invalid, "Query interrupted").with_subtype("python_interrupt"));
        }
        if state == PendingExecutionResult::Blocked {
            connection.wait_for_task(pending)?;
        }
    }
}
