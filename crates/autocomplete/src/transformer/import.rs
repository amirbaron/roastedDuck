//! The transform functions of DuckDB's `transform_import.cpp`.

#![allow(
    clippy::unused_self,
    reason = "transform functions have the signature of the rule registry"
)]

use super::prelude::*;

impl PegTransformer {
    /// `TransformImportStatement`: `IMPORT DATABASE 'path'` is `PRAGMA
    /// import_database('path')`.
    pub(super) fn transform_import_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let name = parse_result.list()?.child(2)?.string_literal()?.0;
        Ok(SqlStatement::new(SqlStatementKind::Pragma(Box::new(
            PragmaInfo {
                name: "import_database".to_owned(),
                parameters: vec![ParsedExpression::constant(Value::varchar(name))],
                named_parameters: CaseInsensitiveMap::default(),
            },
        ))))
    }
}
