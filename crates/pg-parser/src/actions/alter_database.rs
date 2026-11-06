//! Semantic actions for the rules in `grammar/statements/alter_database.y`.

use super::prelude::*;

/// ```text
/// AlterDatabaseStmt:
///       ALTER DATABASE ColId SET IDENT TO ColId
///     | ALTER DATABASE IF_P EXISTS ColId SET IDENT TO ColId
/// ```
pub(super) fn alter_database_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALTER DATABASE ColId SET IDENT TO ColId
        0 => {
            let ident = rhs.str(5)?;
            if !ident
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(b"alias")
            {
                return Err(rhs.error_at(
                    rustdb_common::byte_format!(
                        "expected SET ALIAS TO, got SET {} TO",
                        c_str(ident.as_deref())
                    ),
                    rhs.loc(5),
                ));
            }
            let n = AlterDatabaseStmt {
                dbname: rhs.str(3)?,
                new_name: rhs.str(7)?,
                alter_type: AlterDatabaseType::Rename,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER DATABASE IF_P EXISTS ColId SET IDENT TO ColId
        1 => {
            let ident = rhs.str(7)?;
            if !ident
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(b"alias")
            {
                return Err(rhs.error_at(
                    rustdb_common::byte_format!(
                        "expected SET ALIAS TO, got SET {} TO",
                        c_str(ident.as_deref())
                    ),
                    rhs.loc(7),
                ));
            }
            let n = AlterDatabaseStmt {
                dbname: rhs.str(5)?,
                new_name: rhs.str(9)?,
                alter_type: AlterDatabaseType::Rename,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
