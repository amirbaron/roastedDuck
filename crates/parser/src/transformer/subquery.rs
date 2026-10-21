//! Subquery expressions (`transform_subquery.cpp`).

use rustdb_ast::{
    CaseCheck, ExpressionType, OrderModifier, ParsedExpression, ParsedExpressionKind, QueryNodeKind,
    ResultModifier, SelectStatement, SubqueryExpression, SubqueryType,
};
use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_pg_parser::nodes::{SubLink, SubLinkType};
use rustdb_types::LogicalType;

use super::build;
use super::expression::{negate_comparison, operator_to_expression_type};
use super::{Transformer, node_string, required};
use crate::equality::for_each_child_mut;

/// `RemoveOrderQualificationRecursive`: `t.x` becomes `x` (subqueries are not entered).
fn remove_order_qualification(expr: &mut ParsedExpression) {
    if let ParsedExpressionKind::ColumnRef(column_ref) = &mut expr.kind
        && column_ref.column_names.len() > 1
        && let Some(last) = column_ref.column_names.pop()
    {
        column_ref.column_names = vec![last];
    }
    for_each_child_mut(expr, &mut remove_order_qualification);
}

fn subquery_expression(
    subquery_type: SubqueryType,
    subquery: SelectStatement,
    child: Option<ParsedExpression>,
    comparison_type: ExpressionType,
) -> SubqueryExpression {
    SubqueryExpression {
        subquery_type,
        subquery: Box::new(subquery),
        child: child.map(Box::new),
        comparison_type,
    }
}

impl<'a> Transformer<'a> {
    /// `TransformSubquery`: `EXISTS (...)`, `x op ANY/ALL (...)`, `(SELECT ...)`, `ARRAY(...)`.
    #[inline(never)]
    pub(crate) fn transform_subquery(&mut self, root: &'a SubLink) -> Result<ParsedExpression> {
        let location = u64::try_from(root.location).ok();
        let wrap = |subquery: SubqueryExpression| {
            let mut result =
                ParsedExpression::new(ExpressionType::Subquery, ParsedExpressionKind::Subquery(subquery));
            result.query_location = location;
            result
        };
        let subselect = required(root.subselect.as_ref(), "subquery")?;
        match root.sub_link_type {
            SubLinkType::Exists => {
                let subquery = self.transform_select_stmt_node(subselect, true)?;
                Ok(wrap(subquery_expression(
                    SubqueryType::Exists,
                    subquery,
                    None,
                    ExpressionType::Invalid,
                )))
            }
            SubLinkType::Any | SubLinkType::All => {
                let child = self.transform_opt_expression(root.testexpr.as_ref())?;
                let mut comparison_type = match root.oper_name.first() {
                    None => ExpressionType::CompareEqual,
                    Some(name) => operator_to_expression_type(node_string(name)?),
                };
                if !matches!(
                    comparison_type,
                    ExpressionType::CompareEqual
                        | ExpressionType::CompareNotequal
                        | ExpressionType::CompareGreaterthan
                        | ExpressionType::CompareGreaterthanorequalto
                        | ExpressionType::CompareLessthan
                        | ExpressionType::CompareLessthanorequalto
                ) {
                    return Err(Error::parser(
                        "ANY and ALL operators require one of =,<>,>,<,>=,<= comparisons!",
                    ));
                }
                let subquery = self.transform_select_stmt_node(subselect, true)?;
                if root.sub_link_type == SubLinkType::All {
                    comparison_type = negate_comparison(comparison_type)?;
                    let any = wrap(subquery_expression(
                        SubqueryType::Any,
                        subquery,
                        child,
                        comparison_type,
                    ));
                    return Ok(build::operator(ExpressionType::OperatorNot, vec![any]));
                }
                Ok(wrap(subquery_expression(
                    SubqueryType::Any,
                    subquery,
                    child,
                    comparison_type,
                )))
            }
            SubLinkType::Expr => {
                let subquery = self.transform_select_stmt_node(subselect, true)?;
                Ok(wrap(subquery_expression(
                    SubqueryType::Scalar,
                    subquery,
                    None,
                    ExpressionType::Invalid,
                )))
            }
            SubLinkType::Array => {
                let subquery = self.transform_select_stmt_node(subselect, true)?;
                let select = transform_array_subquery(subquery, location)?;
                Ok(wrap(subquery_expression(
                    SubqueryType::Scalar,
                    select,
                    None,
                    ExpressionType::Invalid,
                )))
            }
            other => Err(Error::not_implemented(format!(
                "Subquery of type {} not implemented\n",
                other as i32
            ))),
        }
    }
}

/// The ARRAY subquery rewrite: `SELECT CASE WHEN array_agg(col) IS NULL THEN [] ELSE
/// array_agg(col) END FROM (subquery)`, with the subquery's ORDER BY moved into the
/// aggregate.
fn transform_array_subquery(mut subquery: SelectStatement, location: Option<u64>) -> Result<SelectStatement> {
    let is_select = matches!(subquery.node.kind, QueryNodeKind::Select(_));
    let array_agg_child = if let QueryNodeKind::Select(select) = &subquery.node.kind {
        if select.select_list.len() != 1 {
            return Err(Error::binder(format!(
                "Subquery returns {} columns - expected 1",
                select.select_list.len()
            ))
            .with_position(location));
        }
        build::positional_reference(1)
    } else {
        let mut columns_star = build::star_expression(String::new());
        columns_star.columns = true;
        ParsedExpression::new(
            ExpressionType::Star,
            ParsedExpressionKind::Star(Box::new(columns_star)),
        )
    };
    let mut order_bys = OrderModifier { orders: Vec::new() };
    for modifier in &subquery.node.modifiers {
        if let ResultModifier::Order(order) = modifier {
            order_bys = order.clone();
            break;
        }
    }
    let mut array_idx = 0;
    for order in &mut order_bys.orders {
        if order.expression.expression_type == ExpressionType::ValueConstant {
            if let ParsedExpressionKind::Constant(constant) = &order.expression.kind
                && let Ok(value) = constant.value.try_cast(&LogicalType::BIGINT)
            {
                let index = value.as_i128().unwrap_or(0);
                let positional = u64::try_from(index).unwrap_or(u64::MAX);
                *order.expression = build::positional_reference(positional);
            }
        } else if is_select {
            array_idx += 1;
            let alias = format!("__array_internal_idx_{array_idx}");
            let mut expression =
                std::mem::replace(&mut *order.expression, build::column_ref(vec![alias.clone()]));
            expression.alias = alias;
            if let QueryNodeKind::Select(select) = &mut subquery.node.kind {
                select.select_list.push(expression);
            }
        } else {
            remove_order_qualification(&mut order.expression);
        }
    }
    let aggr = build::function_full(
        String::new(),
        String::new(),
        "array_agg",
        vec![array_agg_child],
        None,
        order_bys,
        false,
        false,
        false,
    );
    let agg_is_null = build::operator(ExpressionType::OperatorIsNull, vec![aggr.clone()]);
    let empty_list = build::function("list_value", Vec::new());
    let case = build::case_expression(
        vec![CaseCheck {
            when_expr: Box::new(agg_is_null),
            then_expr: Box::new(empty_list),
        }],
        aggr,
    );
    let mut select = build::select_node();
    select.select_list.push(case);
    *select.from_table = build::subquery_ref(subquery);
    Ok(build::select_statement(build::query_node(QueryNodeKind::Select(
        Box::new(select),
    ))))
}
