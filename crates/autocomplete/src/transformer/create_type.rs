//! The transform functions of DuckDB's `transform_create_type.cpp`: `CREATE TYPE name AS
//! ENUM (...)` / `AS type`.

use std::collections::HashSet;

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

/// `DEFAULT_SCHEMA`, the schema of a `CreateTypeInfo()`.
const DEFAULT_SCHEMA: &str = "main";

impl PegTransformer {
    /// `TransformCreateTypeStmt`: `TYPE [IF NOT EXISTS] name AS type`.
    pub(super) fn transform_create_type_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(1)?;
        let qualified_name: QualifiedName = self.transform_child(list, 2)?;
        let mut create_type_info: CreateTypeInfo = self.transform_child(list, 4)?;
        create_type_info.name = qualified_name.name;
        let mut info = create_info(DEFAULT_SCHEMA, CreateInfoKind::Type(create_type_info));
        info.catalog = qualified_name.catalog;
        info.schema = qualified_name.schema;
        info.on_conflict = on_create_conflict(if_not_exists);
        Ok(info)
    }

    /// `TransformCreateType`: `ENUM (query)`, `ENUM ('a', ...)` or a type.
    pub(super) fn transform_create_type(&mut self, parse_result: &ParseResult) -> Result<CreateTypeInfo> {
        let choice = parse_result.list()?.choice(0)?;
        let mut result = CreateTypeInfo {
            name: String::new().into(),
            logical_type: LogicalType::INVALID,
            query: None,
        };
        if choice.name == "EnumSelectType" {
            let query: SelectStatement = self.transform(choice)?;
            result.query = Some(Box::new(query));
            result.logical_type = LogicalType::INVALID;
        } else {
            result.logical_type = self.transform(choice)?;
        }
        Ok(result)
    }

    /// `TransformEnumSelectType`: `ENUM (query)`.
    pub(super) fn transform_enum_select_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        self.transform(parse_result.list()?.child(1)?.parens_content()?)
    }

    /// `TransformEnumStringLiteralList`: `ENUM ('a', ...)`; the values must be unique.
    pub(super) fn transform_enum_string_literal_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LogicalType> {
        let extract_parens = parse_result.list()?.child(1)?.parens_content()?;
        let Some(string_list) = extract_parens.optional()? else {
            return Ok(LogicalType::enum_type(Vec::new()));
        };
        let mut values = Vec::new();
        for string_literal in string_list.list_elements()? {
            values.push(string_literal.string_literal()?.0.to_owned());
        }
        // `EnumTypeInfoTemplated` rejects duplicates.
        let mut seen = HashSet::with_capacity(values.len());
        for value in &values {
            if !seen.insert(value.as_str()) {
                return Err(Error::invalid_input(format!(
                    "Attempted to create ENUM type with duplicate value {value}"
                )));
            }
        }
        Ok(LogicalType::enum_type(values))
    }
}
