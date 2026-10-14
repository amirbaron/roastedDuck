//! Semantic actions for the rules in `grammar/statements/drop.y`.

use super::prelude::*;

/// ```text
/// DropStmt:
///       DROP drop_type_any_name IF_P EXISTS any_name_list opt_drop_behavior
///     | DROP drop_type_any_name any_name_list opt_drop_behavior
///     | DROP drop_type_name IF_P EXISTS name_list opt_drop_behavior
///     | DROP drop_type_name name_list opt_drop_behavior
///     | DROP drop_type_name_on_any_name name ON any_name opt_drop_behavior
///     | DROP drop_type_name_on_any_name IF_P EXISTS name ON any_name opt_drop_behavior
/// ```
pub(super) fn drop_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DROP drop_type_any_name IF_P EXISTS any_name_list opt_drop_behavior
        0 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                missing_ok: true,
                objects: rhs.list(5)?,
                behavior: rhs.drop_behavior(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DROP drop_type_any_name any_name_list opt_drop_behavior
        1 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                missing_ok: false,
                objects: rhs.list(3)?,
                behavior: rhs.drop_behavior(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DROP drop_type_name IF_P EXISTS name_list opt_drop_behavior
        2 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                missing_ok: true,
                objects: rhs.list(5)?,
                behavior: rhs.drop_behavior(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DROP drop_type_name name_list opt_drop_behavior
        3 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                missing_ok: false,
                objects: rhs.list(3)?,
                behavior: rhs.drop_behavior(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DROP drop_type_name_on_any_name name ON any_name opt_drop_behavior
        4 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                objects: vec![Node::List(append(
                    rhs.list(5)?,
                    Node::Value(Value::String(required(rhs.str(3)?)?.into())),
                ))],
                behavior: rhs.drop_behavior(6)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DROP drop_type_name_on_any_name IF_P EXISTS name ON any_name opt_drop_behavior
        5 => {
            let n = DropStmt {
                remove_type: rhs.object_type(2)?,
                objects: vec![Node::List(append(
                    rhs.list(7)?,
                    Node::Value(Value::String(required(rhs.str(5)?)?.into())),
                ))],
                behavior: rhs.drop_behavior(8)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// drop_type_any_name:
///       TABLE
///     | SEQUENCE
///     | FUNCTION
///     | MACRO
///     | MACRO TABLE
///     | VIEW
///     | MATERIALIZED VIEW
///     | INDEX
///     | FOREIGN TABLE
///     | COLLATION
///     | CONVERSION_P
///     | SCHEMA
///     | STATISTICS
///     | TEXT_P SEARCH PARSER
///     | TEXT_P SEARCH DICTIONARY
///     | TEXT_P SEARCH TEMPLATE
///     | TEXT_P SEARCH CONFIGURATION
///     | TYPE_P
/// ```
pub(super) fn drop_type_any_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
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
        // MATERIALIZED VIEW
        6 => Ok(SemValue::ObjectType(ObjectType::Matview)),
        // INDEX
        7 => Ok(SemValue::ObjectType(ObjectType::Index)),
        // FOREIGN TABLE
        8 => Ok(SemValue::ObjectType(ObjectType::ForeignTable)),
        // COLLATION
        9 => Ok(SemValue::ObjectType(ObjectType::Collation)),
        // CONVERSION_P
        10 => Ok(SemValue::ObjectType(ObjectType::Conversion)),
        // SCHEMA
        11 => Ok(SemValue::ObjectType(ObjectType::Schema)),
        // STATISTICS
        12 => Ok(SemValue::ObjectType(ObjectType::StatisticExt)),
        // TEXT_P SEARCH PARSER
        13 => Ok(SemValue::ObjectType(ObjectType::Tsparser)),
        // TEXT_P SEARCH DICTIONARY
        14 => Ok(SemValue::ObjectType(ObjectType::Tsdictionary)),
        // TEXT_P SEARCH TEMPLATE
        15 => Ok(SemValue::ObjectType(ObjectType::Tstemplate)),
        // TEXT_P SEARCH CONFIGURATION
        16 => Ok(SemValue::ObjectType(ObjectType::Tsconfiguration)),
        // TYPE_P
        17 => Ok(SemValue::ObjectType(ObjectType::Type)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// drop_type_name:
///       ACCESS METHOD
///     | EVENT TRIGGER
///     | EXTENSION
///     | FOREIGN DATA_P WRAPPER
///     | PUBLICATION
///     | SERVER
/// ```
pub(super) fn drop_type_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ACCESS METHOD
        0 => Ok(SemValue::ObjectType(ObjectType::AccessMethod)),
        // EVENT TRIGGER
        1 => Ok(SemValue::ObjectType(ObjectType::EventTrigger)),
        // EXTENSION
        2 => Ok(SemValue::ObjectType(ObjectType::Extension)),
        // FOREIGN DATA_P WRAPPER
        3 => Ok(SemValue::ObjectType(ObjectType::Fdw)),
        // PUBLICATION
        4 => Ok(SemValue::ObjectType(ObjectType::Publication)),
        // SERVER
        5 => Ok(SemValue::ObjectType(ObjectType::ForeignServer)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// any_name_list:
///       any_name
///     | any_name_list ',' any_name
/// ```
pub(super) fn any_name_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // any_name
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // any_name_list ',' any_name
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_drop_behavior:
///       CASCADE
///     | RESTRICT
///     | /* empty */
/// ```
pub(super) fn opt_drop_behavior<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CASCADE
        0 => Ok(SemValue::DropBehavior(DropBehavior::Cascade)),
        // RESTRICT
        1 => Ok(SemValue::DropBehavior(DropBehavior::Restrict)),
        // /* empty */
        2 => Ok(SemValue::DropBehavior(DropBehavior::Restrict)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// drop_type_name_on_any_name:
///       POLICY
///     | RULE
///     | TRIGGER
/// ```
pub(super) fn drop_type_name_on_any_name<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // POLICY
        0 => Ok(SemValue::ObjectType(ObjectType::Policy)),
        // RULE
        1 => Ok(SemValue::ObjectType(ObjectType::Rule)),
        // TRIGGER
        2 => Ok(SemValue::ObjectType(ObjectType::Trigger)),
        _ => Err(rhs.bad_alternative()),
    }
}
