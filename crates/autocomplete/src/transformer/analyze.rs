//! The transform functions of DuckDB's `transform_analyze.cpp`.

use super::prelude::*;

/// `VacuumStatement(options)`, of the table and columns of `target` if given.
pub(super) fn vacuum_statement(options: VacuumOptions, target: Option<AnalyzeTarget>) -> SqlStatement {
    let mut info = VacuumInfo {
        options,
        has_table: false,
        table_ref: None,
        columns: Vec::new(),
    };
    if let Some(target) = target {
        info.columns = target.columns;
        info.table_ref = target.table.map(Box::new);
        info.has_table = true;
    }
    SqlStatement::new(SqlStatementKind::Vacuum(Box::new(info)))
}

impl PegTransformer {
    /// `TransformAnalyzeStatement`: `ANALYZE VERBOSE? AnalyzeTarget?`.
    pub(super) fn transform_analyze_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let vacuum_options = VacuumOptions {
            vacuum: false,
            analyze: true,
        };
        let target = self.transform_optional(list, 2)?;
        Ok(vacuum_statement(vacuum_options, target))
    }

    /// `TransformAnalyzeTarget`: `BaseTableName NameList?`.
    pub(super) fn transform_analyze_target(&mut self, parse_result: &ParseResult) -> Result<AnalyzeTarget> {
        let list = parse_result.list()?;
        let BaseTable(table) = self.transform_child(list, 0)?;
        let mut result = AnalyzeTarget {
            table: Some(table),
            columns: Vec::new(),
        };
        if let Some(columns) = self.transform_optional(list, 1)? {
            result.columns = columns;
        }
        Ok(result)
    }
}
