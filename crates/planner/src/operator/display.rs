//! A textual rendering of logical plans for debugging and plan-shape comparisons: one line
//! per operator with its type and expressions, children indented below.

use std::fmt::Write as _;

use super::{LogicalKind, LogicalOperator};
use crate::expression::expression_list_to_string;

/// Renders `plan` as an indented tree.
pub fn render_plan(plan: &LogicalOperator) -> String {
    let mut out = String::new();
    render(plan, 0, &mut out);
    out
}

fn render(op: &LogicalOperator, depth: usize, out: &mut String) {
    let _ = write!(out, "{}{}", "  ".repeat(depth), op.operator_type().name());
    let details = match &op.kind {
        LogicalKind::Aggregate(aggregate) => format!(
            " groups=[{}] aggregates=[{}]",
            expression_list_to_string(&aggregate.groups),
            expression_list_to_string(&op.expressions)
        ),
        LogicalKind::Order { orders, .. } | LogicalKind::TopN { orders, .. } => {
            let orders: Vec<String> = orders.iter().map(ToString::to_string).collect();
            format!(" [{}]", orders.join(", "))
        }
        LogicalKind::ComparisonJoin(join) => {
            let conditions: Vec<String> = join
                .conditions
                .iter()
                .map(|c| {
                    format!(
                        "{} {} {}",
                        c.left,
                        rustdb_ast::expression_type_to_operator(c.comparison),
                        c.right
                    )
                })
                .collect();
            format!(" {} [{}]", join.join_type.name(), conditions.join(", "))
        }
        LogicalKind::AnyJoin { join_type, condition } => format!(" {} [{condition}]", join_type.name()),
        LogicalKind::ExpressionGet { expressions, .. } => {
            let rows: Vec<String> = expressions
                .iter()
                .map(|row| format!("({})", expression_list_to_string(row)))
                .collect();
            format!(" {}", rows.join(", "))
        }
        _ if !op.expressions.is_empty() => format!(" [{}]", expression_list_to_string(&op.expressions)),
        _ => String::new(),
    };
    out.push_str(&details);
    out.push('\n');
    for child in &op.children {
        render(child, depth + 1, out);
    }
}
