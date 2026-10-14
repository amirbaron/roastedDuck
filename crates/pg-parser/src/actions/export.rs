//! Semantic actions for the rules in `grammar/statements/export.y`.

use super::prelude::*;

/// ```text
/// ExportStmt:
///       EXPORT_P DATABASE Sconst copy_options
///     | EXPORT_P DATABASE ColId TO Sconst copy_options
/// ```
pub(super) fn export_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // EXPORT_P DATABASE Sconst copy_options
        0 => {
            let copy_options = rhs.list(4)?;
            let mut n = ExportStmt {
                database: None,
                filename: rhs.str(3)?,
                options: List::new(),
                ..Default::default()
            };
            if !copy_options.is_empty() {
                n.options = concat(n.options, copy_options);
            }
            Ok(SemValue::node(Node::from(n)))
        }
        // EXPORT_P DATABASE ColId TO Sconst copy_options
        1 => {
            let copy_options = rhs.list(6)?;
            let mut n = ExportStmt {
                database: rhs.str(3)?,
                filename: rhs.str(5)?,
                options: List::new(),
                ..Default::default()
            };
            if !copy_options.is_empty() {
                n.options = concat(n.options, copy_options);
            }
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ImportStmt:
///       IMPORT_P DATABASE Sconst
/// ```
pub(super) fn import_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // IMPORT_P DATABASE Sconst
        0 => {
            let n = ImportStmt {
                filename: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
