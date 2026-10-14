//! Semantic actions for the rules in `grammar/statements/comment_on.y`.

use super::prelude::*;

/// ```text
/// CommentOnStmt:
///       COMMENT ON comment_on_type_any_name qualified_name IS comment_value
///     | COMMENT ON COLUMN a_expr IS comment_value
/// ```
pub(super) fn comment_on_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COMMENT ON comment_on_type_any_name qualified_name IS comment_value
        0 => {
            let n = CommentOnStmt {
                object_type: rhs.object_type(3)?,
                name: rhs.ptr::<RangeVar>(4)?,
                value: rhs.node(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // COMMENT ON COLUMN a_expr IS comment_value
        1 => {
            let n = CommentOnStmt {
                object_type: ObjectType::Column,
                column_expr: rhs.node(4)?,
                value: rhs.node(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// comment_value:
///       Sconst
///     | NULL_P
/// ```
pub(super) fn comment_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        0 => Ok(SemValue::node(make_string_const(
            required(rhs.str(1)?)?,
            rhs.loc(1),
        ))),
        // NULL_P
        1 => Ok(SemValue::node(make_null_a_const(rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// comment_on_type_any_name:
///       TABLE
///     | SEQUENCE
///     | FUNCTION
///     | MACRO
///     | MACRO TABLE
///     | VIEW
///     | DATABASE
///     | INDEX
///     | SCHEMA
///     | TYPE_P
/// ```
pub(super) fn comment_on_type_any_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TABLE
        0 => Ok(SemValue::ObjectType(ObjectType::Table)),
        // SEQUENCE
        1 => Ok(SemValue::ObjectType(ObjectType::Sequence)),
        // FUNCTION
        2 => Ok(SemValue::ObjectType(ObjectType::Function)),
        // MACRO
        3 => Ok(SemValue::ObjectType(ObjectType::Function)),
        // MACRO TABLE
        4 => Ok(SemValue::ObjectType(ObjectType::TableMacro)),
        // VIEW
        5 => Ok(SemValue::ObjectType(ObjectType::View)),
        // DATABASE
        6 => Ok(SemValue::ObjectType(ObjectType::Database)),
        // INDEX
        7 => Ok(SemValue::ObjectType(ObjectType::Index)),
        // SCHEMA
        8 => Ok(SemValue::ObjectType(ObjectType::Schema)),
        // TYPE_P
        9 => Ok(SemValue::ObjectType(ObjectType::Type)),
        _ => Err(rhs.bad_alternative()),
    }
}
