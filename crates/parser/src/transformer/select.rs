//! SELECT statements and query nodes (`transform_select.cpp`, `transform_select_node.cpp`,
//! `transform_show.cpp`, `transform_show_select.cpp`, the VALUES list of
//! `transform_insert.cpp`).

use roastedduck_ast::{
    DistinctModifier, ExpressionListRef, LimitModifier, LimitPercentModifier, OrderModifier, QueryNode,
    QueryNodeKind, ResultModifier, SelectStatement, SetOperationNode, SetOperationType, ShowRef, ShowType,
    TableRef, TableRefKind,
};
use roastedduck_common::{Error, Result};
use roastedduck_pg_parser::nodes::{
    List, Node, SelectStmt, SetOperation, VariableShowSelectStmt, VariableShowStmt, WindowDef,
};

use super::build;
use super::helpers::transform_qualified_name;
use super::{Transformer, cast_node, required};

/// `SetOperationsMatch`: whether `node` is a "simple" UNION of the same kind as `root`,
/// whose children can be merged into `root`'s.
fn set_operations_match(root: &SelectStmt, node: &Node) -> bool {
    let Node::SelectStmt(stmt) = node else {
        return false;
    };
    if root.op != stmt.op || root.all != stmt.all {
        return false;
    }
    if root.op != SetOperation::Union && root.op != SetOperation::UnionByName {
        return false;
    }
    stmt.with_clause.is_none()
        && stmt.sort_clause.is_empty()
        && stmt.limit_count.is_none()
        && stmt.limit_offset.is_none()
        && stmt.sample_options.is_none()
}

/// A DISTINCT clause holding only a NULL cell is a plain DISTINCT (no ON targets).
fn has_distinct_targets(list: &List) -> bool {
    !matches!(list.first(), None | Some(Node::List(_)))
}

impl<'a> Transformer<'a> {
    /// `TransformSelectStmt(PGSelectStmt &)`.
    pub(crate) fn transform_select_stmt(
        &mut self,
        select: &'a SelectStmt,
        is_select: bool,
    ) -> Result<SelectStatement> {
        Ok(build::select_statement(
            self.transform_select_node_internal(select, is_select)?,
        ))
    }

    /// `TransformSelectStmt(PGNode &)`: also accepts SHOW / DESCRIBE.
    pub(crate) fn transform_select_stmt_node(
        &mut self,
        node: &'a Node,
        is_select: bool,
    ) -> Result<SelectStatement> {
        Ok(build::select_statement(
            self.transform_select_node(node, is_select)?,
        ))
    }

    /// `TransformSelectNode`.
    pub(crate) fn transform_select_node(&mut self, node: &'a Node, is_select: bool) -> Result<QueryNode> {
        match node {
            Node::VariableShowSelectStmt(stmt) => self.transform_show_select(stmt),
            Node::VariableShowStmt(stmt) => self.transform_show(stmt),
            other => self.transform_select_node_internal(cast_node(other)?, is_select),
        }
    }

    /// `TransformSelectNodeInternal`.
    fn transform_select_node_internal(
        &mut self,
        select: &'a SelectStmt,
        is_select: bool,
    ) -> Result<QueryNode> {
        if is_select {
            if select.into_clause.is_some() {
                return Err(Error::parser("SELECT INTO not supported!"));
            }
            if !select.locking_clause.is_empty() {
                return Err(Error::parser("SELECT locking clause is not supported!"));
            }
        }
        if select.pivot.is_some() {
            return self.transform_pivot_statement(select);
        }
        self.transform_select_internal(select)
    }

    /// `TransformModifiers`: ORDER BY and LIMIT/OFFSET.
    pub(crate) fn transform_modifiers(&mut self, stmt: &'a SelectStmt, node: &mut QueryNode) -> Result<()> {
        let mut orders = Vec::new();
        self.transform_order_by(&stmt.sort_clause, &mut orders)?;
        if !orders.is_empty() {
            node.modifiers
                .push(ResultModifier::Order(OrderModifier { orders }));
        }
        if stmt.limit_count.is_none() && stmt.limit_offset.is_none() {
            return Ok(());
        }
        if let Some(Node::LimitPercent(percent)) = &stmt.limit_count {
            let limit = self.transform_opt_boxed(percent.limit_percent.as_ref())?;
            let offset = self.transform_opt_boxed(stmt.limit_offset.as_ref())?;
            node.modifiers
                .push(ResultModifier::LimitPercent(LimitPercentModifier {
                    limit,
                    offset,
                }));
            return Ok(());
        }
        let mut modifier = LimitModifier {
            limit: None,
            offset: None,
        };
        if stmt.offset_first {
            modifier.offset = self.transform_opt_boxed(stmt.limit_offset.as_ref())?;
            modifier.limit = self.transform_opt_boxed(stmt.limit_count.as_ref())?;
        } else {
            modifier.limit = self.transform_opt_boxed(stmt.limit_count.as_ref())?;
            modifier.offset = self.transform_opt_boxed(stmt.limit_offset.as_ref())?;
        }
        node.modifiers.push(ResultModifier::Limit(modifier));
        Ok(())
    }

    /// `TransformSetOperationChildren`: the operands of a set operation, with nested
    /// simple UNIONs of the same kind flattened in order.
    fn transform_set_operation_children(&mut self, stmt: &'a SelectStmt) -> Result<Vec<QueryNode>> {
        let mut pending: Vec<&'a Node> = vec![
            required(stmt.larg.as_ref(), "set operation operand")?,
            required(stmt.rarg.as_ref(), "set operation operand")?,
        ];
        let mut children = Vec::new();
        let mut i = 0;
        while i < pending.len() {
            let node = pending[i];
            if set_operations_match(stmt, node) {
                let Node::SelectStmt(select) = node else {
                    return Err(Error::internal("Set operation operand is not a SELECT"));
                };
                pending[i] = required(select.larg.as_ref(), "set operation operand")?;
                pending.insert(i + 1, required(select.rarg.as_ref(), "set operation operand")?);
            } else {
                children.push(self.transform_select_node(node, true)?);
                i += 1;
            }
        }
        Ok(children)
    }

    /// `TransformSelectInternal`.
    pub(crate) fn transform_select_internal(&mut self, stmt: &'a SelectStmt) -> Result<QueryNode> {
        self.nested(|t| t.transform_select_internal_inner(stmt))
    }

    fn transform_select_internal_inner(&mut self, stmt: &'a SelectStmt) -> Result<QueryNode> {
        let mut node = match stmt.op {
            SetOperation::None => {
                let mut node = build::query_node(QueryNodeKind::Select(Box::new(build::select_node())));
                if let Some(with) = &stmt.with_clause {
                    self.transform_cte(with, &mut node.cte_map)?;
                }
                for window in &stmt.window_clause {
                    let window_def: &'a WindowDef = cast_node(window)?;
                    let name = window_def.name.clone().unwrap_or_default();
                    if self.frame().window_clauses.contains_key(&name) {
                        return Err(Error::parser(roastedduck_common::byte_format!(
                            "window \"{name}\" is already defined"
                        )));
                    }
                    self.frame().window_clauses.insert(name, window_def);
                }
                let QueryNodeKind::Select(result) = &mut node.kind else {
                    return Err(Error::internal("Expected a SELECT node"));
                };
                if let Some(distinct) = &stmt.distinct_clause {
                    let mut modifier = DistinctModifier {
                        distinct_on_targets: Vec::new(),
                    };
                    if has_distinct_targets(distinct) {
                        self.transform_expression_list(distinct, &mut modifier.distinct_on_targets)?;
                    }
                    node.modifiers.push(ResultModifier::Distinct(modifier));
                }
                if stmt.values_lists.is_empty() {
                    if stmt.target_list.is_empty() {
                        return Err(Error::parser("SELECT clause without selection list"));
                    }
                    if stmt.from_first {
                        *result.from_table = self.transform_from(&stmt.from_clause)?;
                        self.transform_expression_list(&stmt.target_list, &mut result.select_list)?;
                    } else {
                        self.transform_expression_list(&stmt.target_list, &mut result.select_list)?;
                        *result.from_table = self.transform_from(&stmt.from_clause)?;
                    }
                } else {
                    *result.from_table = self.transform_values_list(&stmt.values_lists)?;
                    result.select_list.push(build::star());
                }
                result.where_clause = self.transform_opt_boxed(stmt.where_clause.as_ref())?;
                self.transform_group_by(&stmt.group_clause, result)?;
                result.having = self.transform_opt_boxed(stmt.having_clause.as_ref())?;
                result.qualify = self.transform_opt_boxed(stmt.qualify_clause.as_ref())?;
                result.sample = self.transform_sample_options(stmt.sample_options.as_ref())?;
                node
            }
            SetOperation::Union
            | SetOperation::Except
            | SetOperation::Intersect
            | SetOperation::UnionByName => {
                let mut cte_map = roastedduck_ast::CommonTableExpressionMap::default();
                if let Some(with) = &stmt.with_clause {
                    self.transform_cte(with, &mut cte_map)?;
                }
                let children = self.transform_set_operation_children(stmt)?;
                let setop_type = match stmt.op {
                    SetOperation::Union => SetOperationType::Union,
                    SetOperation::Except => SetOperationType::Except,
                    SetOperation::Intersect => SetOperationType::Intersect,
                    _ => SetOperationType::UnionByName,
                };
                if stmt.sample_options.is_some() {
                    return Err(Error::parser(
                        "SAMPLE clause is only allowed in regular SELECT statements",
                    ));
                }
                let mut node = build::query_node(QueryNodeKind::SetOperation(SetOperationNode {
                    setop_type,
                    setop_all: stmt.all,
                    children,
                }));
                node.cte_map = cte_map;
                node
            }
        };
        self.transform_modifiers(stmt, &mut node)?;
        Ok(node)
    }

    /// `TransformValuesList`: `VALUES (...), (...)`.
    pub(crate) fn transform_values_list(&mut self, list: &'a List) -> Result<TableRef> {
        let mut values: Vec<Vec<roastedduck_ast::ParsedExpression>> = Vec::with_capacity(list.len());
        for row in list {
            let Node::List(row) = row else {
                return Err(Error::internal("VALUES row is not a list"));
            };
            let row_values = self.transform_expressions(row)?;
            if let Some(first) = values.first()
                && first.len() != row_values.len()
            {
                return Err(Error::parser("VALUES lists must all be the same length"));
            }
            values.push(row_values);
        }
        let mut result = build::table_ref(TableRefKind::ExpressionList(Box::new(ExpressionListRef {
            expected_names: Vec::new(),
            expected_types: Vec::new(),
            values,
        })));
        result.alias = "valueslist".into();
        Ok(result)
    }

    /// `TransformShow`: `SHOW TABLES`, `DESCRIBE tbl`, `SUMMARIZE tbl`, ...
    fn transform_show(&mut self, stmt: &'a VariableShowStmt) -> Result<QueryNode> {
        let mut showref = ShowRef {
            table_name: String::new().into(),
            query: None,
            show_type: ShowType::Describe,
            catalog_name: String::new().into(),
            schema_name: String::new().into(),
        };
        if let Some(set) = &stmt.set {
            if set == "__show_tables_from_database" {
                showref.show_type = ShowType::ShowFrom;
                let qualified_name =
                    transform_qualified_name(required(stmt.relation.as_deref(), "relation")?)?;
                if !qualified_name.catalog.is_empty() {
                    return Err(Error::parser(
                        "Expected \"SHOW TABLES FROM database\", \"SHOW TABLES FROM schema\", or \"SHOW TABLES \
                         FROM database.schema\"",
                    ));
                }
                if qualified_name.schema.is_empty() {
                    showref.schema_name = qualified_name.name;
                } else {
                    showref.catalog_name = qualified_name.schema;
                    showref.schema_name = qualified_name.name;
                }
            } else {
                showref.table_name = set.into();
                showref.show_type = ShowType::ShowUnqualified;
            }
        } else {
            let relation = required(stmt.relation.as_deref(), "relation")?;
            if relation.schemaname.is_none() {
                let table_name = relation
                    .relname
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(
                    table_name.as_slice(),
                    b"databases" | b"schemas" | b"tables" | b"variables"
                ) {
                    showref.table_name = roastedduck_common::byte_format!("\"{table_name}\"");
                    showref.show_type = ShowType::ShowUnqualified;
                }
            }
        }
        if showref.table_name.is_empty() && showref.show_type != ShowType::ShowFrom {
            let mut select = build::select_node();
            select.select_list.push(build::star());
            *select.from_table = self.transform_range_var(required(stmt.relation.as_deref(), "relation")?)?;
            showref.query = Some(Box::new(build::query_node(QueryNodeKind::Select(Box::new(
                select,
            )))));
        }
        if showref.show_type == ShowType::Describe {
            showref.show_type = if stmt.is_summary != 0 {
                ShowType::Summary
            } else {
                ShowType::Describe
            };
        }
        let mut select = build::select_node();
        select.select_list.push(build::star());
        *select.from_table = build::table_ref(TableRefKind::Show(Box::new(showref)));
        Ok(build::query_node(QueryNodeKind::Select(Box::new(select))))
    }

    /// `TransformShowStmt`.
    pub(crate) fn transform_show_stmt(&mut self, stmt: &'a VariableShowStmt) -> Result<SelectStatement> {
        Ok(build::select_statement(self.transform_show(stmt)?))
    }

    /// `TransformShowSelect`: `DESCRIBE SELECT ...`, `SUMMARIZE SELECT ...`.
    fn transform_show_select(&mut self, stmt: &'a VariableShowSelectStmt) -> Result<QueryNode> {
        let query = self.transform_select_node(required(stmt.stmt.as_ref(), "statement")?, true)?;
        let showref = ShowRef {
            table_name: String::new().into(),
            query: Some(Box::new(query)),
            show_type: if stmt.is_summary != 0 {
                ShowType::Summary
            } else {
                ShowType::Describe
            },
            catalog_name: String::new().into(),
            schema_name: String::new().into(),
        };
        let mut select = build::select_node();
        select.select_list.push(build::star());
        *select.from_table = build::table_ref(TableRefKind::Show(Box::new(showref)));
        Ok(build::query_node(QueryNodeKind::Select(Box::new(select))))
    }

    /// `TransformShowSelectStmt`.
    pub(crate) fn transform_show_select_stmt(
        &mut self,
        stmt: &'a VariableShowSelectStmt,
    ) -> Result<SelectStatement> {
        Ok(build::select_statement(self.transform_show_select(stmt)?))
    }
}
