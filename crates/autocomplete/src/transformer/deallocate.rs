//! The transform functions of DuckDB's `transform_deallocate.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDeallocateStatement`.
    pub(super) fn transform_deallocate_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformDeallocateStatement", parse_result))
    }
}
