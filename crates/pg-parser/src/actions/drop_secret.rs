//! Semantic actions for the rules in `grammar/statements/drop_secret.y`.

use super::prelude::*;
use rustdb_common::ByteString;

/// ```text
/// DropSecretStmt:
///       DROP opt_persist SECRET ColId opt_storage_drop_specifier
///     | DROP opt_persist SECRET IF_P EXISTS ColId opt_storage_drop_specifier
/// ```
pub(super) fn drop_secret_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DROP opt_persist SECRET ColId opt_storage_drop_specifier
        0 => {
            let n = DropSecretStmt {
                persist_type: rhs.str(2)?,
                secret_name: rhs.str(4)?,
                secret_storage: rhs.str(5)?,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // DROP opt_persist SECRET IF_P EXISTS ColId opt_storage_drop_specifier
        1 => {
            let n = DropSecretStmt {
                persist_type: rhs.str(2)?,
                secret_name: rhs.str(6)?,
                secret_storage: rhs.str(7)?,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_storage_drop_specifier:
///       /* empty */
///     | FROM IDENT
/// ```
pub(super) fn opt_storage_drop_specifier<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Str(Some(ByteString::new()))),
        _ => Err(rhs.bad_alternative()),
    }
}
