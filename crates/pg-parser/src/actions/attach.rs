//! Semantic actions for the rules in `grammar/statements/attach.y`.

use super::prelude::*;

/// ```text
/// AttachStmt:
///       ATTACH opt_database Sconst opt_database_alias opt_attach_options
///     | ATTACH IF_P NOT EXISTS opt_database Sconst opt_database_alias opt_attach_options
///     | ATTACH OR REPLACE opt_database Sconst opt_database_alias opt_attach_options
/// ```
pub(super) fn attach_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ATTACH opt_database Sconst opt_database_alias opt_attach_options
        0 => {
            let n = AttachStmt {
                path: rhs.str(3)?,
                name: rhs.str(4)?,
                options: rhs.list(5)?,
                onconflict: OnCreateConflict::Error,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ATTACH IF_P NOT EXISTS opt_database Sconst opt_database_alias opt_attach_options
        1 => {
            let n = AttachStmt {
                path: rhs.str(6)?,
                name: rhs.str(7)?,
                options: rhs.list(8)?,
                onconflict: OnCreateConflict::Ignore,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ATTACH OR REPLACE opt_database Sconst opt_database_alias opt_attach_options
        2 => {
            let n = AttachStmt {
                path: rhs.str(5)?,
                name: rhs.str(6)?,
                options: rhs.list(7)?,
                onconflict: OnCreateConflict::Replace,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// DetachStmt:
///       DETACH ColLabel
///     | DETACH DATABASE ColLabel
///     | DETACH DATABASE IF_P EXISTS ColLabel
/// ```
pub(super) fn detach_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DETACH ColLabel
        0 => {
            let n = DetachStmt {
                missing_ok: false,
                db_name: rhs.str(2)?,
            };
            Ok(SemValue::node(n))
        }
        // DETACH DATABASE ColLabel
        1 => {
            let n = DetachStmt {
                missing_ok: false,
                db_name: rhs.str(3)?,
            };
            Ok(SemValue::node(n))
        }
        // DETACH DATABASE IF_P EXISTS ColLabel
        2 => {
            let n = DetachStmt {
                missing_ok: true,
                db_name: rhs.str(5)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_database_alias:
///       AS ColId
///     | /* empty */
/// ```
pub(super) fn opt_database_alias<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_opt_arg:
///       a_expr
///     | /* empty */
/// ```
pub(super) fn generic_opt_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_opt_elem:
///       ColLabel generic_opt_arg
/// ```
pub(super) fn generic_opt_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColLabel generic_opt_arg
        0 => Ok(SemValue::node(make_def_elem(
            rhs.string(1)?,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_opt_list:
///       generic_opt_elem
///     | generic_opt_list ',' generic_opt_elem
/// ```
pub(super) fn generic_opt_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // generic_opt_elem
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // generic_opt_list ',' generic_opt_elem
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_attach_options:
///       '(' generic_opt_list ')'
///     | /* empty */
/// ```
pub(super) fn opt_attach_options<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
