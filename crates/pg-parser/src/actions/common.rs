//! Semantic actions for the rules in `grammar/statements/common.y`.

use super::prelude::*;

/// ```text
/// qualified_name:
///       ColIdOrString
///     | ColId indirection
/// ```
pub(super) fn qualified_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString
        0 => Ok(SemValue::node(make_range_var(None, rhs.str(1)?, rhs.loc(1)))),
        // ColId indirection
        1 => {
            let col_id = rhs.str(1)?;
            let indirection = rhs.list(2)?;
            check_qualified_name(&indirection, rhs.scanner())?;
            let mut range = make_range_var(None, None, rhs.loc(1));
            match indirection.as_slice() {
                [relname] => {
                    range.schemaname = col_id;
                    range.relname = Some(str_val(relname)?);
                }
                [schemaname, relname] => {
                    range.catalogname = col_id;
                    range.schemaname = Some(str_val(schemaname)?);
                    range.relname = Some(str_val(relname)?);
                }
                // "improper qualified name (too many dotted names)" is formatted with the
                // unimplemented NameListToString()
                _ => return Err(unimplemented("NameListToString")),
            }
            Ok(SemValue::node(range))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ColId:
///       IDENT
///     | unreserved_keyword
///     | col_name_keyword
/// ```
pub(super) fn col_id<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unreserved_keyword
        // col_name_keyword
        1 | 2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.to_owned()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// indirection:
///       indirection_el
///     | indirection indirection_el
/// ```
pub(super) fn indirection<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // indirection_el
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // indirection indirection_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// indirection_el:
///       '.' attr_name
///     | '[' a_expr ']'
///     | '[' opt_slice_bound SINGLE_COLON opt_slice_bound ']'
///     | '[' opt_slice_bound SINGLE_COLON opt_slice_bound SINGLE_COLON opt_slice_bound ']'
///     | '[' opt_slice_bound SINGLE_COLON '-' SINGLE_COLON opt_slice_bound ']'
/// ```
pub(super) fn indirection_el<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '.' attr_name
        0 => Ok(SemValue::node(Node::string(rhs.string(2)?))),
        // '[' a_expr ']'
        1 => {
            let ai = AIndices {
                is_slice: false,
                lidx: None,
                uidx: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON opt_slice_bound ']'
        2 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                uidx: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON opt_slice_bound SINGLE_COLON opt_slice_bound ']'
        3 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                uidx: rhs.node(4)?,
                step: rhs.node(6)?,
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON '-' SINGLE_COLON opt_slice_bound ']'
        4 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                step: rhs.node(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ColLabel:
///       IDENT
///     | other_keyword
///     | unreserved_keyword
///     | reserved_keyword
/// ```
pub(super) fn col_label<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // other_keyword
        // unreserved_keyword
        // reserved_keyword
        1..=3 => Ok(SemValue::Str(Some(rhs.keyword(1)?.to_owned()))),
        _ => Err(rhs.bad_alternative()),
    }
}
