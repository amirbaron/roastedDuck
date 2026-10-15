//! Binding of each expression class (DuckDB's `planner/binder/expression/bind_*.cpp`):
//! the base behaviour of [`ExpressionBinder`], written as functions over any binder so
//! that specialised binders can fall back to it.

mod bind_function;
mod bind_star;
mod bind_subquery;

use rustdb_ast::{ExpressionType, ParsedExpression, ParsedExpressionKind};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::expression::{
    BoundBetweenExpression, BoundCaseCheck, BoundCaseExpression, Expression, ExpressionKind,
};
use crate::expression_binder::{
    BindError, BindResult, BoundColumnReferenceInfo, ExpressionBinder, for_each_child, is_potential_alias,
    is_unnest_function, parameter_not_resolved, take_bound, unsupported,
};
use crate::function_binder::{add_cast_to_type, try_bind_comparison};

pub use bind_function::bind_function_expression;
pub use bind_star::expand_star_expression;
pub use bind_subquery::bind_subquery_expression;

/// Binds every child of `expr` and reports the first error (DuckDB's
/// `BindUnsupportedExpression`), or else `message` at `expr`'s location.
pub fn bind_unsupported_expression<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
    message: &str,
) -> Result<BindResult> {
    let mut error = None;
    let location = expr.query_location;
    for_each_child(expr, &mut |child| binder.bind_child(child, depth, &mut error))?;
    Ok(BindResult::Unbound(error.unwrap_or_else(|| {
        Error::binder(message.to_owned()).with_position(location).into()
    })))
}

/// The class dispatch of the base expression binder (DuckDB's
/// `ExpressionBinder::BindExpression(unique_ptr<ParsedExpression> &, ...)`).
pub fn bind_expression_base<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
    root_expression: bool,
) -> Result<BindResult> {
    // DuckDB's `StackCheck`.
    let max_depth = binder.binder_ref().context.config.max_expression_depth;
    if binder.state_ref().stack_depth + 1 >= max_depth {
        return Err(Error::binder(format!(
            "Max expression depth limit of {max_depth} exceeded. Use \"SET max_expression_depth TO x\" to \
             increase the maximum expression depth."
        )));
    }
    binder.state().stack_depth += 1;
    let result = bind_expression_kind(binder, expr, depth, root_expression);
    binder.state().stack_depth -= 1;
    result
}

/// The dispatch of [`bind_expression_base`] on the expression class.
fn bind_expression_kind<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
    root_expression: bool,
) -> Result<BindResult> {
    match &expr.kind {
        ParsedExpressionKind::Between(_) => bind_between(binder, expr, depth),
        ParsedExpressionKind::Case(_) => bind_case(binder, expr, depth),
        ParsedExpressionKind::Cast(_) => bind_cast(binder, expr, depth),
        ParsedExpressionKind::Collate(_) => bind_collate(binder, expr, depth),
        ParsedExpressionKind::ColumnRef(_) => bind_column_ref(binder, expr, depth, root_expression),
        ParsedExpressionKind::LambdaRef(_) => bind_lambda_ref(binder, expr, depth),
        ParsedExpressionKind::Comparison(_) => bind_comparison(binder, expr, depth),
        ParsedExpressionKind::Conjunction(_) => bind_conjunction(binder, expr, depth),
        ParsedExpressionKind::Constant(constant) => Ok(Expression::constant(constant.value.clone()).into()),
        ParsedExpressionKind::Type(_) => Err(Error::not_implemented("binding of type expressions")),
        ParsedExpressionKind::Function(function) => {
            if is_unnest_function(&function.function_name) {
                binder.bind_unnest(expr, depth, root_expression)
            } else {
                bind_function_expression(binder, expr, depth)
            }
        }
        ParsedExpressionKind::Lambda(_) => Err(Error::not_implemented("lambda functions")),
        ParsedExpressionKind::Operator(_) => bind_operator(binder, expr, depth),
        ParsedExpressionKind::Subquery(_) => bind_subquery_expression(binder, expr, depth),
        ParsedExpressionKind::Parameter(_) => bind_parameter(binder, expr),
        ParsedExpressionKind::PositionalReference(_) => {
            bind_positional_reference(binder, expr, depth, root_expression)
        }
        ParsedExpressionKind::Star(_) => Ok(BindResult::Unbound(unsupported(
            expr,
            "STAR expression is not supported here",
        ))),
        ParsedExpressionKind::Window(_) => binder.bind_window(expr, depth),
        ParsedExpressionKind::Bound(bound) if bound.payload.is::<bind_subquery::PendingSubquery>() => {
            bind_subquery_expression(binder, expr, depth)
        }
        ParsedExpressionKind::Default | ParsedExpressionKind::Bound(_) => {
            Err(Error::not_implemented(format!(
                "Unimplemented expression class in ExpressionBinder::BindExpression: {}",
                expr.expression_class().name()
            )))
        }
    }
}

/// Binds the children named by `select`, in order, keeping the first error.
fn bind_children<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    children: Vec<&mut ParsedExpression>,
    depth: u64,
) -> Result<Option<BindError>> {
    let mut error = None;
    for child in children {
        binder.bind_child(child, depth, &mut error)?;
    }
    Ok(error)
}

/// DuckDB's `BindExpression(ColumnRefExpression &)`.
pub fn bind_column_ref<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
    root_expression: bool,
) -> Result<BindResult> {
    let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
        return Err(Error::internal("expected a column reference"));
    };
    let names = column.column_names.clone();
    let location = expr.query_location;
    let qualified = binder.qualify_column_name(&names, location)?;
    let mut qualified = match qualified {
        Ok(qualified) => qualified,
        Err(error) => {
            if is_potential_alias(&names) {
                if let Some(result) = binder.try_resolve_alias_reference(expr, depth, root_expression)? {
                    return Ok(result);
                }
                if let Some(mut function) = names.last().and_then(|name| binder.sql_value_function(name)) {
                    return binder.bind_expression(&mut function, depth, false);
                }
            }
            return Ok(BindResult::Unbound(error.with_position(location)));
        }
    };
    qualified.query_location = location;
    let ParsedExpressionKind::ColumnRef(qualified_column) = &qualified.kind else {
        // A struct extract, struct pack, lambda reference or generated column.
        let alias = qualified.alias.clone();
        let result = binder.bind_expression(&mut qualified, depth, false)?;
        return Ok(match result {
            BindResult::Bound(bound) => BindResult::Bound(bound.with_alias(alias)),
            unbound @ BindResult::Unbound(_) => unbound,
        });
    };
    let qualified_names = qualified_column.column_names.clone();
    let column_name = qualified_names.last().cloned().unwrap_or_default();
    let table_name = qualified_names
        .get(qualified_names.len().saturating_sub(2))
        .cloned()
        .unwrap_or_default();
    let alias = qualified.alias.clone();
    let inner = binder.binder();
    let result = match &mut inner.macro_binding {
        Some(macro_binding) if macro_binding.alias.alias == table_name => macro_binding
            .bind(&column_name, &alias, depth)
            .map_err(BindError::from),
        _ => inner
            .bind_context
            .bind_column(qualified_column, &alias, depth)?
            .map_err(BindError::from),
    };
    match result {
        Ok(bound) => {
            binder.state().bound_columns.push(BoundColumnReferenceInfo {
                name: column_name,
                query_location: location,
            });
            Ok(bound.into())
        }
        Err(error) => Ok(BindResult::Unbound(error.with_position(location))),
    }
}

/// DuckDB's `BindExpression(LambdaRefExpression &)`.
fn bind_lambda_ref<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let ParsedExpressionKind::LambdaRef(lambda) = &expr.kind else {
        return Err(Error::internal("expected a lambda reference"));
    };
    let index = lambda.lambda_idx;
    let name = lambda.column_name.clone();
    let bindings = &mut binder.state().lambda_bindings;
    let Some(binding) = bindings.get_mut(index as usize) else {
        return Err(Error::internal("lambda reference out of range"));
    };
    let bound = binding.bind(&name, "", depth)?;
    let column = bound
        .as_column_ref()
        .copied()
        .ok_or_else(|| Error::internal("lambda binding"))?;
    Ok(Expression::new(
        ExpressionType::BoundLambdaRef,
        bound.return_type.clone(),
        ExpressionKind::LambdaRef(crate::expression::BoundLambdaRefExpression {
            binding: column.binding,
            lambda_index: index,
            depth,
        }),
    )
    .with_alias(name)
    .into())
}

/// DuckDB's `BindExpression(ComparisonExpression &)`.
fn bind_comparison<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let comparison_type = expr.expression_type;
    let location = expr.query_location;
    let ParsedExpressionKind::Comparison(comparison) = &mut expr.kind else {
        return Err(Error::internal("expected a comparison"));
    };
    if let Some(error) = bind_children(binder, vec![&mut comparison.left, &mut comparison.right], depth)? {
        return Ok(BindResult::Unbound(error));
    }
    let left = take_bound(&mut comparison.left)?;
    let right = take_bound(&mut comparison.right)?;
    let left_type = crate::expression_binder::expression_return_type(&left);
    let right_type = crate::expression_binder::expression_return_type(&right);
    let Some(input_type) = try_bind_comparison(&left_type, &right_type, comparison_type)? else {
        return Ok(BindResult::Unbound(
            Error::binder(format!(
                "Cannot compare values of type {left_type} and type {right_type} - an explicit cast is required"
            ))
            .with_position(location)
            .into(),
        ));
    };
    let context = binder.binder_ref().context.clone();
    let try_cast = input_type.id() == LogicalTypeId::Enum;
    let left = add_cast_to_type(&context, left, &input_type, try_cast)?;
    let right = add_cast_to_type(&context, right, &input_type, try_cast)?;
    let left = push_collation(left, &input_type)?;
    let right = push_collation(right, &input_type)?;
    Ok(Expression::comparison(comparison_type, left, right).into())
}

/// Applies a VARCHAR collation to an expression (DuckDB's `ExpressionBinder::PushCollation`).
/// Collations are not ported yet; without one this is the identity.
pub fn push_collation(expression: Expression, ty: &LogicalType) -> Result<Expression> {
    match crate::expression_binder::collation(ty) {
        Some(collation) if ty.id() == LogicalTypeId::Varchar && !collation.eq_ignore_ascii_case("binary") => {
            Err(Error::not_implemented(format!("collation \"{collation}\"")))
        }
        _ => Ok(expression),
    }
}

/// DuckDB's `BindExpression(ConjunctionExpression &)`.
fn bind_conjunction<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let conjunction_type = expr.expression_type;
    let ParsedExpressionKind::Conjunction(conjunction) = &mut expr.kind else {
        return Err(Error::internal("expected a conjunction"));
    };
    if let Some(error) = bind_children(binder, conjunction.children.iter_mut().collect(), depth)? {
        return Ok(BindResult::Unbound(error));
    }
    let context = binder.binder_ref().context.clone();
    let mut children = Vec::with_capacity(conjunction.children.len());
    for child in &mut conjunction.children {
        children.push(add_cast_to_type(
            &context,
            take_bound(child)?,
            &LogicalType::BOOLEAN,
            false,
        )?);
    }
    Ok(Expression::conjunction(conjunction_type, children).into())
}

/// DuckDB's `BindExpression(CastExpression &)`.
fn bind_cast<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let ParsedExpressionKind::Cast(cast) = &mut expr.kind else {
        return Err(Error::internal("expected a cast"));
    };
    if let Some(error) = binder.bind_in_place(&mut cast.child, depth, false)? {
        return Ok(BindResult::Unbound(error));
    }
    let target = bind_logical_type(&cast.cast_type)?;
    let child = take_bound(&mut cast.child)?;
    let context = binder.binder_ref().context.clone();
    if cast.try_cast && crate::expression_binder::expression_return_type(&child) == target {
        return Ok(child.into());
    }
    Ok(add_cast_to_type(&context, child, &target, cast.try_cast)?.into())
}

/// Resolves a type written in SQL (DuckDB's `Binder::BindLogicalType`): user types and
/// unbound type names are not ported yet.
pub fn bind_logical_type(ty: &LogicalType) -> Result<LogicalType> {
    match (ty.id(), ty.info()) {
        (LogicalTypeId::Unbound, Some(rustdb_types::TypeInfo::Unbound(info))) => {
            let bound = info.default_bind()?;
            if bound.id() == LogicalTypeId::Unbound {
                return Err(Error::new(
                    ErrorKind::Catalog,
                    format!("Type with name {} does not exist!", info.name),
                ));
            }
            bind_logical_type(&bound)
        }
        (LogicalTypeId::List, _) => match ty.child_type() {
            Some(child) => Ok(LogicalType::list(bind_logical_type(child)?)),
            None => Ok(ty.clone()),
        },
        (LogicalTypeId::Array, _) => match ty.array_child() {
            Some((child, size)) => Ok(LogicalType::array(bind_logical_type(child)?, size)),
            None => Ok(ty.clone()),
        },
        (LogicalTypeId::Struct, _) => {
            let children = ty
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| Ok((name.clone(), bind_logical_type(child)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok(LogicalType::struct_type(children))
        }
        (LogicalTypeId::Map, _) => match ty.map_key_value() {
            Some((key, value)) => Ok(LogicalType::map(
                bind_logical_type(key)?,
                bind_logical_type(value)?,
            )),
            None => Ok(ty.clone()),
        },
        _ => Ok(ty.clone()),
    }
}

/// DuckDB's `BindExpression(CaseExpression &)`.
fn bind_case<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let location = expr.query_location;
    let ParsedExpressionKind::Case(case) = &mut expr.kind else {
        return Err(Error::internal("expected a CASE"));
    };
    let mut error = None;
    for check in &mut case.case_checks {
        binder.bind_child(&mut check.when_expr, depth, &mut error)?;
        binder.bind_child(&mut check.then_expr, depth, &mut error)?;
    }
    binder.bind_child(&mut case.else_expr, depth, &mut error)?;
    if let Some(error) = error {
        return Ok(BindResult::Unbound(error));
    }
    let else_expr = take_bound(&mut case.else_expr)?;
    let mut checks = Vec::with_capacity(case.case_checks.len());
    for check in &mut case.case_checks {
        checks.push((
            take_bound(&mut check.when_expr)?,
            take_bound(&mut check.then_expr)?,
        ));
    }
    let mut return_type = crate::expression_binder::expression_return_type(&else_expr);
    for (_, then) in &checks {
        let then_type = crate::expression_binder::expression_return_type(then);
        return_type = LogicalType::try_max_logical_type(&return_type, &then_type).ok_or_else(|| {
            Error::binder(format!(
                "Cannot mix values of type {return_type} and {then_type} in CASE expression - an explicit cast is \
                 required"
            ))
            .with_position(location)
        })?;
    }
    let context = binder.binder_ref().context.clone();
    let mut case_checks = Vec::with_capacity(checks.len());
    for (when, then) in checks {
        case_checks.push(BoundCaseCheck {
            when_expr: add_cast_to_type(&context, when, &LogicalType::BOOLEAN, false)?,
            then_expr: add_cast_to_type(&context, then, &return_type, false)?,
        });
    }
    let else_expr = add_cast_to_type(&context, else_expr, &return_type, false)?;
    Ok(Expression::new(
        ExpressionType::CaseExpr,
        return_type,
        ExpressionKind::Case(Box::new(BoundCaseExpression {
            case_checks,
            else_expr,
        })),
    )
    .into())
}

/// DuckDB's `BindExpression(BetweenExpression &)`.
fn bind_between<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let between_type = expr.expression_type;
    let location = expr.query_location;
    let ParsedExpressionKind::Between(between) = &mut expr.kind else {
        return Err(Error::internal("expected BETWEEN"));
    };
    let children = vec![&mut *between.input, &mut *between.lower, &mut *between.upper];
    if let Some(error) = bind_children(binder, children, depth)? {
        return Ok(BindResult::Unbound(error));
    }
    let input = take_bound(&mut between.input)?;
    let lower = take_bound(&mut between.lower)?;
    let upper = take_bound(&mut between.upper)?;
    let input_type = crate::expression_binder::expression_return_type(&input);
    let lower_type = crate::expression_binder::expression_return_type(&lower);
    let upper_type = crate::expression_binder::expression_return_type(&upper);
    let mismatch = |left: &LogicalType, right: &LogicalType| {
        Error::binder(format!(
            "Cannot mix values of type {left} and {right} in BETWEEN clause - an explicit cast is required"
        ))
        .with_position(location)
    };
    let common = try_bind_comparison(&input_type, &lower_type, between_type)?
        .ok_or_else(|| mismatch(&input_type, &lower_type))?;
    let common = try_bind_comparison(&common, &upper_type, between_type)?
        .ok_or_else(|| mismatch(&common, &upper_type))?;
    let context = binder.binder_ref().context.clone();
    let input = push_collation(add_cast_to_type(&context, input, &common, false)?, &common)?;
    let lower = push_collation(add_cast_to_type(&context, lower, &common, false)?, &common)?;
    let upper = push_collation(add_cast_to_type(&context, upper, &common, false)?, &common)?;
    if !input.is_volatile() && !input.has_parameter() && !input.has_subquery() {
        // Two comparisons are easier on the optimizer; it folds them back into a BETWEEN.
        let left = Expression::comparison(ExpressionType::CompareGreaterthanorequalto, input.clone(), lower);
        let right = Expression::comparison(ExpressionType::CompareLessthanorequalto, input, upper);
        return Ok(Expression::conjunction(ExpressionType::ConjunctionAnd, vec![left, right]).into());
    }
    Ok(Expression::new(
        ExpressionType::CompareBetween,
        LogicalType::BOOLEAN,
        ExpressionKind::Between(Box::new(BoundBetweenExpression {
            input,
            lower,
            upper,
            lower_inclusive: true,
            upper_inclusive: true,
        })),
    )
    .into())
}

/// DuckDB's `BindExpression(CollateExpression &)`.
fn bind_collate<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let ParsedExpressionKind::Collate(collate) = &mut expr.kind else {
        return Err(Error::internal("expected COLLATE"));
    };
    if let Some(error) = binder.bind_in_place(&mut collate.child, depth, false)? {
        return Ok(BindResult::Unbound(error));
    }
    let mut child = take_bound(&mut collate.child)?;
    if child.has_parameter() {
        return Err(parameter_not_resolved());
    }
    if child.return_type.id() != LogicalTypeId::Varchar {
        return Err(Error::binder("collations are only supported for type varchar")
            .with_position(child.query_location));
    }
    let collation_type = LogicalType::with_info(
        LogicalTypeId::Varchar,
        rustdb_types::TypeInfo::String(rustdb_types::StringTypeInfo {
            collation: collate.collation.clone(),
        }),
    );
    // Validates the collation.
    push_collation(
        Expression::constant(rustdb_types::Value::null(LogicalType::VARCHAR)),
        &collation_type,
    )?;
    child.return_type = collation_type;
    Ok(child.into())
}

/// DuckDB's `BindExpression(OperatorExpression &)`.
fn bind_operator<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let operator_type = expr.expression_type;
    if operator_type == ExpressionType::GroupingFunction {
        return binder.bind_grouping_function(expr, depth);
    }
    let location = expr.query_location;
    let ParsedExpressionKind::Operator(operator) = &mut expr.kind else {
        return Err(Error::internal("expected an operator"));
    };
    if let Some(error) = bind_children(binder, operator.children.iter_mut().collect(), depth)? {
        return Ok(BindResult::Unbound(error));
    }
    let function_name = match operator_type {
        ExpressionType::OperatorUnpack => {
            return Ok(BindResult::error(
                "UNPACK not allowed here, should have been resolved earlier",
            ));
        }
        ExpressionType::ArrayExtract => {
            let base_type = crate::expression_binder::peek_bound(&operator.children[0])
                .map_or(LogicalTypeId::Invalid, |e| e.return_type.id());
            Some(if base_type == LogicalTypeId::Map {
                "map_extract_value"
            } else {
                "array_extract"
            })
        }
        ExpressionType::ArraySlice => Some("array_slice"),
        ExpressionType::StructExtract => {
            let (Some(base), Some(name)) = (
                operator
                    .children
                    .first()
                    .and_then(crate::expression_binder::peek_bound),
                operator
                    .children
                    .get(1)
                    .and_then(crate::expression_binder::peek_bound),
            ) else {
                return Err(Error::internal("struct extract without two children"));
            };
            if base.has_parameter() || base.return_type.id() == LogicalTypeId::Unknown {
                return Err(parameter_not_resolved());
            }
            match base.return_type.id() {
                LogicalTypeId::Union => Some("union_extract"),
                LogicalTypeId::Map => Some("map_extract_value"),
                LogicalTypeId::Variant => Some("variant_extract"),
                LogicalTypeId::Struct | LogicalTypeId::SqlNull => Some("struct_extract"),
                _ => {
                    return Ok(BindResult::error(format!(
                        "Cannot extract field {name} from expression \"{base}\" because it is not a struct, union, \
                         map, or json"
                    )));
                }
            }
        }
        ExpressionType::ArrayConstructor => Some("list_value"),
        ExpressionType::Arrow => Some("json_extract"),
        ExpressionType::OperatorTry => {
            if let Some(child) = operator
                .children
                .first()
                .and_then(crate::expression_binder::peek_bound)
            {
                if child.has_subquery() {
                    return Err(Error::binder(
                        "TRY can not be used in combination with a scalar subquery",
                    ));
                }
                if child.is_volatile() {
                    return Err(Error::binder(
                        "TRY can not be used in combination with a volatile function",
                    ));
                }
            }
            None
        }
        _ => None,
    };
    if let Some(name) = function_name {
        let children = std::mem::take(&mut operator.children);
        let mut function = crate::expression_binder::function_call(name, children);
        function.query_location = location;
        return binder.bind_expression(&mut function, depth, false);
    }
    let mut children = Vec::with_capacity(operator.children.len());
    for child in &mut operator.children {
        children.push(take_bound(child)?);
    }
    let context = binder.binder_ref().context.clone();
    let result_type = match operator_type {
        ExpressionType::OperatorIsNull | ExpressionType::OperatorIsNotNull => {
            if matches!(
                children[0].return_type.id(),
                LogicalTypeId::Invalid | LogicalTypeId::Unknown
            ) {
                return Err(parameter_not_resolved());
            }
            LogicalType::BOOLEAN
        }
        ExpressionType::CompareIn | ExpressionType::CompareNotIn => {
            resolve_coalesce_type(&context, operator_type, &mut children, location)?;
            LogicalType::BOOLEAN
        }
        ExpressionType::OperatorCoalesce => {
            let result = resolve_coalesce_type(&context, operator_type, &mut children, location)?;
            if children.len() == 1 {
                return Ok(children.remove(0).into());
            }
            result
        }
        ExpressionType::OperatorTry => children[0].return_type.clone(),
        ExpressionType::OperatorNot => {
            let child = children.remove(0);
            children.insert(
                0,
                add_cast_to_type(&context, child, &LogicalType::BOOLEAN, false)?,
            );
            LogicalType::BOOLEAN
        }
        _ => {
            return Err(Error::internal(
                "Unrecognized expression type for ResolveOperatorType",
            ));
        }
    };
    Ok(Expression::operator(operator_type, result_type, children).into())
}

/// The common type of IN lists and COALESCE arguments, with casts added
/// (DuckDB's `ResolveCoalesceType`).
fn resolve_coalesce_type(
    context: &crate::context::ClientContext,
    operator_type: ExpressionType,
    children: &mut Vec<Expression>,
    location: Option<u64>,
) -> Result<LogicalType> {
    let Some(first) = children.first() else {
        return Err(Error::internal("IN requires at least a single child node"));
    };
    let is_in = matches!(
        operator_type,
        ExpressionType::CompareIn | ExpressionType::CompareNotIn
    );
    let mut max_type = crate::expression_binder::expression_return_type(first);
    for child in children.iter().skip(1) {
        let child_type = crate::expression_binder::expression_return_type(child);
        let next = if is_in {
            try_bind_comparison(&max_type, &child_type, operator_type)?
        } else {
            LogicalType::try_max_logical_type(&max_type, &child_type)
        };
        max_type = next.ok_or_else(|| {
            let message = if is_in {
                format!(
                    "Cannot mix values of type {max_type} and {child_type} in {} clause - an explicit cast is \
                     required",
                    if operator_type == ExpressionType::CompareIn { "IN" } else { "NOT IN" }
                )
            } else {
                format!(
                    "Cannot mix values of type {max_type} and {child_type} in COALESCE operator - an explicit cast is \
                     required"
                )
            };
            Error::binder(message).with_position(location)
        })?;
    }
    let mut cast_children = Vec::with_capacity(children.len());
    for child in children.drain(..) {
        let child = add_cast_to_type(context, child, &max_type, false)?;
        cast_children.push(if is_in {
            push_collation(child, &max_type)?
        } else {
            child
        });
    }
    *children = cast_children;
    Ok(max_type)
}

/// DuckDB's `BindExpression(ParameterExpression &)`.
fn bind_parameter<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
) -> Result<BindResult> {
    let ParsedExpressionKind::Parameter(parameter) = &expr.kind else {
        return Err(Error::internal("expected a parameter"));
    };
    let identifier = parameter.identifier.clone();
    let alias = expr.alias.clone();
    let inner = binder.binder_ref();
    let supplied = inner.with_parameters(|parameters| {
        parameters
            .parameter_data
            .get(&identifier)
            .map(|data| (data.value.clone(), parameters.return_type(&identifier)))
    });
    let Some(supplied) = supplied else {
        return Err(Error::binder(
            "Unexpected prepared parameter. This type of statement can't be prepared!",
        ));
    };
    if let Some((value, return_type)) = supplied {
        let constant = Expression::constant(value).with_alias(alias);
        if matches!(
            return_type.id(),
            LogicalTypeId::IntegerLiteral | LogicalTypeId::StringLiteral
        ) {
            return Ok(constant.into());
        }
        let context = inner.context.clone();
        return Ok(add_cast_to_type(&context, constant, &return_type, false)?.into());
    }
    let bound = inner
        .with_parameters(|parameters| parameters.bind_parameter_expression(&identifier, &alias))
        .ok_or_else(|| Error::internal("parameters disappeared"))?;
    Ok(bound.into())
}

/// DuckDB's `BindPositionalReference`.
fn bind_positional_reference<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
    root_expression: bool,
) -> Result<BindResult> {
    let ParsedExpressionKind::PositionalReference(reference) = &expr.kind else {
        return Err(Error::internal("expected a positional reference"));
    };
    if depth != 0 {
        return Err(Error::internal(
            "Positional reference expression could not be bound",
        ));
    }
    let mut column = binder.binder_ref().bind_context.position_to_column(reference)?;
    column.alias = std::mem::take(&mut expr.alias);
    column.query_location = expr.query_location;
    *expr = column;
    binder.bind_expression(expr, depth, root_expression)
}
