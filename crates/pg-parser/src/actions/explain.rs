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
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // EXPLAIN analyze_keyword opt_verbose ExplainableStmt
        1 => {
            let mut n = ExplainStmt {
                query: rhs.node(4)?,
                options: vec![make_def_elem("analyze", None, rhs.loc(2)).into()],
                ..Default::default()
            };
            if rhs.bool(3)? {
                n.options = append(n.options, make_def_elem("verbose", None, rhs.loc(3)).into());
            }
            Ok(SemValue::node(Node::from(n)))
        }
        // EXPLAIN VERBOSE ExplainableStmt
        2 => {
            let n = ExplainStmt {
                query: rhs.node(3)?,
                options: vec![make_def_elem("verbose", None, rhs.loc(2)).into()],
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // EXPLAIN '(' explain_option_list ')' ExplainableStmt
        3 => {
            let n = ExplainStmt {
                query: rhs.node(5)?,
                options: rhs.list(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
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
        0 => Ok(SemValue::node(Node::Value(Value::String(
            required(rhs.str(1)?)?.into(),
        )))),
        // NumericOnly
        1 => Ok(SemValue::Node(rhs.value(1)?.map(Node::Value))),
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
        // IDENT
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // unreserved_keyword
        1 => Ok(SemValue::Str(Some(rhs.keyword(1)?.to_owned()))),
        // other_keyword
        2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.to_owned()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// NonReservedWord_or_Sconst:
///       NonReservedWord
///     | Sconst
/// ```
pub(super) fn non_reserved_word_or_sconst<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // NonReservedWord
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // Sconst
        1 => Ok(SemValue::Str(rhs.str(1)?)),
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
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<DefElem>(1)?,
        )?)])),
        // explain_option_list ',' explain_option_elem
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<DefElem>(3)?)?),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// analyze_keyword:
///       ANALYZE
///     | ANALYSE
/// ```
pub(super) fn analyze_keyword<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ANALYZE
        0 => Ok(SemValue::None),
        // ANALYSE
        1 => Ok(SemValue::None),
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
        0 => Ok(SemValue::Str(Some("true".to_owned()))),
        // FALSE_P
        1 => Ok(SemValue::Str(Some("false".to_owned()))),
        // ON
        2 => Ok(SemValue::Str(Some("on".to_owned()))),
        // NonReservedWord_or_Sconst
        3 => Ok(SemValue::Str(rhs.str(1)?)),
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
            required(rhs.str(1)?)?,
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
        // NonReservedWord
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // analyze_keyword
        1 => Ok(SemValue::Str(Some("analyze".to_owned()))),
        _ => Err(rhs.bad_alternative()),
    }
}
