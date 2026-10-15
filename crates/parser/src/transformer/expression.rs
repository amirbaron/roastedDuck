//! Expressions (`transform/expression/*.cpp`): the dispatch and the small transforms.

use rustdb_ast::{
    CaseCheck, CollateExpression, ExpressionType, LambdaExpression, LambdaSyntaxType, ParameterExpression,
    ParsedExpression, ParsedExpressionKind,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{
    AIndirection, BoolExpr, BoolExprType, BoolTestType, BooleanTest, CaseExpr, CaseWhen, CoalesceExpr,
    CollateClause, GroupingFunc, IntervalConstant, LambdaFunction, List, MultiAssignRef, NamedArgExpr, Node,
    NullTest, NullTestType, ParamRef, PositionalReference, ResTarget, SingleArrowFunction, TypeCast, Value,
    interval_field, interval_mask,
};
use rustdb_types::{LogicalType, Value as DuckValue};

use super::build::{self, set_location};
use super::{ParamType, Transformer, cast_node, node_string, node_tag, required};

/// `NegateComparisonExpression`.
pub(crate) fn negate_comparison(expression_type: ExpressionType) -> Result<ExpressionType> {
    use ExpressionType as E;
    Ok(match expression_type {
        E::CompareEqual => E::CompareNotequal,
        E::CompareNotequal => E::CompareEqual,
        E::CompareLessthan => E::CompareGreaterthanorequalto,
        E::CompareGreaterthan => E::CompareLessthanorequalto,
        E::CompareLessthanorequalto => E::CompareGreaterthan,
        E::CompareGreaterthanorequalto => E::CompareLessthan,
        E::CompareDistinctFrom => E::CompareNotDistinctFrom,
        E::CompareNotDistinctFrom => E::CompareDistinctFrom,
        _ => return Err(Error::internal("Unsupported comparison type in negation")),
    })
}

/// `OperatorToExpressionType`: the comparison an operator stands for, if any.
pub(crate) fn operator_to_expression_type(op: &str) -> ExpressionType {
    match op {
        "=" | "==" => ExpressionType::CompareEqual,
        "!=" | "<>" => ExpressionType::CompareNotequal,
        "<" => ExpressionType::CompareLessthan,
        ">" => ExpressionType::CompareGreaterthan,
        "<=" => ExpressionType::CompareLessthanorequalto,
        ">=" => ExpressionType::CompareGreaterthanorequalto,
        _ => ExpressionType::Invalid,
    }
}

impl<'a> Transformer<'a> {
    /// `TransformExpression(PGNode &)`.
    pub(crate) fn transform_expression(&mut self, node: &'a Node) -> Result<ParsedExpression> {
        self.nested(|t| t.transform_expression_inner(node))
    }

    /// `TransformExpression(optional_ptr<PGNode>)`: `None` for a NULL node.
    pub(crate) fn transform_opt_expression(
        &mut self,
        node: Option<&'a Node>,
    ) -> Result<Option<ParsedExpression>> {
        node.map(|node| self.transform_expression(node)).transpose()
    }

    /// [`Self::transform_opt_expression`], boxed.
    pub(crate) fn transform_opt_boxed(
        &mut self,
        node: Option<&'a Node>,
    ) -> Result<Option<Box<ParsedExpression>>> {
        Ok(self.transform_opt_expression(node)?.map(Box::new))
    }

    /// A child DuckDB dereferences unconditionally.
    pub(crate) fn transform_required(
        &mut self,
        node: Option<&'a Node>,
        what: &str,
    ) -> Result<ParsedExpression> {
        self.transform_expression(required(node, what)?)
    }

    fn transform_expression_inner(&mut self, node: &'a Node) -> Result<ParsedExpression> {
        match node {
            Node::ColumnRef(column_ref) => self.transform_column_ref(column_ref),
            Node::AConst(constant) => self.transform_constant(constant),
            Node::AExpr(expr) => self.transform_a_expr(expr),
            Node::FuncCall(call) => self.transform_func_call(call),
            Node::BoolExpr(expr) => self.transform_bool_expr(expr),
            Node::TypeCast(cast) => self.transform_type_cast(cast),
            Node::CaseExpr(expr) => self.transform_case(expr),
            Node::SubLink(link) => self.transform_subquery(link),
            Node::CoalesceExpr(expr) => self.transform_coalesce(expr),
            Node::NullTest(test) => self.transform_null_test(test),
            Node::ResTarget(target) => self.transform_res_target(target),
            Node::ParamRef(param) => self.transform_param_ref(param),
            Node::NamedArgExpr(arg) => self.transform_named_arg(arg),
            Node::SetToDefault(_) => Ok(ParsedExpression::new(
                ExpressionType::ValueDefault,
                ParsedExpressionKind::Default,
            )),
            Node::CollateClause(collate) => self.transform_collate_expr(collate),
            Node::IntervalConstant(interval) => self.transform_interval(interval),
            Node::LambdaFunction(lambda) => self.transform_lambda(lambda),
            Node::SingleArrowFunction(lambda) => self.transform_single_arrow(lambda),
            Node::AIndirection(indirection) => self.transform_array_access(indirection),
            Node::PositionalReference(reference) => Self::transform_positional_reference(reference),
            Node::GroupingFunc(grouping) => self.transform_grouping_function(grouping),
            Node::AStar(star) => self.transform_star_expression(star),
            Node::BooleanTest(test) => self.transform_boolean_test(test),
            Node::MultiAssignRef(assign) => self.transform_multi_assign_ref(assign),
            Node::Value(value @ Value::String(_)) => Self::transform_value(value),
            other => {
                let (tag, name) = node_tag(other);
                Err(Error::not_implemented(format!("Expression type {name} ({tag})")))
            }
        }
    }

    /// `TransformExpressionList`.
    pub(crate) fn transform_expression_list(
        &mut self,
        list: &'a List,
        result: &mut Vec<ParsedExpression>,
    ) -> Result<()> {
        result.reserve(list.len());
        for node in list {
            let expr = self.transform_expression(node)?;
            result.push(expr);
        }
        Ok(())
    }

    /// [`Self::transform_expression_list`] into a new vector.
    pub(crate) fn transform_expressions(&mut self, list: &'a List) -> Result<Vec<ParsedExpression>> {
        let mut result = Vec::new();
        self.transform_expression_list(list, &mut result)?;
        Ok(result)
    }

    /// `TransformResTarget`.
    fn transform_res_target(&mut self, root: &'a ResTarget) -> Result<ParsedExpression> {
        let mut expr = self.transform_required(root.val.as_ref(), "target value")?;
        if let Some(name) = &root.name {
            expr.alias.clone_from(name);
        }
        Ok(expr)
    }

    /// `TransformNamedArg`.
    fn transform_named_arg(&mut self, root: &'a NamedArgExpr) -> Result<ParsedExpression> {
        let mut expr = self.transform_required(root.arg.as_ref(), "named argument")?;
        if let Some(name) = &root.name {
            expr.alias.clone_from(name);
        }
        Ok(expr)
    }

    /// `TransformBoolExpr`.
    fn transform_bool_expr(&mut self, root: &'a BoolExpr) -> Result<ParsedExpression> {
        let mut result: Option<ParsedExpression> = None;
        for node in &root.args {
            let mut next = self.transform_expression(node)?;
            result = Some(match root.boolop {
                BoolExprType::And | BoolExprType::Or => {
                    let conjunction = if root.boolop == BoolExprType::And {
                        ExpressionType::ConjunctionAnd
                    } else {
                        ExpressionType::ConjunctionOr
                    };
                    match result {
                        None => next,
                        Some(left) => build::conjunction(conjunction, left, next),
                    }
                }
                BoolExprType::Not => {
                    let ty = next.expression_type;
                    if ty == ExpressionType::CompareIn {
                        next.expression_type = ExpressionType::CompareNotIn;
                        next
                    } else if matches!(
                        ty,
                        ExpressionType::CompareEqual
                            | ExpressionType::CompareNotequal
                            | ExpressionType::CompareLessthan
                            | ExpressionType::CompareGreaterthan
                            | ExpressionType::CompareLessthanorequalto
                            | ExpressionType::CompareGreaterthanorequalto
                            | ExpressionType::CompareDistinctFrom
                            | ExpressionType::CompareNotDistinctFrom
                    ) {
                        next.expression_type = negate_comparison(ty)?;
                        next
                    } else {
                        build::operator(ExpressionType::OperatorNot, vec![next])
                    }
                }
            });
        }
        let mut result = result.ok_or_else(|| Error::internal("Boolean expression without arguments"))?;
        set_location(&mut result, root.location);
        Ok(result)
    }

    /// `TransformBooleanTest`.
    fn transform_boolean_test(&mut self, node: &'a BooleanTest) -> Result<ParsedExpression> {
        let argument = self.transform_required(node.arg.as_ref(), "boolean test argument")?;
        let comparison = |comparison_type: ExpressionType, value: bool, argument: ParsedExpression| {
            let mut constant = build::constant(DuckValue::boolean(value));
            set_location(&mut constant, node.location);
            let cast_argument = build::cast(LogicalType::BOOLEAN, argument, false);
            let mut result = build::comparison(comparison_type, cast_argument, constant);
            set_location(&mut result, node.location);
            result
        };
        Ok(match node.booltesttype {
            BoolTestType::True => comparison(ExpressionType::CompareNotDistinctFrom, true, argument),
            BoolTestType::NotTrue => comparison(ExpressionType::CompareDistinctFrom, true, argument),
            BoolTestType::False => comparison(ExpressionType::CompareNotDistinctFrom, false, argument),
            BoolTestType::NotFalse => comparison(ExpressionType::CompareDistinctFrom, false, argument),
            BoolTestType::Unknown | BoolTestType::NotUnknown => {
                let operator_type = if node.booltesttype == BoolTestType::Unknown {
                    ExpressionType::OperatorIsNull
                } else {
                    ExpressionType::OperatorIsNotNull
                };
                let mut result = build::operator(operator_type, vec![argument]);
                set_location(&mut result, node.location);
                result
            }
        })
    }

    /// `TransformCase`.
    fn transform_case(&mut self, root: &'a CaseExpr) -> Result<ParsedExpression> {
        let root_arg = self.transform_opt_expression(root.arg.as_ref())?;
        let mut checks = Vec::with_capacity(root.args.len());
        for cell in &root.args {
            let when: &CaseWhen = cast_node(cell)?;
            let test_raw = self.transform_required(when.expr.as_ref(), "CASE condition")?;
            let when_expr = match &root_arg {
                Some(arg) => build::comparison(ExpressionType::CompareEqual, arg.clone(), test_raw),
                None => test_raw,
            };
            let then_expr = self.transform_required(when.result.as_ref(), "CASE result")?;
            checks.push(CaseCheck {
                when_expr: Box::new(when_expr),
                then_expr: Box::new(then_expr),
            });
        }
        let else_expr = match &root.defresult {
            Some(node) => self.transform_expression(node)?,
            None => build::constant(DuckValue::null(LogicalType::SQLNULL)),
        };
        let mut result = build::case_expression(checks, else_expr);
        set_location(&mut result, root.location);
        Ok(result)
    }

    /// `TransformTypeCast`.
    fn transform_type_cast(&mut self, root: &'a TypeCast) -> Result<ParsedExpression> {
        let type_name = required(root.type_name.as_deref(), "cast type")?;
        let target_type = self.transform_type_name(type_name)?;
        let try_cast = root.try_cast != 0;
        // A constant string cast to BLOB becomes a BLOB constant.
        if !try_cast
            && is_blob(&target_type)
            && let Some(Node::AConst(constant)) = &root.arg
            && let Value::String(text) = &constant.val
        {
            let location = u64::try_from(root.location).ok();
            let blob = parse_blob(text, location)?;
            let mut result = build::constant(DuckValue::blob(&blob));
            set_location(&mut result, root.location);
            return Ok(result);
        }
        let expression = self.transform_required(root.arg.as_ref(), "cast argument")?;
        let mut result = build::cast(target_type, expression, try_cast);
        set_location(&mut result, root.location);
        Ok(result)
    }

    /// `TransformCoalesce`: `COALESCE(a, b, ...)` is an `OPERATOR_COALESCE`.
    fn transform_coalesce(&mut self, root: &'a CoalesceExpr) -> Result<ParsedExpression> {
        let mut children = Vec::with_capacity(root.args.len());
        for node in &root.args {
            children.push(self.transform_expression(node)?);
        }
        Ok(build::operator(ExpressionType::OperatorCoalesce, children))
    }

    /// `TransformNullTest`.
    fn transform_null_test(&mut self, root: &'a NullTest) -> Result<ParsedExpression> {
        let arg = self.transform_required(root.arg.as_ref(), "IS NULL argument")?;
        if root.argisrow {
            return Err(Error::not_implemented("IS NULL argisrow"));
        }
        let expression_type = if root.nulltesttype == NullTestType::Null {
            ExpressionType::OperatorIsNull
        } else {
            ExpressionType::OperatorIsNotNull
        };
        let mut result = build::operator(expression_type, vec![arg]);
        set_location(&mut result, root.location);
        Ok(result)
    }

    /// `TransformCollateExpr`.
    pub(crate) fn transform_collate_expr(&mut self, collate: &'a CollateClause) -> Result<ParsedExpression> {
        let child = self.transform_required(collate.arg.as_ref(), "COLLATE argument")?;
        let collation = Self::transform_collation(Some(collate))?;
        Ok(ParsedExpression::new(
            ExpressionType::Collate,
            ParsedExpressionKind::Collate(CollateExpression {
                child: Box::new(child),
                collation,
            }),
        ))
    }

    /// `TransformCollation`: the dotted collation name.
    pub(crate) fn transform_collation(collate: Option<&CollateClause>) -> Result<String> {
        let Some(collate) = collate else {
            return Ok(String::new());
        };
        let mut collation = String::new();
        for node in &collate.collname {
            let Node::Value(Value::String(argument)) = node else {
                return Err(Error::parser("Expected a string as collation type!"));
            };
            if collation.is_empty() {
                collation.clone_from(argument);
            } else {
                collation.push('.');
                collation.push_str(argument);
            }
        }
        Ok(collation)
    }

    /// `TransformLambda`: `lambda x, y: expr`.
    fn transform_lambda(&mut self, node: &'a LambdaFunction) -> Result<ParsedExpression> {
        let parameters = super::string_list(&node.lhs)?;
        let rhs = self.transform_required(node.rhs.as_ref(), "lambda body")?;
        let lhs = if parameters.len() == 1 {
            build::column_ref(parameters)
        } else {
            let children = parameters
                .into_iter()
                .map(|name| build::column_ref(vec![name]))
                .collect();
            build::function("row", children)
        };
        let mut result = ParsedExpression::new(
            ExpressionType::Lambda,
            ParsedExpressionKind::Lambda(LambdaExpression {
                lhs: Box::new(lhs),
                expr: Box::new(rhs),
                syntax_type: LambdaSyntaxType::LambdaKeyword,
            }),
        );
        set_location(&mut result, node.location);
        Ok(result)
    }

    /// `TransformSingleArrow`: `x -> expr`.
    fn transform_single_arrow(&mut self, node: &'a SingleArrowFunction) -> Result<ParsedExpression> {
        let lhs = self.transform_required(node.lhs.as_ref(), "lambda parameters")?;
        let rhs = self.transform_required(node.rhs.as_ref(), "lambda body")?;
        let mut result = ParsedExpression::new(
            ExpressionType::Lambda,
            ParsedExpressionKind::Lambda(LambdaExpression {
                lhs: Box::new(lhs),
                expr: Box::new(rhs),
                syntax_type: LambdaSyntaxType::SingleArrow,
            }),
        );
        set_location(&mut result, node.location);
        Ok(result)
    }

    /// `TransformPositionalReference`: `#n`.
    fn transform_positional_reference(node: &PositionalReference) -> Result<ParsedExpression> {
        let index = u64::try_from(node.position)
            .ok()
            .filter(|&index| index > 0)
            .ok_or_else(|| Error::parser("Positional reference node needs to be >= 1"))?;
        let mut result = build::positional_reference(index);
        set_location(&mut result, node.location);
        Ok(result)
    }

    /// `TransformGroupingFunction`: `GROUPING(a, b)`.
    fn transform_grouping_function(&mut self, grouping: &'a GroupingFunc) -> Result<ParsedExpression> {
        let mut children = Vec::with_capacity(grouping.args.len());
        for node in &grouping.args {
            children.push(self.transform_expression(node)?);
        }
        let mut result = build::operator(ExpressionType::GroupingFunction, children);
        set_location(&mut result, grouping.location);
        Ok(result)
    }

    /// `TransformMultiAssignRef`: one column of `SET (a, b) = (x, y)`.
    fn transform_multi_assign_ref(&mut self, root: &'a MultiAssignRef) -> Result<ParsedExpression> {
        let source = required(root.source.as_ref(), "assignment source")?;
        let Node::FuncCall(func) = source else {
            return self.transform_expression(source);
        };
        let is_row = func
            .funcname
            .last()
            .and_then(|name| node_string(name).ok())
            .is_some_and(|name| name.eq_ignore_ascii_case("row"));
        if !is_row {
            return self.transform_expression(source);
        }
        let value_count = func.args.len() as i64;
        if i64::from(root.ncolumns) < value_count || func.args.is_empty() {
            return Err(Error::parser(format!(
                "Could not perform assignment, expected {} values, got {value_count}",
                root.ncolumns
            )));
        }
        let index = usize::try_from(root.colno.max(1) - 1).unwrap_or(0);
        let Some(node) = func.args.get(index) else {
            return Err(Error::parser(format!(
                "Could not perform assignment, expected {} values, got {}",
                root.ncolumns,
                func.args.len()
            )));
        };
        self.transform_expression(node)
    }

    /// `TransformParamRef`: `?`, `$1`, `$name`.
    fn transform_param_ref(&mut self, node: &ParamRef) -> Result<ParsedExpression> {
        let (param_type, mut identifier) = if let Some(name) = &node.name {
            (ParamType::Named, name.clone())
        } else {
            if node.number < 0 {
                return Err(Error::parser("Parameter numbers cannot be negative"));
            }
            let param_type = if node.number == 0 {
                ParamType::AutoIncrement
            } else {
                ParamType::Positional
            };
            (param_type, node.number.to_string())
        };
        let known = self.get_param(&identifier, param_type)?;
        let index = match known {
            Some(index) => index,
            None => {
                let index = if node.number != 0 {
                    u64::try_from(node.number).unwrap_or(0)
                } else {
                    let index = self.param_count() + 1;
                    if node.name.is_none() {
                        identifier = index.to_string();
                    }
                    index
                };
                self.set_param(&identifier, index, param_type)?;
                index
            }
        };
        self.param_count = self.param_count.max(index);
        Ok(ParsedExpression::new(
            ExpressionType::ValueParameter,
            ParsedExpressionKind::Parameter(ParameterExpression { identifier }),
        ))
    }

    /// `TransformArrayAccess`: `x[i]`, `x[a:b:c]`, `x.field`, `x.f(...)`.
    fn transform_array_access(&mut self, node: &'a AIndirection) -> Result<ParsedExpression> {
        let mut result = self.transform_required(node.arg.as_ref(), "indirection source")?;
        let mut list_size = 0;
        for target in &node.indirection {
            result = match target {
                Node::AIndices(indices) => {
                    if indices.is_slice {
                        let empty_list =
                            || build::constant(DuckValue::list(LogicalType::INTEGER, Vec::new()));
                        let lower = match &indices.lidx {
                            Some(lower) => self.transform_expression(lower)?,
                            None => empty_list(),
                        };
                        let upper = match &indices.uidx {
                            Some(upper) => self.transform_expression(upper)?,
                            None => empty_list(),
                        };
                        let mut children = vec![result, lower, upper];
                        if let Some(step) = &indices.step {
                            children.push(self.transform_expression(step)?);
                        }
                        build::operator(ExpressionType::ArraySlice, children)
                    } else {
                        let index = self.transform_required(indices.uidx.as_ref(), "array index")?;
                        build::operator(ExpressionType::ArrayExtract, vec![result, index])
                    }
                }
                Node::Value(value @ Value::String(_)) => {
                    let field = Self::transform_value(value)?;
                    build::operator(ExpressionType::StructExtract, vec![result, field])
                }
                Node::FuncCall(func) => {
                    let mut function = self.transform_func_call(func)?;
                    if function.expression_type != ExpressionType::Function {
                        return Err(Error::parser(format!(
                            "{result}.{function}() call must be a function"
                        )));
                    }
                    let ParsedExpressionKind::Function(function_expr) = &mut function.kind else {
                        return Err(Error::internal("Function expression without a function"));
                    };
                    function_expr.children.insert(0, result);
                    function
                }
                _ => return Err(Error::not_implemented("Unimplemented subscript type")),
            };
            list_size += 1;
            self.check_depth(list_size)?;
        }
        Ok(result)
    }

    /// `TransformInterval`: `INTERVAL x unit`.
    fn transform_interval(&mut self, node: &'a IntervalConstant) -> Result<ParsedExpression> {
        // `val_type` is the node tag of the value: an expression, a string or an integer.
        const T_PG_INTEGER: i32 = 218;
        const T_PG_STRING: i32 = 220;
        const T_PG_A_EXPR: i32 = 355;
        let mut expr = match node.val_type {
            T_PG_A_EXPR => self.transform_required(node.eval.as_ref(), "interval value")?,
            T_PG_STRING => build::varchar_constant(node.sval.as_deref().unwrap_or_default()),
            T_PG_INTEGER => build::constant(DuckValue::integer(node.ival)),
            _ => return Err(Error::internal("Unsupported interval transformation")),
        };
        set_location(&mut expr, node.location);
        let Some(first) = node.typmods.first() else {
            let mut result = build::cast(LogicalType::INTERVAL, expr, false);
            set_location(&mut result, node.location);
            return Ok(result);
        };
        let Node::AConst(mask_const) = first else {
            return Err(Error::internal("Interval type modifier is not a constant"));
        };
        let Value::Integer(mask) = mask_const.val else {
            return Err(Error::internal("Interval type modifier is not an integer"));
        };
        let has = |field: i32| mask & i64::from(interval_mask(field)) != 0;
        use interval_field as f;
        let combined = [
            (f::YEAR, f::MONTH, "YEAR TO MONTH"),
            (f::DAY, f::HOUR, "DAY TO HOUR"),
            (f::DAY, f::MINUTE, "DAY TO MINUTE"),
            (f::DAY, f::SECOND, "DAY TO SECOND"),
            (f::HOUR, f::MINUTE, "HOUR TO MINUTE"),
            (f::HOUR, f::SECOND, "HOUR TO SECOND"),
            (f::MINUTE, f::SECOND, "MINUTE TO SECOND"),
        ];
        for (a, b, name) in combined {
            if has(a) && has(b) {
                return Err(Error::parser(format!("{name} is not supported")));
            }
        }
        let single = [
            (f::YEAR, "to_years", LogicalType::INTEGER),
            (f::MONTH, "to_months", LogicalType::INTEGER),
            (f::DAY, "to_days", LogicalType::INTEGER),
            (f::HOUR, "to_hours", LogicalType::BIGINT),
            (f::MINUTE, "to_minutes", LogicalType::BIGINT),
            (f::SECOND, "to_seconds", LogicalType::DOUBLE),
            (f::MILLISECOND, "to_milliseconds", LogicalType::DOUBLE),
            (f::MICROSECOND, "to_microseconds", LogicalType::BIGINT),
            (f::WEEK, "to_weeks", LogicalType::INTEGER),
            (f::QUARTER, "to_quarters", LogicalType::INTEGER),
            (f::DECADE, "to_decades", LogicalType::INTEGER),
            (f::CENTURY, "to_centuries", LogicalType::INTEGER),
            (f::MILLENNIUM, "to_millennia", LogicalType::INTEGER),
        ];
        let Some((_, function_name, target_type)) = single.into_iter().find(|(field, _, _)| has(*field))
        else {
            return Err(Error::internal("Unsupported interval post-fix"));
        };
        let mut expr = build::cast(LogicalType::DOUBLE, expr, false);
        set_location(&mut expr, node.location);
        if target_type != LogicalType::DOUBLE {
            expr = build::cast(target_type, build::function("trunc", vec![expr]), false);
        }
        let mut result = build::function(function_name, vec![expr]);
        set_location(&mut result, node.location);
        Ok(result)
    }
}

fn is_blob(ty: &LogicalType) -> bool {
    // An unbound `BLOB` type name: TransformTypeName returns UNBOUND types, so DuckDB's
    // comparison with `LogicalType::BLOB` only matches a bound BLOB.
    *ty == LogicalType::BLOB
}

/// `Blob::ToBlob` of a string constant cast to BLOB.
fn parse_blob(text: &str, location: Option<u64>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    rustdb_types::blob::parse_text(text.as_bytes(), &mut out)
        .map_err(|message| Error::conversion(message).with_position(location))?;
    Ok(out)
}
