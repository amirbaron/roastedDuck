//! Semantic actions for the rules in `grammar/statements/variable_show.y`.

use super::prelude::*;

/// ```text
/// VariableShowStmt:
///       show_or_describe SelectStmt
///     | SUMMARIZE SelectStmt
///     | SUMMARIZE qualified_name
///     | show_or_describe TABLES FROM qualified_name
///     | show_or_describe qualified_name
///     | show_or_describe TIME ZONE
///     | show_or_describe TRANSACTION ISOLATION LEVEL
///     | show_or_describe ALL opt_tables
///     | show_or_describe
/// ```
pub(super) fn variable_show_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // show_or_describe SelectStmt
        0 => {
            let n = VariableShowSelectStmt {
                stmt: rhs.node(2)?,
                name: Some("select".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // SUMMARIZE SelectStmt
        1 => {
            let n = VariableShowSelectStmt {
                stmt: rhs.node(2)?,
                name: Some("select".to_owned()),
                is_summary: 1,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // SUMMARIZE qualified_name
        2 => {
            let n = VariableShowStmt {
                relation: rhs.ptr::<RangeVar>(2)?,
                is_summary: 1,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe TABLES FROM qualified_name
        3 => {
            let n = VariableShowStmt {
                relation: rhs.ptr::<RangeVar>(4)?,
                set: Some("__show_tables_from_database".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe qualified_name
        4 => {
            let n = VariableShowStmt {
                relation: rhs.ptr::<RangeVar>(2)?,
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe TIME ZONE
        5 => {
            let n = VariableShowStmt {
                set: Some("timezone".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe TRANSACTION ISOLATION LEVEL
        6 => {
            let n = VariableShowStmt {
                set: Some("transaction_isolation".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe ALL opt_tables
        7 => {
            let n = VariableShowStmt {
                set: Some("__show_tables_expanded".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // show_or_describe
        8 => {
            let n = VariableShowStmt {
                set: Some("__show_tables_expanded".to_owned()),
                is_summary: 0,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// var_name:
///       ColId
///     | var_name '.' ColId
/// ```
pub(super) fn var_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // var_name '.' ColId
        1 => Ok(SemValue::Str(Some(format!(
            "{}.{}",
            c_str(&rhs.str(1)?),
            c_str(&rhs.str(3)?)
        )))),
        _ => Err(rhs.bad_alternative()),
    }
}
