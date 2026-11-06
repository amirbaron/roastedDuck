//! The transform functions of DuckDB's `transform_create_view.cpp`: `CREATE [RECURSIVE]
//! VIEW`, and the conversion of a `UNION` into a recursive CTE that recursive views and
//! `WITH RECURSIVE` share.

use rustdb_parser::build;

use super::create_table::{create_info, on_create_conflict};
use super::prelude::*;

/// `PEGTransformerFactory::ToRecursiveCTE`: a `UNION [ALL]` becomes a recursive CTE node
/// named `name` (an n-ary union is split into all operands but the last, and the last);
/// other nodes are returned as they are.
pub(super) fn to_recursive_cte(
    node: QueryNode,
    name: &[u8],
    aliases: &[ByteString],
    key_targets: &[ParsedExpression],
) -> Result<QueryNode> {
    let QueryNodeKind::SetOperation(set_node) = &node.kind else {
        return Ok(node);
    };
    if set_node.setop_type != SetOperationType::Union {
        return Ok(node);
    }
    if set_node.children.len() < 2 {
        return Err(Error::parser(
            "Expected at least two children to set operation node in recursive CTE",
        ));
    }
    let QueryNode {
        modifiers,
        cte_map,
        kind,
    } = node;
    let QueryNodeKind::SetOperation(mut owned_set_node) = kind else {
        return Err(Error::internal("Expected a set operation node"));
    };
    let union_all = owned_set_node.setop_all;
    let (left, right) = if owned_set_node.children.len() == 2 {
        let right = owned_set_node.children.pop();
        let left = owned_set_node.children.pop();
        (left, right)
    } else {
        // An n-ary flattened node: left is all operands but the last, right the last,
        // like the left-recursive binary tree of the grammar.
        let right = owned_set_node.children.pop();
        let left = if owned_set_node.children.len() == 1 {
            owned_set_node.children.pop()
        } else {
            // The CTEs moved to the recursive node; the modifiers stay with the union.
            Some(QueryNode {
                modifiers,
                cte_map: CommonTableExpressionMap::default(),
                kind: QueryNodeKind::SetOperation(owned_set_node),
            })
        };
        (left, right)
    };
    let (Some(left), Some(right)) = (left, right) else {
        return Err(Error::internal("Expected two operands in recursive CTE"));
    };
    let mut recursive_node = build::query_node(QueryNodeKind::RecursiveCte(Box::new(RecursiveCteNode {
        cte_name: name.to_owned().into(),
        union_all,
        left: Box::new(left),
        right: Box::new(right),
        aliases: aliases.to_vec(),
        key_targets: key_targets.to_vec(),
    })));
    recursive_node.cte_map = cte_map;
    Ok(recursive_node)
}

/// `PEGTransformerFactory::WrapRecursiveView`: `SELECT aliases FROM view` with the
/// recursive node as the CTE `view`.
fn wrap_recursive_view(
    catalog: &[u8],
    schema: &[u8],
    view_name: &[u8],
    aliases: &[ByteString],
    inner_node: QueryNode,
) -> SelectStatement {
    let cte_info = CommonTableExpressionInfo {
        aliases: aliases.to_vec(),
        query: Box::new(build::select_statement(inner_node)),
        materialized: CteMaterialize::CteMaterializeDefault,
        key_targets: Vec::new(),
    };
    let mut outer_select = build::select_node();
    outer_select.select_list = aliases
        .iter()
        .map(|column| ParsedExpression::column_ref([column.clone()]))
        .collect();
    *outer_select.from_table = build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
        schema_name: schema.to_owned().into(),
        table_name: view_name.to_owned().into(),
        column_name_alias: Vec::new(),
        catalog_name: catalog.to_owned().into(),
        at_clause: None,
    })));
    let mut outer_node = build::query_node(QueryNodeKind::Select(Box::new(outer_select)));
    outer_node.cte_map.map.insert(view_name, cte_info);
    build::select_statement(outer_node)
}

/// `PEGTransformerFactory::ConvertToRecursiveView`: the query of a recursive view.
fn convert_to_recursive_view(
    catalog: &[u8],
    schema: &[u8],
    view_name: &[u8],
    aliases: &[ByteString],
    node: QueryNode,
) -> Result<SelectStatement> {
    let result_node = to_recursive_cte(node, view_name, aliases, &[])?;
    Ok(wrap_recursive_view(
        catalog,
        schema,
        view_name,
        aliases,
        result_node,
    ))
}

impl PegTransformer {
    /// `TransformCreateViewStmt`: `[RECURSIVE] VIEW [IF NOT EXISTS] name [(columns)] AS
    /// query`.
    pub(super) fn transform_create_view_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        let list = parse_result.list()?;
        let if_not_exists = list.has(2)?;
        let qualified_name: QualifiedName = self.transform_child(list, 3)?;
        let column_list: Vec<ByteString> = self.transform_optional(list, 4)?.unwrap_or_default();
        let select_statement: SelectStatement = self.transform_child(list, 6)?;
        let query = if list.has(0)? {
            convert_to_recursive_view(
                &qualified_name.catalog,
                &qualified_name.schema,
                &qualified_name.name,
                &column_list,
                *select_statement.node,
            )?
        } else {
            select_statement
        };
        self.pivot_entry_check("view")?;
        let view_info = CreateViewInfo {
            view_name: qualified_name.name,
            aliases: column_list,
            types: Vec::new(),
            query: Box::new(query),
            names: Vec::new(),
            column_comments: OrderedMap::default(),
        };
        let mut info = create_info(&qualified_name.schema, CreateInfoKind::View(Box::new(view_info)));
        info.catalog = qualified_name.catalog;
        info.on_conflict = on_create_conflict(if_not_exists);
        Ok(info)
    }
}
