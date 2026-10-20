//! The transform functions of DuckDB's `transform_detach.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDetachStatement`: `DETACH DATABASE? IfExists? CatalogName`.
    pub(super) fn transform_detach_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let if_not_found = if list.has(2)? {
            OnEntryNotFound::ReturnNull
        } else {
            OnEntryNotFound::ThrowException
        };
        let name = list.identifier(3)?.to_owned();
        Ok(SqlStatement::new(SqlStatementKind::Detach(DetachInfo {
            name,
            if_not_found,
        })))
    }
}
