//! Semantic actions for the rules in `grammar/statements/transaction.y`.

use super::prelude::*;

/// ```text
/// TransactionStmt:
///       ABORT_P opt_transaction
///     | BEGIN_P opt_transaction opt_transaction_type
///     | START opt_transaction opt_transaction_type
///     | COMMIT opt_transaction
///     | END_P opt_transaction
///     | ROLLBACK opt_transaction
/// ```
pub(super) fn transaction_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ABORT_P opt_transaction
        0 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Rollback,
                options: List::new(),
                transaction_type: TransactionStmtType::Default,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // BEGIN_P opt_transaction opt_transaction_type
        1 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Begin,
                transaction_type: rhs.transaction_stmt_type(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // START opt_transaction opt_transaction_type
        2 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Start,
                transaction_type: rhs.transaction_stmt_type(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // COMMIT opt_transaction
        3 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Commit,
                options: List::new(),
                transaction_type: TransactionStmtType::Default,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // END_P opt_transaction
        4 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Commit,
                options: List::new(),
                transaction_type: TransactionStmtType::Default,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ROLLBACK opt_transaction
        5 => {
            let n = TransactionStmt {
                kind: TransactionStmtKind::Rollback,
                options: List::new(),
                transaction_type: TransactionStmtType::Default,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_transaction:
///       WORK
///     | TRANSACTION
///     | /* empty */
/// ```
pub(super) fn opt_transaction<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WORK
        0 => Ok(SemValue::None),
        // TRANSACTION
        1 => Ok(SemValue::None),
        // /* empty */
        2 => Ok(SemValue::None),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_transaction_type:
///       READ_P ONLY
///     | READ_P WRITE_P
///     | /* empty */
/// ```
pub(super) fn opt_transaction_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // READ_P ONLY
        0 => Ok(SemValue::TransactionStmtType(TransactionStmtType::ReadOnly)),
        // READ_P WRITE_P
        1 => Ok(SemValue::TransactionStmtType(TransactionStmtType::ReadWrite)),
        // /* empty */
        2 => Ok(SemValue::TransactionStmtType(TransactionStmtType::Default)),
        _ => Err(rhs.bad_alternative()),
    }
}
