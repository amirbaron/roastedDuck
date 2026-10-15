//! The shape of a logical plan: operator types, their expressions (class and type, with
//! nested expressions) and children. Computed from rustdb's plan and from DuckDB's
//! `json_serialize_plan`, it shows whether the binder built the same plan where rustdb
//! cannot execute it yet.

use rustdb_planner::operator::LogicalKind;
use rustdb_planner::{Expression, LogicalOperator};
use rustdb_serde::json::{JsonValue, parse_json};

fn join(parts: impl IntoIterator<Item = String>) -> String {
    parts.into_iter().collect::<Vec<_>>().join(",")
}

fn rust_expression(expression: &Expression) -> String {
    let children = expression.children();
    let head = format!(
        "{}/{}",
        expression.expression_class().name(),
        expression.expression_type.name()
    );
    if children.is_empty() {
        head
    } else {
        format!("{head}{{{}}}", join(children.into_iter().map(rust_expression)))
    }
}

/// The shape of rustdb's (resolved, unoptimized) plan.
pub fn rust_shape(op: &LogicalOperator) -> String {
    let mut text = op.operator_type().name().to_owned();
    if let LogicalKind::ExpressionGet { expressions, .. } = &op.kind {
        let rows = expressions
            .iter()
            .map(|row| format!("({})", join(row.iter().map(rust_expression))));
        text.push_str(&format!("[{}]", join(rows)));
    }
    if !op.expressions.is_empty() {
        text.push_str(&format!("[{}]", join(op.expressions.iter().map(rust_expression))));
    }
    if let LogicalKind::Aggregate(aggregate) = &op.kind
        && !aggregate.groups.is_empty()
    {
        text.push_str(&format!("g[{}]", join(aggregate.groups.iter().map(rust_expression))));
    }
    if !op.children.is_empty() {
        text.push_str(&format!("({})", join(op.children.iter().map(rust_shape))));
    }
    text
}

fn string_field<'a>(value: &'a JsonValue, key: &str) -> &'a str {
    match value.get(key) {
        Some(JsonValue::Str(text)) => text,
        _ => "",
    }
}

/// The expressions nested anywhere inside `value`, in document order.
fn nested_expressions<'a>(value: &'a JsonValue, out: &mut Vec<&'a JsonValue>) {
    match value {
        JsonValue::Object(entries) => {
            if value.get("expression_class").is_some() {
                out.push(value);
                return;
            }
            for (key, child) in entries {
                if key != "value" {
                    nested_expressions(child, out);
                }
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                nested_expressions(item, out);
            }
        }
        _ => {}
    }
}

fn json_expression(expression: &JsonValue) -> String {
    let mut children = Vec::new();
    if let JsonValue::Object(entries) = expression {
        for (key, child) in entries {
            if key != "value" {
                nested_expressions(child, &mut children);
            }
        }
    }
    let head = format!(
        "{}/{}",
        string_field(expression, "expression_class"),
        string_field(expression, "type")
    );
    if children.is_empty() {
        head
    } else {
        format!("{head}{{{}}}", join(children.into_iter().map(json_expression)))
    }
}

fn json_operator(op: &JsonValue) -> String {
    let mut text = string_field(op, "type").to_owned();
    let list = |key: &str| match op.get(key) {
        Some(JsonValue::Array(items)) => items.iter().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let expressions = list("expressions");
    if !expressions.is_empty() {
        // The rows of an expression get are lists themselves.
        let shape = |item: &JsonValue| match item {
            JsonValue::Array(row) => format!("({})", join(row.iter().map(json_expression))),
            _ => json_expression(item),
        };
        text.push_str(&format!("[{}]", join(expressions.into_iter().map(shape))));
    }
    let groups = list("groups");
    if !groups.is_empty() {
        text.push_str(&format!("g[{}]", join(groups.into_iter().map(json_expression))));
    }
    let children = list("children");
    if !children.is_empty() {
        text.push_str(&format!("({})", join(children.into_iter().map(json_operator))));
    }
    text
}

/// The shape of the first plan of a `json_serialize_plan` document, or its error.
pub fn duckdb_shape(json: &str) -> Result<String, String> {
    let root = parse_json(json).map_err(|error| format!("<unreadable plan JSON: {error}>"))?;
    if matches!(root.get("error"), Some(JsonValue::Bool(true))) {
        return Err(format!(
            "{}: {}",
            string_field(&root, "error_type"),
            string_field(&root, "error_message")
        ));
    }
    match root.get("plans") {
        Some(JsonValue::Array(plans)) if !plans.is_empty() => Ok(json_operator(&plans[0])),
        _ => Err("<no plans>".to_owned()),
    }
}
