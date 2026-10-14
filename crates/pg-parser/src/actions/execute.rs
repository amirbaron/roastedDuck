//! Semantic actions for the rules in `grammar/statements/execute.y`.

use super::prelude::*;

/// ```text
/// ExecuteStmt:
///       EXECUTE name execute_param_clause
///     | CREATE_P OptTemp TABLE create_as_target AS EXECUTE name execute_param_clause opt_with_data
///     | CREATE_P OptTemp TABLE IF_P NOT EXISTS create_as_target AS EXECUTE name execute_param_clause opt_with_data
/// ```
pub(super) fn execute_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // EXECUTE name execute_param_clause
        0 => {
            let n = ExecuteStmt {
                name: rhs.str(2)?,
                params: rhs.list(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp TABLE create_as_target AS EXECUTE name execute_param_clause opt_with_data
        1 => {
            let mut into = rhs.take::<IntoClause>(4)?;
            // cram additional flags into the IntoClause
            required_mut(&mut into.rel)?.relpersistence = rhs.rel_persistence(2)?;
            into.skip_data = !rhs.bool(9)?;
            let execute = ExecuteStmt {
                name: rhs.str(7)?,
                params: rhs.list(8)?,
            };
            Ok(SemValue::node(CreateTableAsStmt {
                query: Some(execute.into()),
                into: Some(into),
                relkind: ObjectType::Table,
                is_select_into: false,
                onconflict: OnCreateConflict::Error,
            }))
        }
        // CREATE_P OptTemp TABLE IF_P NOT EXISTS create_as_target AS EXECUTE name execute_param_clause opt_with_data
        2 => {
            let mut into = rhs.take::<IntoClause>(7)?;
            required_mut(&mut into.rel)?.relpersistence = rhs.rel_persistence(2)?;
            into.skip_data = !rhs.bool(12)?;
            let execute = ExecuteStmt {
                name: rhs.str(10)?,
                params: rhs.list(11)?,
            };
            Ok(SemValue::node(CreateTableAsStmt {
                query: Some(execute.into()),
                into: Some(into),
                relkind: ObjectType::Table,
                is_select_into: false,
                onconflict: OnCreateConflict::Ignore,
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// execute_param_expr:
///       a_expr
///     | param_name COLON_EQUALS a_expr
/// ```
pub(super) fn execute_param_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr
        0 => Ok(SemValue::Node(rhs.node(1)?)),
        // param_name COLON_EQUALS a_expr
        1 => {
            let na = NamedArgExpr {
                name: rhs.str(1)?,
                arg: rhs.node(3)?,
                argnumber: -1,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(na)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// execute_param_list:
///       execute_param_expr
///     | execute_param_list ',' execute_param_expr
/// ```
pub(super) fn execute_param_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // execute_param_expr
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // execute_param_list ',' execute_param_expr
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// execute_param_clause:
///       '(' execute_param_list ')'
///     | /* empty */
/// ```
pub(super) fn execute_param_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' execute_param_list ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
