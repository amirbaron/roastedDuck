//! Semantic actions for the rules in `grammar/statements/create_schema.y`.

use super::prelude::*;

/// ```text
/// CreateSchemaStmt:
///       CREATE_P SCHEMA qualified_name OptSchemaEltList
///     | CREATE_P SCHEMA IF_P NOT EXISTS qualified_name OptSchemaEltList
///     | CREATE_P OR REPLACE SCHEMA qualified_name OptSchemaEltList
/// ```
pub(super) fn create_schema_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P SCHEMA qualified_name OptSchemaEltList
        0 => {
            let qualified_name = rhs.take::<RangeVar>(3)?;
            let mut n = CreateSchemaStmt { ..Default::default() };
            if qualified_name.catalogname.is_some() {
                return Err(rhs.error_at(
                    "CREATE SCHEMA too many dots: expected \"catalog.schema\" or \"schema\"",
                    rhs.loc(3),
                ));
            }
            if qualified_name.schemaname.is_some() {
                n.catalogname = qualified_name.schemaname;
                n.schemaname = qualified_name.relname;
            } else {
                n.schemaname = qualified_name.relname;
            }
            n.schema_elts = rhs.list(4)?;
            n.onconflict = OnCreateConflict::Error;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P SCHEMA IF_P NOT EXISTS qualified_name OptSchemaEltList
        1 => {
            let qualified_name = rhs.take::<RangeVar>(6)?;
            let opt_schema_elt_list = rhs.list(7)?;
            let mut n = CreateSchemaStmt { ..Default::default() };
            if qualified_name.catalogname.is_some() {
                return Err(rhs.error_at(
                    "CREATE SCHEMA too many dots: expected \"catalog.schema\" or \"schema\"",
                    rhs.loc(6),
                ));
            }
            if qualified_name.schemaname.is_some() {
                n.catalogname = qualified_name.schemaname;
                n.schemaname = qualified_name.relname;
            } else {
                n.schemaname = qualified_name.relname;
            }
            if !opt_schema_elt_list.is_empty() {
                return Err(rhs.error_at(
                    "CREATE SCHEMA IF NOT EXISTS cannot include schema elements",
                    rhs.loc(7),
                ));
            }
            n.schema_elts = opt_schema_elt_list;
            n.onconflict = OnCreateConflict::Ignore;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OR REPLACE SCHEMA qualified_name OptSchemaEltList
        2 => {
            let qualified_name = rhs.take::<RangeVar>(5)?;
            let mut n = CreateSchemaStmt { ..Default::default() };
            if qualified_name.catalogname.is_some() {
                return Err(rhs.error_at(
                    "CREATE SCHEMA too many dots: expected \"catalog.schema\" or \"schema\"",
                    rhs.loc(5),
                ));
            }
            if qualified_name.schemaname.is_some() {
                n.catalogname = qualified_name.schemaname;
                n.schemaname = qualified_name.relname;
            } else {
                n.schemaname = qualified_name.relname;
            }
            n.schema_elts = rhs.list(6)?;
            n.onconflict = OnCreateConflict::Replace;
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptSchemaEltList:
///       OptSchemaEltList schema_stmt
///     | /* empty */
/// ```
pub(super) fn opt_schema_elt_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // OptSchemaEltList schema_stmt
        0 => {
            if rhs.location < 0 {
                rhs.location = rhs.loc(2);
            }
            Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(2)?)?)))
        }
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
