//! Semantic actions for the rules in `grammar/statements/create_type.y`.

use super::prelude::*;

/// ```text
/// CreateTypeStmt:
///       CREATE_P OptTemp TYPE_P qualified_name AS create_type_value
///     | CREATE_P OptTemp TYPE_P IF_P NOT EXISTS qualified_name AS create_type_value
///     | CREATE_P OR REPLACE OptTemp TYPE_P qualified_name AS create_type_value
/// ```
pub(super) fn create_type_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp TYPE_P qualified_name AS create_type_value
        0 => {
            let mut qualified_name = rhs.take::<RangeVar>(4)?;
            let mut n = rhs.take::<CreateTypeStmt>(6)?;
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.type_name = Some(qualified_name);
            n.onconflict = OnCreateConflict::Error;
            Ok(SemValue::node(n))
        }
        // CREATE_P OptTemp TYPE_P IF_P NOT EXISTS qualified_name AS create_type_value
        1 => {
            let mut qualified_name = rhs.take::<RangeVar>(7)?;
            let mut n = rhs.take::<CreateTypeStmt>(9)?;
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.type_name = Some(qualified_name);
            n.onconflict = OnCreateConflict::Ignore;
            Ok(SemValue::node(n))
        }
        // CREATE_P OR REPLACE OptTemp TYPE_P qualified_name AS create_type_value
        2 => {
            let mut qualified_name = rhs.take::<RangeVar>(6)?;
            let mut n = rhs.take::<CreateTypeStmt>(8)?;
            qualified_name.relpersistence = rhs.rel_persistence(4)?;
            n.type_name = Some(qualified_name);
            n.onconflict = OnCreateConflict::Replace;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// create_type_value:
///       ENUM_P select_with_parens
///     | ENUM_P '(' opt_enum_val_list ')'
///     | Typename
/// ```
pub(super) fn create_type_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ENUM_P select_with_parens
        0 => {
            let n = CreateTypeStmt {
                kind: NewTypeKind::Enum,
                query: rhs.node(2)?,
                vals: List::new(),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ENUM_P '(' opt_enum_val_list ')'
        1 => {
            let n = CreateTypeStmt {
                kind: NewTypeKind::Enum,
                vals: rhs.list(3)?,
                query: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // Typename
        2 => {
            let type_name = rhs.take::<TypeName>(1)?;
            let stmt = if type_name.names.last().and_then(Node::str_val) == Some("enum") {
                CreateTypeStmt {
                    kind: NewTypeKind::Enum,
                    vals: type_name.typmods,
                    ..Default::default()
                }
            } else {
                CreateTypeStmt {
                    kind: NewTypeKind::Alias,
                    of_type: Some(type_name),
                    ..Default::default()
                }
            };
            Ok(SemValue::node(stmt))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_enum_val_list:
///       enum_val_list
///     | /* empty */
/// ```
pub(super) fn opt_enum_val_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// enum_val_list:
///       Sconst
///     | enum_val_list ',' Sconst
/// ```
pub(super) fn enum_val_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        0 => Ok(SemValue::List(vec![make_string_const(
            required(rhs.str(1)?)?,
            rhs.loc(1),
        )])),
        // enum_val_list ',' Sconst
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            make_string_const(required(rhs.str(3)?)?, rhs.loc(3)),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}
