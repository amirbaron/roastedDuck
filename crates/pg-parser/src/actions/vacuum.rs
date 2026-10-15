//! Semantic actions for the rules in `grammar/statements/vacuum.y`.

use super::prelude::*;

/// ```text
/// VacuumStmt:
///       VACUUM opt_full opt_freeze opt_verbose
///     | VACUUM opt_full opt_freeze opt_verbose qualified_name opt_name_list
///     | VACUUM opt_full opt_freeze opt_verbose AnalyzeStmt
///     | VACUUM '(' vacuum_option_list ')'
///     | VACUUM '(' vacuum_option_list ')' qualified_name opt_name_list
/// ```
pub(super) fn vacuum_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // VACUUM opt_full opt_freeze opt_verbose
        0 => {
            let mut n = VacuumStmt {
                options: vacuum_option::VACUUM,
                ..Default::default()
            };
            if rhs.bool(2)? {
                n.options |= vacuum_option::FULL;
            }
            if rhs.bool(3)? {
                n.options |= vacuum_option::FREEZE;
            }
            if rhs.bool(4)? {
                n.options |= vacuum_option::VERBOSE;
            }
            n.relation = None;
            n.va_cols = List::new();
            Ok(SemValue::node(n))
        }
        // VACUUM opt_full opt_freeze opt_verbose qualified_name opt_name_list
        1 => {
            let mut n = VacuumStmt {
                options: vacuum_option::VACUUM,
                ..Default::default()
            };
            if rhs.bool(2)? {
                n.options |= vacuum_option::FULL;
            }
            if rhs.bool(3)? {
                n.options |= vacuum_option::FREEZE;
            }
            if rhs.bool(4)? {
                n.options |= vacuum_option::VERBOSE;
            }
            n.relation = rhs.ptr::<RangeVar>(5)?;
            n.va_cols = rhs.list(6)?;
            Ok(SemValue::node(n))
        }
        // VACUUM opt_full opt_freeze opt_verbose AnalyzeStmt
        2 => {
            let mut n = rhs.take::<VacuumStmt>(5)?;
            n.options |= vacuum_option::VACUUM;
            if rhs.bool(2)? {
                n.options |= vacuum_option::FULL;
            }
            if rhs.bool(3)? {
                n.options |= vacuum_option::FREEZE;
            }
            if rhs.bool(4)? {
                n.options |= vacuum_option::VERBOSE;
            }
            Ok(SemValue::node(n))
        }
        // VACUUM '(' vacuum_option_list ')'
        3 => {
            let n = VacuumStmt {
                options: vacuum_option::VACUUM | rhs.int(3)?,
                relation: None,
                va_cols: List::new(),
            };
            Ok(SemValue::node(n))
        }
        // VACUUM '(' vacuum_option_list ')' qualified_name opt_name_list
        4 => {
            let mut n = VacuumStmt {
                options: vacuum_option::VACUUM | rhs.int(3)?,
                relation: rhs.ptr::<RangeVar>(5)?,
                va_cols: rhs.list(6)?,
            };
            if !n.va_cols.is_empty() {
                n.options |= vacuum_option::ANALYZE;
            }
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// vacuum_option_elem:
///       analyze_keyword
///     | VERBOSE
///     | FREEZE
///     | FULL
///     | IDENT
/// ```
pub(super) fn vacuum_option_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // analyze_keyword
        0 => Ok(SemValue::Int(vacuum_option::ANALYZE)),
        // VERBOSE
        1 => Ok(SemValue::Int(vacuum_option::VERBOSE)),
        // FREEZE
        2 => Ok(SemValue::Int(vacuum_option::FREEZE)),
        // FULL
        3 => Ok(SemValue::Int(vacuum_option::FULL)),
        // IDENT
        4 => {
            let ident = rhs.string(1)?;
            if ident != "disable_page_skipping" {
                return Err(rhs.error_at(format!("unrecognized VACUUM option \"{ident}\""), rhs.loc(1)));
            }
            Ok(SemValue::Int(vacuum_option::DISABLE_PAGE_SKIPPING))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_full:
///       FULL
///     | /* empty */
/// ```
pub(super) fn opt_full<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FULL
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// vacuum_option_list:
///       vacuum_option_elem
///     | vacuum_option_list ',' vacuum_option_elem
/// ```
pub(super) fn vacuum_option_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // vacuum_option_list ',' vacuum_option_elem
        1 => Ok(SemValue::Int(rhs.int(1)? | rhs.int(3)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_freeze:
///       FREEZE
///     | /* empty */
/// ```
pub(super) fn opt_freeze<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FREEZE
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}
