use std::sync::Arc;

use rustdb_catalog::{Transaction, TransactionManager};
use rustdb_common::{Error, ErrorKind, Result};

/// A connection's transaction state (DuckDB's `TransactionContext`).
///
/// Outside `BEGIN … COMMIT` every statement runs in its own auto-committed transaction.
/// A failed statement inside an explicit transaction invalidates it until `ROLLBACK`.
#[derive(Debug)]
pub(crate) struct TransactionContext {
    manager: Arc<TransactionManager>,
    current: Option<Arc<Transaction>>,
    auto_commit: bool,
    invalidated: bool,
    /// `BEGIN TRANSACTION READ ONLY`.
    read_only: bool,
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
    pub fn new(manager: Arc<TransactionManager>) -> Self {
        Self {
            manager,
            current: None,
            auto_commit: true,
            invalidated: false,
            read_only: false,
        }
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
        if self.auto_commit {
            return match self.current.take() {
                Some(transaction) if success => self.manager.commit(&transaction).map_err(commit_error),
                Some(transaction) => {
                    self.manager.rollback(&transaction);
                    Ok(())
                }
                None => Ok(()),
            };
        }
        if !success && invalidates {
            self.invalidated = true;
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
        if !self.auto_commit {
            return Err(transaction_error(
                "cannot start a transaction within a transaction",
            ));
        }
        self.auto_commit = false;
        self.read_only = read_only;
        Ok(())
    }

    /// `COMMIT`: an invalidated transaction is rolled back instead, without an error
    /// (`PhysicalTransaction` turns the COMMIT into a ROLLBACK).
    pub fn commit(&mut self) -> Result<()> {
        if self.auto_commit {
            return Err(transaction_error("cannot commit - no transaction is active"));
        }
        self.auto_commit = true;
        let invalidated = std::mem::take(&mut self.invalidated);
        match self.current.take() {
            Some(transaction) if invalidated => {
                self.manager.rollback(&transaction);
                Ok(())
            }
            Some(transaction) => self.manager.commit(&transaction).map_err(commit_error),
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
        if let Some(transaction) = self.current.take() {
            self.manager.rollback(&transaction);
        }
        Ok(())
    }
}

impl Drop for TransactionContext {
    fn drop(&mut self) {
        // A connection closed mid-transaction rolls back, as in DuckDB.
        if let Some(transaction) = self.current.take() {
            self.manager.rollback(&transaction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_transactions_follow_duckdb_rules() {
        let mut context = TransactionContext::new(Arc::default());
        assert_eq!(
            context.commit().unwrap_err().to_string(),
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
