//! PIVOT statements (`transform_pivot_stmt.cpp`).

use std::collections::BTreeSet;

use rustdb_ast::{
    CommonTableExpressionMap, DistinctModifier, ExpressionType, OrderByNode, OrderByNullType, OrderModifier,
    OrderType, PivotRef, QueryNode, QueryNodeKind, ResultModifier, SqlStatement, SqlStatementKind,
    TableRefKind,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::SelectStmt;
use rustdb_types::{LogicalType, Value};

use super::build::{self, set_ref_location};
use super::{PivotEntry, Transformer, required, string_list};

impl<'a> Transformer<'a> {
    /// `TransformPivotStatement`: `PIVOT t ON col USING agg GROUP BY g`.
    pub(crate) fn transform_pivot_statement(&mut self, select: &'a SelectStmt) -> Result<QueryNode> {
        let pivot = required(select.pivot.as_deref(), "pivot")?;
        let current_param_count = self.param_count();
        let source = self.transform_table_ref_node(required(pivot.source.as_ref(), "pivot source")?)?;
        let has_parameters = self.param_count() > current_param_count;
        let mut select_node = build::select_node();
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &select.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        if pivot.columns.is_empty() {
            // No pivot columns: not actually a pivot.
            *select_node.from_table = source;
            if !pivot.groups.is_empty() {
                let groups = string_list(&pivot.groups)?;
                let mut set = BTreeSet::new();
                for (index, group) in groups.into_iter().enumerate() {
                    let column_ref = build::column_ref(vec![group]);
                    select_node.select_list.push(column_ref.clone());
                    select_node.group_expressions.push(column_ref);
                    set.insert(index as u64);
                }
                select_node.group_sets.push(set);
            }
            self.transform_expression_list(&pivot.aggrs, &mut select_node.select_list)?;
            let mut node = build::query_node(QueryNodeKind::Select(Box::new(select_node)));
            node.cte_map = cte_map;
            self.transform_modifiers(select, &mut node)?;
            return Ok(node);
        }
        let mut columns = self.transform_pivot_list(&pivot.columns)?;
        for column in &mut columns {
            if !column.pivot_enum.is_empty() || !column.entries.is_empty() {
                continue;
            }
            let [pivot_expression] = column.pivot_expressions.as_slice() else {
                return Err(Error::internal(
                    "PIVOT statement with multiple names in pivot entry!?",
                ));
            };
            let enum_name = rustdb_common::byte_format!("__pivot_enum_{}", build::random_uuid());
            let mut base_cte_map = CommonTableExpressionMap::default();
            self.extract_ctes(&mut base_cte_map);
            let mut base = build::select_node();
            *base.from_table = source.clone();
            self.pivot_entries.push(PivotEntry {
                enum_name: enum_name.clone(),
                base: Some(base),
                base_cte_map,
                column: pivot_expression.clone(),
                subquery: column.subquery.take().map(|subquery| *subquery),
                has_parameters,
            });
            column.pivot_enum = enum_name;
        }
        select_node.select_list.push(build::star());
        let mut aggregates = Vec::new();
        let unpivot_names = string_list(&pivot.unpivots)?;
        if pivot.unpivots.is_empty() {
            if pivot.aggrs.is_empty() {
                aggregates.push(build::function("count_star", Vec::new()));
            } else {
                self.transform_expression_list(&pivot.aggrs, &mut aggregates)?;
            }
        }
        let groups = string_list(&pivot.groups)?;
        let mut pivot_ref = build::table_ref(TableRefKind::Pivot(Box::new(PivotRef {
            source: Box::new(source),
            aggregates,
            unpivot_names: unpivot_names.into_iter().collect(),
            pivots: columns,
            groups: groups.into_iter().collect(),
            column_name_alias: Vec::new(),
            include_nulls: false,
            bound: None,
        })));
        set_ref_location(&mut pivot_ref, pivot.location);
        *select_node.from_table = pivot_ref;
        let mut node = build::query_node(QueryNodeKind::Select(Box::new(select_node)));
        node.cte_map = cte_map;
        self.transform_modifiers(select, &mut node)?;
        Ok(node)
    }

    /// `CreatePivotStatement`: the `CREATE TYPE ... AS ENUM` statements the pivot
    /// entries need, followed by the statement itself.
    pub(crate) fn create_pivot_statement(&mut self, statement: SqlStatement) -> Result<SqlStatement> {
        let mut statements = Vec::with_capacity(self.pivot_entries.len() + 1);
        for entry in std::mem::take(&mut self.pivot_entries) {
            if entry.has_parameters {
                return Err(Error::parser(rustdb_common::byte_format!(
                    "PIVOT statements with pivot elements extracted from the data cannot have parameters in \
                     their source.\nIn order to use parameters the PIVOT values must be manually specified, \
                     e.g.:\nPIVOT ... ON {} IN (val1, val2, ...)",
                    entry.column
                )));
            }
            let mut enum_statement = SqlStatement::new(generate_create_enum_statement(entry));
            enum_statement.query = statement_to_string(&enum_statement);
            statements.push(enum_statement);
        }
        let mut result = SqlStatement::new(SqlStatementKind::Multi(Vec::new()));
        result.location = statement.location;
        result.length = statement.length;
        let mut statement = statement;
        statement.query = statement_to_string(&statement);
        statements.push(statement);
        result.kind = SqlStatementKind::Multi(statements);
        Ok(result)
    }
}

/// `GenerateCreateEnumStmt`: `CREATE TEMPORARY TYPE name AS ENUM (SELECT DISTINCT
/// col::VARCHAR FROM source WHERE col IS NOT NULL ORDER BY 1)`, or of the IN subquery.
fn generate_create_enum_statement(entry: PivotEntry) -> SqlStatementKind {
    let subselect = if let Some(subquery) = entry.subquery {
        subquery
    } else {
        let mut select = entry.base.unwrap_or_else(build::select_node);
        select
            .select_list
            .push(build::cast(LogicalType::VARCHAR, entry.column.clone(), false));
        select.where_clause = Some(Box::new(build::operator(
            ExpressionType::OperatorIsNotNull,
            vec![entry.column],
        )));
        let mut node = build::query_node(QueryNodeKind::Select(Box::new(select)));
        node.cte_map = entry.base_cte_map;
        node.modifiers.push(ResultModifier::Distinct(DistinctModifier {
            distinct_on_targets: Vec::new(),
        }));
        node.modifiers.push(ResultModifier::Order(OrderModifier {
            orders: vec![OrderByNode {
                order_type: OrderType::Ascending,
                null_order: OrderByNullType::OrderDefault,
                expression: Box::new(build::constant(Value::integer(1))),
            }],
        }));
        node
    };
    super::ddl::create_enum_type_from_query(entry.enum_name, build::select_statement(subselect))
}

/// `SQLStatement::ToString` for the statements a PIVOT expands into.
fn statement_to_string(statement: &SqlStatement) -> rustdb_common::ByteString {
    match &statement.kind {
        SqlStatementKind::Select(select) => select.to_sql_bytes(),
        // `CreateStatement::ToString`.
        SqlStatementKind::Create(info) => info.to_sql().unwrap_or_default(),
        _ => rustdb_common::ByteString::new(),
    }
}
