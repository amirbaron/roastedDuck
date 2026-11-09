//! The transform functions of DuckDB's `transform_export.cpp`: `EXPORT DATABASE [db TO]
//! 'path' [(options)]`.

use roastedduck_parser::hash_order::CiHashMap;

use super::explain::vector_element;
use super::prelude::*;

/// `CopyInfo()`: DuckDB's defaults of a COPY / EXPORT.
fn copy_info() -> CopyInfo {
    CopyInfo {
        catalog: String::new().into(),
        schema: "main".to_owned().into(),
        table: String::new().into(),
        select_list: Vec::new(),
        is_from: false,
        format: String::new().into(),
        file_path: String::new().into(),
        options: CaseInsensitiveMap::default(),
        select_statement: None,
        is_format_auto_detected: true,
        parsed_options: CaseInsensitiveMap::default(),
        file_path_expression: None,
    }
}

impl PegTransformer {
    /// `TransformExportStatement`: `EXPORT DATABASE ExportSource? StringLiteral
    /// GenericCopyOptionList?`.
    pub(super) fn transform_export_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let mut info = copy_info();
        let list = parse_result.list()?;
        info.file_path = list.child(3)?.string_literal()?.0.into();
        info.format = "csv".into();
        info.is_from = false;

        if let Some(parens) = list.optional(4)? {
            let option_list: Vec<GenericCopyOption> = self.transform(parens)?;
            let mut option_result = CiHashMap::new();
            for option in option_list {
                if option.name == "format" {
                    info.format = vector_element(&option.children, 0)?.to_varchar_bytes()?;
                    info.is_format_auto_detected = false;
                } else {
                    option_result.set(option.name.to_ascii_uppercase(), option.children);
                }
            }
            info.options = option_result.into_ordered().into_iter().collect();
        }

        let mut database = String::new();
        if let Some(database_result) = list.optional(2)? {
            database = self.transform(database_result)?;
        }
        Ok(SqlStatement::new(SqlStatementKind::Export(ExportStatement {
            info: Box::new(info),
            database: database.into(),
        })))
    }

    /// `TransformExportSource`: `CatalogName 'TO'`.
    pub(super) fn transform_export_source(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }
}
