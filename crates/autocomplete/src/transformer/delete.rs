//! The transform functions of DuckDB's `transform_delete.cpp`: `DELETE` and `TRUNCATE`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDeleteStatement`: `WithClause? DELETE FROM TargetOptAlias
    /// DeleteUsingClause? WhereClause? ReturningClause?`.
    pub(super) fn transform_delete_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = list.optional(0)? {
            cte_map = self.transform(with)?;
        }
        let BaseTable(table) = self.transform_child(list, 3)?;
        let mut result = DeleteStatement {
            table: Box::new(table),
            condition: None,
            using_clauses: Vec::new(),
            returning_list: Vec::new(),
            cte_map,
        };
        if let Some(using_clauses) = self.transform_optional(list, 4)? {
            result.using_clauses = using_clauses;
        }
        if let Some(condition) = self.transform_optional(list, 5)? {
            result.condition = Some(Box::new(condition));
        }
        if let Some(returning_list) = self.transform_optional(list, 6)? {
            result.returning_list = returning_list;
        }
        Ok(SqlStatement::new(SqlStatementKind::Delete(Box::new(result))))
    }

    /// `TransformTargetOptAlias`: `BaseTableName AS? ColId?`.
    pub(super) fn transform_target_opt_alias(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        let list = parse_result.list()?;
        let mut table_ref: BaseTable = self.transform_child(list, 0)?;
        if let Some(alias) = self.transform_optional(list, 2)? {
            table_ref.0.alias = alias;
        }
        Ok(table_ref)
    }

    /// `TransformDeleteUsingClause`: `USING List(TableRef)`.
    pub(super) fn transform_delete_using_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<TableRef>> {
        parse_result
            .list()?
            .child(1)?
            .list_elements()?
            .into_iter()
            .map(|table_ref| self.transform(table_ref))
            .collect()
    }

    /// `TransformTruncateStatement`: `TRUNCATE TABLE? BaseTableName` is a `DELETE`
    /// without condition.
    pub(super) fn transform_truncate_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let BaseTable(table) = self.transform_child(parse_result.list()?, 2)?;
        Ok(SqlStatement::new(SqlStatementKind::Delete(Box::new(
            DeleteStatement {
                table: Box::new(table),
                condition: None,
                using_clauses: Vec::new(),
                returning_list: Vec::new(),
                cte_map: CommonTableExpressionMap::default(),
            },
        ))))
    }
}
