//! Semantic actions for the rules in `grammar/statements/delete.y`.

use super::prelude::*;

/// ```text
/// DeleteStmt:
///       opt_with_clause DELETE_P FROM relation_expr_opt_alias using_clause where_or_current_clause returning_clause
///     | TRUNCATE opt_table relation_expr_opt_alias
/// ```
pub(super) fn delete_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_with_clause DELETE_P FROM relation_expr_opt_alias using_clause where_or_current_clause returning_clause
        0 => {
            let n = DeleteStmt {
                relation: rhs.ptr::<RangeVar>(4)?,
                using_clause: rhs.list(5)?,
                where_clause: rhs.node(6)?,
                returning_list: rhs.list(7)?,
                with_clause: rhs.ptr::<WithClause>(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // TRUNCATE opt_table relation_expr_opt_alias
        1 => {
            let n = DeleteStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                using_clause: List::new(),
                where_clause: None,
                returning_list: List::new(),
                with_clause: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// relation_expr_opt_alias:
///       relation_expr
///     | relation_expr ColId
///     | relation_expr AS ColId
/// ```
pub(super) fn relation_expr_opt_alias<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // relation_expr
        0 => Ok(SemValue::Node((rhs.ptr::<RangeVar>(1)?).map(Node::from))),
        // relation_expr ColId
        1 => {
            let mut relation_expr = rhs.take::<RangeVar>(1)?;
            let alias = Alias {
                aliasname: rhs.str(2)?,
                ..Default::default()
            };
            relation_expr.alias = Some(Box::new(alias));
            Ok(SemValue::node(relation_expr))
        }
        // relation_expr AS ColId
        2 => {
            let mut relation_expr = rhs.take::<RangeVar>(1)?;
            let alias = Alias {
                aliasname: rhs.str(3)?,
                ..Default::default()
            };
            relation_expr.alias = Some(Box::new(alias));
            Ok(SemValue::node(relation_expr))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// where_or_current_clause:
///       WHERE a_expr
///     | /* empty */
/// ```
pub(super) fn where_or_current_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WHERE a_expr
        0 => Ok(SemValue::Node(rhs.node(2)?)),
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// using_clause:
///       USING from_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn using_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // USING from_list_opt_comma
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
