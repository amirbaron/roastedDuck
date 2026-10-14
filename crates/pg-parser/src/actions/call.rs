//! Semantic actions for the rules in `grammar/statements/call.y`.

use super::prelude::*;

/// ```text
/// CallStmt:
///       CALL_P func_application
/// ```
pub(super) fn call_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CALL_P func_application
        0 => {
            let n = CallStmt {
                func: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
