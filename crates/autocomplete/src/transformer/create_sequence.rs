//! The transform functions of DuckDB's `transform_create_sequence.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateSequenceStmt`.
    pub(super) fn transform_create_sequence_stmt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateSequenceStmt", parse_result))
    }

    /// `TransformSequenceOption`.
    pub(super) fn transform_sequence_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSequenceOption", parse_result))
    }

    /// `TransformSeqSetCycle`.
    pub(super) fn transform_seq_set_cycle(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqSetCycle", parse_result))
    }

    /// `TransformSeqSetIncrement`.
    pub(super) fn transform_seq_set_increment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqSetIncrement", parse_result))
    }

    /// `TransformSeqSetMinMax`.
    pub(super) fn transform_seq_set_min_max(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqSetMinMax", parse_result))
    }

    /// `TransformSeqMinOrMax`.
    pub(super) fn transform_seq_min_or_max(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSeqMinOrMax", parse_result))
    }

    /// `TransformSeqNoMinMax`.
    pub(super) fn transform_seq_no_min_max(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqNoMinMax", parse_result))
    }

    /// `TransformSeqStartWith`.
    pub(super) fn transform_seq_start_with(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqStartWith", parse_result))
    }

    /// `TransformSeqOwnedBy`.
    pub(super) fn transform_seq_owned_by(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, SequenceOption)> {
        Err(not_ported("TransformSeqOwnedBy", parse_result))
    }
}
