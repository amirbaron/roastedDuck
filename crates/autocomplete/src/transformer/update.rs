//! The transform functions of DuckDB's `transform_update.cpp`.

use super::prelude::*;

/// `UpdateSetInfo()`: no assignments, no condition.
const fn update_set_info() -> UpdateSetInfo {
    UpdateSetInfo {
        condition: None,
        columns: Vec::new(),
        expressions: Vec::new(),
    }
}

impl PegTransformer {
    /// `TransformUpdateStatement`: `WithClause? UPDATE UpdateTarget UpdateSetClause
    /// FromClause? WhereClause? ReturningClause?`.
    pub(super) fn transform_update_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = list.optional(0)? {
            cte_map = self.transform(with)?;
        }
        let table: TableRef = self.transform_child(list, 2)?;
        let set_info: UpdateSetInfo = self.transform_child(list, 3)?;
        let mut result = UpdateStatement {
            table: Box::new(table),
            from_table: None,
            set_info: Box::new(set_info),
            returning_list: Vec::new(),
            cte_map,
            prioritize_table_when_binding: false,
        };
        if let Some(from_table) = self.transform_optional(list, 4)? {
            result.from_table = Some(Box::new(from_table));
        }
        if let Some(condition) = self.transform_optional(list, 5)? {
            result.set_info.condition = Some(Box::new(condition));
        }
        if let Some(returning_list) = self.transform_optional(list, 6)? {
            result.returning_list = returning_list;
        }
        Ok(SqlStatement::new(SqlStatementKind::Update(Box::new(result))))
    }

    /// `TransformUpdateTarget`: `BaseTableSet / BaseTableAliasSet`.
    pub(super) fn transform_update_target(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformBaseTableSet`: `BaseTableName SET`.
    pub(super) fn transform_base_table_set(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let BaseTable(table_ref) = self.transform_child(parse_result.list()?, 0)?;
        Ok(table_ref)
    }

    /// `TransformBaseTableAliasSet`: `BaseTableName UpdateAlias? SET`.
    pub(super) fn transform_base_table_alias_set(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        let list = parse_result.list()?;
        let BaseTable(mut table_ref) = self.transform_child(list, 0)?;
        if let Some(alias) = self.transform_optional(list, 1)? {
            table_ref.alias = alias;
        }
        Ok(table_ref)
    }

    /// `TransformUpdateAlias`: `AS? ColId`.
    pub(super) fn transform_update_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformUpdateSetClause`: `UpdateSetElementList / UpdateSetTuple`.
    pub(super) fn transform_update_set_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UpdateSetInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformUpdateSetTuple`: `(a, b) = expr`; a `ROW(...)` assigns its elements,
    /// any other expression is assigned to every column.
    pub(super) fn transform_update_set_tuple(&mut self, parse_result: &ParseResult) -> Result<UpdateSetInfo> {
        let list = parse_result.list()?;
        let column_list = list.child(0)?.parens_content()?.list_elements()?;
        let mut result = update_set_info();
        result.columns.reserve(column_list.len());
        for column in column_list {
            result.columns.push(column.identifier()?.to_owned().into());
        }

        let expr: ParsedExpression = self.transform_child(list, 2)?;
        match expr.kind {
            ParsedExpressionKind::Function(func_expr)
                if func_expr.function_name.eq_ignore_ascii_case("row") =>
            {
                if func_expr.children.len() != result.columns.len() {
                    return Err(Error::parser(format!(
                        "Could not perform assignment, expected {} values, got {}",
                        result.columns.len(),
                        func_expr.children.len()
                    )));
                }
                result.expressions = func_expr.children;
            }
            kind => result.expressions = vec![ParsedExpression { kind, ..expr }; result.columns.len()],
        }
        Ok(result)
    }

    /// `TransformUpdateSetElementList`: `List(UpdateSetElement)`.
    pub(super) fn transform_update_set_element_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UpdateSetInfo> {
        let mut result = update_set_info();
        for update_element in parse_result.list()?.child(0)?.list_elements()? {
            let (column, expression): (String, ParsedExpression) = self.transform(update_element)?;
            result.columns.push(column.into());
            result.expressions.push(expression);
        }
        Ok(result)
    }

    /// `TransformUpdateSetElement`: `UpdateSetColumnTarget '=' Expression`.
    pub(super) fn transform_update_set_element(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        let list = parse_result.list()?;
        let column_name: String = self.transform_child(list, 0)?;
        let expr: ParsedExpression = self.transform_child(list, 2)?;
        Ok((column_name, expr))
    }

    /// `TransformUpdateSetColumnTarget`: `ColumnName ('.' Identifier)*`; qualified
    /// columns are not supported.
    pub(super) fn transform_update_set_column_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        let list = parse_result.list()?;
        let column_name = list.identifier(0)?.to_owned();
        if list.has(1)? {
            return Err(Error::parser(
                "Qualified column names in UPDATE .. SET not supported",
            ));
        }
        Ok(column_name)
    }
}
