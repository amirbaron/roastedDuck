//! The transform functions of DuckDB's `transform_checkpoint.cpp`.

#![allow(
    clippy::unused_self,
    reason = "transform functions have the signature of the rule registry"
)]

use super::prelude::*;

impl PegTransformer {
    /// `TransformCheckpointStatement`: `FORCE? CHECKPOINT [db]` is `CALL
    /// system.main.[force_]checkpoint([db])`.
    pub(super) fn transform_checkpoint_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let checkpoint_name = if list.has(0)? {
            "force_checkpoint"
        } else {
            "checkpoint"
        };
        let mut function = ParsedExpression::function(checkpoint_name, Vec::new());
        if let ParsedExpressionKind::Function(function) = &mut function.kind {
            "system".clone_into(&mut function.catalog);
            "main".clone_into(&mut function.schema);
            if let Some(catalog_name) = list.optional(2)? {
                function.children.push(ParsedExpression::constant(Value::varchar(
                    catalog_name.identifier()?,
                )));
            }
        }
        Ok(SqlStatement::new(SqlStatementKind::Call(CallStatement {
            function: Box::new(function),
        })))
    }
}
