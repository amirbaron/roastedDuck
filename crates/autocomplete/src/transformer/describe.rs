//! The transform functions of DuckDB's `transform_describe.cpp`: `SHOW`, `DESCRIBE` and
//! `SUMMARIZE`, which select `*` from a `ShowRef`.

use roastedduck_parser::build;

use super::prelude::*;

/// `ShowRef()`: a `DESCRIBE` of nothing yet.
const fn show_ref() -> ShowRef {
    ShowRef {
        table_name: ByteString::new(),
        query: None,
        show_type: ShowType::Describe,
        catalog_name: ByteString::new(),
        schema_name: ByteString::new(),
    }
}

/// `make_uniq<BaseTableRef>()` with a table name.
fn base_table_ref(table_name: String) -> TableRef {
    build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
        schema_name: ByteString::new(),
        table_name: table_name.into(),
        column_name_alias: Vec::new(),
        catalog_name: ByteString::new(),
        at_clause: None,
    })))
}

/// `SELECT * FROM from_table`.
fn select_star_from(from_table: TableRef) -> QueryNode {
    let mut select_node = build::select_node();
    select_node.select_list.push(build::star());
    *select_node.from_table = from_table;
    build::query_node(QueryNodeKind::Select(Box::new(select_node)))
}

/// `SELECT * FROM` the show ref.
fn select_star_from_show(showref: ShowRef) -> QueryNode {
    select_star_from(build::table_ref(TableRefKind::Show(Box::new(showref))))
}

/// The base table ref of a `BaseTableRef` result.
fn base_table(table: &TableRef) -> Result<&BaseTableRef> {
    match &table.kind {
        TableRefKind::BaseTable(base_table) => Ok(base_table),
        _ => Err(Error::internal("Expected a base table reference")),
    }
}

impl PegTransformer {
    /// `TransformDescribeStatement`: `ShowTables / ShowSelect / ShowAllTables /
    /// ShowQualifiedName`.
    pub(super) fn transform_describe_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        let node: QueryNode = self.transform(parse_result.list()?.choice(0)?)?;
        Ok(build::select_statement(node))
    }

    /// `TransformShowSelect`: `ShowOrDescribeOrSummarize SelectStatementInternal`.
    pub(super) fn transform_show_select(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut result = show_ref();
        result.show_type = self.transform_child(list, 0)?;
        let select_statement: SelectStatement = self.transform_child(list, 1)?;
        result.query = Some(select_statement.node);
        Ok(select_star_from_show(result))
    }

    /// `TransformShowTables`: `ShowOrDescribe TABLES FROM QualifiedName`.
    pub(super) fn transform_show_tables(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut showref = show_ref();
        showref.show_type = ShowType::ShowFrom;
        let qualified_name: QualifiedName = self.transform_child(list, 3)?;
        if !qualified_name.catalog.is_empty() {
            return Err(Error::parser(
                "Expected \"SHOW TABLES FROM database\", \"SHOW TABLES FROM schema\", or \"SHOW TABLES FROM \
                 database.schema\"",
            ));
        }
        if qualified_name.schema.is_empty() {
            showref.schema_name = qualified_name.name;
        } else {
            showref.catalog_name = qualified_name.schema;
            showref.schema_name = qualified_name.name;
        }
        Ok(select_star_from_show(showref))
    }

    /// `TransformShowAllTables`: `ShowOrDescribe ALL TABLES`.
    pub(super) fn transform_show_all_tables(&mut self, _parse_result: &ParseResult) -> Result<QueryNode> {
        let mut result = show_ref();
        // Legacy reasons, see bind_showref.cpp.
        result.table_name = "__show_tables_expanded".into();
        result.show_type = ShowType::ShowUnqualified;
        Ok(select_star_from_show(result))
    }

    /// `TransformShowQualifiedName`: `ShowOrDescribeOrSummarize (BaseTableName /
    /// StringLiteral)?`.
    pub(super) fn transform_show_qualified_name(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        let list = parse_result.list()?;
        let mut showref = show_ref();
        showref.show_type = self.transform_child(list, 0)?;

        if let Some(table_name_parens) = list.optional(1)? {
            let choice = table_name_parens.list()?.choice(0)?;
            let is_string = choice.result_type() == ParseResultType::String;
            if is_string {
                // Case: SHOW 'something' or DESCRIBE 'something'.
                showref.table_name = choice.string_literal()?.0.into();
            } else {
                // Case: a relation / table reference.
                let BaseTable(table) = self.transform(choice)?;
                let base_table = base_table(&table)?;
                if showref.show_type == ShowType::ShowFrom {
                    // SHOW TABLES FROM [database].[schema].
                    if base_table.schema_name.is_empty() {
                        showref.schema_name.clone_from(&base_table.table_name);
                    } else {
                        showref.catalog_name.clone_from(&base_table.schema_name);
                        showref.schema_name.clone_from(&base_table.table_name);
                    }
                } else if base_table.schema_name.is_empty() {
                    // Unqualified relations: databases, tables, schemas, variables.
                    let table_name = base_table.table_name.to_ascii_lowercase();
                    if matches!(
                        table_name.as_bytes(),
                        b"databases" | b"tables" | b"schemas" | b"variables"
                    ) {
                        showref.table_name = byte_format!("\"{table_name}\"");
                        showref.show_type = ShowType::ShowUnqualified;
                    }
                }
            }
            if showref.table_name.is_empty() && showref.show_type != ShowType::ShowFrom {
                let from_table = if is_string {
                    // Case: SHOW 'something' or DESCRIBE 'something'.
                    base_table_ref(choice.string_literal()?.0.to_owned())
                } else {
                    // Case: a relation / table reference, transformed again.
                    let BaseTable(table) = self.transform(choice)?;
                    table
                };
                showref.query = Some(Box::new(select_star_from(from_table)));
            }
        } else {
            // Case: no relation specified (e.g. just "SHOW TABLES").
            if showref.show_type == ShowType::Summary {
                return Err(Error::parser("Expected table name with SUMMARIZE"));
            }
            showref.table_name = "__show_tables_expanded".into();
            showref.show_type = ShowType::ShowUnqualified;
        }
        Ok(select_star_from_show(showref))
    }

    /// `TransformShowOrDescribeOrSummarize`: `ShowOrDescribe / Summarize`.
    pub(super) fn transform_show_or_describe_or_summarize(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ShowType> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformShowOrDescribe`: `ShowRule / DescribeRule`.
    pub(super) fn transform_show_or_describe(&mut self, parse_result: &ParseResult) -> Result<ShowType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformSummarize`: `SummarizeRule`.
    pub(super) fn transform_summarize(&mut self, parse_result: &ParseResult) -> Result<ShowType> {
        Self::transform_enum(parse_result.list()?.child(0)?)
    }
}
