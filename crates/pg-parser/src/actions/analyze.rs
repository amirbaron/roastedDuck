//! Semantic actions for the rules in `grammar/statements/analyze.y`.

use super::prelude::*;

/// ```text
/// AnalyzeStmt:
///       analyze_keyword opt_verbose
///     | analyze_keyword opt_verbose qualified_name opt_name_list
/// ```
pub(super) fn analyze_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // analyze_keyword opt_verbose
        0 => {
            let mut n = VacuumStmt {
                options: vacuum_option::ANALYZE,
                ..Default::default()
            };
            if rhs.bool(2)? {
                n.options |= vacuum_option::VERBOSE;
            }
            n.relation = None;
            n.va_cols = List::new();
            Ok(SemValue::node(Node::from(n)))
        }
        // analyze_keyword opt_verbose qualified_name opt_name_list
        1 => {
            let mut n = VacuumStmt {
                options: vacuum_option::ANALYZE,
                ..Default::default()
            };
            if rhs.bool(2)? {
                n.options |= vacuum_option::VERBOSE;
            }
            n.relation = rhs.ptr::<RangeVar>(3)?;
            n.va_cols = rhs.list(4)?;
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
