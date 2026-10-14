//! Semantic actions for the rules in `grammar/statements/prepare.y`.

use super::prelude::*;

/// ```text
/// PrepareStmt:
///       PREPARE name prep_type_clause AS PreparableStmt
/// ```
pub(super) fn prepare_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // PREPARE name prep_type_clause AS PreparableStmt
        0 => {
            let n = PrepareStmt {
                name: rhs.str(2)?,
                argtypes: rhs.list(3)?,
                query: rhs.node(5)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// prep_type_clause:
///       '(' type_list ')'
///     | /* empty */
/// ```
pub(super) fn prep_type_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' type_list ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
