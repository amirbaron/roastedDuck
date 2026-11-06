//! The transform functions of DuckDB's `transform_prepare.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformPrepareStatement`: `PREPARE name TypeList? AS Statement`.
    pub(super) fn transform_prepare_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let name = list.identifier(1)?.to_owned();
        if list.has(2)? {
            return Err(Error::not_implemented(
                "TypeList for prepared statement has not been implemented.",
            ));
        }
        let statement: SqlStatement = self.transform_child(list, 4)?;
        self.clear_parameters();
        Ok(SqlStatement::new(SqlStatementKind::Prepare(PrepareStatement {
            name: name.into(),
            statement: Box::new(statement),
        })))
    }
}
