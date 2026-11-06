//! Semantic actions for the rules in `grammar/statements/update_extensions.y`.

use super::prelude::*;

/// ```text
/// UpdateExtensionsStmt:
///       opt_with_clause UPDATE EXTENSIONS opt_column_list
/// ```
pub(super) fn update_extensions_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_with_clause UPDATE EXTENSIONS opt_column_list
        0 => {
            let n = UpdateExtensionsStmt {
                extensions: rhs.list(4)?,
            };
            if rhs.ptr::<WithClause>(1)?.is_some() {
                return Err(rhs.error_at(
                    "Providing a with clause with an UPDATE EXTENSIONS statement is not allowed",
                    rhs.loc(1),
                ));
            }
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
