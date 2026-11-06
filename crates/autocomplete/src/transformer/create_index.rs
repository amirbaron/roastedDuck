//! The transform functions of DuckDB's `transform_create_index.cpp`: `CREATE INDEX` and
//! the `WITH (name = value, ...)` option lists.

use rustdb_parser::hash_order::CiHashMap;

use super::create_table::{base_table_ref, create_info, on_create_conflict};
use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateIndexStmt`: `[UNIQUE] INDEX [IF NOT EXISTS] name ON table
    /// [(columns)] [USING type] [(expressions)] [WITH (...)] [WHERE ...]`.
    pub(super) fn transform_create_index_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let unique = list.has(0)?;
        let constraint_type = if unique {
            IndexConstraintType::Unique
        } else {
            IndexConstraintType::None
        };
        let if_not_exists = list.has(2)?;
        let Some(index_name) = list.optional(3)? else {
            return Err(Error::not_implemented(
                "Please provide an index name, e.g., CREATE INDEX my_name ...",
            ));
        };
        let index_name = index_name.identifier()?.to_owned();
        let table = base_table_ref(self.transform_child(list, 5)?)?;
        let mut index_info = CreateIndexInfo {
            name: index_name.into(),
            table: table.table_name.clone(),
            constraint_type,
            parsed_expressions: Vec::new(),
            scan_types: Vec::new(),
            names: Vec::new(),
            column_ids: Vec::new(),
            options: CaseInsensitiveMap::default(),
            index_type_name: "ART".to_owned().into(),
        };
        if let Some(column_list) = list.optional(6)? {
            let column_list: Vec<String> = self.transform(column_list)?;
            for column in column_list {
                index_info
                    .parsed_expressions
                    .push(ParsedExpression::column_ref(vec![
                        table.table_name.clone(),
                        column.into(),
                    ]));
            }
        }
        if let Some(index_type) = self.transform_optional(list, 7)? {
            index_info.index_type_name = index_type;
        }
        if let Some(index_elements) = list.optional(8)? {
            for index_element in index_elements.parens_content()?.list_elements()? {
                let expr: ParsedExpression = self.transform(index_element)?;
                if expr.expression_type == ExpressionType::Collate {
                    return Err(Error::not_implemented("Index with collation not supported yet!"));
                }
                index_info.parsed_expressions.push(expr);
            }
        }
        if let Some(with_list) = list.optional(9)? {
            let options_expr: CaseInsensitiveMap<ParsedExpression, ByteString> = self.transform(with_list)?;
            // `index_info->options` is a `case_insensitive_map_t`, filled in the
            // iteration order of the options.
            let mut options: CiHashMap<Value, ByteString> = CiHashMap::default();
            for (name, expr) in options_expr.iter() {
                let ParsedExpressionKind::Constant(const_expr) = &expr.kind else {
                    return Err(Error::invalid_input(
                        "Create index option must be a constant value",
                    ));
                };
                options.set(name.to_owned(), const_expr.value.clone());
            }
            index_info.options = options.into_ordered().into_iter().collect();
        }
        if list.has(10)? {
            return Err(Error::not_implemented(
                "Creating partial indexes is not supported currently",
            ));
        }
        let mut info = create_info(&table.schema_name, CreateInfoKind::Index(Box::new(index_info)));
        info.catalog = table.catalog_name;
        info.on_conflict = on_create_conflict(if_not_exists);
        Ok(info)
    }

    /// `TransformIndexType`: `USING name`.
    pub(super) fn transform_index_type(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(1)?.to_owned())
    }

    /// `TransformIndexElement`: the expression; `ASC` / `DESC` and `NULLS` are ignored.
    pub(super) fn transform_index_element(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformWithList`: `WITH (options)`.
    pub(super) fn transform_with_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression, ByteString>> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformRelOptionOrOids`.
    pub(super) fn transform_rel_option_or_oids(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression, ByteString>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformRelOptionList`: `(name [= value], ...)`, a `case_insensitive_map_t` in
    /// its libc++ iteration order; the first of duplicate names wins.
    pub(super) fn transform_rel_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression, ByteString>> {
        let rel_option_list = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut result: CiHashMap<ParsedExpression> = CiHashMap::new();
        for rel_option in rel_option_list {
            let (name, value): (String, ParsedExpression) = self.transform(rel_option)?;
            result.insert(name, value);
        }
        Ok(result.into_ordered().into_iter().collect())
    }

    /// `TransformOids`: `WITH | WITHOUT OIDS` is not supported.
    pub(super) fn transform_oids(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression, ByteString>> {
        Err(Error::not_implemented("Oids for index are not yet implemented."))
    }

    /// `TransformRelOptionName`: a dotted name or a string.
    pub(super) fn transform_rel_option_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name.eq_ignore_ascii_case("DottedIdentifier") {
            let dotted_identifier: Vec<String> = self.transform(choice)?;
            return Ok(dotted_identifier.join("."));
        }
        Ok(choice.string_literal()?.0.to_owned())
    }

    /// `TransformRelOptionArgumentOpt`: `= value`; keywords and strings are string
    /// constants, `NULL` / `NONE` a NULL.
    pub(super) fn transform_rel_option_argument_opt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let def_arg_choice = parse_result.list()?.list(1)?.choice(0)?;
        match def_arg_choice.name {
            "ReservedKeyword" => {
                let keyword = def_arg_choice.list()?.choice(0)?.keyword()?;
                Ok(ParsedExpression::constant(Value::varchar(keyword)))
            }
            "StringLiteral" => {
                let text: String = self.transform(def_arg_choice)?;
                Ok(ParsedExpression::constant(Value::varchar(text)))
            }
            "NoneLiteral" | "NullLiteral" => Ok(ParsedExpression::constant(Value::default())),
            "NumberLiteral" | "Expression" => self.transform(def_arg_choice),
            other => Err(Error::parser(format!(
                "Unexpected rule encountered in TransformRelOptionArgumentOpt: {other}"
            ))),
        }
    }

    /// `TransformRelOption`: `name [= value]`; without a value, NULL.
    pub(super) fn transform_rel_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        let list = parse_result.list()?;
        let option_name: String = self.transform_child(list, 0)?;
        let Some(argument) = list.optional(1)? else {
            return Ok((option_name, ParsedExpression::constant(Value::default())));
        };
        let value = self.transform(argument)?;
        Ok((option_name, value))
    }

    /// `TransformIndexName`.
    pub(super) fn transform_index_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }
}
