//! Semantic actions for the rules in `grammar/statements/index.y`.

use super::prelude::*;

/// ```text
/// IndexStmt:
///       CREATE_P opt_unique INDEX opt_concurrently opt_index_name ON qualified_name access_method_clause '(' index_params ')' opt_reloptions where_clause
///     | CREATE_P opt_unique INDEX opt_concurrently IF_P NOT EXISTS index_name ON qualified_name access_method_clause '(' index_params ')' opt_reloptions where_clause
/// ```
pub(super) fn index_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P opt_unique INDEX opt_concurrently opt_index_name ON qualified_name access_method_clause '(' index_params ')' opt_reloptions where_clause
        0 => {
            let n = IndexStmt {
                unique: rhs.bool(2)?,
                concurrent: rhs.bool(4)?,
                idxname: rhs.str(5)?,
                relation: rhs.ptr::<RangeVar>(7)?,
                access_method: rhs.str(8)?,
                index_params: rhs.list(10)?,
                options: rhs.list(12)?,
                where_clause: rhs.node(13)?,
                onconflict: OnCreateConflict::Error,
            };
            Ok(SemValue::node(n))
        }
        // CREATE_P opt_unique INDEX opt_concurrently IF_P NOT EXISTS index_name ON qualified_name access_method_clause '(' index_params ')' opt_reloptions where_clause
        1 => {
            let n = IndexStmt {
                unique: rhs.bool(2)?,
                concurrent: rhs.bool(4)?,
                idxname: rhs.str(8)?,
                relation: rhs.ptr::<RangeVar>(10)?,
                access_method: rhs.str(11)?,
                index_params: rhs.list(13)?,
                options: rhs.list(15)?,
                where_clause: rhs.node(16)?,
                onconflict: OnCreateConflict::Ignore,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// access_method_clause:
///       USING access_method
///     | /* empty */
/// ```
pub(super) fn access_method_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Str(Some("art".to_owned()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_concurrently:
///       CONCURRENTLY
///     | /* empty */
/// ```
pub(super) fn opt_concurrently<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CONCURRENTLY
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_index_name:
///       index_name
///     | /* empty */
/// ```
pub(super) fn opt_index_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_reloptions:
///       WITH reloptions
///     | /* empty */
/// ```
pub(super) fn opt_reloptions<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_unique:
///       UNIQUE
///     | /* empty */
/// ```
pub(super) fn opt_unique<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // UNIQUE
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}
