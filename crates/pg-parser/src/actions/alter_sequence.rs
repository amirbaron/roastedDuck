//! Semantic actions for the rules in `grammar/statements/alter_sequence.y`.

use super::prelude::*;

/// ```text
/// AlterSeqStmt:
///       ALTER SEQUENCE qualified_name SeqOptList
///     | ALTER SEQUENCE IF_P EXISTS qualified_name SeqOptList
/// ```
pub(super) fn alter_seq_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALTER SEQUENCE qualified_name SeqOptList
        0 => {
            let n = AlterSeqStmt {
                sequence: rhs.ptr::<RangeVar>(3)?,
                options: rhs.list(4)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER SEQUENCE IF_P EXISTS qualified_name SeqOptList
        1 => {
            let n = AlterSeqStmt {
                sequence: rhs.ptr::<RangeVar>(5)?,
                options: rhs.list(6)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// SeqOptList:
///       SeqOptElem
///     | SeqOptList SeqOptElem
/// ```
pub(super) fn seq_opt_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SeqOptElem
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<DefElem>(1)?,
        )?)])),
        // SeqOptList SeqOptElem
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<DefElem>(2)?)?),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_with:
///       WITH
///     | WITH_LA
///     | /* empty */
/// ```
pub(super) fn opt_with<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH
        0 => Ok(SemValue::None),
        // WITH_LA
        1 => Ok(SemValue::None),
        // /* empty */
        2 => Ok(SemValue::None),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// NumericOnly:
///       FCONST
///     | '+' FCONST
///     | '-' FCONST
///     | SignedIconst
/// ```
pub(super) fn numeric_only<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FCONST
        0 => Ok(SemValue::node(Value::Float(required(rhs.str(1)?)?.into()))),
        // '+' FCONST
        1 => Ok(SemValue::node(Value::Float(required(rhs.str(2)?)?.into()))),
        // '-' FCONST
        2 => {
            let mut numeric_only = Value::Float(required(rhs.str(2)?)?.into());
            do_negate_float(&mut numeric_only);
            Ok(SemValue::node(numeric_only))
        }
        // SignedIconst
        3 => Ok(SemValue::node(Value::Integer(i64::from(rhs.int(1)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// SeqOptElem:
///       AS SimpleTypename
///     | CACHE NumericOnly
///     | CYCLE
///     | NO CYCLE
///     | INCREMENT opt_by NumericOnly
///     | MAXVALUE NumericOnly
///     | MINVALUE NumericOnly
///     | NO MAXVALUE
///     | NO MINVALUE
///     | OWNED BY any_name
///     | SEQUENCE NAME_P any_name
///     | START opt_with NumericOnly
///     | RESTART
///     | RESTART opt_with NumericOnly
/// ```
pub(super) fn seq_opt_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // AS SimpleTypename
        0 => Ok(SemValue::node(make_def_elem(
            "as",
            rhs.ptr::<TypeName>(2)?.map(Node::from),
            rhs.loc(1),
        ))),
        // CACHE NumericOnly
        1 => Ok(SemValue::node(make_def_elem(
            "cache",
            rhs.value(2)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // CYCLE
        2 => Ok(SemValue::node(make_def_elem(
            "cycle",
            Some(Node::Value(Value::Integer(i64::from(true)))),
            rhs.loc(1),
        ))),
        // NO CYCLE
        3 => Ok(SemValue::node(make_def_elem(
            "cycle",
            Some(Node::Value(Value::Integer(i64::from(false)))),
            rhs.loc(1),
        ))),
        // INCREMENT opt_by NumericOnly
        4 => Ok(SemValue::node(make_def_elem(
            "increment",
            rhs.value(3)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // MAXVALUE NumericOnly
        5 => Ok(SemValue::node(make_def_elem(
            "maxvalue",
            rhs.value(2)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // MINVALUE NumericOnly
        6 => Ok(SemValue::node(make_def_elem(
            "minvalue",
            rhs.value(2)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // NO MAXVALUE
        7 => Ok(SemValue::node(make_def_elem("maxvalue", None, rhs.loc(1)))),
        // NO MINVALUE
        8 => Ok(SemValue::node(make_def_elem("minvalue", None, rhs.loc(1)))),
        // OWNED BY any_name
        9 => Ok(SemValue::node(make_def_elem(
            "owned_by",
            Node::from_list(rhs.list(3)?),
            rhs.loc(1),
        ))),
        // SEQUENCE NAME_P any_name
        10 => Ok(SemValue::node(make_def_elem(
            "sequence_name",
            Node::from_list(rhs.list(3)?),
            rhs.loc(1),
        ))),
        // START opt_with NumericOnly
        11 => Ok(SemValue::node(make_def_elem(
            "start",
            rhs.value(3)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // RESTART
        12 => Ok(SemValue::node(make_def_elem("restart", None, rhs.loc(1)))),
        // RESTART opt_with NumericOnly
        13 => Ok(SemValue::node(make_def_elem(
            "restart",
            rhs.value(3)?.map(Node::Value),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_by:
///       BY
///     | /* empty */
/// ```
pub(super) fn opt_by<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BY
        0 => Ok(SemValue::None),
        // /* empty */
        1 => Ok(SemValue::None),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// SignedIconst:
///       Iconst
///     | '+' Iconst
///     | '-' Iconst
/// ```
pub(super) fn signed_iconst<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Iconst
        0 => Ok(SemValue::Int(rhs.int(1)?)),
        // '+' Iconst
        1 => Ok(SemValue::Int(rhs.int(2)?)),
        // '-' Iconst
        2 => Ok(SemValue::Int(-rhs.int(2)?)),
        _ => Err(rhs.bad_alternative()),
    }
}
