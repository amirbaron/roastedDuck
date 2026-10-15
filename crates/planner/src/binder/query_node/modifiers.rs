//! DISTINCT, ORDER BY and LIMIT of query nodes (DuckDB's `PrepareModifiers`,
//! `BindModifiers`, `BindLimitValue` and `VisitQueryNode`).

use rustdb_ast::{ExpressionType, ParsedExpression, ParsedExpressionKind, ResultModifier};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::{CastParameters, ValueCast};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::BoundResultModifier;
use crate::binder::expression::{expand_star_expression, push_collation};
use crate::binder::{BindScope, Binder};
use crate::expression::{BoundOrderByNode, BoundOrderModifier, Expression, ExpressionKind};
use crate::expression_binder::{BaseBinder, ExpressionBinder, OrderBinder, SelectBindState};
use crate::generated::enums::DistinctType;
use crate::operator::{BoundLimitNode, LogicalKind, LogicalOperator};
use crate::ColumnBinding;

/// Where ORDER BY terms are matched: the node's own select list (SELECT), or the select
/// lists of the operands' binders (set operations, which cannot add columns).
pub enum OrderTargets<'a> {
    SelectList(&'a mut Vec<ParsedExpression>),
    SetOperation(&'a [Binder]),
}

/// Binds one ORDER BY / DISTINCT ON term (DuckDB's `BindOrderExpression`).
fn bind_order_expression(
    scope: &BindScope<'_>,
    targets: &mut OrderTargets<'_>,
    bind_state: &mut SelectBindState,
    component: &str,
    expr: ParsedExpression,
) -> Result<Option<Expression>> {
    let mut order_binder = match targets {
        OrderTargets::SelectList(list) => OrderBinder::new(vec![&*scope.binder], Some(&mut **list), bind_state, component),
        OrderTargets::SetOperation(binders) => OrderBinder::new(binders.iter().collect(), None, bind_state, component),
    };
    order_binder.bind(expr)
}

/// Expands stars in a modifier term against the operand that defines the columns.
fn expand_order_star(
    scope: &mut BindScope<'_>,
    targets: &OrderTargets<'_>,
    expr: ParsedExpression,
) -> Result<Vec<ParsedExpression>> {
    let mut list = Vec::new();
    match targets {
        OrderTargets::SelectList(_) => expand_star_expression(scope, expr, &mut list)?,
        OrderTargets::SetOperation(binders) => {
            let Some(first) = binders.first() else {
                return Err(Error::internal("set operation without operands"));
            };
            let mut binder = first.clone();
            let mut operand_scope = BindScope::root(&mut binder);
            expand_star_expression(&mut operand_scope, expr, &mut list)?;
        }
    }
    Ok(list)
}

/// DuckDB's `Binder::PrepareModifiers`: binds DISTINCT ON / ORDER BY to select-list
/// positions and LIMIT/OFFSET values; the result types come later in [`bind_modifiers`].
pub fn prepare_modifiers(
    scope: &mut BindScope<'_>,
    modifiers: &mut Vec<ResultModifier>,
    mut targets: OrderTargets<'_>,
    bind_state: &mut SelectBindState,
    column_count: usize,
    bound: &mut Vec<BoundResultModifier>,
) -> Result<()> {
    for modifier in modifiers.drain(..) {
        match modifier {
            ResultModifier::Distinct(mut distinct) => {
                let distinct_type = if distinct.distinct_on_targets.is_empty() {
                    DistinctType::Distinct
                } else {
                    DistinctType::DistinctOn
                };
                if distinct.distinct_on_targets.is_empty() {
                    for i in 0..column_count {
                        distinct
                            .distinct_on_targets
                            .push(ParsedExpression::constant(Value::integer(1 + i as i32)));
                    }
                }
                let mut targets_bound: Vec<Expression> = Vec::new();
                for target in distinct.distinct_on_targets {
                    for expr in expand_order_star(scope, &targets, target)? {
                        let Some(expr) = bind_order_expression(scope, &mut targets, bind_state, "DISTINCT ON", expr)? else {
                            continue;
                        };
                        if !targets_bound.iter().any(|existing| existing.equals(&expr)) {
                            targets_bound.push(expr);
                        }
                    }
                }
                bound.push(BoundResultModifier::Distinct {
                    distinct_type,
                    targets: targets_bound,
                });
            }
            ResultModifier::Order(order) => {
                let config = scope.context.config.clone();
                if let [single] = order.orders.as_slice()
                    && let ParsedExpressionKind::Star(star) = &single.expression.kind
                    && star.exclude_list.is_empty()
                    && star.replace_list.is_empty()
                    && star.expr.is_none()
                {
                    // ORDER BY ALL
                    let order_type = config.resolve_order(single.order_type);
                    let null_order = config.resolve_null_order(order_type, single.null_order);
                    bound.push(BoundResultModifier::Order(BoundOrderModifier {
                        orders: vec![BoundOrderByNode::new(
                            order_type,
                            null_order,
                            Expression::constant(Value::varchar("ALL")),
                        )],
                    }));
                    continue;
                }
                let mut orders = Vec::new();
                for node in order.orders {
                    let order_type = config.resolve_order(node.order_type);
                    let null_order = config.resolve_null_order(order_type, node.null_order);
                    for expr in expand_order_star(scope, &targets, *node.expression)? {
                        if let Some(expr) = bind_order_expression(scope, &mut targets, bind_state, "ORDER BY", expr)? {
                            orders.push(BoundOrderByNode::new(order_type, null_order, expr));
                        }
                    }
                }
                if !orders.is_empty() {
                    bound.push(BoundResultModifier::Order(BoundOrderModifier { orders }));
                }
            }
            ResultModifier::Limit(limit) => {
                let limit_node = match limit.limit {
                    Some(value) => bind_limit_value(scope, &mut targets, bind_state, *value, false, false)?,
                    None => BoundLimitNode::Unset,
                };
                let offset_node = match limit.offset {
                    Some(value) => bind_limit_value(scope, &mut targets, bind_state, *value, false, true)?,
                    None => BoundLimitNode::Unset,
                };
                bound.push(BoundResultModifier::Limit {
                    limit: limit_node,
                    offset: offset_node,
                });
            }
            ResultModifier::LimitPercent(limit) => {
                let limit_node = match limit.limit {
                    Some(value) => bind_limit_value(scope, &mut targets, bind_state, *value, true, false)?,
                    None => BoundLimitNode::Unset,
                };
                let offset_node = match limit.offset {
                    Some(value) => bind_limit_value(scope, &mut targets, bind_state, *value, false, true)?,
                    None => BoundLimitNode::Unset,
                };
                bound.push(BoundResultModifier::Limit {
                    limit: limit_node,
                    offset: offset_node,
                });
            }
        }
    }
    Ok(())
}

/// DuckDB's `Binder::BindLimitValue`.
fn bind_limit_value(
    scope: &mut BindScope<'_>,
    targets: &mut OrderTargets<'_>,
    bind_state: &mut SelectBindState,
    mut value: ParsedExpression,
    is_percentage: bool,
    is_offset: bool,
) -> Result<BoundLimitNode> {
    let target_type = if is_percentage { LogicalType::DOUBLE } else { LogicalType::BIGINT };
    let original = value.clone();
    let mut new_binder = scope.binder.child()?;
    let expr = {
        let mut child_scope = BindScope {
            binder: &mut new_binder,
            outer: Some(scope),
        };
        let mut binder = { let (inner, outer) = child_scope.split(); BaseBinder::new(inner, outer) };
        binder.state().target_type = Some(target_type.clone());
        binder.bind(&mut value, true)?
    };
    if expr.has_subquery() {
        let OrderTargets::SelectList(_) = targets else {
            return Err(Error::binder("Subquery in LIMIT/OFFSET not supported in set operation"));
        };
        let mut order_binder = match targets {
            OrderTargets::SelectList(list) => OrderBinder::new(vec![&*scope.binder], Some(&mut **list), bind_state, "LIMIT"),
            OrderTargets::SetOperation(_) => return Err(Error::internal("set operation limit")),
        };
        let reference = order_binder.create_extra_reference(original)?;
        return Ok(if is_percentage {
            BoundLimitNode::ExpressionPercentage(Box::new(reference))
        } else {
            BoundLimitNode::ExpressionValue(Box::new(reference))
        });
    }
    if expr.is_foldable() {
        let value = scope.context.evaluator.evaluate_scalar(&expr)?;
        let value = value.cast_with(&scope.context.casts, &target_type, &mut CastParameters::new())?;
        if is_percentage {
            let percentage = if value.is_null() { 100.0 } else { value.as_f64().unwrap_or_default() };
            if percentage.is_nan() || !(0.0..=100.0).contains(&percentage) {
                return Err(Error::out_of_range("Limit percent out of range, should be between 0% and 100%"));
            }
            return Ok(BoundLimitNode::ConstantPercentage(percentage));
        }
        let constant = if value.is_null() {
            if is_offset { 0 } else { i64::MAX }
        } else {
            value.as_i128().unwrap_or_default() as i64
        };
        if constant < 0 {
            return Err(Error::binder("LIMIT/OFFSET cannot be negative").with_position(expr.query_location));
        }
        return Ok(BoundLimitNode::ConstantValue(constant as u64));
    }
    if !new_binder.correlated_columns.is_empty() {
        return Err(Error::binder("Correlated columns not supported in LIMIT/OFFSET"));
    }
    scope.binder.move_correlated_expressions(&mut new_binder);
    Ok(if is_percentage {
        BoundLimitNode::ExpressionPercentage(Box::new(expr))
    } else {
        BoundLimitNode::ExpressionValue(Box::new(expr))
    })
}

/// A reference to result column `index` (DuckDB's `CreateOrderExpression`).
fn create_order_expression(
    expr: &Expression,
    names: &[String],
    types: &[LogicalType],
    table_index: u64,
    index: usize,
) -> Result<Expression> {
    let Some(ty) = types.get(index) else {
        return Err(Error::binder(format!(
            "ORDER term out of range - should be between 1 and {}",
            types.len()
        ))
        .with_position(expr.query_location));
    };
    let mut alias = expr.alias.clone();
    if alias.is_empty() {
        alias = names.get(index).cloned().unwrap_or_default();
    }
    Ok(Expression::column_ref(alias, ty.clone(), ColumnBinding::new(table_index, index as u64), 0))
}

/// Turns a position constant into a column reference (DuckDB's
/// `FinalizeBindOrderExpression`); `None` for ORDER BY ALL.
fn finalize_order_expression(
    expr: Expression,
    table_index: u64,
    names: &[String],
    types: &[LogicalType],
    bind_state: &SelectBindState,
) -> Result<Option<Expression>> {
    let Some(value) = expr.as_constant() else {
        return Err(Error::internal("Unknown type in FinalizeBindOrderExpression"));
    };
    match value.logical_type().id() {
        LogicalTypeId::UBigInt => {
            let index = value.as_i128().unwrap_or_default() as usize;
            create_order_expression(&expr, names, types, table_index, bind_state.final_index(index)).map(Some)
        }
        LogicalTypeId::Varchar => Ok(None),
        LogicalTypeId::Struct => {
            let children = value.children().unwrap_or_default();
            let index = children.first().and_then(Value::as_i128).unwrap_or_default() as usize;
            let mut result = create_order_expression(&expr, names, types, table_index, index)?;
            if let Some(collation) = children.get(1) {
                let collation = collation.to_varchar()?;
                if !collation.is_empty() {
                    if types[index].id() != LogicalTypeId::Varchar {
                        return Err(Error::binder("COLLATE can only be applied to varchar columns")
                            .with_position(result.query_location));
                    }
                    result.return_type = LogicalType::with_info(
                        LogicalTypeId::Varchar,
                        rustdb_types::TypeInfo::String(rustdb_types::StringTypeInfo { collation }),
                    );
                }
            }
            Ok(Some(result))
        }
        _ => Err(Error::internal("Unknown type in FinalizeBindOrderExpression")),
    }
}

/// Gives modifier terms their result types (DuckDB's `Binder::BindModifiers`).
pub fn bind_modifiers(
    modifiers: &mut [BoundResultModifier],
    table_index: u64,
    names: &[String],
    types: &[LogicalType],
    bind_state: &SelectBindState,
) -> Result<()> {
    for modifier in modifiers {
        match modifier {
            BoundResultModifier::Distinct { targets, .. } => {
                let mut finalized = Vec::with_capacity(targets.len());
                for target in targets.drain(..) {
                    let target = finalize_order_expression(target, table_index, names, types, bind_state)?
                        .ok_or_else(|| Error::internal("DISTINCT ON ORDER BY ALL not supported"))?;
                    let ty = target.return_type.clone();
                    finalized.push(push_collation(target, &ty)?);
                }
                *targets = finalized;
            }
            BoundResultModifier::Limit { limit, offset } => {
                for node in [limit, offset] {
                    if let Some(expr) = node.expression_mut() {
                        if expr.expression_type == ExpressionType::ValueConstant {
                            if let Some(finalized) =
                                finalize_order_expression(expr.clone(), table_index, names, types, bind_state)?
                            {
                                *expr = finalized;
                            }
                        }
                        if let ExpressionKind::ColumnRef(column) = &expr.kind
                            && let Some(ty) = types.get(column.binding.column_index as usize)
                        {
                            expr.return_type = ty.clone();
                        }
                    }
                }
            }
            BoundResultModifier::Order(order) => {
                let mut order_by_all = false;
                let mut finalized = Vec::with_capacity(order.orders.len());
                for node in order.orders.drain(..) {
                    match finalize_order_expression(node.expression, table_index, names, types, bind_state)? {
                        Some(expression) => finalized.push(BoundOrderByNode { expression, ..node }),
                        None => {
                            order_by_all = true;
                            finalized.push(BoundOrderByNode {
                                expression: Expression::constant(Value::null(LogicalType::SQLNULL)),
                                ..node
                            });
                        }
                    }
                }
                if order_by_all {
                    let (order_type, null_order) = (finalized[0].order_type, finalized[0].null_order);
                    finalized = types
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| {
                            let alias = names.get(i).cloned().unwrap_or_default();
                            BoundOrderByNode::new(
                                order_type,
                                null_order,
                                Expression::column_ref(alias, ty.clone(), ColumnBinding::new(table_index, i as u64), 0),
                            )
                        })
                        .collect();
                }
                for node in &mut finalized {
                    let ty = node.expression.return_type.clone();
                    let expression = std::mem::replace(&mut node.expression, Expression::constant(Value::default()));
                    node.expression = push_collation(expression, &ty)?;
                }
                order.orders = finalized;
            }
        }
    }
    Ok(())
}

/// Plans the modifiers on top of `root` (DuckDB's `Binder::VisitQueryNode`).
pub fn visit_query_node(modifiers: Vec<BoundResultModifier>, mut root: LogicalOperator) -> LogicalOperator {
    for modifier in modifiers {
        match modifier {
            BoundResultModifier::Distinct { distinct_type, targets } => {
                if targets.is_empty() {
                    continue;
                }
                root = LogicalOperator::new(LogicalKind::Distinct {
                    distinct_type,
                    distinct_targets: targets,
                    order_by: None,
                })
                .with_children(vec![root]);
            }
            BoundResultModifier::Order(order) => {
                if let LogicalKind::Distinct {
                    distinct_type: DistinctType::DistinctOn,
                    order_by,
                    ..
                } = &mut root.kind
                {
                    *order_by = Some(order.clone());
                }
                root = LogicalOperator::new(LogicalKind::Order {
                    orders: order.orders,
                    projection_map: Vec::new(),
                })
                .with_children(vec![root]);
            }
            BoundResultModifier::Limit { limit, offset } => {
                root = LogicalOperator::new(LogicalKind::Limit { limit, offset }).with_children(vec![root]);
            }
        }
    }
    root
}

/// "Parameter not supported" for parameters in ORDER BY (DuckDB's `ParameterNotAllowedException`).
pub fn parameter_not_allowed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::ParameterNotAllowed, message.into())
}
