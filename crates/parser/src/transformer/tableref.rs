//! Table references (`transform/tableref/*.cpp`).

use rustdb_ast::{
    AtClause, BaseTableRef, JoinRef, JoinRefType, JoinType, OrdinalityType, PivotColumn, PivotColumnEntry,
    PivotRef, QueryNodeKind, SubqueryRef, TableFunctionRef, TableRef, TableRefKind,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{
    AtClause as PgAtClause, JoinExpr, JoinRefType as PgJoinRefType, JoinType as PgJoinType, List, Node,
    Pivot, PivotExpr, RangeFunction, RangeSubselect, RangeVar,
};

use super::build::{self, set_ref_location};
use super::helpers::transform_alias;
use super::{Transformer, cast_node, parser_error_at, required, string_list};
use crate::equality::{has_subquery, is_scalar};

impl<'a> Transformer<'a> {
    /// `TransformTableRefNode`.
    pub(crate) fn transform_table_ref_node(&mut self, node: &'a Node) -> Result<TableRef> {
        self.nested(|t| match node {
            Node::RangeVar(range_var) => t.transform_range_var(range_var),
            Node::JoinExpr(join) => t.transform_join(join),
            Node::RangeSubselect(subselect) => t.transform_range_subselect(subselect),
            Node::RangeFunction(function) => t.transform_range_function(function),
            Node::PivotExpr(pivot) => t.transform_pivot(pivot),
            other => Err(Error::not_implemented(rustdb_common::byte_format!(
                "From Type {} not supported",
                super::node_tag(other).0
            ))),
        })
    }

    /// `TransformFrom`: no FROM is an empty table ref, several entries a chain of
    /// implicit cross products.
    pub(crate) fn transform_from(&mut self, root: &'a List) -> Result<TableRef> {
        match root.as_slice() {
            [] => Ok(build::empty_table_ref()),
            [single] => self.transform_table_ref_node(single),
            entries => {
                let mut result: Option<TableRef> = None;
                let mut pending_left: Option<TableRef> = None;
                for (index, node) in entries.iter().enumerate() {
                    let next = self.transform_table_ref_node(node)?;
                    if let Some(left) = pending_left.take() {
                        result = Some(cross_product(left, next));
                    } else if let Some(previous) = result.take() {
                        result = Some(cross_product(previous, next));
                    } else {
                        pending_left = Some(next);
                    }
                    self.check_depth(index as u64 + 1)?;
                }
                result.ok_or_else(|| Error::internal("FROM list without entries"))
            }
        }
    }

    /// `TransformRangeVar`: a base table.
    pub(crate) fn transform_range_var(&mut self, root: &'a RangeVar) -> Result<TableRef> {
        let (alias, column_name_alias) = transform_alias(root.alias.as_deref())?;
        let at_clause = match &root.at_clause {
            Some(node) => {
                let at: &PgAtClause = cast_node(node)?;
                let expr = self.transform_required(at.expr.as_ref(), "AT clause")?;
                Some(Box::new(AtClause {
                    unit: at.unit.clone().unwrap_or_default(),
                    expr: Box::new(expr),
                }))
            }
            None => None,
        };
        let sample = self.transform_sample_options(root.sample.as_ref())?;
        let mut result = build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
            schema_name: root.schemaname.clone().unwrap_or_default(),
            table_name: root.relname.clone().unwrap_or_default(),
            column_name_alias: column_name_alias.into_iter().collect(),
            catalog_name: root.catalogname.clone().unwrap_or_default(),
            at_clause,
        })));
        result.alias = alias;
        result.sample = sample;
        set_ref_location(&mut result, root.location);
        Ok(result)
    }

    /// `TransformJoin`.
    fn transform_join(&mut self, root: &'a JoinExpr) -> Result<TableRef> {
        let mut ref_type = JoinRefType::Regular;
        let join_type = match root.jointype {
            PgJoinType::Inner => JoinType::Inner,
            PgJoinType::Left => JoinType::Left,
            PgJoinType::Full => JoinType::Outer,
            PgJoinType::Right => JoinType::Right,
            PgJoinType::Semi => JoinType::Semi,
            PgJoinType::Anti => JoinType::Anti,
            PgJoinType::Position => {
                ref_type = JoinRefType::Positional;
                JoinType::Inner
            }
            other => {
                return Err(Error::not_implemented(rustdb_common::byte_format!(
                    "Join type {} not supported\n",
                    other as i32
                )));
            }
        };
        let left = self.transform_table_ref_node(required(root.larg.as_ref(), "join operand")?)?;
        let right = self.transform_table_ref_node(required(root.rarg.as_ref(), "join operand")?)?;
        match root.joinreftype {
            PgJoinRefType::Natural => ref_type = JoinRefType::Natural,
            PgJoinRefType::Asof => ref_type = JoinRefType::Asof,
            PgJoinRefType::Regular => {}
        }
        let mut join = JoinRef {
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            join_type,
            ref_type,
            using_columns: Vec::new(),
            delim_flipped: false,
            duplicate_eliminated_columns: Vec::new(),
            is_implicit: false,
        };
        if !root.using_clause.is_empty() {
            join.using_columns = string_list(&root.using_clause)?.into_iter().collect();
            let mut result = build::table_ref(TableRefKind::Join(Box::new(join)));
            set_ref_location(&mut result, root.location);
            return Ok(result);
        }
        if root.quals.is_none() && join.ref_type == JoinRefType::Regular {
            join.ref_type = JoinRefType::Cross;
        }
        join.condition = self.transform_opt_boxed(root.quals.as_ref())?;
        let mut result = build::table_ref(TableRefKind::Join(Box::new(join)));
        set_ref_location(&mut result, root.location);
        if let Some(alias) = root.alias.as_deref() {
            // A join with an alias becomes `(SELECT * FROM join) AS alias`.
            let mut select = build::select_node();
            select.select_list.push(build::star());
            *select.from_table = result;
            let statement =
                build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select))));
            let (alias, column_name_alias) = transform_alias(Some(alias))?;
            let mut subquery = build::table_ref(TableRefKind::Subquery(SubqueryRef {
                subquery: Box::new(statement),
                column_name_alias: column_name_alias.into_iter().collect(),
            }));
            set_ref_location(&mut subquery, root.location);
            subquery.alias = alias;
            return Ok(subquery);
        }
        Ok(result)
    }

    /// `TransformRangeSubselect`: a subquery in FROM, transformed by a child transformer.
    fn transform_range_subselect(&mut self, root: &'a RangeSubselect) -> Result<TableRef> {
        let subquery_node = required(root.subquery.as_ref(), "subquery")?;
        let subquery = self.child(|child| child.transform_select_stmt_node(subquery_node, true))?;
        let (alias, column_name_alias) = transform_alias(root.alias.as_deref())?;
        let mut result = build::table_ref(TableRefKind::Subquery(SubqueryRef {
            subquery: Box::new(subquery),
            column_name_alias: column_name_alias.into_iter().collect(),
        }));
        result.alias = alias;
        result.sample = self.transform_sample_options(root.sample.as_ref())?;
        Ok(result)
    }

    /// `TransformRangeFunction`: a table function.
    fn transform_range_function(&mut self, root: &'a RangeFunction) -> Result<TableRef> {
        if root.is_rowsfrom {
            return Err(Error::not_implemented("ROWS FROM() not implemented"));
        }
        let [function_sublist] = root.functions.as_slice() else {
            return Err(Error::not_implemented("Need exactly one function"));
        };
        let Node::List(function_sublist) = function_sublist else {
            return Err(Error::internal("Table function entry is not a list"));
        };
        let call_tree = required(function_sublist.first(), "table function")?;
        let has_coldef = function_sublist
            .get(1)
            .is_some_and(|coldef| !matches!(coldef, Node::List(list) if list.is_empty()));
        if has_coldef {
            return Err(Error::not_implemented(
                "Explicit column definition not supported yet",
            ));
        }
        let with_ordinality = if root.ordinality {
            OrdinalityType::WithOrdinality
        } else {
            OrdinalityType::WithoutOrdinality
        };
        let (function, location) = match call_tree {
            Node::FuncCall(call) => (self.transform_func_call(call)?, call.location),
            _ => return Err(Error::parser("Not a function call or value function")),
        };
        let (alias, column_name_alias) = transform_alias(root.alias.as_deref())?;
        let mut result = build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
            function: Box::new(function),
            column_name_alias: column_name_alias.into_iter().collect(),
            with_ordinality,
        }));
        set_ref_location(&mut result, location);
        result.alias = alias;
        result.sample = self.transform_sample_options(root.sample.as_ref())?;
        Ok(result)
    }

    /// `TransformPivotColumn`: one `ON ... IN (...)` of a PIVOT, or the `ON` of an UNPIVOT.
    pub(crate) fn transform_pivot_column(&mut self, pivot: &'a Pivot) -> Result<PivotColumn> {
        let mut column = PivotColumn {
            pivot_expressions: Vec::new(),
            unpivot_names: Vec::new(),
            entries: Vec::new(),
            pivot_enum: rustdb_common::ByteString::new(),
            subquery: None,
        };
        if !pivot.pivot_columns.is_empty() {
            self.transform_expression_list(&pivot.pivot_columns, &mut column.pivot_expressions)?;
            for expr in &column.pivot_expressions {
                if is_scalar(expr) {
                    return Err(parser_error_at(
                        expr.query_location,
                        rustdb_common::byte_format!("Cannot pivot on constant value \"{expr}\""),
                    ));
                }
                if has_subquery(expr) {
                    return Err(parser_error_at(
                        expr.query_location,
                        rustdb_common::byte_format!("Cannot pivot on subquery \"{expr}\""),
                    ));
                }
            }
        } else if !pivot.unpivot_columns.is_empty() {
            column.unpivot_names = string_list(&pivot.unpivot_columns)?.into_iter().collect();
        } else {
            return Err(Error::internal(
                "Either pivot_columns or unpivot_columns must be defined",
            ));
        }
        for node in &pivot.pivot_value {
            let expr = self.transform_expression(node)?;
            column.entries.push(PivotColumnEntry {
                values: Vec::new(),
                alias: expr.alias.clone(),
                star_expr: Some(Box::new(expr)),
            });
        }
        column.subquery = match &pivot.subquery {
            Some(node) => Some(Box::new(self.transform_select_node(node, true)?)),
            None => None,
        };
        if let Some(pivot_enum) = &pivot.pivot_enum {
            column.pivot_enum.clone_from(pivot_enum);
        }
        Ok(column)
    }

    /// `TransformPivotList`.
    pub(crate) fn transform_pivot_list(&mut self, list: &'a List) -> Result<Vec<PivotColumn>> {
        list.iter()
            .map(|node| self.transform_pivot_column(cast_node(node)?))
            .collect()
    }

    /// `TransformPivot`: `FROM t PIVOT (...)` / `UNPIVOT (...)`.
    fn transform_pivot(&mut self, root: &'a PivotExpr) -> Result<TableRef> {
        let source = self.transform_table_ref_node(required(root.source.as_ref(), "pivot source")?)?;
        let aggregates = self.transform_expressions(&root.aggrs)?;
        let unpivot_names = string_list(&root.unpivots)?;
        let is_pivot = unpivot_names.is_empty();
        let pivots: Vec<PivotColumn> = self.transform_pivot_list(&root.pivots)?.into_iter().collect();
        if !is_pivot && pivots.len() > 1 {
            return Err(Error::parser("UNPIVOT requires a single pivot element"));
        }
        let groups = string_list(&root.groups)?;
        if !is_pivot && pivots.iter().any(|pivot| pivot.unpivot_names.len() != 1) {
            return Err(Error::parser(
                "UNPIVOT requires a single column name for the PIVOT IN clause",
            ));
        }
        let (alias, column_name_alias) = transform_alias(root.alias.as_deref())?;
        let mut result = build::table_ref(TableRefKind::Pivot(Box::new(PivotRef {
            source: Box::new(source),
            aggregates,
            unpivot_names: unpivot_names.into_iter().collect(),
            pivots,
            groups: groups.into_iter().collect(),
            column_name_alias: column_name_alias.into_iter().collect(),
            include_nulls: root.include_nulls,
            bound: None,
        })));
        result.alias = alias;
        set_ref_location(&mut result, root.location);
        Ok(result)
    }
}

/// A `JoinRef(CROSS)` marked as implicit (a comma in FROM).
fn cross_product(left: TableRef, right: TableRef) -> TableRef {
    build::table_ref(TableRefKind::Join(Box::new(JoinRef {
        left: Box::new(left),
        right: Box::new(right),
        condition: None,
        join_type: JoinType::Inner,
        ref_type: JoinRefType::Cross,
        using_columns: Vec::new(),
        delim_flipped: false,
        duplicate_eliminated_columns: Vec::new(),
        is_implicit: true,
    })))
}
