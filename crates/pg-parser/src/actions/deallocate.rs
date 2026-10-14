//! Semantic actions for the rules in `grammar/statements/deallocate.y`.

use super::prelude::*;

/// ```text
/// DeallocateStmt:
///       DEALLOCATE name
///     | DEALLOCATE PREPARE name
///     | DEALLOCATE ALL
///     | DEALLOCATE PREPARE ALL
/// ```
pub(super) fn deallocate_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DEALLOCATE name
        0 => {
            let n = DeallocateStmt {
                name: rhs.str(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DEALLOCATE PREPARE name
        1 => {
            let n = DeallocateStmt {
                name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DEALLOCATE ALL
        2 => {
            let n = DeallocateStmt {
                name: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DEALLOCATE PREPARE ALL
        3 => {
            let n = DeallocateStmt {
                name: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
