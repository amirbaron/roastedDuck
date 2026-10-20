//! The transform functions of DuckDB's `transform_insert.cpp`.

#![allow(
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "transform functions have the signature of the rule registry"
)]

use super::prelude::*;

/// `OnConflictInfo()`: an `ON CONFLICT` that throws.
const fn on_conflict_info() -> OnConflictInfo {
    OnConflictInfo {
        action_type: OnConflictAction::Throw,
        indexed_columns: Vec::new(),
        set_info: None,
        condition: None,
    }
}

impl PegTransformer {
    /// `TransformInsertStatement`: `WithClause? INSERT OrAction? INTO InsertTarget
    /// ByNameOrPosition? InsertColumnList? InsertValues OnConflictClause?
    /// ReturningClause?`.
    pub(super) fn transform_insert_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = list.optional(0)? {
            cte_map = self.transform(with)?;
        }
        let or_action = list.optional(2)?;
        let BaseTable(insert_target) = self.transform_child(list, 4)?;
        // DuckDB leaves the insert alias out here (`TODO(Dtenwolde)`).
        let TableRefKind::BaseTable(base_table) = &insert_target.kind else {
            return Err(Error::internal("InsertTarget is not a base table"));
        };
        let mut result = InsertStatement {
            select_statement: None,
            columns: Vec::new(),
            catalog: base_table.catalog_name.clone(),
            schema: base_table.schema_name.clone(),
            table: base_table.table_name.clone(),
            table_ref: None,
            returning_list: Vec::new(),
            on_conflict_info: None,
            cte_map,
            default_values: false,
            column_order: InsertColumnOrder::InsertByPosition,
        };
        if let Some(column_order) = self.transform_optional(list, 5)? {
            result.column_order = column_order;
        }
        if let Some(columns) = self.transform_optional(list, 6)? {
            result.columns = columns;
        }
        let insert_values: InsertValues = self.transform_child(list, 7)?;
        if !result.columns.is_empty() && insert_values.default_values {
            return Err(Error::parser(
                "You can not provide both a column list and DEFAULT VALUES, please remove one of the two",
            ));
        }
        if insert_values.default_values {
            result.default_values = true;
        }
        if let Some(select_statement) = insert_values.select_statement {
            result.select_statement = Some(Box::new(select_statement));
        }
        if let Some(on_conflict_clause) = list.optional(8)? {
            if or_action.is_some() {
                // OR REPLACE | OR IGNORE are shorthands for the ON CONFLICT clause.
                return Err(Error::parser(
                    "You can not provide both OR REPLACE|IGNORE and an ON CONFLICT clause, please remove the first \
                     if you want to have more granual control",
                ));
            }
            let on_conflict_info: OnConflictInfo = self.transform(on_conflict_clause)?;
            result.on_conflict_info = Some(Box::new(on_conflict_info));
            result.table_ref = Some(Box::new(insert_target));
        } else if let Some(or_action) = or_action {
            let mut on_conflict_info = on_conflict_info();
            on_conflict_info.action_type = self.transform(or_action)?;
            result.on_conflict_info = Some(Box::new(on_conflict_info));
            result.table_ref = Some(Box::new(insert_target));
        }
        if let Some(returning_list) = self.transform_optional(list, 9)? {
            result.returning_list = returning_list;
        }
        Ok(SqlStatement::new(SqlStatementKind::Insert(Box::new(result))))
    }

    /// `TransformOrAction`: `OR (REPLACE / IGNORE)`.
    pub(super) fn transform_or_action(&mut self, parse_result: &ParseResult) -> Result<OnConflictAction> {
        let replace_or_ignore_keyword = parse_result.list()?.choice(1)?.keyword()?;
        if replace_or_ignore_keyword.eq_ignore_ascii_case("replace") {
            Ok(OnConflictAction::Replace)
        } else if replace_or_ignore_keyword.eq_ignore_ascii_case("ignore") {
            Ok(OnConflictAction::Nothing)
        } else {
            Err(Error::internal(format!(
                "Unexpected keyword {replace_or_ignore_keyword} encountered for OrAction"
            )))
        }
    }

    /// `TransformInsertTarget`: `BaseTableName InsertAlias?`.
    pub(super) fn transform_insert_target(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        let list = parse_result.list()?;
        let mut table_ref: BaseTable = self.transform_child(list, 0)?;
        if let Some(alias) = self.transform_optional(list, 1)? {
            table_ref.0.alias = alias;
        }
        Ok(table_ref)
    }

    /// `TransformInsertAlias`: `AS Identifier`.
    pub(super) fn transform_insert_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(1)?.to_owned())
    }

    /// `TransformOnConflictClause`: `ON CONFLICT OnConflictTarget? OnConflictAction`.
    pub(super) fn transform_on_conflict_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        let list = parse_result.list()?;
        let mut result: OnConflictInfo = self.transform_child(list, 3)?;
        if let Some(on_conflict_target) = list.optional(2)? {
            let expression_target: OnConflictExpressionTarget = self.transform(on_conflict_target)?;
            result.indexed_columns = expression_target.indexed_columns;
            if let Some(where_clause) = expression_target.where_clause {
                result.condition = Some(Box::new(where_clause));
            }
        }
        Ok(result)
    }

    /// `TransformOnConflictTarget`: `OnConflictExpressionTarget /
    /// OnConflictIndexTarget`.
    pub(super) fn transform_on_conflict_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOnConflictExpressionTarget`: `ColumnIdList WhereClause?`.
    pub(super) fn transform_on_conflict_expression_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        let list = parse_result.list()?;
        Ok(OnConflictExpressionTarget {
            indexed_columns: self.transform_child(list, 0)?,
            where_clause: self.transform_optional(list, 1)?,
        })
    }

    /// `TransformOnConflictIndexTarget`: `ON CONSTRAINT name` is not supported.
    pub(super) fn transform_on_conflict_index_target(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        Err(Error::not_implemented(
            "ON CONSTRAINT conflict target is not supported yet",
        ))
    }

    /// `TransformOnConflictAction`: `OnConflictUpdate / OnConflictNothing`.
    pub(super) fn transform_on_conflict_action(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOnConflictUpdate`: `DO UPDATE SET UpdateSetClause WhereClause?`.
    pub(super) fn transform_on_conflict_update(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        let list = parse_result.list()?;
        let mut set_info: UpdateSetInfo = self.transform_child(list, 3)?;
        if let Some(condition) = self.transform_optional(list, 4)? {
            set_info.condition = Some(Box::new(condition));
        }
        Ok(OnConflictInfo {
            action_type: OnConflictAction::Update,
            set_info: Some(Box::new(set_info)),
            ..on_conflict_info()
        })
    }

    /// `TransformOnConflictNothing`: `DO NOTHING`.
    pub(super) fn transform_on_conflict_nothing(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        Ok(OnConflictInfo {
            action_type: OnConflictAction::Nothing,
            ..on_conflict_info()
        })
    }

    /// `TransformInsertValues`: `SelectStatementInternal / DefaultValues`.
    pub(super) fn transform_insert_values(&mut self, parse_result: &ParseResult) -> Result<InsertValues> {
        let choice = parse_result.list()?.choice(0)?;
        match choice.name {
            "DefaultValues" => Ok(InsertValues {
                default_values: true,
                select_statement: None,
            }),
            "SelectStatementInternal" => Ok(InsertValues {
                default_values: false,
                select_statement: Some(self.transform(choice)?),
            }),
            _ => Err(Error::internal("Unexpected choice in InsertValues statement.")),
        }
    }

    /// `TransformByNameOrPosition`: `BY (NAME / POSITION)`.
    pub(super) fn transform_by_name_or_position(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<InsertColumnOrder> {
        Self::transform_enum(parse_result.list()?.choice(1)?)
    }

    /// `TransformInsertColumnList`: `Parens(ColumnList)`.
    pub(super) fn transform_insert_column_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        let column_list = parse_result.list()?.child(0)?.parens_content()?;
        self.transform(column_list)
    }

    /// `TransformColumnList`: `List(ColId)`.
    pub(super) fn transform_column_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        parse_result
            .list()?
            .child(0)?
            .list_elements()?
            .into_iter()
            .map(|column| self.transform(column))
            .collect()
    }

    /// `TransformReturningClause`: `RETURNING TargetList`.
    pub(super) fn transform_returning_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_child(parse_result.list()?, 1)
    }
}
