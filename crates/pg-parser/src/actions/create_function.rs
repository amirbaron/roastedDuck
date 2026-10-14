//! Semantic actions for the rules in `grammar/statements/create_function.y`.

use super::prelude::*;

/// ```text
/// CreateFunctionStmt:
///       CREATE_P OptTemp macro_alias qualified_name table_macro_list
///     | CREATE_P OptTemp macro_alias IF_P NOT EXISTS qualified_name table_macro_list
///     | CREATE_P OR REPLACE OptTemp macro_alias qualified_name table_macro_list
///     | CREATE_P OptTemp macro_alias qualified_name macro_definition_list
///     | CREATE_P OptTemp macro_alias IF_P NOT EXISTS qualified_name macro_definition_list
///     | CREATE_P OR REPLACE OptTemp macro_alias qualified_name macro_definition_list
/// ```
pub(super) fn create_function_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp macro_alias qualified_name table_macro_list
        0 => {
            let mut qualified_name = rhs.take::<RangeVar>(4)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(5)?;
            n.onconflict = OnCreateConflict::Error;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp macro_alias IF_P NOT EXISTS qualified_name table_macro_list
        1 => {
            let mut qualified_name = rhs.take::<RangeVar>(7)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(8)?;
            n.onconflict = OnCreateConflict::Ignore;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OR REPLACE OptTemp macro_alias qualified_name table_macro_list
        2 => {
            let mut qualified_name = rhs.take::<RangeVar>(6)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(4)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(7)?;
            n.onconflict = OnCreateConflict::Replace;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp macro_alias qualified_name macro_definition_list
        3 => {
            let mut qualified_name = rhs.take::<RangeVar>(4)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(5)?;
            n.onconflict = OnCreateConflict::Error;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp macro_alias IF_P NOT EXISTS qualified_name macro_definition_list
        4 => {
            let mut qualified_name = rhs.take::<RangeVar>(7)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(8)?;
            n.onconflict = OnCreateConflict::Ignore;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OR REPLACE OptTemp macro_alias qualified_name macro_definition_list
        5 => {
            let mut qualified_name = rhs.take::<RangeVar>(6)?;
            let mut n = CreateFunctionStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(4)?;
            n.name = Some(qualified_name);
            n.functions = rhs.list(7)?;
            n.onconflict = OnCreateConflict::Replace;
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// table_macro_definition:
///       param_list AS TABLE select_no_parens
/// ```
pub(super) fn table_macro_definition<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // param_list AS TABLE select_no_parens
        0 => {
            let n = FunctionDefinition {
                params: rhs.list(1)?,
                query: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// table_macro_definition_parens:
///       param_list AS TABLE select_with_parens
/// ```
pub(super) fn table_macro_definition_parens<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // param_list AS TABLE select_with_parens
        0 => {
            let n = FunctionDefinition {
                params: rhs.list(1)?,
                query: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// table_macro_list_internal:
///       table_macro_definition_parens
///     | table_macro_list_internal ',' table_macro_definition_parens
/// ```
pub(super) fn table_macro_list_internal<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // table_macro_definition_parens
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // table_macro_list_internal ',' table_macro_definition_parens
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// table_macro_list:
///       table_macro_definition
///     | table_macro_list_internal
/// ```
pub(super) fn table_macro_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // table_macro_definition
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// macro_definition:
///       param_list AS a_expr
/// ```
pub(super) fn macro_definition<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // param_list AS a_expr
        0 => {
            let n = FunctionDefinition {
                params: rhs.list(1)?,
                function: rhs.node(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// macro_definition_list:
///       macro_definition
///     | macro_definition_list ',' macro_definition
/// ```
pub(super) fn macro_definition_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // macro_definition
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // macro_definition_list ',' macro_definition
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// param_list:
///       '(' ')'
///     | '(' MacroParameterList ',' ')'
///     | '(' MacroParameterList ')'
/// ```
pub(super) fn param_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' ')'
        0 => Ok(SemValue::List(List::new())),
        // '(' MacroParameterList ',' ')'
        1 => Ok(SemValue::List(rhs.list(2)?)),
        // '(' MacroParameterList ')'
        2 => Ok(SemValue::List(rhs.list(2)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// MacroParameterList:
///       MacroParameter
///     | MacroParameterList ',' MacroParameter
/// ```
pub(super) fn macro_parameter_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // MacroParameter
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // MacroParameterList ',' MacroParameter
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// MacroParameter:
///       param_name opt_Typename
///     | param_name opt_Typename COLON_EQUALS a_expr
///     | param_name opt_Typename EQUALS_GREATER a_expr
/// ```
pub(super) fn macro_parameter<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // param_name opt_Typename
        0 => {
            let n = FunctionParameter {
                name: rhs.str(1)?,
                type_name: rhs.ptr::<TypeName>(2)?,
                default_value: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // param_name opt_Typename COLON_EQUALS a_expr
        1 => {
            let n = FunctionParameter {
                name: rhs.str(1)?,
                type_name: rhs.ptr::<TypeName>(2)?,
                default_value: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // param_name opt_Typename EQUALS_GREATER a_expr
        2 => {
            let n = FunctionParameter {
                name: rhs.str(1)?,
                type_name: rhs.ptr::<TypeName>(2)?,
                default_value: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
