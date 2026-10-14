//! Semantic actions for the rules in `grammar/statements/alter_schema.y`.

use super::prelude::*;

/// ```text
/// AlterObjectSchemaStmt:
///       ALTER TABLE relation_expr SET SCHEMA name
///     | ALTER TABLE IF_P EXISTS relation_expr SET SCHEMA name
///     | ALTER SEQUENCE qualified_name SET SCHEMA name
///     | ALTER SEQUENCE IF_P EXISTS qualified_name SET SCHEMA name
///     | ALTER VIEW qualified_name SET SCHEMA name
///     | ALTER VIEW IF_P EXISTS qualified_name SET SCHEMA name
/// ```
pub(super) fn alter_object_schema_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALTER TABLE relation_expr SET SCHEMA name
        0 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(3)?,
                newschema: rhs.str(6)?,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER TABLE IF_P EXISTS relation_expr SET SCHEMA name
        1 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::Table,
                relation: rhs.ptr::<RangeVar>(5)?,
                newschema: rhs.str(8)?,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        // ALTER SEQUENCE qualified_name SET SCHEMA name
        2 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::Sequence,
                relation: rhs.ptr::<RangeVar>(3)?,
                newschema: rhs.str(6)?,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER SEQUENCE IF_P EXISTS qualified_name SET SCHEMA name
        3 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::Sequence,
                relation: rhs.ptr::<RangeVar>(5)?,
                newschema: rhs.str(8)?,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        // ALTER VIEW qualified_name SET SCHEMA name
        4 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::View,
                relation: rhs.ptr::<RangeVar>(3)?,
                newschema: rhs.str(6)?,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER VIEW IF_P EXISTS qualified_name SET SCHEMA name
        5 => {
            let n = AlterObjectSchemaStmt {
                object_type: ObjectType::View,
                relation: rhs.ptr::<RangeVar>(5)?,
                newschema: rhs.str(8)?,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
