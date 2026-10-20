//! The transform functions of DuckDB's `transform_transaction.cpp`: `BEGIN`, `COMMIT`
//! and `ROLLBACK`.

#![allow(
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "transform functions have the signature of the rule registry"
)]

use super::prelude::*;

/// `TransactionInfo(type)`: the default modifier and policy.
const fn transaction_info(transaction_type: TransactionType) -> TransactionInfo {
    TransactionInfo {
        transaction_type,
        modifier: TransactionModifierType::TransactionDefaultModifier,
        invalidation_policy: TransactionInvalidationPolicy::StandardPolicy,
        auto_rollback: false,
    }
}

impl PegTransformer {
    /// `TransformTransactionStatement`: `BeginTransaction / CommitTransaction /
    /// RollbackTransaction`.
    pub(super) fn transform_transaction_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let info: TransactionInfo = self.transform(parse_result.list()?.choice(0)?)?;
        Ok(SqlStatement::new(SqlStatementKind::Transaction(info)))
    }

    /// `TransformBeginTransaction`: `(BEGIN / START) TRANSACTION? ReadOrWrite?`.
    pub(super) fn transform_begin_transaction(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        let list = parse_result.list()?;
        let mut info = transaction_info(TransactionType::BeginTransaction);
        info.modifier = match list.optional(2)? {
            Some(read_or_write) => self.transform(read_or_write)?,
            None => TransactionModifierType::TransactionDefaultModifier,
        };
        Ok(info)
    }

    /// `TransformReadOrWrite`: `READ (ONLY / WRITE)`.
    pub(super) fn transform_read_or_write(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionModifierType> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformReadOnlyOrReadWrite`.
    pub(super) fn transform_read_only_or_read_write(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionModifierType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformCommitTransaction`.
    pub(super) fn transform_commit_transaction(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        Ok(transaction_info(TransactionType::Commit))
    }

    /// `TransformRollbackTransaction`.
    pub(super) fn transform_rollback_transaction(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        Ok(transaction_info(TransactionType::Rollback))
    }
}
