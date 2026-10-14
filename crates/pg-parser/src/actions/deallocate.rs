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
            let n = DeallocateStmt { name: rhs.str(2)? };
            Ok(SemValue::node(n))
        }
        // DEALLOCATE PREPARE name
        1 => {
            let n = DeallocateStmt { name: rhs.str(3)? };
            Ok(SemValue::node(n))
        }
        // DEALLOCATE ALL
        // DEALLOCATE PREPARE ALL
        2 | 3 => {
            let n = DeallocateStmt { name: None };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
