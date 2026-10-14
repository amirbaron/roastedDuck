//! Semantic actions for the rules in `grammar/statements/pragma.y`.

use super::prelude::*;

/// ```text
/// PragmaStmt:
///       PRAGMA_P ColId
///     | PRAGMA_P ColId '=' var_list
///     | PRAGMA_P ColId '(' func_arg_list ')'
/// ```
pub(super) fn pragma_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // PRAGMA_P ColId
        0 => {
            let n = PragmaStmt {
                kind: PragmaKind::Nothing,
                name: rhs.str(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // PRAGMA_P ColId '=' var_list
        1 => {
            let n = PragmaStmt {
                kind: PragmaKind::Assignment,
                name: rhs.str(2)?,
                args: rhs.list(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // PRAGMA_P ColId '(' func_arg_list ')'
        2 => {
            let n = PragmaStmt {
                kind: PragmaKind::Call,
                name: rhs.str(2)?,
                args: rhs.list(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
