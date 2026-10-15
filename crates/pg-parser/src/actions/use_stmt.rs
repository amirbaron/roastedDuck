//! Semantic actions for the rules in `grammar/statements/use.y`.

use super::prelude::*;

/// ```text
/// UseStmt:
///       USE_P qualified_name
/// ```
pub(super) fn use_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // USE_P qualified_name
        0 => {
            let n = UseStmt {
                name: rhs.ptr::<RangeVar>(2)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
