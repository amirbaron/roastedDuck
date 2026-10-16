//! Expansion of `*` and `COLUMNS(...)` (DuckDB's `bind_star_expression.cpp`).

use rustdb_ast::{
    ExpressionClass, ExpressionType, LambdaExpression, LambdaSyntaxType, ParsedExpression,
    ParsedExpressionKind, StarExpression,
};
use rustdb_common::string_util::candidates_error_message;
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::binder::BindScope;
use crate::expression_binder::{
    ExpressionBinder, TableFunctionBinder, for_each_child, function_call, parameter_not_resolved,
};

/// What kind of star an expression contains (DuckDB's `StarExpressionType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarExpressionType {
    None,
    Star,
    Unpacked,
}

fn is_star(expr: &ParsedExpression) -> bool {
    matches!(&expr.kind, ParsedExpressionKind::Star(star) if !star.columns)
}

fn is_columns(expr: &ParsedExpression) -> bool {
    matches!(&expr.kind, ParsedExpressionKind::Star(star) if star.columns)
}

/// The name a COLUMNS(*) list uses for a column (DuckDB's `GetColumnsStringValue`).
fn columns_string_value(expr: &ParsedExpression) -> String {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(column) => column.column_names.last().cloned().unwrap_or_default(),
        _ => expr.to_string(),
    }
}

/// Finds the star of `expr` (DuckDB's `FindStarExpression`); a `*` inside COLUMNS is
/// replaced by the list of column names.
pub fn find_star_expression(
    scope: &BindScope<'_>,
    expr: &mut ParsedExpression,
    star: &mut Option<StarExpression>,
    is_root: bool,
    in_columns: bool,
) -> Result<StarExpressionType> {
    if expr.expression_type == ExpressionType::OperatorUnpack {
        let ParsedExpressionKind::Operator(operator) = &mut expr.kind else {
            return Err(Error::internal("UNPACK without children"));
        };
        let result = match operator.children.first_mut() {
            Some(child) => find_star_expression(scope, child, star, is_root, in_columns)?,
            None => StarExpressionType::None,
        };
        if result != StarExpressionType::Star {
            return Err(Error::binder(
                "UNPACK can only be used in combination with a STAR (*) expression or COLUMNS expression",
            ));
        }
        return Ok(StarExpressionType::Unpacked);
    }
    let mut in_columns = in_columns;
    let mut has_star = StarExpressionType::None;
    if let ParsedExpressionKind::Star(current) = &expr.kind {
        if !current.columns {
            if is_root {
                *star = Some((**current).clone());
                return Ok(StarExpressionType::Star);
            }
            if !in_columns {
                return Err(Error::binder(
                    "STAR expression is only allowed as the root element of an expression. Use COLUMNS(*) instead.",
                ));
            }
            if !current.replace_list.is_empty() {
                return Err(Error::binder(
                    "STAR expression with REPLACE list is only allowed as the root element of COLUMNS",
                ));
            }
            if !current.rename_list.is_empty() {
                return Err(Error::binder(
                    "STAR expression with RENAME list is only allowed as the root element of COLUMNS",
                ));
            }
            let list = scope.bind_context.generate_all_column_expressions(current)?;
            let values = list
                .iter()
                .map(|e| Value::varchar(columns_string_value(e)))
                .collect();
            *expr = ParsedExpression::constant(Value::list(LogicalType::VARCHAR, values));
            return Ok(StarExpressionType::Star);
        }
        if in_columns {
            return Err(Error::binder(
                "COLUMNS expression is not allowed inside another COLUMNS expression",
            ));
        }
        in_columns = true;
        if let Some(existing) = star {
            if !stars_equal(existing, current) {
                return Err(Error::binder(
                    "Multiple different STAR/COLUMNS in the same expression are not supported",
                )
                .with_position(expr.query_location));
            }
            return Ok(StarExpressionType::Star);
        }
        *star = Some((**current).clone());
        has_star = StarExpressionType::Star;
    }
    let found_here = has_star == StarExpressionType::Star;
    for_each_child(expr, &mut |child| {
        let result = find_star_expression(scope, child, star, false, in_columns)?;
        if result != StarExpressionType::None {
            has_star = result;
        }
        Ok(())
    })?;
    // DuckDB keeps a pointer to the COLUMNS star, so a `*` replaced inside its expression
    // (e.g. `COLUMNS(lambda c: ...)`, i.e. `list_filter(*, ...)`) is seen by the caller.
    if found_here && let ParsedExpressionKind::Star(current) = &expr.kind {
        *star = Some((**current).clone());
    }
    Ok(has_star)
}

/// `StarExpression::Equals`: aliases and query locations are ignored.
fn stars_equal(left: &StarExpression, right: &StarExpression) -> bool {
    let wrap = |star: &StarExpression| {
        ParsedExpression::new(
            ExpressionType::Star,
            ParsedExpressionKind::Star(Box::new(star.clone())),
        )
    };
    crate::parsed_equality::parsed_equals(&wrap(left), &wrap(right))
}

/// Replaces the star in `expr` by `replacement` (DuckDB's `ReplaceStarExpression`).
fn replace_star_expression(expr: &mut ParsedExpression, replacement: &ParsedExpression) -> Result<()> {
    if is_star(expr) || is_columns(expr) {
        let alias = std::mem::take(&mut expr.alias);
        *expr = replacement.clone();
        if !alias.is_empty() {
            expr.alias = alias;
        }
        return Ok(());
    }
    for_each_child(expr, &mut |child| replace_star_expression(child, replacement))
}

/// DuckDB's `ReplaceColumnsAlias`: `\0` is the column name, `\n` a regex group.
fn replace_columns_alias(alias: &str, column_name: &str, regex: Option<&regex::Regex>) -> Result<String> {
    let mut result = String::with_capacity(alias.len());
    let mut chars = alias.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        let Some(code) = chars.next() else {
            return Err(Error::binder(format!(
                "Unterminated backslash in COLUMNS(*) \"{alias}\" alias. Backslashes must either be escaped or \
                 followed by a number"
            )));
        };
        match code {
            '\\' => result.push('\\'),
            '0' => result.push_str(column_name),
            '1'..='9' => {
                let Some(regex) = regex else {
                    return Err(Error::binder(
                        "Only the backslash escape code \\0 can be used when no regex is supplied to COLUMNS(*)",
                    ));
                };
                let group = code as usize - '0' as usize;
                if let Some(captures) = regex.captures(column_name)
                    && let Some(matched) = captures.get(group)
                {
                    result.push_str(matched.as_str());
                }
            }
            _ => {
                return Err(Error::binder(format!(
                    "Invalid backslash code in COLUMNS(*) \"{alias}\" alias. Backslashes must either be escaped or \
                     followed by a number"
                )));
            }
        }
    }
    Ok(result)
}

/// Rewrites `* LIKE 'pattern'` into `COLUMNS(list_filter(*, x -> x LIKE 'pattern'))`
/// (DuckDB's `TryTransformStarLike`).
fn try_transform_star_like(root: &mut ParsedExpression) -> Result<()> {
    const SUPPORTED: [&str; 11] = [
        "~~",
        "!~~",
        "~~~",
        "!~~~",
        "~~*",
        "!~~*",
        "regexp_full_match",
        "not_like_escape",
        "ilike_escape",
        "not_ilike_escape",
        "like_escape",
    ];
    let inverse = root.expression_type == ExpressionType::OperatorNot;
    let location = root.query_location;
    let target: &mut ParsedExpression = if inverse {
        match &mut root.kind {
            ParsedExpressionKind::Operator(operator) => match operator.children.first_mut() {
                Some(child) => child,
                None => return Ok(()),
            },
            _ => return Ok(()),
        }
    } else {
        root
    };
    let ParsedExpressionKind::Function(function) = &mut target.kind else {
        return Ok(());
    };
    if !(2..=3).contains(&function.children.len()) {
        return Ok(());
    }
    let ParsedExpressionKind::Star(star) = &function.children[0].kind else {
        return Ok(());
    };
    if star.columns {
        return Ok(());
    }
    if !SUPPORTED.contains(&function.function_name.as_str()) {
        return Err(Error::binder(format!(
            "Function \"{}\" cannot be applied to a star expression",
            function.function_name
        ))
        .with_position(location));
    }
    if function.children[1].expression_class() != ExpressionClass::Constant {
        return Err(
            Error::binder("Pattern applied to a star expression must be a constant").with_position(location),
        );
    }
    if !star.rename_list.is_empty() {
        return Err(
            Error::binder("Rename list cannot be combined with a filtering operation")
                .with_position(location),
        );
    }
    if !star.replace_list.is_empty() {
        return Err(
            Error::binder("Replace list cannot be combined with a filtering operation")
                .with_position(location),
        );
    }
    let star = (**star).clone();
    let original_alias = root.alias.clone();
    let star_expr = std::mem::replace(
        &mut function_child(root, inverse)?.children[0],
        ParsedExpression::column_ref(["__lambda_col"]),
    );
    let regex_shortcut = !inverse
        && function_child(root, inverse)?.function_name == "regexp_full_match"
        && star.exclude_list.is_empty();
    let child_expr = if regex_shortcut {
        function_child(root, inverse)?.children.remove(1)
    } else {
        let body = std::mem::replace(
            root,
            ParsedExpression::constant(Value::null(LogicalType::SQLNULL)),
        );
        let lambda = ParsedExpression::new(
            ExpressionType::Lambda,
            ParsedExpressionKind::Lambda(LambdaExpression {
                lhs: Box::new(ParsedExpression::column_ref(["__lambda_col"])),
                expr: Box::new(body),
                syntax_type: LambdaSyntaxType::SingleArrowStorage,
            }),
        );
        function_call("list_filter", vec![star_expr, lambda])
    };
    let mut columns = StarExpression {
        relation_name: star.relation_name,
        exclude_list: Vec::new(),
        replace_list: CaseInsensitiveMap::default(),
        rename_list: rustdb_common::OrderedMap::default(),
        columns: true,
        expr: Some(Box::new(child_expr)),
    };
    if regex_shortcut {
        columns.exclude_list = star.exclude_list;
    }
    *root = ParsedExpression::new(
        ExpressionType::Star,
        ParsedExpressionKind::Star(Box::new(columns)),
    )
    .with_alias(original_alias);
    Ok(())
}

fn function_child(root: &mut ParsedExpression, inverse: bool) -> Result<&mut rustdb_ast::FunctionExpression> {
    let target = if inverse {
        match &mut root.kind {
            ParsedExpressionKind::Operator(operator) => operator.children.first_mut(),
            _ => None,
        }
    } else {
        Some(root)
    };
    match target.map(|target| &mut target.kind) {
        Some(ParsedExpressionKind::Function(function)) => Ok(function),
        _ => Err(Error::internal("star LIKE rewrite lost its function")),
    }
}

/// The column reference behind an expanded star entry (DuckDB's `GetResolvedColumnExpression`).
fn resolved_column_name(expr: &ParsedExpression) -> Option<String> {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(column) => column.column_names.last().cloned(),
        ParsedExpressionKind::Operator(operator)
            if expr.expression_type == ExpressionType::OperatorCoalesce =>
        {
            operator.children.first().and_then(resolved_column_name)
        }
        _ => None,
    }
}

/// Expands the star in `expr`, if any, into `new_select_list` (DuckDB's
/// `ExpandStarExpression`).
pub fn expand_star_expression(
    scope: &mut BindScope<'_>,
    mut expr: ParsedExpression,
    new_select_list: &mut Vec<ParsedExpression>,
) -> Result<()> {
    try_transform_star_like(&mut expr)?;
    let mut star = None;
    let star_type = find_star_expression(scope, &mut expr, &mut star, true, false)?;
    let Some(star) = star.filter(|_| star_type != StarExpressionType::None) else {
        new_select_list.push(expr);
        return Ok(());
    };
    let mut star_list = scope.bind_context.generate_all_column_expressions(&star)?;
    let mut regex = None;
    if let Some(columns_expr) = &star.expr {
        let mut child = (**columns_expr).clone();
        let value = {
            let mut binder = {
                let (inner, outer) = scope.split();
                TableFunctionBinder::new(inner, outer, "", "COLUMNS expression")
            };
            let result = binder.bind(&mut child, true)?;
            if !result.is_foldable() {
                if result.has_parameter() {
                    return Err(parameter_not_resolved());
                }
                return Err(Error::binder("Unsupported expression in COLUMNS"));
            }
            scope.context.evaluator.evaluate_scalar(&result)?
        };
        let star_location = None;
        match value.logical_type().id() {
            LogicalTypeId::Varchar => {
                if value.is_null() {
                    return Err(Error::binder("COLUMNS does not support NULL as regex argument"));
                }
                let pattern = value.to_varchar()?;
                let compiled = regex::Regex::new(&pattern).map_err(|error| {
                    Error::binder(format!("Failed to compile regex \"{pattern}\": {error}"))
                        .with_position(star_location)
                })?;
                let mut candidates = Vec::new();
                let mut new_list = Vec::new();
                for expanded in star_list {
                    let Some(name) = resolved_column_name(&expanded) else {
                        continue;
                    };
                    if compiled.is_match(&name) {
                        new_list.push(expanded);
                    } else {
                        candidates.push(name);
                    }
                }
                if new_list.is_empty() {
                    let suggestion = if candidates.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n{}",
                            candidates_error_message(&candidates, &pattern, "Did you mean")
                        )
                    };
                    return Err(Error::binder(format!(
                        "No matching columns found that match regex \"{pattern}\"{suggestion}"
                    )));
                }
                star_list = new_list;
                regex = Some(compiled);
            }
            LogicalTypeId::List
                if value.logical_type().child_type().map(LogicalType::id) == Some(LogicalTypeId::Varchar) =>
            {
                let children = value.children().unwrap_or_default();
                if value.is_null() || children.is_empty() {
                    return Err(Error::binder(format!(
                        "Star expression \"{star}\" resulted in an empty set of columns"
                    )));
                }
                let mut selected: Vec<(String, bool)> = Vec::new();
                for child in children {
                    if child.is_null() {
                        return Err(Error::binder(
                            "Columns expression does not support NULL input parameters",
                        ));
                    }
                    let name = child.to_varchar()?;
                    if !selected
                        .iter()
                        .any(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                    {
                        selected.push((name, false));
                    }
                }
                let mut new_list = Vec::new();
                for expanded in star_list {
                    let name = columns_string_value(&expanded);
                    if let Some(entry) = selected.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                        entry.1 = true;
                        new_list.push(expanded);
                    }
                }
                if let Some((name, _)) = selected.iter().find(|(_, found)| !found) {
                    return Err(Error::binder(format!(
                        "Column \"{name}\" was selected but was not found in the FROM clause"
                    )));
                }
                star_list = new_list;
            }
            _ => {
                return Err(Error::binder(
                    "COLUMNS expects either a VARCHAR argument (regex) or a LIST of VARCHAR (list of columns)",
                ));
            }
        }
    }
    if star_type == StarExpressionType::Unpacked {
        if expr.expression_type == ExpressionType::OperatorUnpack {
            return Err(Error::binder(
                "*COLUMNS not allowed at the root level, use COLUMNS instead",
            ));
        }
        replace_unpacked_star_expression(&mut expr, &star_list, &star, regex.as_ref())?;
        new_select_list.push(expr);
        return Ok(());
    }
    for replacement in &star_list {
        let mut new_expr = expr.clone();
        replace_star_expression(&mut new_expr, replacement)?;
        if star.columns
            && let Some(column) = resolved_column_name(replacement)
        {
            new_expr.alias = if new_expr.alias.is_empty() {
                column
            } else {
                replace_columns_alias(&new_expr.alias, &column, regex.as_ref())?
            };
        }
        new_select_list.push(new_expr);
    }
    Ok(())
}

/// The expressions an unpacked `*COLUMNS(...)` argument stands for (DuckDB's `AddChild`
/// in `bind_unpacked_star_expression.cpp`); other arguments are kept.
fn add_unpacked_child(
    child: ParsedExpression,
    new_children: &mut Vec<ParsedExpression>,
    star_list: &[ParsedExpression],
    star: &StarExpression,
    regex: Option<&regex::Regex>,
) -> Result<()> {
    if child.expression_type != ExpressionType::OperatorUnpack {
        new_children.push(child);
        return Ok(());
    }
    let ParsedExpressionKind::Operator(operator) = child.kind else {
        return Err(Error::internal("UNPACK without children"));
    };
    let unpack_child = operator
        .children
        .into_iter()
        .next()
        .ok_or_else(|| Error::internal("UNPACK without children"))?;
    for replacement in star_list {
        let mut new_expr = unpack_child.clone();
        replace_star_expression(&mut new_expr, replacement)?;
        if star.columns
            && let Some(column) = resolved_column_name(replacement)
        {
            new_expr.alias = if new_expr.alias.is_empty() {
                column
            } else {
                replace_columns_alias(&new_expr.alias, &column, regex)?
            };
        }
        new_children.push(new_expr);
    }
    Ok(())
}

/// Expands `*COLUMNS(...)` arguments of functions and of COALESCE/IN in place (DuckDB's
/// `Binder::ReplaceUnpackedStarExpression`).
fn replace_unpacked_star_expression(
    expr: &mut ParsedExpression,
    star_list: &[ParsedExpression],
    star: &StarExpression,
    regex: Option<&regex::Regex>,
) -> Result<()> {
    let expression_type = expr.expression_type;
    match &mut expr.kind {
        ParsedExpressionKind::Function(function) => {
            let mut new_children = Vec::with_capacity(function.children.len());
            for child in std::mem::take(&mut function.children) {
                add_unpacked_child(child, &mut new_children, star_list, star, regex)?;
            }
            function.children = new_children;
            let orders = &mut function.order_bys.orders;
            if !orders.is_empty() {
                let mut new_orders = Vec::with_capacity(orders.len());
                for order in orders.iter() {
                    add_unpacked_child(
                        (*order.expression).clone(),
                        &mut new_orders,
                        star_list,
                        star,
                        regex,
                    )?;
                }
                if new_orders.len() != orders.len() {
                    return Err(Error::not_implemented(
                        "*COLUMNS(...) is not supported in the order expression",
                    ));
                }
                for (order, expression) in orders.iter_mut().zip(new_orders) {
                    *order.expression = expression;
                }
            }
        }
        ParsedExpressionKind::Operator(operator) => {
            if expression_type == ExpressionType::OperatorUnpack {
                return Err(Error::binder("*COLUMNS() can not be used in this place"));
            }
            if !matches!(
                expression_type,
                ExpressionType::OperatorCoalesce | ExpressionType::CompareIn | ExpressionType::CompareNotIn
            ) {
                return Err(Error::binder(format!(
                    "*COLUMNS() can not be used together with the '{}' operator",
                    expression_type.name()
                )));
            }
            let mut new_children = Vec::with_capacity(operator.children.len());
            for child in std::mem::take(&mut operator.children) {
                add_unpacked_child(child, &mut new_children, star_list, star, regex)?;
            }
            operator.children = new_children;
        }
        _ => {}
    }
    for_each_child(expr, &mut |child| {
        replace_unpacked_star_expression(child, star_list, star, regex)
    })
}
