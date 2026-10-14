//! Semantic actions for the rules in `grammar/statements/variable_set.y`.

use super::prelude::*;

/// ```text
/// VariableSetStmt:
///       SET set_rest
///     | SET LOCAL set_rest
///     | SET SESSION set_rest
///     | SET GLOBAL set_rest
///     | SET VARIABLE_P set_rest
/// ```
pub(super) fn variable_set_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SET set_rest
        0 => {
            let mut n = rhs.take::<VariableSetStmt>(2)?;
            n.scope = VariableSetScope::Default;
            Ok(SemValue::node(n))
        }
        // SET LOCAL set_rest
        1 => {
            let mut n = rhs.take::<VariableSetStmt>(3)?;
            n.scope = VariableSetScope::Local;
            Ok(SemValue::node(n))
        }
        // SET SESSION set_rest
        2 => {
            let mut n = rhs.take::<VariableSetStmt>(3)?;
            n.scope = VariableSetScope::Session;
            Ok(SemValue::node(n))
        }
        // SET GLOBAL set_rest
        3 => {
            let mut n = rhs.take::<VariableSetStmt>(3)?;
            n.scope = VariableSetScope::Global;
            Ok(SemValue::node(n))
        }
        // SET VARIABLE_P set_rest
        4 => {
            let mut n = rhs.take::<VariableSetStmt>(3)?;
            n.scope = VariableSetScope::Variable;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_rest:
///       generic_set
///     | var_name FROM CURRENT_P
///     | TIME ZONE zone_value
///     | SCHEMA Sconst
/// ```
pub(super) fn set_rest<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // var_name FROM CURRENT_P
        1 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Current,
                name: rhs.str(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // TIME ZONE zone_value
        2 => {
            let zone_value = rhs.node(3)?;
            let mut n = VariableSetStmt {
                kind: VariableSetKind::Value,
                name: Some("timezone".to_owned()),
                ..Default::default()
            };
            if zone_value.is_some() {
                n.args = vec![required(zone_value)?];
            } else {
                n.kind = VariableSetKind::Default;
            }
            Ok(SemValue::node(n))
        }
        // SCHEMA Sconst
        3 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Value,
                name: Some("search_path".to_owned()),
                args: vec![make_string_const(required(rhs.str(2)?)?, rhs.loc(2))],
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_set:
///       var_name TO var_list
///     | var_name '=' var_list
/// ```
pub(super) fn generic_set<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // var_name TO var_list
        // var_name '=' var_list
        0 | 1 => {
            let n = VariableSetStmt {
                kind: VariableSetKind::Value,
                name: rhs.str(1)?,
                args: rhs.list(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// zone_value:
///       Sconst
///     | IDENT
///     | ConstInterval Sconst opt_interval
///     | ConstInterval '(' Iconst ')' Sconst
///     | NumericOnly
///     | DEFAULT
///     | LOCAL
/// ```
pub(super) fn zone_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        // IDENT
        0 | 1 => Ok(SemValue::node(make_string_const(
            required(rhs.str(1)?)?,
            rhs.loc(1),
        ))),
        // ConstInterval Sconst opt_interval
        2 => {
            let mut type_name = rhs.take::<TypeName>(1)?;
            let typmods = rhs.list(3)?;
            if let Some(Node::AConst(range)) = typmods.first() {
                let fields = match range.val {
                    Value::Integer(fields) => fields,
                    _ => 0,
                };
                let allowed =
                    i64::from(interval_mask(interval_field::HOUR) | interval_mask(interval_field::MINUTE));
                if fields & !allowed != 0 {
                    return Err(rhs.error_at("time zone interval must be HOUR or HOUR TO MINUTE", rhs.loc(3)));
                }
            }
            type_name.typmods = typmods;
            Ok(SemValue::node(make_string_const_cast(
                rhs.string(2)?,
                rhs.loc(2),
                Some(type_name),
            )))
        }
        // ConstInterval '(' Iconst ')' Sconst
        3 => {
            let mut t = rhs.take::<TypeName>(1)?;
            t.typmods = vec![
                make_int_const(INTERVAL_FULL_RANGE, -1),
                make_int_const(rhs.int(3)?, rhs.loc(3)),
            ];
            Ok(SemValue::node(make_string_const_cast(
                required(rhs.str(5)?)?,
                rhs.loc(5),
                Some(t),
            )))
        }
        // NumericOnly
        4 => Ok(SemValue::node(make_a_const(required(rhs.value(1)?)?, rhs.loc(1)))),
        // DEFAULT
        // LOCAL
        5 | 6 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// var_list:
///       var_value
///     | var_list ',' var_value
/// ```
pub(super) fn var_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // var_value
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // var_list ',' var_value
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}
