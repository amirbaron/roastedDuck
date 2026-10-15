//! Operators (`transform_operator.cpp`).

use rustdb_ast::{
    BetweenExpression, ExpressionType, ParsedExpression, ParsedExpressionKind, QueryNodeKind,
    SubqueryExpression, SubqueryType,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{AExpr, AExprKind, Node};

use super::build::{self, set_location};
use super::expression::{negate_comparison, operator_to_expression_type};
use super::{Transformer, node_string, required};

impl<'a> Transformer<'a> {
    /// `TransformUnaryOperator`: a prefix (or `__postfix`) operator function.
    fn transform_unary_operator(op: &str, child: ParsedExpression) -> ParsedExpression {
        let mut result = build::function(op, vec![child]);
        if let ParsedExpressionKind::Function(function) = &mut result.kind {
            function.is_operator = true;
        }
        result
    }

    /// `TransformBinaryOperator`.
    fn transform_binary_operator(
        &self,
        op: &str,
        left: ParsedExpression,
        right: ParsedExpression,
    ) -> ParsedExpression {
        let op = if self.options.integer_division && op == "/" {
            "//"
        } else {
            op
        };
        if op == "~" || op == "!~" {
            let result = build::function("regexp_full_match", vec![left, right]);
            return if op == "!~" {
                build::operator(ExpressionType::OperatorNot, vec![result])
            } else {
                result
            };
        }
        let target_type = operator_to_expression_type(op);
        if target_type != ExpressionType::Invalid {
            return build::comparison(target_type, left, right);
        }
        let mut result = build::function(op, vec![left, right]);
        if let ParsedExpressionKind::Function(function) = &mut result.kind {
            function.is_operator = true;
        }
        result
    }

    /// `TransformInExpression`: `x IN (a, b)` or `x IN list_expression`.
    fn transform_in_expression(&mut self, name: &str, root: &'a AExpr) -> Result<ParsedExpression> {
        let left = self.transform_opt_expression(root.lexpr.as_ref())?;
        let operator_type = if name == "<>" {
            ExpressionType::CompareNotIn
        } else {
            ExpressionType::CompareIn
        };
        let right = required(root.rexpr.as_ref(), "IN list")?;
        if let Node::List(list) = right {
            let mut children: Vec<ParsedExpression> = left.into_iter().collect();
            self.transform_expression_list(list, &mut children)?;
            return Ok(build::operator(operator_type, children));
        }
        let expr = self.transform_expression(right)?;
        let mut children = vec![expr];
        children.extend(left);
        let result = build::function("contains", children);
        Ok(if operator_type == ExpressionType::CompareNotIn {
            build::operator(ExpressionType::OperatorNot, vec![result])
        } else {
            result
        })
    }

    /// `TransformAExpr`.
    pub(crate) fn transform_a_expr(&mut self, root: &'a AExpr) -> Result<ParsedExpression> {
        let mut result = self.transform_a_expr_internal(root)?;
        set_location(&mut result, root.location);
        Ok(result)
    }

    fn transform_a_expr_internal(&mut self, root: &'a AExpr) -> Result<ParsedExpression> {
        let name = node_string(required(root.name.first(), "operator name")?)?;
        match root.kind {
            AExprKind::OpAll | AExprKind::OpAny => {
                let left = self.transform_opt_boxed(root.lexpr.as_ref())?;
                let right = self.transform_opt_expression(root.rexpr.as_ref())?;
                let mut select = build::select_node();
                select
                    .select_list
                    .push(build::function("UNNEST", right.into_iter().collect()));
                let statement =
                    build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select))));
                let mut comparison_type = operator_to_expression_type(name);
                let mut location = None;
                if let Ok(loc) = u64::try_from(root.location) {
                    location = Some(loc);
                }
                if comparison_type == ExpressionType::Invalid {
                    return Err(Error::parser(format!(
                        "Unsupported comparison \"{name}\" for ANY/ALL subquery"
                    )));
                }
                if root.kind == AExprKind::OpAll {
                    comparison_type = negate_comparison(comparison_type)?;
                }
                let mut subquery = ParsedExpression::new(
                    ExpressionType::Subquery,
                    ParsedExpressionKind::Subquery(SubqueryExpression {
                        subquery_type: SubqueryType::Any,
                        subquery: Box::new(statement),
                        child: left,
                        comparison_type,
                    }),
                );
                subquery.query_location = location;
                if root.kind == AExprKind::OpAll {
                    return Ok(build::operator(ExpressionType::OperatorNot, vec![subquery]));
                }
                Ok(subquery)
            }
            AExprKind::In => self.transform_in_expression(name, root),
            AExprKind::Nullif => {
                let left = self.transform_opt_expression(root.lexpr.as_ref())?;
                let right = self.transform_opt_expression(root.rexpr.as_ref())?;
                Ok(build::function("nullif", left.into_iter().chain(right).collect()))
            }
            AExprKind::Between | AExprKind::NotBetween => {
                let Some(Node::List(args)) = &root.rexpr else {
                    return Err(Error::internal("(NOT) BETWEEN needs two args"));
                };
                let [lower, upper] = args.as_slice() else {
                    return Err(Error::internal("(NOT) BETWEEN needs two args"));
                };
                let input = self.transform_required(root.lexpr.as_ref(), "BETWEEN input")?;
                let lower = self.transform_expression(lower)?;
                let upper = self.transform_expression(upper)?;
                let between = ParsedExpression::new(
                    ExpressionType::CompareBetween,
                    ParsedExpressionKind::Between(BetweenExpression {
                        input: Box::new(input),
                        lower: Box::new(lower),
                        upper: Box::new(upper),
                    }),
                );
                Ok(if root.kind == AExprKind::Between {
                    between
                } else {
                    build::operator(ExpressionType::OperatorNot, vec![between])
                })
            }
            AExprKind::Similar => {
                let left = self.transform_required(root.lexpr.as_ref(), "SIMILAR TO input")?;
                let right = self.transform_required(root.rexpr.as_ref(), "SIMILAR TO pattern")?;
                let ParsedExpressionKind::Function(similar) = right.kind else {
                    return Err(Error::internal("SIMILAR TO pattern is not similar_escape"));
                };
                let mut children = similar.children.into_iter();
                let (Some(pattern), Some(escape)) = (children.next(), children.next()) else {
                    return Err(Error::internal("similar_escape needs two arguments"));
                };
                let ParsedExpressionKind::Constant(constant) = &escape.kind else {
                    return Err(Error::not_implemented("Custom escape in SIMILAR TO"));
                };
                if escape.expression_type != ExpressionType::ValueConstant || !constant.value.is_null() {
                    return Err(Error::not_implemented("Custom escape in SIMILAR TO"));
                }
                let result = build::function("regexp_full_match", vec![left, pattern]);
                Ok(if name == "!~" {
                    build::operator(ExpressionType::OperatorNot, vec![result])
                } else {
                    result
                })
            }
            AExprKind::NotDistinct | AExprKind::Distinct => {
                let left = self.transform_required(root.lexpr.as_ref(), "comparison input")?;
                let right = self.transform_required(root.rexpr.as_ref(), "comparison input")?;
                let comparison_type = if root.kind == AExprKind::NotDistinct {
                    ExpressionType::CompareNotDistinctFrom
                } else {
                    ExpressionType::CompareDistinctFrom
                };
                Ok(build::comparison(comparison_type, left, right))
            }
            _ => {
                let left = self.transform_opt_expression(root.lexpr.as_ref())?;
                let right = self.transform_opt_expression(root.rexpr.as_ref())?;
                match (left, right) {
                    (None, Some(right)) => Ok(Self::transform_unary_operator(name, right)),
                    (Some(left), None) => {
                        Ok(Self::transform_unary_operator(&format!("{name}__postfix"), left))
                    }
                    (Some(left), Some(right)) => Ok(self.transform_binary_operator(name, left, right)),
                    (None, None) => Err(Error::internal("Operator without arguments")),
                }
            }
        }
    }
}
