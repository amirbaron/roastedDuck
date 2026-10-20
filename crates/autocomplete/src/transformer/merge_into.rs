//! The transform functions of DuckDB's `transform_merge_into.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use super::prelude::*;

/// `make_uniq<MergeIntoAction>()`: a value-initialized action (`MERGE_UPDATE`).
const fn merge_into_action() -> MergeIntoAction {
    MergeIntoAction {
        action_type: MergeActionType::MergeUpdate,
        condition: None,
        update_info: None,
        insert_columns: Vec::new(),
        expressions: Vec::new(),
        column_order: InsertColumnOrder::InsertByPosition,
        default_values: false,
        exclude_columns: BTreeSet::new(),
    }
}

/// `MergeIntoStatement::ActionConditionToString`.
const fn action_condition_to_string(condition: MergeActionCondition) -> &'static str {
    match condition {
        MergeActionCondition::WhenMatched => "WHEN MATCHED",
        MergeActionCondition::WhenNotMatchedByTarget => "WHEN NOT MATCHED",
        MergeActionCondition::WhenNotMatchedBySource => "WHEN NOT MATCHED BY SOURCE",
    }
}

impl PegTransformer {
    /// `TransformMergeIntoStatement`: `WithClause? MERGE INTO TargetOptAlias
    /// MergeIntoUsingClause JoinQualifier MergeMatch+ ReturningClause?`; the unconditional
    /// action of each condition comes after its conditional ones.
    pub(super) fn transform_merge_into_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let cte_map = self.transform_optional(list, 0)?.unwrap_or_default();
        let BaseTable(target) = self.transform_child(list, 3)?;
        let source: TableRef = self.transform_child(list, 4)?;
        let mut result = MergeIntoStatement {
            target: Box::new(target),
            source: Box::new(source),
            join_condition: None,
            using_columns: Vec::new(),
            actions: BTreeMap::new(),
            returning_list: Vec::new(),
            cte_map,
        };
        let join_condition: JoinQualifier = self.transform_child(list, 5)?;
        if let Some(on_clause) = join_condition.on_clause {
            result.join_condition = Some(Box::new(on_clause));
        } else {
            result.using_columns = join_condition.using_columns;
        }

        let mut unconditional_actions: BTreeMap<MergeActionCondition, MergeIntoAction> = BTreeMap::new();
        for merge_match in list.repeat(6)? {
            let (action_condition, action): (MergeActionCondition, MergeIntoAction) =
                self.transform(merge_match)?;
            if action.condition.is_none() {
                if unconditional_actions.contains_key(&action_condition) {
                    let action_condition_str = action_condition_to_string(action_condition);
                    return Err(Error::parser(format!(
                        "Unconditional {action_condition_str} clause was already defined - only one unconditional \
                         {action_condition_str} clause is supported"
                    )));
                }
                unconditional_actions.insert(action_condition, action);
                continue;
            }
            result.actions.entry(action_condition).or_default().push(action);
        }
        // Finally add the unconditional actions.
        for (action_condition, action) in unconditional_actions {
            result.actions.entry(action_condition).or_default().push(action);
        }
        if let Some(returning_list) = self.transform_optional(list, 7)? {
            result.returning_list = returning_list;
        }
        Ok(SqlStatement::new(SqlStatementKind::MergeInto(Box::new(result))))
    }

    /// `TransformMergeIntoUsingClause`: `USING TableRef`.
    pub(super) fn transform_merge_into_using_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformMergeMatch`: `MatchedClause / NotMatchedClause`.
    pub(super) fn transform_merge_match(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformMatchedClause`: `WHEN MATCHED AndExpression? THEN MatchedClauseAction`.
    pub(super) fn transform_matched_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        let list = parse_result.list()?;
        let mut merge_into_action: MergeIntoAction = self.transform_child(list, 4)?;
        if let Some(condition) = self.transform_optional(list, 2)? {
            merge_into_action.condition = Some(Box::new(condition));
        }
        Ok((MergeActionCondition::WhenMatched, merge_into_action))
    }

    /// `TransformMatchedClauseAction`: `UPDATE ... / DELETE / INSERT ... / DO NOTHING /
    /// ERROR ...`.
    pub(super) fn transform_matched_clause_action(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformUpdateMatchClause`: `UPDATE UpdateMatchInfo?`.
    pub(super) fn transform_update_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let mut result = match parse_result.list()?.optional(1)? {
            Some(update_info) => self.transform(update_info)?,
            None => merge_into_action(),
        };
        result.action_type = MergeActionType::MergeUpdate;
        Ok(result)
    }

    /// `TransformUpdateMatchInfo`: `UpdateMatchSetClause / ByNameOrPosition`.
    pub(super) fn transform_update_match_info(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let mut result = merge_into_action();
        let choice = parse_result.list()?.choice(0)?;
        if choice.name == "ByNameOrPosition" {
            result.column_order = self.transform(choice)?;
        } else {
            let update_info: Option<UpdateSetInfo> = self.transform(choice)?;
            result.update_info = update_info.map(Box::new);
        }
        Ok(result)
    }

    /// `TransformUpdateMatchSetClause`: `SET (UpdateSetClause / '*')`; `SET *` has no
    /// assignments (DuckDB's null `UpdateSetInfo`).
    pub(super) fn transform_update_match_set_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Option<UpdateSetInfo>> {
        let choice = parse_result.list()?.list(1)?.choice(0)?;
        if choice.result_type() == ParseResultType::Keyword {
            // We found the '*'.
            return Ok(None);
        }
        self.transform(choice).map(Some)
    }

    /// `TransformDeleteMatchClause`: `DELETE`.
    pub(super) fn transform_delete_match_clause(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Ok(MergeIntoAction {
            action_type: MergeActionType::MergeDelete,
            ..merge_into_action()
        })
    }

    /// `TransformInsertMatchClause`: `INSERT InsertMatchInfo?`.
    pub(super) fn transform_insert_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let mut result = match parse_result.list()?.optional(1)? {
            Some(insert_info) => self.transform(insert_info)?,
            None => merge_into_action(),
        };
        result.action_type = MergeActionType::MergeInsert;
        Ok(result)
    }

    /// `TransformInsertMatchInfo`: `InsertValuesList / InsertDefaultValues /
    /// InsertByNameOrPosition`.
    pub(super) fn transform_insert_match_info(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformInsertDefaultValues`: `DEFAULT VALUES`.
    pub(super) fn transform_insert_default_values(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Ok(MergeIntoAction {
            default_values: true,
            ..merge_into_action()
        })
    }

    /// `TransformInsertByNameOrPosition`: `ByNameOrPosition? '*'?`.
    pub(super) fn transform_insert_by_name_or_position(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let mut result = merge_into_action();
        if let Some(column_order) = self.transform_optional(parse_result.list()?, 0)? {
            result.column_order = column_order;
        }
        Ok(result)
    }

    /// `TransformInsertValuesList`: `InsertColumnList? VALUES Parens(List(Expression))`.
    pub(super) fn transform_insert_values_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let list = parse_result.list()?;
        let mut result = merge_into_action();
        if let Some(insert_columns) = self.transform_optional(list, 0)? {
            result.insert_columns = insert_columns;
        }
        for expr in list.child(2)?.parens_content()?.list_elements()? {
            result.expressions.push(self.transform(expr)?);
        }
        Ok(result)
    }

    /// `TransformDoNothingMatchClause`: `DO NOTHING`.
    pub(super) fn transform_do_nothing_match_clause(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Ok(MergeIntoAction {
            action_type: MergeActionType::MergeDoNothing,
            ..merge_into_action()
        })
    }

    /// `TransformErrorMatchClause`: `ERROR Expression?`.
    pub(super) fn transform_error_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        let mut result = MergeIntoAction {
            action_type: MergeActionType::MergeError,
            ..merge_into_action()
        };
        if let Some(expression) = parse_result.list()?.optional(1)? {
            result.expressions.push(self.transform(expression)?);
        }
        Ok(result)
    }

    /// `TransformAndExpression`: `AND Expression`.
    pub(super) fn transform_and_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformNotMatchedClause`: `WHEN NOT MATCHED BySourceOrTarget? AndExpression?
    /// THEN MatchedClauseAction`; without `BY SOURCE` / `BY TARGET` it is by target.
    pub(super) fn transform_not_matched_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        let list = parse_result.list()?;
        let mut result: MergeIntoAction = self.transform_child(list, 6)?;
        if let Some(condition) = self.transform_optional(list, 4)? {
            result.condition = Some(Box::new(condition));
        }
        let action_condition = self
            .transform_optional(list, 3)?
            .unwrap_or(MergeActionCondition::WhenNotMatchedByTarget);
        Ok((action_condition, result))
    }

    /// `TransformBySourceOrTarget`: `BySource / ByTarget`.
    pub(super) fn transform_by_source_or_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeActionCondition> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }
}
