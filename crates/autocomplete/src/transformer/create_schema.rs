//! The transform functions of DuckDB's `transform_create_schema.cpp`.

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

/// `DEFAULT_SCHEMA`, the schema of a `CreateSchemaInfo()`.
const DEFAULT_SCHEMA: &str = "main";

impl PegTransformer {
    /// `TransformCreateSchemaStmt`: `SCHEMA [IF NOT EXISTS] [catalog.]schema`.
    pub(super) fn transform_create_schema_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(1)?;
        let qualified_name: QualifiedName = self.transform_child(list, 2)?;
        if !qualified_name.catalog.is_empty() {
            return Err(Error::parser(
                "CREATE SCHEMA too many dots: expected \"catalog.schema\" or \"schema\"",
            ));
        }
        let mut info = create_info(DEFAULT_SCHEMA, CreateInfoKind::Schema);
        info.on_conflict = on_create_conflict(if_not_exists);
        info.catalog = qualified_name.schema;
        info.schema = qualified_name.name;
        Ok(info)
    }
}
