//! The transform functions of DuckDB's `transform_transaction.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformTransactionStatement`.
    pub(super) fn transform_transaction_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformTransactionStatement", parse_result))
    }

    /// `TransformBeginTransaction`.
    pub(super) fn transform_begin_transaction(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        Err(not_ported("TransformBeginTransaction", parse_result))
    }

    /// `TransformReadOrWrite`.
    pub(super) fn transform_read_or_write(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionModifierType> {
        Err(not_ported("TransformReadOrWrite", parse_result))
    }

    /// `TransformReadOnlyOrReadWrite`.
    pub(super) fn transform_read_only_or_read_write(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionModifierType> {
        Err(not_ported("TransformReadOnlyOrReadWrite", parse_result))
    }

    /// `TransformCommitTransaction`.
    pub(super) fn transform_commit_transaction(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        Err(not_ported("TransformCommitTransaction", parse_result))
    }

    /// `TransformRollbackTransaction`.
    pub(super) fn transform_rollback_transaction(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TransactionInfo> {
        Err(not_ported("TransformRollbackTransaction", parse_result))
    }
}
