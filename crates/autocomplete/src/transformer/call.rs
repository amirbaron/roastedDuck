//! The transform functions of DuckDB's `transform_call.cpp`.

use rustdb_parser::build;

use super::prelude::*;

impl PegTransformer {
    /// `TransformCallStatement`: `CALL QualifiedTableFunction TableFunctionArguments`.
    pub(super) fn transform_call_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let table_function_name: QualifiedName = self.transform_child(list, 1)?;
        let function_children: Vec<ParsedExpression> = self.transform_child(list, 2)?;
        let function = build::function_full(
            table_function_name.catalog,
            table_function_name.schema,
            &table_function_name.name,
            function_children,
            None,
            OrderModifier { orders: Vec::new() },
            false,
            false,
            false,
        );
        Ok(SqlStatement::new(SqlStatementKind::Call(CallStatement {
            function: Box::new(function),
        })))
    }
}
