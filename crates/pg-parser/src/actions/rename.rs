//! Semantic actions for the rules in `grammar/statements/rename.y`.

use super::prelude::*;

/// ```text
/// RenameStmt:
///       ALTER SCHEMA name RENAME TO name
///     | ALTER TABLE relation_expr RENAME TO name
///     | ALTER TABLE IF_P EXISTS relation_expr RENAME TO name
///     | ALTER SEQUENCE qualified_name RENAME TO name
///     | ALTER SEQUENCE IF_P EXISTS qualified_name RENAME TO name
///     | ALTER VIEW qualified_name RENAME TO name
///     | ALTER VIEW IF_P EXISTS qualified_name RENAME TO name
///     | ALTER INDEX qualified_name RENAME TO name
///     | ALTER INDEX IF_P EXISTS qualified_name RENAME TO name
///     | ALTER TABLE relation_expr RENAME opt_column qualified_column TO name
///     | ALTER TABLE IF_P EXISTS relation_expr RENAME opt_column qualified_column TO name
///     | ALTER TABLE relation_expr RENAME CONSTRAINT name TO name
///     | ALTER TABLE IF_P EXISTS relation_expr RENAME CONSTRAINT name TO name
/// ```
pub(super) fn rename_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALTER SCHEMA name RENAME TO name
        0 => {
            let n = RenameStmt {
                rename_type: ObjectType::Schema,
                subname: rhs.str(3)?,
                newname: rhs.str(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE relation_expr RENAME TO name
        1 => {
            let n = RenameStmt {
                rename_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(3)?,
                subname: None,
                newname: rhs.str(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE IF_P EXISTS relation_expr RENAME TO name
        2 => {
            let n = RenameStmt {
                rename_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(5)?,
                subname: None,
                newname: rhs.str(8)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER SEQUENCE qualified_name RENAME TO name
        3 => {
            let n = RenameStmt {
                rename_type: ObjectType::Sequence,
                relation: rhs.ptr::<RangeVar>(3)?,
                subname: None,
                newname: rhs.str(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER SEQUENCE IF_P EXISTS qualified_name RENAME TO name
        4 => {
            let n = RenameStmt {
                rename_type: ObjectType::Sequence,
                relation: rhs.ptr::<RangeVar>(5)?,
                subname: None,
                newname: rhs.str(8)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER VIEW qualified_name RENAME TO name
        5 => {
            let n = RenameStmt {
                rename_type: ObjectType::View,
                relation: rhs.ptr::<RangeVar>(3)?,
                subname: None,
                newname: rhs.str(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER VIEW IF_P EXISTS qualified_name RENAME TO name
        6 => {
            let n = RenameStmt {
                rename_type: ObjectType::View,
                relation: rhs.ptr::<RangeVar>(5)?,
                subname: None,
                newname: rhs.str(8)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER INDEX qualified_name RENAME TO name
        7 => {
            let n = RenameStmt {
                rename_type: ObjectType::Index,
                relation: rhs.ptr::<RangeVar>(3)?,
                subname: None,
                newname: rhs.str(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER INDEX IF_P EXISTS qualified_name RENAME TO name
        8 => {
            let n = RenameStmt {
                rename_type: ObjectType::Index,
                relation: rhs.ptr::<RangeVar>(5)?,
                subname: None,
                newname: rhs.str(8)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE relation_expr RENAME opt_column qualified_column TO name
        9 => {
            let n = RenameStmt {
                rename_type: ObjectType::Column,
                relation_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(3)?,
                name_list: rhs.list(6)?,
                newname: rhs.str(8)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE IF_P EXISTS relation_expr RENAME opt_column qualified_column TO name
        10 => {
            let n = RenameStmt {
                rename_type: ObjectType::Column,
                relation_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(5)?,
                name_list: rhs.list(8)?,
                newname: rhs.str(10)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE relation_expr RENAME CONSTRAINT name TO name
        11 => {
            let n = RenameStmt {
                rename_type: ObjectType::Tabconstraint,
                relation: rhs.ptr::<RangeVar>(3)?,
                subname: rhs.str(6)?,
                newname: rhs.str(8)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // ALTER TABLE IF_P EXISTS relation_expr RENAME CONSTRAINT name TO name
        12 => {
            let n = RenameStmt {
                rename_type: ObjectType::Tabconstraint,
                relation: rhs.ptr::<RangeVar>(5)?,
                subname: rhs.str(8)?,
                newname: rhs.str(10)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_column:
///       COLUMN
///     | /* empty */
/// ```
pub(super) fn opt_column<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COLUMN
        0 => Ok(SemValue::Int(i32::from(token::COLUMN))),
        // /* empty */
        1 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}
