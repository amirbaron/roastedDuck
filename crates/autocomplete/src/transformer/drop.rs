//! The transform functions of DuckDB's `transform_drop.cpp`: `DROP ...`; one object at
//! a time.

use super::create_table::{base_table_ref, on_entry_not_found};
use super::prelude::*;

/// A `DropInfo()` of `catalog_type`.
fn drop_info(catalog_type: CatalogType) -> DropInfo {
    DropInfo {
        catalog_type,
        catalog: String::new().into(),
        schema: String::new().into(),
        name: String::new().into(),
        if_not_found: OnEntryNotFound::ThrowException,
        cascade: false,
        allow_drop_internal: false,
        extra_drop_info: None,
    }
}

/// The one entry of `List(entry)`.
fn single_entry(entries: &ParseResult) -> Result<&ParseResult> {
    let entries = entries.list_elements()?;
    if entries.len() > 1 {
        return Err(Error::not_implemented("Can only drop one object at a time"));
    }
    entries
        .first()
        .copied()
        .ok_or_else(|| Error::internal("DROP without an object"))
}

impl PegTransformer {
    /// `TransformDropStatement`: `DROP entries [CASCADE | RESTRICT]`.
    pub(super) fn transform_drop_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut drop_entry: DropInfo = self.transform_child(list, 1)?;
        if let Some(cascade) = self.transform_optional(list, 2)? {
            drop_entry.cascade = cascade;
        }
        Ok(SqlStatement::new(SqlStatementKind::Drop(Box::new(drop_entry))))
    }

    /// `TransformDropEntries`: what is dropped.
    pub(super) fn transform_drop_entries(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformDropTable`: `TABLE | VIEW | MATERIALIZED VIEW [IF EXISTS] table`.
    pub(super) fn transform_drop_table(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let catalog_type: CatalogType = self.transform_child(list, 0)?;
        let if_exists = list.has(1)?;
        let base_table_pr = single_entry(list.child(2)?)?;
        let base_table = base_table_ref(self.transform(base_table_pr)?)?;
        let mut info = drop_info(catalog_type);
        info.catalog = base_table.catalog_name;
        info.schema = base_table.schema_name;
        info.name = base_table.table_name;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformTableOrView`.
    pub(super) fn transform_table_or_view(&mut self, parse_result: &ParseResult) -> Result<CatalogType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformDropTableFunction`: `MACRO TABLE [IF EXISTS] name`.
    pub(super) fn transform_drop_table_function(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        list.list(0)?;
        let catalog_type: CatalogType = Self::transform_enum(list.child(0)?)?;
        let if_exists = list.has(1)?;
        let table_function = single_entry(list.child(2)?)?;
        let mut info = drop_info(catalog_type);
        info.name = table_function.identifier()?.into();
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformDropFunction`: `MACRO | FUNCTION [IF EXISTS] name`.
    pub(super) fn transform_drop_function(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let function: QualifiedName = self.transform(single_entry(list.child(2)?)?)?;
        let mut info = drop_info(CatalogType::MacroEntry);
        info.catalog = function.catalog;
        info.schema = function.schema;
        info.name = function.name;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformDropSchema`: `SCHEMA [IF EXISTS] [catalog.]schema`.
    pub(super) fn transform_drop_schema(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let schema: QualifiedName = self.transform(single_entry(list.child(2)?)?)?;
        let mut info = drop_info(CatalogType::SchemaEntry);
        info.catalog = schema.catalog;
        info.name = schema.schema;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformQualifiedSchemaName`: `[catalog.]schema`, as the catalog and schema.
    pub(super) fn transform_qualified_schema_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut result = QualifiedName::default();
        if let Some(catalog) = self.transform_optional(list, 0)? {
            result.catalog = catalog;
        }
        result.schema = list.identifier(1)?.into();
        Ok(result)
    }

    /// `TransformDropIndex`: `INDEX [IF EXISTS] name`.
    pub(super) fn transform_drop_index(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let index: QualifiedName = self.transform(single_entry(list.child(2)?)?)?;
        let mut info = drop_info(CatalogType::IndexEntry);
        info.catalog = index.catalog;
        info.schema = index.schema;
        info.name = index.name;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformQualifiedIndexName`: `[catalog.][schema.]name`.
    pub(super) fn transform_qualified_index_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut result = QualifiedName::default();
        if let Some(catalog) = self.transform_optional(list, 0)? {
            result.catalog = catalog;
        }
        if let Some(schema) = self.transform_optional(list, 1)? {
            result.schema = schema;
        }
        result.name = list.identifier(2)?.into();
        Ok(result)
    }

    /// `TransformDropSequence`: `SEQUENCE [IF EXISTS] name`.
    pub(super) fn transform_drop_sequence(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let sequence: QualifiedName = self.transform(single_entry(list.child(2)?)?)?;
        let mut info = drop_info(CatalogType::SequenceEntry);
        if sequence.schema.is_empty() {
            info.schema = sequence.catalog;
        } else {
            info.catalog = sequence.catalog;
            info.schema = sequence.schema;
        }
        info.name = sequence.name;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformDropCollation`: `COLLATION [IF EXISTS] name`. DuckDB drops it as a
    /// sequence entry.
    pub(super) fn transform_drop_collation(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let collation = single_entry(list.child(2)?)?.identifier()?;
        let mut info = drop_info(CatalogType::SequenceEntry);
        info.name = collation.into();
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformDropType`: `TYPE [IF EXISTS] name`.
    pub(super) fn transform_drop_type(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let if_exists = list.has(1)?;
        let ty: QualifiedName = self.transform(single_entry(list.child(2)?)?)?;
        let mut info = drop_info(CatalogType::TypeEntry);
        if ty.schema.is_empty() {
            info.schema = ty.catalog;
        } else {
            info.catalog = ty.catalog;
            info.schema = ty.schema;
        }
        info.name = ty.name;
        info.if_not_found = on_entry_not_found(if_exists);
        Ok(info)
    }

    /// `TransformDropBehavior`: true for `CASCADE`.
    pub(super) fn transform_drop_behavior(&mut self, parse_result: &ParseResult) -> Result<bool> {
        let keyword = parse_result.list()?.choice(0)?.keyword()?;
        Ok(keyword.eq_ignore_ascii_case("cascade"))
    }

    /// `TransformDropSecret`: `[TEMPORARY | PERSISTENT] SECRET [IF EXISTS] name [FROM
    /// storage]`.
    pub(super) fn transform_drop_secret(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        let list = parse_result.list()?;
        let mut info = drop_info(CatalogType::SecretEntry);
        let mut extra_drop_info = ExtraDropSecretInfo {
            persist_mode: SecretPersistType::Default,
            secret_storage: String::new().into(),
        };
        if let Some(persist_mode) = self.transform_optional(list, 0)? {
            extra_drop_info.persist_mode = persist_mode;
        }
        let if_exists = list.has(2)?;
        info.if_not_found = on_entry_not_found(if_exists);
        info.name = self.transform_child(list, 3)?;
        if let Some(secret_storage) = self.transform_optional(list, 4)? {
            extra_drop_info.secret_storage = secret_storage;
        }
        info.extra_drop_info = Some(Box::new(ExtraDropInfo::Secret(extra_drop_info)));
        Ok(info)
    }

    /// `TransformDropSecretStorage`: `FROM storage`.
    pub(super) fn transform_drop_secret_storage(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(1)?.to_owned())
    }
}
