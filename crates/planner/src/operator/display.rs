//! A textual rendering of logical plans for debugging and plan-shape comparisons: one line
//! per operator with its type and expressions, children indented below.

use rustdb_common::{ByteString, byte_format, byte_write};

use super::{LogicalKind, LogicalOperator};
use crate::expression::expression_list_to_string;

/// Renders `plan` as an indented tree.
pub fn render_plan(plan: &LogicalOperator) -> ByteString {
    let mut out = ByteString::new();
    render(plan, 0, &mut out);
    out
}

fn render(op: &LogicalOperator, depth: usize, out: &mut ByteString) {
    let _ = byte_write!(out, "{}{}", "  ".repeat(depth), op.operator_type().name());
    let details = match &op.kind {
        LogicalKind::Aggregate(aggregate) => byte_format!(
            " groups=[{}] aggregates=[{}]",
            expression_list_to_string(&aggregate.groups),
            expression_list_to_string(&op.expressions)
        ),
        LogicalKind::Order { orders, .. } | LogicalKind::TopN { orders, .. } => {
            let orders: Vec<ByteString> = orders
                .iter()
                .map(crate::expression::BoundOrderByNode::to_bytes)
                .collect();
            byte_format!(" [{}]", ByteString::join(&orders, b", "))
        }
        LogicalKind::ComparisonJoin(join) => {
            let conditions: Vec<ByteString> = join
                .conditions
                .iter()
                .map(|c| {
                    byte_format!(
                        "{} {} {}",
                        c.left.to_bytes(),
                        rustdb_ast::expression_type_to_operator(c.comparison),
                        c.right.to_bytes()
                    )
                })
                .collect();
            byte_format!(
                " {} [{}]",
                join.join_type.name(),
                ByteString::join(&conditions, b", ")
            )
        }
        LogicalKind::AnyJoin { join_type, condition } => {
            byte_format!(" {} [{}]", join_type.name(), condition.to_bytes())
        }
        LogicalKind::ExpressionGet { expressions, .. } => {
            let rows: Vec<ByteString> = expressions
                .iter()
                .map(|row| byte_format!("({})", expression_list_to_string(row)))
                .collect();
            byte_format!(" {}", ByteString::join(&rows, b", "))
        }
        _ if !op.expressions.is_empty() => byte_format!(" [{}]", expression_list_to_string(&op.expressions)),
        _ => ByteString::new(),
    };
    out.push_bytes(&details);
    out.push(b'\n');
    for child in &op.children {
        render(child, depth + 1, out);
    }
}
