//! Function calls and window functions (`transform_function.cpp`).

use rustdb_ast::{
    CaseCheck, ExpressionType, OrderModifier, ParsedExpression, ParsedExpressionKind, WindowBoundary,
    WindowExcludeMode, WindowExpression, expression_type_to_string,
};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{FuncCall, IgnoreNulls, WindowDef, frame_option as fo};
use rustdb_types::LogicalType;

use super::build::{self, set_location};
use super::star::is_empty_star;
use super::{Transformer, node_string};
use crate::equality::expressions_equal;

/// `WindowExpression::WindowToExpressionType`: window functions by name; anything else
/// is an aggregate.
pub fn window_to_expression_type(name: &str) -> ExpressionType {
    match name {
        "rank" => ExpressionType::WindowRank,
        "rank_dense" | "dense_rank" => ExpressionType::WindowRankDense,
        "percent_rank" => ExpressionType::WindowPercentRank,
        "row_number" => ExpressionType::WindowRowNumber,
        "first_value" | "first" => ExpressionType::WindowFirstValue,
        "last_value" | "last" => ExpressionType::WindowLastValue,
        "nth_value" => ExpressionType::WindowNthValue,
        "cume_dist" => ExpressionType::WindowCumeDist,
        "lead" => ExpressionType::WindowLead,
        "lag" => ExpressionType::WindowLag,
        "ntile" => ExpressionType::WindowNtile,
        "fill" => ExpressionType::WindowFill,
        _ => ExpressionType::WindowAggregate,
    }
}

/// `IsOrderableWindowFunction`.
pub fn is_orderable_window_function(expression_type: ExpressionType) -> Result<bool> {
    use ExpressionType as E;
    match expression_type {
        E::WindowFirstValue
        | E::WindowLastValue
        | E::WindowNthValue
        | E::WindowRank
        | E::WindowPercentRank
        | E::WindowRowNumber
        | E::WindowNtile
        | E::WindowCumeDist
        | E::WindowLead
        | E::WindowLag
        | E::WindowFill
        | E::WindowAggregate => Ok(true),
        E::WindowRankDense => Ok(false),
        other => Err(Error::internal(format!(
            "Unknown orderable window type {}",
            expression_type_to_string(other)
        ))),
    }
}

/// `IsExcludableWindowFunction`.
pub fn is_excludable_window_function(expression_type: ExpressionType) -> Result<bool> {
    use ExpressionType as E;
    match expression_type {
        E::WindowFirstValue | E::WindowLastValue | E::WindowNthValue | E::WindowAggregate => Ok(true),
        E::WindowRankDense
        | E::WindowRank
        | E::WindowPercentRank
        | E::WindowRowNumber
        | E::WindowNtile
        | E::WindowCumeDist
        | E::WindowLead
        | E::WindowLag
        | E::WindowFill => Ok(false),
        other => Err(Error::internal(format!(
            "Unknown excludable window type {}",
            expression_type_to_string(other)
        ))),
    }
}

/// `TransformFrameOption`: the boundary for the frame's unit (ROWS, RANGE or GROUPS).
const fn frame_boundary(
    frame_options: i32,
    rows: WindowBoundary,
    range: WindowBoundary,
    groups: WindowBoundary,
) -> WindowBoundary {
    if frame_options & fo::RANGE != 0 {
        range
    } else if frame_options & fo::GROUPS != 0 {
        groups
    } else {
        rows
    }
}

fn new_window(catalog: String, schema: String, name: &str) -> WindowExpression {
    WindowExpression {
        function_name: name.to_ascii_lowercase(),
        schema,
        catalog,
        children: Vec::new(),
        partitions: Vec::new(),
        orders: Vec::new(),
        start: WindowBoundary::Invalid,
        end: WindowBoundary::Invalid,
        start_expr: None,
        end_expr: None,
        offset_expr: None,
        default_expr: None,
        ignore_nulls: false,
        filter_expr: None,
        exclude_clause: WindowExcludeMode::NoOther,
        distinct: false,
        arg_orders: Vec::new(),
    }
}

impl<'a> Transformer<'a> {
    /// `TransformWindowDef`: PARTITION BY and ORDER BY of a window.
    fn transform_window_def(
        &mut self,
        window_spec: &'a WindowDef,
        expr: &mut WindowExpression,
        window_name: Option<&str>,
    ) -> Result<()> {
        if !window_spec.partition_clause.is_empty() {
            if let Some(name) = window_name
                && !expr.partitions.is_empty()
            {
                return Err(Error::parser(format!(
                    "Cannot override PARTITION BY clause of window \"{name}\""
                )));
            }
            self.transform_expression_list(&window_spec.partition_clause, &mut expr.partitions)?;
        }
        if !window_spec.order_clause.is_empty() {
            if let Some(name) = window_name
                && !expr.orders.is_empty()
            {
                return Err(Error::parser(format!(
                    "Cannot override ORDER BY clause of window \"{name}\""
                )));
            }
            self.transform_order_by(&window_spec.order_clause, &mut expr.orders)?;
            for order in &expr.orders {
                if let ParsedExpressionKind::Star(star) = &order.expression.kind
                    && order.expression.expression_type == ExpressionType::Star
                    && star.expr.is_none()
                {
                    return Err(Error::parser("Cannot ORDER BY ALL in a window expression"));
                }
            }
        }
        Ok(())
    }

    /// `TransformWindowFrame`: the frame bounds and exclusion.
    fn transform_window_frame(
        &mut self,
        window_spec: &'a WindowDef,
        expr: &mut WindowExpression,
    ) -> Result<()> {
        use WindowBoundary as B;
        expr.start_expr = self.transform_opt_boxed(window_spec.start_offset.as_ref())?;
        expr.end_expr = self.transform_opt_boxed(window_spec.end_offset.as_ref())?;
        let options = window_spec.frame_options;
        if options & (fo::END_UNBOUNDED_PRECEDING | fo::START_UNBOUNDED_FOLLOWING) != 0 {
            return Err(Error::internal(
                "Window frames starting with unbounded following or ending in unbounded preceding make no sense",
            ));
        }
        if options & fo::START_UNBOUNDED_PRECEDING != 0 {
            expr.start = B::UnboundedPreceding;
        } else if options & fo::START_OFFSET_PRECEDING != 0 {
            expr.start = frame_boundary(
                options,
                B::ExprPrecedingRows,
                B::ExprPrecedingRange,
                B::ExprPrecedingGroups,
            );
        } else if options & fo::START_OFFSET_FOLLOWING != 0 {
            expr.start = frame_boundary(
                options,
                B::ExprFollowingRows,
                B::ExprFollowingRange,
                B::ExprFollowingGroups,
            );
        } else if options & fo::START_CURRENT_ROW != 0 {
            expr.start = frame_boundary(
                options,
                B::CurrentRowRows,
                B::CurrentRowRange,
                B::CurrentRowGroups,
            );
        }
        if options & fo::END_UNBOUNDED_FOLLOWING != 0 {
            expr.end = B::UnboundedFollowing;
        } else if options & fo::END_OFFSET_PRECEDING != 0 {
            expr.end = frame_boundary(
                options,
                B::ExprPrecedingRows,
                B::ExprPrecedingRange,
                B::ExprPrecedingGroups,
            );
        } else if options & fo::END_OFFSET_FOLLOWING != 0 {
            expr.end = frame_boundary(
                options,
                B::ExprFollowingRows,
                B::ExprFollowingRange,
                B::ExprFollowingGroups,
            );
        } else if options & fo::END_CURRENT_ROW != 0 {
            expr.end = frame_boundary(
                options,
                B::CurrentRowRows,
                B::CurrentRowRange,
                B::CurrentRowGroups,
            );
        }
        if (options & fo::START_OFFSET != 0 && expr.start_expr.is_none())
            || (options & fo::END_OFFSET != 0 && expr.end_expr.is_none())
        {
            return Err(Error::internal("Failed to transform window boundary expression"));
        }
        expr.exclude_clause = if options & fo::EXCLUDE_CURRENT_ROW != 0 {
            WindowExcludeMode::CurrentRow
        } else if options & fo::EXCLUDE_GROUP != 0 {
            WindowExcludeMode::Group
        } else if options & fo::EXCLUDE_TIES != 0 {
            WindowExcludeMode::Ties
        } else {
            WindowExcludeMode::NoOther
        };
        if expr.exclude_clause != WindowExcludeMode::NoOther
            && !expr.arg_orders.is_empty()
            && !is_excludable_window_function(window_type_of(expr))?
        {
            return Err(Error::parser(format!(
                "EXCLUDE is not supported for the window function \"{}\"",
                expr.function_name
            )));
        }
        Ok(())
    }

    /// `TransformFuncCall`.
    pub(crate) fn transform_func_call(&mut self, root: &'a FuncCall) -> Result<ParsedExpression> {
        let names: Vec<&str> = root.funcname.iter().map(node_string).collect::<Result<_>>()?;
        let (catalog, schema, function_name) = match names.as_slice() {
            [catalog, schema, name] => ((*catalog).to_owned(), (*schema).to_owned(), *name),
            [schema, name] => (String::new(), (*schema).to_owned(), *name),
            [name] => (String::new(), String::new(), *name),
            _ => {
                return Err(Error::parser(
                    "TransformFuncCall - Expected 1, 2 or 3 qualifications",
                ));
            }
        };
        let mut children = Vec::new();
        self.transform_expression_list(&root.args, &mut children)?;
        if children.len() == 1
            && is_empty_star(&children[0])
            && !root.agg_distinct
            && root.agg_order.is_empty()
        {
            // COUNT(*) becomes COUNT().
            children.clear();
        }
        let mut lowercase_name = function_name.to_ascii_lowercase();
        if let Some(over) = &root.over {
            return self.transform_window_call(root, over, catalog, schema, &lowercase_name, children);
        }
        if root.agg_ignore_nulls != IgnoreNulls::Default {
            return Err(Error::parser(
                "RESPECT/IGNORE NULLS is not supported for non-window functions",
            ));
        }
        let mut filter = self.transform_opt_boxed(root.agg_filter.as_ref())?;
        let mut order_bys = OrderModifier { orders: Vec::new() };
        self.transform_order_by(&root.agg_order, &mut order_bys.orders)?;
        let mut distinct = root.agg_distinct;

        if root.agg_within_group {
            if order_bys.orders.len() != 1 {
                return Err(Error::parser(
                    "Cannot use multiple ORDER BY clauses with WITHIN GROUP",
                ));
            }
            match lowercase_name.as_str() {
                "percentile_cont" => {
                    if children.len() != 1 {
                        return Err(Error::parser("Wrong number of arguments for PERCENTILE_CONT"));
                    }
                    "quantile_cont".clone_into(&mut lowercase_name);
                }
                "percentile_disc" => {
                    if children.len() != 1 {
                        return Err(Error::parser("Wrong number of arguments for PERCENTILE_DISC"));
                    }
                    "quantile_disc".clone_into(&mut lowercase_name);
                }
                "mode" => {
                    if !children.is_empty() {
                        return Err(Error::parser("Wrong number of arguments for MODE"));
                    }
                }
                _ => {
                    return Err(Error::parser(format!(
                        "Unknown ordered aggregate \"{function_name}\"."
                    )));
                }
            }
        }
        if lowercase_name == "count" && children.is_empty() {
            "count_star".clone_into(&mut lowercase_name);
        }
        match lowercase_name.as_str() {
            "if" => {
                let [when, then, otherwise]: [ParsedExpression; 3] = children
                    .try_into()
                    .map_err(|_| Error::parser("Wrong number of arguments to IF."))?;
                let check = CaseCheck {
                    when_expr: Box::new(when),
                    then_expr: Box::new(then),
                };
                return Ok(build::case_expression(vec![check], otherwise));
            }
            "unpack" => {
                if children.len() != 1 {
                    return Err(Error::parser("Wrong number of arguments to the UNPACK operator"));
                }
                return Ok(build::operator(ExpressionType::OperatorUnpack, children));
            }
            "try" => {
                if children.len() != 1 {
                    return Err(Error::parser(
                        "Wrong number of arguments provided to TRY expression",
                    ));
                }
                return Ok(build::operator(ExpressionType::OperatorTry, children));
            }
            "construct_array" => {
                return Ok(build::operator(ExpressionType::ArrayConstructor, children));
            }
            "__internal_position_operator" => {
                if children.len() != 2 {
                    return Err(Error::parser(
                        "Wrong number of arguments to __internal_position_operator.",
                    ));
                }
                children.swap(0, 1);
                "position".clone_into(&mut lowercase_name);
            }
            "ifnull" => {
                if children.len() != 2 {
                    return Err(Error::parser("Wrong number of arguments to IFNULL."));
                }
                return Ok(build::operator(ExpressionType::OperatorCoalesce, children));
            }
            "list" if order_bys.orders.len() == 1 => {
                if children.len() != 1 {
                    return Err(Error::parser("Wrong number of arguments to LIST."));
                }
                let order_by = &order_bys.orders[0];
                if expressions_equal(&children[0], &order_by.expression) {
                    let sense = build::varchar_constant(order_by.order_type.name());
                    let nulls = build::varchar_constant(order_by.null_order.name());
                    let unordered = build::function_full(
                        catalog.clone(),
                        schema.clone(),
                        &lowercase_name,
                        std::mem::take(&mut children),
                        filter.take(),
                        OrderModifier { orders: Vec::new() },
                        distinct,
                        false,
                        root.export_state,
                    );
                    "list_sort".clone_into(&mut lowercase_name);
                    order_bys.orders.clear();
                    distinct = false;
                    children = vec![unordered, sense, nulls];
                }
            }
            "date" => {
                let [child]: [ParsedExpression; 1] = children
                    .try_into()
                    .map_err(|_| Error::parser("Wrong number of arguments provided to DATE function"))?;
                return Ok(build::cast(LogicalType::DATE, child, false));
            }
            _ => {}
        }
        let mut function = build::function_full(
            catalog,
            schema,
            &lowercase_name,
            children,
            filter,
            order_bys,
            distinct,
            false,
            root.export_state,
        );
        set_location(&mut function, root.location);
        Ok(function)
    }

    /// The `OVER (...)` branch of `TransformFuncCall`.
    fn transform_window_call(
        &mut self,
        root: &'a FuncCall,
        over: &'a WindowDef,
        catalog: String,
        schema: String,
        lowercase_name: &str,
        mut children: Vec<ParsedExpression>,
    ) -> Result<ParsedExpression> {
        if self.in_window_definition() {
            return Err(Error::parser(
                "window functions are not allowed in window definitions",
            ));
        }
        let win_fun_type = window_to_expression_type(lowercase_name);
        if win_fun_type == ExpressionType::Invalid {
            return Err(Error::internal("Unknown/unsupported window function"));
        }
        if win_fun_type != ExpressionType::WindowAggregate && root.agg_distinct {
            return Err(Error::parser(
                "DISTINCT is not implemented for non-aggregate window functions!",
            ));
        }
        if !root.agg_order.is_empty() && !is_orderable_window_function(win_fun_type)? {
            return Err(Error::parser(format!(
                "ORDER BY is not supported for the window function \"{lowercase_name}\""
            )));
        }
        if win_fun_type != ExpressionType::WindowAggregate && root.agg_filter.is_some() {
            return Err(Error::parser(
                "FILTER is not implemented for non-aggregate window functions!",
            ));
        }
        if root.export_state {
            return Err(Error::parser(
                "EXPORT_STATE is not supported for window functions!",
            ));
        }
        if win_fun_type == ExpressionType::WindowAggregate && root.agg_ignore_nulls != IgnoreNulls::Default {
            return Err(Error::parser(
                "RESPECT/IGNORE NULLS is not supported for windowed aggregates",
            ));
        }
        let mut expr = new_window(catalog, schema, lowercase_name);
        expr.ignore_nulls = root.agg_ignore_nulls == IgnoreNulls::Ignore;
        expr.distinct = root.agg_distinct;
        expr.filter_expr = self.transform_opt_boxed(root.agg_filter.as_ref())?;
        if !root.agg_order.is_empty() {
            self.transform_order_by(&root.agg_order, &mut expr.arg_orders)?;
        }
        if win_fun_type == ExpressionType::WindowAggregate {
            expr.children = children;
        } else {
            let count = children.len();
            let mut rest = children.drain(..);
            if let Some(first) = rest.next() {
                expr.children.push(first);
            }
            match win_fun_type {
                ExpressionType::WindowLead | ExpressionType::WindowLag => {
                    expr.offset_expr = rest.next().map(Box::new);
                    expr.default_expr = rest.next().map(Box::new);
                    if count > 3 {
                        return Err(Error::parser(format!(
                            "Incorrect number of parameters for function {lowercase_name}"
                        )));
                    }
                }
                ExpressionType::WindowNthValue => {
                    if let Some(second) = rest.next() {
                        expr.children.push(second);
                    }
                    if count > 2 {
                        return Err(Error::parser(format!(
                            "Incorrect number of parameters for function {lowercase_name}"
                        )));
                    }
                }
                _ => {
                    if count > 1 {
                        return Err(Error::parser(format!(
                            "Incorrect number of parameters for function {lowercase_name}"
                        )));
                    }
                }
            }
        }
        let mut window_spec = over;
        if let Some(name) = &window_spec.name {
            window_spec = *self
                .frame()
                .window_clauses
                .get(name)
                .ok_or_else(|| Error::parser(format!("window \"{name}\" does not exist")))?;
        }
        let mut window_ref = window_spec;
        let window_name = window_ref.refname.as_deref();
        if let Some(refname) = &window_spec.refname {
            window_ref = *self
                .frame()
                .window_clauses
                .get(refname)
                .ok_or_else(|| Error::parser(format!("window \"{refname}\" does not exist")))?;
            if window_ref.start_offset.is_some()
                || window_ref.end_offset.is_some()
                || window_ref.frame_options != fo::DEFAULTS
            {
                return Err(Error::parser(format!(
                    "cannot copy window \"{refname}\" because it has a frame clause"
                )));
            }
        }
        self.frame().in_window_definition = true;
        self.transform_window_def(window_ref, &mut expr, None)?;
        if !std::ptr::eq(window_ref, window_spec) {
            self.transform_window_def(window_spec, &mut expr, window_name)?;
        }
        self.transform_window_frame(window_spec, &mut expr)?;
        self.frame().in_window_definition = false;
        let mut result = ParsedExpression::new(win_fun_type, ParsedExpressionKind::Window(Box::new(expr)));
        set_location(&mut result, root.location);
        Ok(result)
    }
}

/// The expression type a window expression was created with; the window function's
/// name determines it.
fn window_type_of(expr: &WindowExpression) -> ExpressionType {
    window_to_expression_type(&expr.function_name)
}
