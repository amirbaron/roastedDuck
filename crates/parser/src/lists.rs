//! Parsing fragments of SQL by wrapping them in a mock statement (DuckDB's
//! `Parser::ParseExpressionList`, `ParseGroupByList`, `ParseOrderList`,
//! `ParseUpdateList`, `ParseValuesList`, `ParseColumnList` and
//! `ParseColumnDefinition`), as the catalog's built-in macros and the relational API do.

use std::collections::BTreeSet;

use rustdb_ast::{
    ColumnDefinition, ColumnList, CreateInfoKind, OrderByNode, ParsedExpression, QueryNodeKind,
    ResultModifier, SelectNode, SqlStatementKind, TableRefKind,
};
use rustdb_common::{Error, Result};

use crate::{ParserOptions, parse_with_options};

/// The GROUP BY of a query (DuckDB's `GroupByNode`).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupBy {
    /// The distinct grouping expressions.
    pub expressions: Vec<ParsedExpression>,
    /// The grouping sets, as indexes into `expressions`.
    pub sets: Vec<BTreeSet<u64>>,
}

/// The single SELECT node of `query`.
fn single_select_node(query: &str, options: &ParserOptions) -> Result<SelectNode> {
    let mut statements = parse_with_options(query, options)?;
    if statements.len() != 1 {
        return Err(Error::parser("Expected a single SELECT statement"));
    }
    let SqlStatementKind::Select(select) = statements.swap_remove(0).kind else {
        return Err(Error::parser("Expected a single SELECT statement"));
    };
    let node = *select.node;
    let QueryNodeKind::Select(select) = node.kind else {
        return Err(Error::parser("Expected a single SELECT node"));
    };
    if !node.modifiers.is_empty() {
        return Err(Error::parser("Cannot have any modifiers in the expression list"));
    }
    Ok(*select)
}

/// `Parser::ParseExpressionList`: the expressions of a SELECT list.
pub fn parse_expression_list(select_list: &str, options: &ParserOptions) -> Result<Vec<ParsedExpression>> {
    let node = single_select_node(&format!("SELECT {select_list}"), options)?;
    if node.where_clause.is_some() {
        return Err(Error::parser("Cannot have a WHERE clause in the expression list"));
    }
    if !node.group_expressions.is_empty() {
        return Err(Error::parser(
            "Cannot have a GROUP BY clause in the expression list",
        ));
    }
    if node.having.is_some() {
        return Err(Error::parser(
            "Cannot have a HAVING clause in the expression list",
        ));
    }
    if node.qualify.is_some() {
        return Err(Error::parser(
            "Cannot have a QUALIFY clause in the expression list",
        ));
    }
    if node.sample.is_some() {
        return Err(Error::parser(
            "Cannot have a SAMPLE clause in the expression list",
        ));
    }
    Ok(node.select_list)
}

/// `Parser::ParseGroupByList`: the expressions and grouping sets of a GROUP BY.
pub fn parse_group_by_list(group_by: &str, options: &ParserOptions) -> Result<GroupBy> {
    let mut statements = parse_with_options(&format!("SELECT 42 GROUP BY {group_by}"), options)?;
    let select = match statements.pop() {
        Some(statement) if statements.is_empty() => match statement.kind {
            SqlStatementKind::Select(select) => select,
            _ => return Err(Error::parser("Expected a single SELECT statement")),
        },
        _ => return Err(Error::parser("Expected a single SELECT statement")),
    };
    let QueryNodeKind::Select(node) = select.node.kind else {
        return Err(Error::internal("Expected a SELECT node"));
    };
    Ok(GroupBy {
        expressions: node.group_expressions,
        sets: node.group_sets,
    })
}

/// `Parser::ParseOrderList`: the terms of an ORDER BY.
pub fn parse_order_list(order_list: &str, options: &ParserOptions) -> Result<Vec<OrderByNode>> {
    let mut statements = parse_with_options(&format!("SELECT * FROM tbl ORDER BY {order_list}"), options)?;
    let select = match statements.pop() {
        Some(statement) if statements.is_empty() => match statement.kind {
            SqlStatementKind::Select(select) => select,
            _ => return Err(Error::parser("Expected a single SELECT statement")),
        },
        _ => return Err(Error::parser("Expected a single SELECT statement")),
    };
    let mut modifiers = select.node.modifiers;
    match (modifiers.pop(), modifiers.is_empty()) {
        (Some(ResultModifier::Order(order)), true) => Ok(order.orders),
        _ => Err(Error::parser("Expected a single ORDER clause")),
    }
}

/// `Parser::ParseUpdateList`: the columns and values of `SET a = x, b = y`.
pub fn parse_update_list(
    update_list: &str,
    options: &ParserOptions,
) -> Result<(Vec<String>, Vec<ParsedExpression>)> {
    let mut statements = parse_with_options(&format!("UPDATE tbl SET {update_list}"), options)?;
    match statements.pop() {
        Some(statement) if statements.is_empty() => match statement.kind {
            SqlStatementKind::Update(update) => {
                let set_info = *update.set_info;
                Ok((set_info.columns, set_info.expressions))
            }
            _ => Err(Error::parser("Expected a single UPDATE statement")),
        },
        _ => Err(Error::parser("Expected a single UPDATE statement")),
    }
}

/// `Parser::ParseValuesList`: the rows of a VALUES list.
pub fn parse_values_list(value_list: &str, options: &ParserOptions) -> Result<Vec<Vec<ParsedExpression>>> {
    let mut statements = parse_with_options(&format!("VALUES {value_list}"), options)?;
    let select = match statements.pop() {
        Some(statement) if statements.is_empty() => match statement.kind {
            SqlStatementKind::Select(select) => select,
            _ => return Err(Error::parser("Expected a single SELECT statement")),
        },
        _ => return Err(Error::parser("Expected a single SELECT statement")),
    };
    let QueryNodeKind::Select(node) = select.node.kind else {
        return Err(Error::parser("Expected a single SELECT node"));
    };
    match node.from_table.kind {
        TableRefKind::ExpressionList(values) => Ok(values.values),
        _ => Err(Error::parser("Expected a single VALUES statement")),
    }
}

/// `Parser::ParseColumnList`: the columns of `CREATE TABLE tbl (...)`.
pub fn parse_column_list(column_list: &str, options: &ParserOptions) -> Result<ColumnList> {
    let mut statements = parse_with_options(&format!("CREATE TABLE tbl ({column_list})"), options)?;
    let info = match statements.pop() {
        Some(statement) if statements.is_empty() => match statement.kind {
            SqlStatementKind::Create(info) => info,
            _ => return Err(Error::parser("Expected a single CREATE statement")),
        },
        _ => return Err(Error::parser("Expected a single CREATE statement")),
    };
    match info.kind {
        CreateInfoKind::Table(table) => Ok(table.columns),
        _ => Err(Error::internal("Expected a single CREATE TABLE statement")),
    }
}

/// `Parser::ParseColumnDefinition`: one column definition.
pub fn parse_column_definition(column_definition: &str, options: &ParserOptions) -> Result<ColumnDefinition> {
    parse_column_list(column_definition, options)?
        .columns
        .into_iter()
        .next()
        .ok_or_else(|| Error::internal("Logical column index 0 out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments() {
        let options = ParserOptions::default();
        assert_eq!(
            parse_expression_list("1, a + b", &options).expect("parses").len(),
            2
        );
        assert_eq!(
            parse_expression_list("1 WHERE x", &options)
                .expect_err("fails")
                .raw_message(),
            "Cannot have a WHERE clause in the expression list"
        );
        let group_by = parse_group_by_list("ROLLUP (a, b)", &options).expect("parses");
        assert_eq!((group_by.expressions.len(), group_by.sets.len()), (2, 3));
        assert_eq!(parse_order_list("a DESC, b", &options).expect("parses").len(), 2);
        let (columns, values) = parse_update_list("a = 1, b = 2", &options).expect("parses");
        assert_eq!((columns.len(), values.len()), (2, 2));
        assert_eq!(
            parse_values_list("(1, 2), (3, 4)", &options)
                .expect("parses")
                .len(),
            2
        );
        let column = parse_column_definition("x INTEGER DEFAULT 3", &options).expect("parses");
        assert_eq!(column.name, "x");
        assert!(column.expression.is_some());
    }
}
