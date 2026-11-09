//! The transform functions of DuckDB's `transform_attach.cpp`: `ATTACH` and the generic
//! `(name value, ...)` option lists `ATTACH`, `EXPLAIN`, `EXPORT`, ... share.

use roastedduck_ast::expression_type_to_string;
use roastedduck_parser::hash_order::CiHashMap;

use super::prelude::*;

/// The constant a `GenericCopyOption` value written as a column is (`GetColumnName`).
fn column_name_value(column_ref: &ColumnRefExpression) -> Result<Value> {
    Value::try_varchar(
        column_ref
            .column_names
            .last()
            .map_or(b"".as_slice(), ByteString::as_bytes),
    )
}

impl PegTransformer {
    /// `TransformAttachStatement`: `ATTACH OrReplace? IfNotExists? DATABASE? DatabasePath
    /// AttachAlias? AttachOptions?`.
    pub(super) fn transform_attach_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let or_replace = list.has(1)?;
        let if_not_exists = list.has(2)?;
        if or_replace && if_not_exists {
            return Err(Error::parser(
                "Cannot specify both OR REPLACE and IF NOT EXISTS at the same time",
            ));
        }
        let on_conflict = if or_replace {
            OnCreateConflict::ReplaceOnConflict
        } else if if_not_exists {
            OnCreateConflict::IgnoreOnConflict
        } else {
            OnCreateConflict::ErrorOnConflict
        };
        let mut info = AttachInfo {
            name: String::new().into(),
            path: self.transform_child(list, 4)?,
            options: OrderedMap::default(),
            on_conflict,
            parsed_options: CaseInsensitiveMap::default(),
        };
        if let Some(name) = self.transform_optional(list, 5)? {
            info.name = name;
        }
        let copy_options: Vec<GenericCopyOption> = self.transform_optional(list, 6)?.unwrap_or_default();
        let mut parsed_options = CiHashMap::new();
        for copy_option in copy_options {
            if let Some(expression) = copy_option.expression {
                parsed_options.set(copy_option.name, expression);
                continue;
            }
            let value = match <[Value; 1]>::try_from(copy_option.children) {
                Ok([value]) => {
                    if value.is_null() {
                        return Err(Error::binder(format!(
                            "NULL is not supported as a valid option for ATTACH option \"{}\"",
                            copy_option.name
                        )));
                    }
                    value
                }
                Err(children) if children.is_empty() => Value::boolean(true),
                Err(_) => {
                    return Err(Error::parser(format!(
                        "Option {} can only have one argument",
                        copy_option.name
                    )));
                }
            };
            info.options.insert(copy_option.name.into(), value);
        }
        info.parsed_options = parsed_options.into_ordered().into_iter().collect();
        Ok(SqlStatement::new(SqlStatementKind::Attach(Box::new(info))))
    }

    /// `TransformAttachAlias`: `AS ColId`.
    pub(super) fn transform_attach_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        let col_id = parse_result.list()?.list(1)?.choice(0)?;
        if col_id.result_type() == ParseResultType::Identifier {
            return Ok(col_id.identifier()?.to_owned());
        }
        self.transform(col_id)
    }

    /// `TransformAttachOptions`: a `GenericCopyOptionList`.
    pub(super) fn transform_attach_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformGenericCopyOptionList`: `Parens(List(GenericCopyOption))`.
    pub(super) fn transform_generic_copy_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        parse_result
            .list()?
            .child(0)?
            .parens_content()?
            .list_elements()?
            .into_iter()
            .map(|option| self.transform(option))
            .collect()
    }

    /// `TransformGenericCopyOption`: `name [value]`; the value is kept as constants
    /// where it is one (a column is its name, a `row(...)`-like placeholder its
    /// elements, `'t'::BOOLEAN` / `'f'::BOOLEAN` booleans), else as the expression.
    pub(super) fn transform_generic_copy_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let list = parse_result.list()?;
        let mut copy_option = GenericCopyOption {
            name: list.identifier(0)?.to_ascii_lowercase(),
            ..GenericCopyOption::default()
        };
        let Some(optional_expression) = list.optional(1)? else {
            return Ok(copy_option);
        };
        let expression: ParsedExpression = self.transform(optional_expression)?;
        match (expression.expression_type, &expression.kind) {
            (ExpressionType::ValueConstant, ParsedExpressionKind::Constant(constant)) => {
                copy_option.children.push(constant.value.clone());
            }
            (ExpressionType::ColumnRef, ParsedExpressionKind::ColumnRef(column_ref)) => {
                copy_option.children.push(column_name_value(column_ref)?);
            }
            (ExpressionType::Placeholder, ParsedExpressionKind::Operator(op_expr)) => {
                for child in &op_expr.children {
                    match &child.kind {
                        ParsedExpressionKind::Constant(constant) => {
                            copy_option.children.push(constant.value.clone());
                        }
                        ParsedExpressionKind::ColumnRef(column_ref) => {
                            copy_option.children.push(column_name_value(column_ref)?);
                        }
                        _ => {
                            return Err(Error::internal(format!(
                                "Unexpected expression type {} encountered for GenericCopyOption",
                                child.expression_class().name()
                            )));
                        }
                    }
                }
            }
            (ExpressionType::Function, _) => copy_option.expression = Some(expression),
            (ExpressionType::Star, _) => copy_option.children.push(Value::varchar("*")),
            (ExpressionType::OperatorCast, ParsedExpressionKind::Cast(cast_expr)) => {
                let constant = match &cast_expr.child.kind {
                    ParsedExpressionKind::Constant(constant) => Some(constant.value.to_varchar()?),
                    _ => None,
                };
                match constant.as_deref() {
                    Some("t") => copy_option.children.push(Value::boolean(true)),
                    Some("f") => copy_option.children.push(Value::boolean(false)),
                    _ => copy_option.expression = Some(expression),
                }
            }
            (expression_type, _) => {
                return Err(Error::not_implemented(format!(
                    "Unrecognized expression type {}",
                    expression_type_to_string(expression_type)
                )));
            }
        }
        Ok(copy_option)
    }

    /// `TransformDatabasePath`: the path string.
    pub(super) fn transform_database_path(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform_child(parse_result.list()?, 0)
    }
}
