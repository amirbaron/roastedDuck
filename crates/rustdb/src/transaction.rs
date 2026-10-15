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
}

fn transaction_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Transaction, message)
}

impl TransactionContext {
    pub fn new(manager: Arc<TransactionManager>) -> Self {
        Self {
            manager,
            current: None,
            auto_commit: true,
            invalidated: false,
        }
    }

    #[cfg(test)]
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
                Some(transaction) if success => self.manager.commit(&transaction),
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

    /// `BEGIN TRANSACTION`
    pub fn begin(&mut self) -> Result<()> {
        if !self.auto_commit {
            return Err(transaction_error(
                "cannot start a transaction within a transaction",
            ));
        }
        self.auto_commit = false;
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
            Some(transaction) => self.manager.commit(&transaction),
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
        context.begin().unwrap();
        assert_eq!(
            context.begin().unwrap_err().to_string(),
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
