use std::sync::Arc;

use roastedduck_catalog::{Transaction, TransactionManager};
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::FunctionClient;
use roastedduck_storage::transaction::CommitCheckpoint;

/// Binding leaves an aborted synthetic transaction for its owning query to roll back.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StatementEnd {
    Client,
    Query { allow_invalidation: bool },
}

/// A connection's transaction state (DuckDB's `TransactionContext`).
///
/// Outside `BEGIN … COMMIT` every statement runs in its own auto-committed transaction.
/// A failed statement inside an explicit transaction invalidates it until `ROLLBACK`.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent transaction flags matching TransactionContext and TransactionInfo"
)]
pub(crate) struct TransactionContext {
    manager: Arc<TransactionManager>,
    current: Option<Arc<Transaction>>,
    auto_commit: bool,
    invalidated: bool,
    /// `BEGIN TRANSACTION READ ONLY`.
    read_only: bool,
    invalidation_policy: roastedduck_ast::TransactionInvalidationPolicy,
    auto_rollback: bool,
    /// The connection's function state, which sees the binder's context of the open
    /// transaction until the transaction ends.
    client: Arc<FunctionClient>,
}

fn transaction_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Transaction, message)
}

/// A failed commit (`TransactionContext::Commit`): fatal and internal errors pass
/// through, the others become "Failed to commit: ...".
fn commit_error(error: Error) -> Error {
    if matches!(error.kind(), ErrorKind::Fatal | ErrorKind::Internal) {
        return error;
    }
    Error::new(
        ErrorKind::Transaction,
        format!("Failed to commit: {}", error.raw_message()),
    )
}

impl TransactionContext {
    pub fn new(manager: Arc<TransactionManager>, client: Arc<FunctionClient>) -> Self {
        Self {
            manager,
            current: None,
            auto_commit: true,
            invalidated: false,
            read_only: false,
            invalidation_policy: roastedduck_ast::TransactionInvalidationPolicy::StandardPolicy,
            auto_rollback: false,
            client,
        }
    }

    /// Ends the open transaction's time as the current one: functions no longer see its
    /// context.
    fn take_current(&mut self) -> Option<Arc<Transaction>> {
        self.client.set_transaction_context(None);
        self.current.take()
    }

    /// Whether the explicit transaction was started read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only && !self.auto_commit
    }

    /// Whether no explicit transaction is open.
    pub fn is_auto_commit(&self) -> bool {
        self.auto_commit
    }

    /// The error of statements run in an aborted transaction (DuckDB's
    /// `ErrorManager::InvalidatedTransaction`).
    pub fn aborted_error() -> Error {
        transaction_error("Current transaction is aborted (please ROLLBACK)")
    }

    /// Whether a transaction is open (explicit, or an auto-commit one mid-statement).
    pub fn has_current(&self) -> bool {
        self.current.is_some()
    }

    /// Whether a failed statement aborted the explicit transaction.
    pub fn is_invalidated(&self) -> bool {
        self.invalidated
    }

    /// The transaction a statement runs in, starting one if needed. Statements that
    /// require a valid transaction check [`TransactionContext::is_invalidated`] once bound.
    pub fn current(&mut self) -> Arc<Transaction> {
        Arc::clone(self.current.get_or_insert_with(|| Arc::new(self.manager.begin())))
    }

    /// The transaction a statement should run in, starting one if needed; fails if the
    /// explicit transaction was aborted.
    #[cfg(test)]
    pub fn active(&mut self) -> Result<Arc<Transaction>> {
        if self.invalidated {
            return Err(Self::aborted_error());
        }
        Ok(self.current())
    }

    /// Finishes a statement: auto-commit transactions commit on success and roll back on
    /// failure; in an explicit transaction, a failure whose error `invalidates` the
    /// transaction aborts it.
    pub fn end_statement(&mut self, success: bool, invalidates: bool) -> Result<()> {
        self.end_statement_checkpointing(success, invalidates, StatementEnd::Client, None)
    }

    /// [`TransactionContext::end_statement`] whose auto-commit runs `checkpoint` instead
    /// of logging its changes (see [`TransactionManager::commit_checkpointing`]).
    pub fn end_statement_checkpointing(
        &mut self,
        success: bool,
        invalidates: bool,
        end: StatementEnd,
        checkpoint: Option<CommitCheckpoint<'_>>,
    ) -> Result<()> {
        if self.auto_commit {
            return match self.take_current() {
                Some(transaction) if success => self
                    .manager
                    .commit_checkpointing(&transaction, checkpoint)
                    .map_err(commit_error),
                Some(transaction) => {
                    self.manager.rollback(&transaction);
                    Ok(())
                }
                None => Ok(()),
            };
        }
        let (allow_invalidation, allow_rollback) = match end {
            StatementEnd::Client => (true, false),
            StatementEnd::Query { allow_invalidation } => (allow_invalidation, true),
        };
        if !success
            && allow_invalidation
            && (invalidates
                || self.invalidation_policy
                    == roastedduck_ast::TransactionInvalidationPolicy::AllErrorsInvalidateTransaction)
        {
            if self.auto_rollback && allow_rollback {
                self.rollback()?;
            } else {
                self.invalidated = true;
            }
        }
        Ok(())
    }

    /// Aborts the explicit transaction (an error under DuckDB's
    /// `ALL_ERRORS_INVALIDATE_TRANSACTION` policy).
    pub fn invalidate(&mut self) {
        if !self.auto_commit {
            self.invalidated = true;
        }
    }

    /// `BEGIN TRANSACTION [READ ONLY]`
    pub fn begin(&mut self, read_only: bool) -> Result<()> {
        self.begin_with_options(
            read_only,
            roastedduck_ast::TransactionInvalidationPolicy::StandardPolicy,
            false,
        )
    }

    pub fn set_invalidation_policy(&mut self, policy: roastedduck_ast::TransactionInvalidationPolicy) {
        self.invalidation_policy = policy;
    }

    pub fn begin_with_options(
        &mut self,
        read_only: bool,
        policy: roastedduck_ast::TransactionInvalidationPolicy,
        auto_rollback: bool,
    ) -> Result<()> {
        if !self.auto_commit {
            return Err(transaction_error(
                "cannot start a transaction within a transaction",
            ));
        }
        self.auto_commit = false;
        self.read_only = read_only;
        self.invalidation_policy = policy;
        self.auto_rollback = auto_rollback;
        Ok(())
    }

    /// `COMMIT`: an invalidated transaction is rolled back instead, without an error
    /// (`PhysicalTransaction` turns the COMMIT into a ROLLBACK).
    ///
    /// With `checkpoint`, the commit runs it instead of logging the changes (see
    /// [`TransactionManager::commit_checkpointing`]).
    pub fn commit(&mut self, checkpoint: Option<CommitCheckpoint<'_>>) -> Result<()> {
        if self.auto_commit {
            return Err(transaction_error("cannot commit - no transaction is active"));
        }
        self.auto_commit = true;
        let invalidated = std::mem::take(&mut self.invalidated);
        match self.take_current() {
            Some(transaction) if invalidated => {
                self.manager.rollback(&transaction);
                Ok(())
            }
            Some(transaction) => self
                .manager
                .commit_checkpointing(&transaction, checkpoint)
                .map_err(commit_error),
            None => Ok(()),
        }
    }

    /// `ROLLBACK`
    pub fn rollback(&mut self) -> Result<()> {
        if self.auto_commit {
            return Err(transaction_error("cannot rollback - no transaction is active"));
        }
        self.auto_commit = true;
        self.invalidated = false;
        if let Some(transaction) = self.take_current() {
            self.manager.rollback(&transaction);
        }
        Ok(())
    }
}

impl Drop for TransactionContext {
    fn drop(&mut self) {
        // A connection closed mid-transaction rolls back, as in DuckDB.
        if let Some(transaction) = self.take_current() {
            self.manager.rollback(&transaction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_transactions_follow_duckdb_rules() {
        let mut context = TransactionContext::new(Arc::default(), Arc::default());
        assert_eq!(
            context.commit(None).unwrap_err().to_string(),
            "TransactionContext Error: cannot commit - no transaction is active"
        );
        context.begin(false).unwrap();
        assert_eq!(
            context.begin(false).unwrap_err().to_string(),
            "TransactionContext Error: cannot start a transaction within a transaction"
        );
        context.active().unwrap();
        context.end_statement(false, true).unwrap();
        assert_eq!(
            context.active().unwrap_err().to_string(),
            "TransactionContext Error: Current transaction is aborted (please ROLLBACK)"
        );
        context.rollback().unwrap();
        assert!(context.is_auto_commit());
        context.active().unwrap();
        context.end_statement(true, false).unwrap();
    }
}
