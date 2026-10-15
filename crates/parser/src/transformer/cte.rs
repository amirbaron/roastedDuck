//! Common table expressions (`transform_cte.cpp`).

use rustdb_ast::{
    CommonTableExpressionInfo, CommonTableExpressionMap, CteMaterialize, ParsedExpression, QueryNodeKind,
    RecursiveCteNode, SelectStatement,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{
    CommonTableExpr, CteMaterialize as PgCteMaterialize, Node, SetOperation, WithClause,
};

use super::build;
use super::{Transformer, cast_node, required, string_list};

impl<'a> Transformer<'a> {
    /// `TransformCTE`: the WITH clause into `cte_map`.
    pub(crate) fn transform_cte(
        &mut self,
        with: &'a WithClause,
        cte_map: &mut CommonTableExpressionMap,
    ) -> Result<()> {
        // `stored_cte_map.push_back(cte_map)`: the map is recorded before it is filled, so
        // CTEs see the earlier ones of the same WITH clause.
        let slot = self.may_pivot().then(|| {
            let frame = self.frame();
            frame.stored_cte_maps.push(CommonTableExpressionMap::default());
            frame.stored_cte_maps.len() - 1
        });
        for node in &with.ctes {
            let cte: &'a CommonTableExpr = cast_node(node)?;
            let mut key_targets = Vec::new();
            if cte
                .recursive_keys
                .first()
                .is_some_and(|key| !matches!(key, Node::List(list) if list.is_empty()))
            {
                self.transform_expression_list(&cte.recursive_keys, &mut key_targets)?;
            }
            let aliases = string_list(&cte.aliascolnames)?;
            if !cte.ctecolnames.is_empty() {
                return Err(Error::not_implemented(
                    "Column name setting not supported in CTEs",
                ));
            }
            if !cte.ctecoltypes.is_empty() {
                return Err(Error::not_implemented(
                    "Column type setting not supported in CTEs",
                ));
            }
            if !cte.ctecoltypmods.is_empty() {
                return Err(Error::not_implemented(
                    "Column type modification not supported in CTEs",
                ));
            }
            if !cte.ctecolcollations.is_empty() {
                return Err(Error::not_implemented("CTE collations not supported"));
            }
            let Some(query) = cte
                .ctequery
                .as_ref()
                .filter(|query| matches!(query, Node::SelectStmt(_)))
            else {
                return Err(Error::parser("A CTE needs a SELECT"));
            };
            let query = if cte.cterecursive || with.recursive {
                self.transform_recursive_cte(cte, &aliases, &key_targets)?
            } else {
                self.child(|child| child.transform_select_stmt_node(query, true))?
            };
            let cte_name = cte.ctename.clone().unwrap_or_default();
            if cte_map.map.contains_key(&cte_name) {
                return Err(Error::parser(format!("Duplicate CTE name \"{cte_name}\"")));
            }
            let materialized = match cte.ctematerialized {
                PgCteMaterialize::Default => CteMaterialize::CteMaterializeDefault,
                PgCteMaterialize::Always => CteMaterialize::CteMaterializeAlways,
                PgCteMaterialize::Never => CteMaterialize::CteMaterializeNever,
            };
            let info = CommonTableExpressionInfo {
                aliases,
                query: Box::new(query),
                materialized,
                key_targets,
            };
            if let Some(slot) = slot {
                self.frame().stored_cte_maps[slot]
                    .map
                    .insert(cte_name.as_str(), info.clone());
            }
            cte_map.map.insert(cte_name, info);
        }
        Ok(())
    }

    /// `TransformRecursiveCTE`: a UNION becomes a recursive CTE node; anything else is a
    /// plain query.
    fn transform_recursive_cte(
        &mut self,
        cte: &'a CommonTableExpr,
        aliases: &[String],
        key_targets: &[ParsedExpression],
    ) -> Result<SelectStatement> {
        let query = required(cte.ctequery.as_ref(), "CTE query")?;
        let stmt = cast_node::<rustdb_pg_parser::nodes::SelectStmt>(query)?;
        if stmt.op != SetOperation::Union {
            let node = self.transform_select_node(query, true)?;
            return Ok(build::select_statement(node));
        }
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &stmt.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        let left =
            self.transform_select_node(required(stmt.larg.as_ref(), "recursive CTE operand")?, true)?;
        let right =
            self.transform_select_node(required(stmt.rarg.as_ref(), "recursive CTE operand")?, true)?;
        let mut node = build::query_node(QueryNodeKind::RecursiveCte(Box::new(RecursiveCteNode {
            cte_name: cte.ctename.clone().unwrap_or_default(),
            union_all: stmt.all,
            left: Box::new(left),
            right: Box::new(right),
            aliases: aliases.to_vec(),
            key_targets: key_targets.to_vec(),
        })));
        node.cte_map = cte_map;
        if stmt.limit_count.is_some() || stmt.limit_offset.is_some() {
            return Err(Error::parser(
                "LIMIT or OFFSET in a recursive query is not allowed",
            ));
        }
        if !stmt.sort_clause.is_empty() {
            return Err(Error::parser("ORDER BY in a recursive query is not allowed"));
        }
        Ok(build::select_statement(node))
    }
}
