//! Semantic actions for the rules in `grammar/statements/update.y`.

use super::prelude::*;

/// ```text
/// UpdateStmt:
///       opt_with_clause UPDATE relation_expr_opt_alias SET set_clause_list_opt_comma from_clause where_or_current_clause returning_clause
/// ```
pub(super) fn update_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_with_clause UPDATE relation_expr_opt_alias SET set_clause_list_opt_comma from_clause where_or_current_clause returning_clause
        0 => {
            let n = UpdateStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                target_list: rhs.list(5)?,
                from_clause: rhs.list(6)?,
                where_clause: rhs.node(7)?,
                returning_list: rhs.list(8)?,
                with_clause: rhs.ptr::<WithClause>(1)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
