//! The transform functions of DuckDB's `transform_comment.cpp`: `COMMENT ON ... IS ...`.

#![allow(
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "transform functions have the signature the rule registry calls"
)]

use super::prelude::*;

impl PegTransformer {
    /// `TransformCommentStatement`: `COMMENT ON type name IS value`; on a column the last
    /// part of the name is the column.
    pub(super) fn transform_comment_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let comment_on_type: CatalogType = self.transform_child(list, 2)?;
        let mut dotted_identifier: Vec<String> = self.transform_child(list, 3)?;
        let comment_value: Value = self.transform_child(list, 5)?;
        let info = match comment_on_type {
            // A column.
            CatalogType::Invalid => {
                let column_name = dotted_identifier
                    .pop()
                    .ok_or_else(|| Error::internal("COMMENT ON COLUMN without a name"))?;
                if dotted_identifier.is_empty() {
                    return Err(Error::parser(format!(
                        "Invalid column reference: '{column_name}'"
                    )));
                }
                let qualified_name = QualifiedName::from_parts(dotted_identifier)?;
                AlterInfo {
                    catalog: qualified_name.catalog,
                    schema: qualified_name.schema,
                    name: qualified_name.name,
                    if_not_found: OnEntryNotFound::ThrowException,
                    allow_internal: false,
                    kind: AlterInfoKind::SetColumnComment(Box::new(SetColumnCommentInfo {
                        catalog_entry_type: CatalogType::Invalid,
                        comment_value,
                        column_name,
                    })),
                }
            }
            CatalogType::DatabaseEntry => {
                return Err(Error::not_implemented(
                    "Adding comments to databases is not implemented",
                ));
            }
            CatalogType::SchemaEntry => {
                return Err(Error::not_implemented(
                    "Adding comments to schemas is not implemented",
                ));
            }
            entry_catalog_type => {
                let qualified_name = QualifiedName::from_parts(dotted_identifier)?;
                AlterInfo {
                    catalog: qualified_name.catalog,
                    schema: qualified_name.schema,
                    name: qualified_name.name,
                    if_not_found: OnEntryNotFound::ThrowException,
                    allow_internal: false,
                    kind: AlterInfoKind::SetComment(SetCommentInfo {
                        entry_catalog_type,
                        comment_value,
                    }),
                }
            }
        };
        Ok(SqlStatement::new(SqlStatementKind::Alter(Box::new(info))))
    }

    /// `TransformCommentOnType`: what is commented on; `COLUMN` is `INVALID`.
    pub(super) fn transform_comment_on_type(&mut self, parse_result: &ParseResult) -> Result<CatalogType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformCommentValue`: `NULL` or a string.
    pub(super) fn transform_comment_value(&mut self, parse_result: &ParseResult) -> Result<Value> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::String {
            return Ok(Value::varchar(choice.string_literal()?.0));
        }
        Ok(Value::default())
    }
}
