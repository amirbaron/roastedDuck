//! The transform functions of DuckDB's `transform_pragma.cpp`.

use super::prelude::*;

/// A `PRAGMA` parameter: an unqualified column is its name and a qualified one its
/// text, as string constants.
fn pragma_parameter(parameter: ParsedExpression) -> ParsedExpression {
    if parameter.expression_type == ExpressionType::ColumnRef
        && let ParsedExpressionKind::ColumnRef(colref) = &parameter.kind
    {
        let text = if colref.column_names.len() > 1 {
            parameter.to_string()
        } else {
            colref.column_names.last().cloned().unwrap_or_default()
        };
        return ParsedExpression::constant(Value::varchar(text));
    }
    parameter
}

/// `PragmaStatement()`: a `PRAGMA` without parameters.
fn pragma_info(name: String) -> PragmaInfo {
    PragmaInfo {
        name,
        parameters: Vec::new(),
        named_parameters: CaseInsensitiveMap::default(),
    }
}

impl PegTransformer {
    /// `TransformPragmaStatement`: `PRAGMA (PragmaAssign / PragmaFunction)`.
    pub(super) fn transform_pragma_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        self.transform(parse_result.list()?.list(1)?.choice(0)?)
    }

    /// `TransformPragmaAssign`: `SettingName '=' VariableList` is a `SET`, except for
    /// SQLite's `PRAGMA table_info = 't'`, which is a call.
    pub(super) fn transform_pragma_assign(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut info = pragma_info(list.identifier(0)?.to_owned());
        let value_list: Vec<ParsedExpression> = self.transform_child(list, 2)?;
        let Ok([expr]) = <[ParsedExpression; 1]>::try_from(value_list) else {
            return Err(Error::parser(
                "PRAGMA statement with assignment should contain exactly one parameter",
            ));
        };
        info.parameters.push(pragma_parameter(expr));
        // SQLite does not distinguish between "PRAGMA table_info='integers'" and
        // "PRAGMA table_info('integers')": pragmas matching the SQLite ones are calls.
        if info.name.eq_ignore_ascii_case("table_info") {
            return Ok(SqlStatement::new(SqlStatementKind::Pragma(Box::new(info))));
        }
        let value = info.parameters.pop().map(Box::new);
        Ok(SqlStatement::new(SqlStatementKind::Set(SetStatement {
            name: info.name,
            scope: SetScope::Automatic,
            value,
        })))
    }

    /// `TransformPragmaFunction`: `PragmaName PragmaParameters?`.
    pub(super) fn transform_pragma_function(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut info = pragma_info(list.identifier(0)?.to_owned());
        if let Some(parameters) = list.optional(1)? {
            let parameters: Vec<ParsedExpression> = self.transform(parameters)?;
            info.parameters
                .extend(parameters.into_iter().map(pragma_parameter));
        }
        Ok(SqlStatement::new(SqlStatementKind::Pragma(Box::new(info))))
    }

    /// `TransformPragmaParameters`: `Parens(List(Expression))`.
    pub(super) fn transform_pragma_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|expr| self.transform(expr))
            .collect()
    }
}
