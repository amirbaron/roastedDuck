//! Semantic actions for the rules in `grammar/statements/create_secret.y`.

use super::prelude::*;
use rustdb_common::ByteString;

/// ```text
/// CreateSecretStmt:
///       CREATE_P opt_persist SECRET opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
///     | CREATE_P opt_persist SECRET IF_P NOT EXISTS opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
///     | CREATE_P OR REPLACE opt_persist SECRET opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
/// ```
pub(super) fn create_secret_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P opt_persist SECRET opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
        0 => {
            let n = CreateSecretStmt {
                persist_type: rhs.str(2)?,
                secret_name: rhs.str(4)?,
                secret_storage: rhs.str(5)?,
                options: rhs.list(7)?,
                onconflict: OnCreateConflict::Error,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // CREATE_P opt_persist SECRET IF_P NOT EXISTS opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
        1 => {
            let n = CreateSecretStmt {
                persist_type: rhs.str(2)?,
                secret_name: rhs.str(7)?,
                secret_storage: rhs.str(8)?,
                options: rhs.list(10)?,
                onconflict: OnCreateConflict::Ignore,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // CREATE_P OR REPLACE opt_persist SECRET opt_secret_name opt_storage_specifier '(' create_secret_generic_opt_list ')'
        2 => {
            let n = CreateSecretStmt {
                persist_type: rhs.str(4)?,
                secret_name: rhs.str(6)?,
                secret_storage: rhs.str(7)?,
                options: rhs.list(9)?,
                onconflict: OnCreateConflict::Replace,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_secret_name:
///       /* empty */
///     | ColId
/// ```
pub(super) fn opt_secret_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_persist:
///       /* empty */
///     | TEMPORARY
///     | PERSISTENT
/// ```
pub(super) fn opt_persist<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Str(Some("default".into()))),
        // TEMPORARY
        1 => Ok(SemValue::Str(Some("temporary".into()))),
        // PERSISTENT
        2 => Ok(SemValue::Str(Some("persistent".into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_storage_specifier:
///       /* empty */
///     | IN_P IDENT
/// ```
pub(super) fn opt_storage_specifier<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Str(Some(ByteString::new()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// create_secret_generic_opt_elem:
///       ColLabel create_secret_generic_opt_arg
/// ```
pub(super) fn create_secret_generic_opt_elem<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // ColLabel create_secret_generic_opt_arg
        0 => Ok(SemValue::node(make_def_elem(
            rhs.string(1)?,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// create_secret_generic_opt_list:
///       create_secret_generic_opt_elem
///     | create_secret_generic_opt_list ',' create_secret_generic_opt_elem
/// ```
pub(super) fn create_secret_generic_opt_list<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // create_secret_generic_opt_elem
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // create_secret_generic_opt_list ',' create_secret_generic_opt_elem
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}
