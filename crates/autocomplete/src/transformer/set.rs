//! The transform functions of DuckDB's `transform_set.cpp`: `SET` and `RESET`.

use super::explain::vector_index_error;
use super::prelude::*;

impl PegTransformer {
    /// `TransformResetStatement`: `RESET (SetVariable / SetSetting)`.
    pub(super) fn transform_reset_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let choice = parse_result.list()?.list(1)?.choice(0)?;
        let setting_info: SettingInfo = self.transform(choice)?;
        Ok(SqlStatement::new(SqlStatementKind::Set(SetStatement {
            name: setting_info.name.into(),
            scope: setting_info.scope,
            value: None,
        })))
    }

    /// `TransformSetAssignment`: `VariableAssign VariableList`.
    pub(super) fn transform_set_assignment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformSetSetting`: `SettingScope? SettingName`.
    pub(super) fn transform_set_setting(&mut self, parse_result: &ParseResult) -> Result<SettingInfo> {
        let list = parse_result.list()?;
        let mut result = SettingInfo {
            name: list.identifier(1)?.to_owned(),
            scope: SetScope::Automatic,
        };
        if let Some(setting_scope) = list.optional(0)? {
            result.scope = Self::transform_enum(setting_scope.list()?.child(0)?)?;
        }
        Ok(result)
    }

    /// `TransformSetStatement`: `SET (StandardAssignment / SetTimeZone)`.
    pub(super) fn transform_set_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let choice = parse_result.list()?.list(1)?.choice(0)?;
        let statement: SetStatement = self.transform(choice)?;
        Ok(SqlStatement::new(SqlStatementKind::Set(statement)))
    }

    /// `TransformSetTimeZone`: `TIME ZONE Expression` sets `timezone`.
    pub(super) fn transform_set_time_zone(&mut self, parse_result: &ParseResult) -> Result<SetStatement> {
        let expr: ParsedExpression = self.transform_child(parse_result.list()?, 2)?;
        Ok(SetStatement {
            name: "timezone".to_owned().into(),
            scope: SetScope::Automatic,
            value: Some(Box::new(expr)),
        })
    }

    /// `TransformSetVariable`: `VariableScope Identifier`.
    pub(super) fn transform_set_variable(&mut self, parse_result: &ParseResult) -> Result<SettingInfo> {
        let list = parse_result.list()?;
        let scope = Self::transform_enum(list.child(0)?)?;
        let name = list.identifier(1)?.to_owned();
        Ok(SettingInfo { name, scope })
    }

    /// `TransformStandardAssignment`: `(SetVariable / SetSetting) SetAssignment`; a column
    /// is its name, `DEFAULT` resets.
    pub(super) fn transform_standard_assignment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SetStatement> {
        let list = parse_result.list()?;
        let setting_info: SettingInfo = self.transform(list.list(0)?.choice(0)?)?;
        let values: Vec<ParsedExpression> = self.transform_child(list, 1)?;
        if values.len() > 1 {
            return Err(Error::parser("SET can only contain a single value"));
        }
        let Some(mut value) = values.into_iter().next() else {
            return Err(vector_index_error(0, 0));
        };
        match &value.kind {
            ParsedExpressionKind::ColumnRef(col_ref) => {
                // A SET value cannot be a column reference.
                let column_name = col_ref.column_names.last().cloned().unwrap_or_default();
                value = ParsedExpression::constant(Value::try_varchar(&column_name)?);
            }
            ParsedExpressionKind::Default => {
                return Ok(SetStatement {
                    name: setting_info.name.into(),
                    scope: setting_info.scope,
                    value: None,
                });
            }
            _ => {}
        }
        Ok(SetStatement {
            name: setting_info.name.into(),
            scope: setting_info.scope,
            value: Some(Box::new(value)),
        })
    }

    /// `TransformVariableList`: `List(Expression)`.
    pub(super) fn transform_variable_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        parse_result
            .list()?
            .child(0)?
            .list_elements()?
            .into_iter()
            .map(|expr| self.transform(expr))
            .collect()
    }
}
