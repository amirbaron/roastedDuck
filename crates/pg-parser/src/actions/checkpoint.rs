//! Semantic actions for the rules in `grammar/statements/checkpoint.y`.

use super::prelude::*;

/// ```text
/// CheckPointStmt:
///       FORCE CHECKPOINT opt_col_id
///     | CHECKPOINT opt_col_id
/// ```
pub(super) fn check_point_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FORCE CHECKPOINT opt_col_id
        0 => {
            let n = CheckPointStmt {
                force: true,
                name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // CHECKPOINT opt_col_id
        1 => {
            let n = CheckPointStmt {
                force: false,
                name: rhs.str(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_col_id:
///       ColId
///     | /* empty */
/// ```
pub(super) fn opt_col_id<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // /* empty */
        1 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}
