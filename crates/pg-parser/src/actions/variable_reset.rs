//! Semantic actions for the rules in `grammar/statements/variable_reset.y`.

use super::prelude::*;

/// ```text
/// VariableResetStmt:
///       RESET reset_rest
///     | RESET LOCAL reset_rest
///     | RESET SESSION reset_rest
///     | RESET GLOBAL reset_rest
///     | RESET VARIABLE_P reset_rest
/// ```
pub(super) fn variable_reset_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // RESET reset_rest
        0 => {
            let mut reset_rest = rhs.take::<VariableSetStmt>(2)?;
            reset_rest.scope = VariableSetScope::Default;
            Ok(SemValue::node(Node::from(reset_rest)))
        }
        // RESET LOCAL reset_rest
        1 => {
            let mut reset_rest = rhs.take::<VariableSetStmt>(3)?;
            reset_rest.scope = VariableSetScope::Local;
            Ok(SemValue::node(Node::from(reset_rest)))
        }
        // RESET SESSION reset_rest
        2 => {
            let mut reset_rest = rhs.take::<VariableSetStmt>(3)?;
            reset_rest.scope = VariableSetScope::Session;
            Ok(SemValue::node(Node::from(reset_rest)))
        }
        // RESET GLOBAL reset_rest
        3 => {
            let mut reset_rest = rhs.take::<VariableSetStmt>(3)?;
            reset_rest.scope = VariableSetScope::Global;
            Ok(SemValue::node(Node::from(reset_rest)))
        }
        // RESET VARIABLE_P reset_rest
        4 => {
            let mut reset_rest = rhs.take::<VariableSetStmt>(3)?;
            reset_rest.scope = VariableSetScope::Variable;
            Ok(SemValue::node(Node::from(reset_rest)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_reset:
///       var_name
///     | ALL
/// ```
pub(super) fn generic_reset<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // var_name
        0 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Reset,
                name: rhs.str(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALL
        1 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::ResetAll,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// reset_rest:
///       generic_reset
///     | TIME ZONE
///     | TRANSACTION ISOLATION LEVEL
/// ```
pub(super) fn reset_rest<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // generic_reset
        0 => Ok(SemValue::Node((rhs.ptr::<VariableSetStmt>(1)?).map(Node::from))),
        // TIME ZONE
        1 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Reset,
                name: Some("timezone".to_owned()),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // TRANSACTION ISOLATION LEVEL
        2 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Reset,
                name: Some("transaction_isolation".to_owned()),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
