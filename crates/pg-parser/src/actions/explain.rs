//! Semantic actions for the rules in `grammar/statements/explain.y`.

use super::prelude::*;

/// ```text
/// ExplainStmt:
///       EXPLAIN ExplainableStmt
///     | EXPLAIN analyze_keyword opt_verbose ExplainableStmt
///     | EXPLAIN VERBOSE ExplainableStmt
///     | EXPLAIN '(' explain_option_list ')' ExplainableStmt
/// ```
pub(super) fn explain_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // EXPLAIN ExplainableStmt
        0 => {
            let n = ExplainStmt {
                query: rhs.node(2)?,
                options: List::new(),
            };
            Ok(SemValue::node(n))
        }
        // EXPLAIN analyze_keyword opt_verbose ExplainableStmt
        1 => {
            let mut n = ExplainStmt {
                query: rhs.node(4)?,
                options: vec![make_def_elem("analyze", None, rhs.loc(2)).into()],
            };
            if rhs.bool(3)? {
                n.options = append(n.options, make_def_elem("verbose", None, rhs.loc(3)).into());
            }
            Ok(SemValue::node(n))
        }
        // EXPLAIN VERBOSE ExplainableStmt
        2 => {
            let n = ExplainStmt {
                query: rhs.node(3)?,
                options: vec![make_def_elem("verbose", None, rhs.loc(2)).into()],
            };
            Ok(SemValue::node(n))
        }
        // EXPLAIN '(' explain_option_list ')' ExplainableStmt
        3 => {
            let n = ExplainStmt {
                query: rhs.node(5)?,
                options: rhs.list(3)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_verbose:
///       VERBOSE
///     | /* empty */
/// ```
pub(super) fn opt_verbose<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // VERBOSE
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// explain_option_arg:
///       opt_boolean_or_string
///     | NumericOnly
///     | /* empty */
/// ```
pub(super) fn explain_option_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_boolean_or_string
        0 => Ok(SemValue::node(Node::string(rhs.string(1)?))),
        // /* empty */
        2 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// NonReservedWord:
///       IDENT
///     | unreserved_keyword
///     | other_keyword
/// ```
pub(super) fn non_reserved_word<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unreserved_keyword
        // other_keyword
        1 | 2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// explain_option_list:
///       explain_option_elem
///     | explain_option_list ',' explain_option_elem
/// ```
pub(super) fn explain_option_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // explain_option_elem
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // explain_option_list ',' explain_option_elem
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_boolean_or_string:
///       TRUE_P
///     | FALSE_P
///     | ON
///     | NonReservedWord_or_Sconst
/// ```
pub(super) fn opt_boolean_or_string<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TRUE_P
        0 => Ok(SemValue::Str(Some("true".into()))),
        // FALSE_P
        1 => Ok(SemValue::Str(Some("false".into()))),
        // ON
        2 => Ok(SemValue::Str(Some("on".into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// explain_option_elem:
///       explain_option_name explain_option_arg
/// ```
pub(super) fn explain_option_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // explain_option_name explain_option_arg
        0 => Ok(SemValue::node(make_def_elem(
            rhs.string(1)?,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// explain_option_name:
///       NonReservedWord
///     | analyze_keyword
/// ```
pub(super) fn explain_option_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // analyze_keyword
        1 => Ok(SemValue::Str(Some("analyze".into()))),
        _ => Err(rhs.bad_alternative()),
    }
}
