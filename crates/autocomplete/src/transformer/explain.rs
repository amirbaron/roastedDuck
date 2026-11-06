//! The transform functions of DuckDB's `transform_explain.cpp`: `EXPLAIN [ANALYZE]
//! [(options)] statement`.

use rustdb_parser::hash_order::CiHashMap;

use super::prelude::*;

/// The error of DuckDB's bounds-checked `vector[index]` past the end.
pub(super) fn vector_index_error(index: usize, size: usize) -> Error {
    Error::internal(format!(
        "Attempted to access index {index} within vector of size {size}"
    ))
}

/// `vector[index]` of DuckDB's bounds-checked `vector`.
pub(super) fn vector_element<T>(vector: &[T], index: usize) -> Result<&T> {
    vector
        .get(index)
        .ok_or_else(|| vector_index_error(index, vector.len()))
}

/// `ParseExplainFormat`: the argument of the `FORMAT` option.
fn parse_explain_format(value: &Value) -> Result<ExplainFormat> {
    if value.logical_type().id() != LogicalTypeId::Varchar {
        return Err(Error::invalid_input("Expected a string as argument to FORMAT"));
    }
    let format_value = value.to_varchar()?;
    let mut format_mapping = CiHashMap::new();
    for (name, format) in [
        ("default", ExplainFormat::Default),
        ("text", ExplainFormat::Text),
        ("json", ExplainFormat::Json),
        ("html", ExplainFormat::Html),
        ("graphviz", ExplainFormat::Graphviz),
        ("yaml", ExplainFormat::Yaml),
        ("mermaid", ExplainFormat::Mermaid),
    ] {
        format_mapping.insert(name.to_owned(), format);
    }
    if let Some(format) = format_mapping.get(&format_value) {
        return Ok(*format);
    }
    let options_list: Vec<&str> = format_mapping.iter().map(|(name, _)| name).collect();
    Err(Error::invalid_input(format!(
        "\"{format_value}\" is not a valid FORMAT argument, valid options are: {}",
        options_list.join(", ")
    )))
}

impl PegTransformer {
    /// `TransformExplainStatement`: `EXPLAIN ANALYZE? GenericCopyOptionList?
    /// ExplainableStatements`.
    pub(super) fn transform_explain_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut explain_type = if list.has(1)? {
            ExplainType::ExplainAnalyze
        } else {
            ExplainType::ExplainStandard
        };
        let mut format_is_set = false;
        let mut explain_format = ExplainFormat::Default;
        if let Some(explain_options) = list.optional(2)? {
            let options: Vec<GenericCopyOption> = self.transform(explain_options)?;
            for option in options {
                let option_name = option.name.to_ascii_lowercase();
                if option_name == "format" {
                    if format_is_set {
                        return Err(Error::invalid_input("FORMAT can not be provided more than once"));
                    }
                    explain_format = parse_explain_format(vector_element(&option.children, 0)?)?;
                    format_is_set = true;
                } else if option_name == "analyze" {
                    explain_type = ExplainType::ExplainAnalyze;
                }
            }
        }
        let statement: SqlStatement = self.transform_child(list, 3)?;
        Ok(SqlStatement::new(SqlStatementKind::Explain(ExplainStatement {
            statement: Box::new(statement),
            explain_type,
            explain_format,
        })))
    }

    /// `TransformExplainableStatements`: a `SELECT` is transformed as a
    /// `SelectStatement`, any other statement as a statement.
    pub(super) fn transform_explainable_statements(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name.eq_ignore_ascii_case("SelectStatementInternal") {
            let select: SelectStatement = self.transform(choice)?;
            return Ok(SqlStatement::new(SqlStatementKind::Select(select)));
        }
        self.transform(choice)
    }
}
