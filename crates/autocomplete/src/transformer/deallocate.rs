//! The transform functions of DuckDB's `transform_deallocate.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDeallocateStatement`: `DEALLOCATE PREPARE? name` drops a prepared
    /// statement.
    pub(super) fn transform_deallocate_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let name = parse_result.list()?.identifier(2)?.to_owned();
        Ok(SqlStatement::new(SqlStatementKind::Drop(Box::new(DropInfo {
            catalog_type: CatalogType::PreparedStatement,
            catalog: String::new(),
            schema: String::new(),
            name,
            if_not_found: OnEntryNotFound::ThrowException,
            cascade: false,
            allow_drop_internal: false,
            extra_drop_info: None,
        }))))
    }
}
