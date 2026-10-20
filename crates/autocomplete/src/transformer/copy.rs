//! The transform functions of DuckDB's `transform_copy.cpp`: `COPY table FROM / TO`,
//! `COPY (query) TO` and `COPY FROM DATABASE`.

use rustdb_parser::hash_order::CiHashMap;

use super::create_table::base_table_ref;
use super::prelude::*;

/// `CopyInfo()`: the defaults of a COPY.
fn copy_info() -> CopyInfo {
    CopyInfo {
        catalog: String::new(),
        schema: "main".to_owned(),
        table: String::new(),
        select_list: Vec::new(),
        is_from: false,
        format: String::new(),
        file_path: String::new(),
        options: CaseInsensitiveMap::default(),
        select_statement: None,
        is_format_auto_detected: true,
        parsed_options: CaseInsensitiveMap::default(),
        file_path_expression: None,
    }
}

/// `SetCopyOptions`: the options go to `parsed_options` (expressions) or `options`
/// (constant values); `FORMAT` sets the format. Both maps are `case_insensitive_map_t`s,
/// kept in their libc++ iteration order.
fn set_copy_options(info: &mut CopyInfo, options: Vec<GenericCopyOption>) -> Result<()> {
    let mut option_names: CiHashMap<()> = CiHashMap::new();
    let mut parsed_options: CiHashMap<Option<ParsedExpression>> = CiHashMap::new();
    let mut copy_options: CiHashMap<Vec<Value>> = CiHashMap::new();
    for option in options {
        if option_names.contains_key(&option.name) {
            return Err(Error::parser(format!(
                "Unexpected duplicate option \"{}\"",
                option.name
            )));
        }
        option_names.insert(option.name.clone(), ());
        let name = option.name.as_str();
        let is = |other: &str| name.eq_ignore_ascii_case(other);
        if is("PARTITION_BY") || is("FORCE_QUOTE") || is("FORCE_NOT_NULL") || is("FORCE_NULL") {
            if let Some(expression) = option.expression {
                parsed_options.set(option.name, Some(expression));
            } else {
                if option.children.is_empty() {
                    return Err(Error::binder(format!(
                        "\"{name}\" expects a column list or * as parameter"
                    )));
                }
                let func_children = option
                    .children
                    .iter()
                    .map(|partition| Ok(ParsedExpression::column_ref([partition.to_varchar()?])))
                    .collect::<Result<Vec<_>>>()?;
                let row_func = rustdb_parser::build::function_full(
                    String::new(),
                    "main".to_owned(),
                    "row",
                    func_children,
                    None,
                    OrderModifier { orders: Vec::new() },
                    false,
                    false,
                    false,
                );
                parsed_options.set(option.name, Some(row_func));
            }
        } else if is("HEADER") || is("ESCAPE") {
            let value = option
                .children
                .first()
                .map(|value| ParsedExpression::constant(value.clone()));
            parsed_options.set(option.name, value);
        } else if is("NULL") || is("NULLSTR") {
            let value = match option.children.first() {
                Some(value) => Some(ParsedExpression::constant(value.clone())),
                None => option.expression,
            };
            parsed_options.set(option.name, value);
        } else if let Some(expression) = option.expression {
            parsed_options.set(option.name, Some(expression));
        } else {
            copy_options.set(option.name, option.children);
        }
    }
    // Erasing from a libc++ hash map keeps the order of the other entries.
    let mut copy_options = copy_options.into_ordered();
    if let Some(position) = copy_options
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case("format"))
    {
        let (_, format_option) = copy_options.remove(position);
        let Some(format) = format_option.first() else {
            return Err(Error::parser(
                "Unsupported parameter type for FORMAT: expected e.g. FORMAT 'csv', 'parquet'",
            ));
        };
        info.format = format.to_varchar()?;
        info.is_format_auto_detected = false;
    }
    info.options = copy_options.into_iter().collect();
    info.parsed_options = parsed_options.into_ordered().into_iter().collect();
    Ok(())
}

/// `PEGTransformerFactory::ExtractFormat`: the extension of the file, without a `.gz` /
/// `.zst` compression suffix; empty if there is none.
fn extract_format(file_path: &str) -> String {
    let lower = file_path.to_ascii_lowercase();
    let format = lower
        .strip_suffix(".gz")
        .or_else(|| lower.strip_suffix(".zst"))
        .unwrap_or(&lower);
    match format.rfind('.') {
        Some(dot_pos) if dot_pos != format.len() - 1 => format[dot_pos + 1..].to_owned(),
        _ => String::new(),
    }
}

/// Sets the file of a COPY: a constant is the path, anything else an expression.
fn set_file_name(info: &mut CopyInfo, file_name: ParsedExpression) -> Result<()> {
    if let ParsedExpressionKind::Constant(const_expr) = &file_name.kind {
        info.file_path = const_expr.value.to_varchar()?;
    } else {
        info.file_path_expression = Some(Box::new(file_name));
    }
    Ok(())
}

impl PegTransformer {
    /// `TransformCopyStatement`: `COPY (CopyTable | CopySelect | CopyFromDatabase)`.
    pub(super) fn transform_copy_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        self.transform(parse_result.list()?.list(1)?.choice(0)?)
    }

    /// `TransformCopySelect`: `COPY (query) TO file [options]`.
    pub(super) fn transform_copy_select(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let select_statement: SelectStatement = self.transform(list.child(0)?.parens_content()?)?;
        let mut info = copy_info();
        info.is_from = false;
        let file_name: ParsedExpression = self.transform_child(list, 2)?;
        set_file_name(&mut info, file_name)?;
        if let Some(options) = list.optional(3)? {
            let options: Vec<GenericCopyOption> = self.transform(options)?;
            set_copy_options(&mut info, options)?;
        }
        info.select_statement = Some(select_statement.node);
        Ok(SqlStatement::new(SqlStatementKind::Copy(Box::new(info))))
    }

    /// `TransformCopyFromDatabase`: `COPY FROM DATABASE a TO b [(SCHEMA | DATA)]`;
    /// without a flag, the `copy_database` pragma.
    pub(super) fn transform_copy_from_database(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let from_database: String = self.transform_child(list, 2)?;
        let to_database: String = self.transform_child(list, 4)?;
        if let Some(copy_database_flag) = list.optional(5)? {
            let copy_type: CopyDatabaseType = self.transform(copy_database_flag)?;
            return Ok(SqlStatement::new(SqlStatementKind::CopyDatabase(
                CopyDatabaseStatement {
                    from_database,
                    to_database,
                    copy_type,
                },
            )));
        }
        Ok(SqlStatement::new(SqlStatementKind::Pragma(Box::new(
            PragmaInfo {
                name: "copy_database".to_owned(),
                parameters: vec![
                    ParsedExpression::constant(Value::varchar(from_database)),
                    ParsedExpression::constant(Value::varchar(to_database)),
                ],
                named_parameters: CaseInsensitiveMap::default(),
            },
        ))))
    }

    /// `TransformCopyDatabaseFlag`: `(SCHEMA | DATA)`.
    pub(super) fn transform_copy_database_flag(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CopyDatabaseType> {
        self.transform(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformSchemaOrData`.
    pub(super) fn transform_schema_or_data(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CopyDatabaseType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformCopyTable`: `COPY table [(columns)] FROM | TO file [options]`; the
    /// format defaults to the file extension.
    pub(super) fn transform_copy_table(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut info = copy_info();
        let base_table = base_table_ref(self.transform_child(list, 0)?)?;
        info.table = base_table.table_name;
        info.schema = base_table.schema_name;
        info.catalog = base_table.catalog_name;
        if let Some(insert_column_list) = list.optional(1)? {
            info.select_list = self.transform(insert_column_list)?;
        }
        info.is_from = self.transform_child(list, 2)?;
        let file_name: ParsedExpression = self.transform_child(list, 3)?;
        set_file_name(&mut info, file_name)?;
        info.format = extract_format(&info.file_path);
        if let Some(copy_options) = list.optional(4)? {
            let generic_options: Vec<GenericCopyOption> = self.transform(copy_options)?;
            set_copy_options(&mut info, generic_options)?;
        }
        Ok(SqlStatement::new(SqlStatementKind::Copy(Box::new(info))))
    }

    /// `TransformFromOrTo`: true for `FROM`.
    pub(super) fn transform_from_or_to(&mut self, parse_result: &ParseResult) -> Result<bool> {
        let keyword = parse_result.list()?.choice(0)?.keyword()?;
        Ok(keyword.eq_ignore_ascii_case("from"))
    }

    /// `TransformCopyFileName`: a parameter or parenthesized expression, or a name as a
    /// string constant.
    pub(super) fn transform_copy_file_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name == "ParensExpression" || choice.name == "Parameter" {
            return self.transform(choice);
        }
        let file_name: String = if choice.result_type() == ParseResultType::Identifier {
            choice.identifier()?.to_owned()
        } else {
            self.transform(choice)?
        };
        Ok(ParsedExpression::constant(Value::varchar(file_name)))
    }

    /// `TransformIdentifierColId`: `identifier.colid`. DuckDB writes the rule name of
    /// the identifier (not its text) before the dot.
    pub(super) fn transform_identifier_col_id(&mut self, parse_result: &ParseResult) -> Result<String> {
        let list = parse_result.list()?;
        let identifier = list.child(0)?;
        identifier.identifier()?;
        let col_id: String = self.transform_child(list, 2)?;
        Ok(format!("{}.{col_id}", identifier.name))
    }

    /// `TransformCopyOptions`: `[WITH] (option, ...)` or the specialized options.
    pub(super) fn transform_copy_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        self.transform(parse_result.list()?.choice(1)?)
    }

    /// `TransformSpecializedOptionList`: `CSV HEADER DELIMITER ',' ...`.
    pub(super) fn transform_specialized_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        let Some(options) = parse_result.list()?.optional(0)? else {
            return Ok(Vec::new());
        };
        options
            .repeat()?
            .iter()
            .map(|option| self.transform(option))
            .collect()
    }

    /// `TransformSpecializedOption`.
    pub(super) fn transform_specialized_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSingleOption`: `BINARY`, `FREEZE`, `OIDS`, `CSV` or `HEADER`.
    pub(super) fn transform_single_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformEncodingOption`: `ENCODING 'name'`.
    pub(super) fn transform_encoding_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let string_literal = parse_result.list()?.child(1)?.string_literal()?.0;
        Ok(GenericCopyOption::new("encoding", Value::varchar(string_literal)))
    }

    /// `TransformForceQuoteOption`: `[FORCE] QUOTE * | (columns)`.
    pub(super) fn transform_force_quote_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let list = parse_result.list()?;
        let force_quote = list.has(0)?;
        let func_name = if force_quote { "force_quote" } else { "quote" };
        let star_or_column_list = list.list(2)?.choice(0)?;
        let mut result = GenericCopyOption {
            name: func_name.to_owned(),
            ..GenericCopyOption::default()
        };
        self.set_star_or_column_list(&mut result, star_or_column_list)?;
        Ok(result)
    }

    /// `TransformQuoteAsOption`: `QUOTE [AS] 'c'`.
    pub(super) fn transform_quote_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let string_literal = parse_result.list()?.child(2)?.string_literal()?.0;
        Ok(GenericCopyOption::new("quote", Value::varchar(string_literal)))
    }

    /// `TransformForceNullOption`: `FORCE [NOT] NULL (columns)`.
    pub(super) fn transform_force_null_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let list = parse_result.list()?;
        let is_not = list.has(1)?;
        let name = if is_not { "force_not_null" } else { "force_null" };
        let column_list: Vec<String> = self.transform_child(list, 3)?;
        Ok(GenericCopyOption {
            name: name.to_owned(),
            children: column_list.into_iter().map(Value::varchar).collect(),
            expression: None,
        })
    }

    /// `TransformPartitionByOption`: `PARTITION BY * | (columns)`.
    pub(super) fn transform_partition_by_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let star_or_column_list = parse_result.list()?.list(2)?.choice(0)?;
        let mut result = GenericCopyOption {
            name: "partition_by".to_owned(),
            ..GenericCopyOption::default()
        };
        self.set_star_or_column_list(&mut result, star_or_column_list)?;
        Ok(result)
    }

    /// The value of `* | (columns)`: a star expression, or the column names.
    fn set_star_or_column_list(
        &mut self,
        option: &mut GenericCopyOption,
        star_or_column_list: &ParseResult,
    ) -> Result<()> {
        if star_or_column_list.name.eq_ignore_ascii_case("StarSymbol") {
            option.expression = Some(ParsedExpression::star());
        } else if star_or_column_list.name.eq_ignore_ascii_case("ColumnList") {
            let column_list: Vec<String> = self.transform(star_or_column_list)?;
            option
                .children
                .extend(column_list.into_iter().map(Value::varchar));
        }
        Ok(())
    }

    /// `TransformNullAsOption`: `NULL [AS] 'text'`.
    pub(super) fn transform_null_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let string_literal = parse_result.list()?.child(2)?.string_literal()?.0;
        Ok(GenericCopyOption::new("null", Value::varchar(string_literal)))
    }

    /// `TransformDelimiterAsOption`: `DELIMITER [AS] 'c'`.
    pub(super) fn transform_delimiter_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let string_literal = parse_result.list()?.child(2)?.string_literal()?.0;
        Ok(GenericCopyOption::new(
            "delimiter",
            Value::varchar(string_literal),
        ))
    }

    /// `TransformEscapeAsOption`: `ESCAPE [AS] 'c'`.
    pub(super) fn transform_escape_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        let string_literal = parse_result.list()?.child(2)?.string_literal()?.0;
        Ok(GenericCopyOption::new("escape", Value::varchar(string_literal)))
    }
}
