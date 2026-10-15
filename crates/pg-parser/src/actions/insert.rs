//! Semantic actions for the rules in `grammar/statements/insert.y`.

use super::prelude::*;

/// ```text
/// InsertStmt:
///       opt_with_clause INSERT opt_or_action INTO insert_target opt_by_name_or_position insert_rest opt_on_conflict returning_clause
/// ```
pub(super) fn insert_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_with_clause INSERT opt_or_action INTO insert_target opt_by_name_or_position insert_rest opt_on_conflict returning_clause
        0 => {
            let mut insert_rest = rhs.take::<InsertStmt>(7)?;
            insert_rest.relation = rhs.ptr::<RangeVar>(5)?;
            insert_rest.on_conflict_alias = rhs.on_conflict_action_alias(3)?;
            insert_rest.on_conflict_clause = rhs.ptr::<OnConflictClause>(8)?;
            insert_rest.returning_list = rhs.list(9)?;
            insert_rest.with_clause = rhs.ptr::<WithClause>(1)?;
            insert_rest.insert_column_order = rhs.insert_column_order(6)?;
            Ok(SemValue::node(insert_rest))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// insert_rest:
///       SelectStmt
///     | OVERRIDING override_kind VALUE_P SelectStmt
///     | '(' insert_column_list ')' SelectStmt
///     | '(' insert_column_list ')' OVERRIDING override_kind VALUE_P SelectStmt
///     | DEFAULT VALUES
/// ```
pub(super) fn insert_rest<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SelectStmt
        0 => {
            let insert_rest = InsertStmt {
                cols: List::new(),
                select_stmt: rhs.node(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(insert_rest))
        }
        // OVERRIDING override_kind VALUE_P SelectStmt
        1 => {
            let insert_rest = InsertStmt {
                cols: List::new(),
                overriding: rhs.overriding_kind(2)?,
                select_stmt: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(insert_rest))
        }
        // '(' insert_column_list ')' SelectStmt
        2 => {
            let insert_rest = InsertStmt {
                cols: rhs.list(2)?,
                select_stmt: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(insert_rest))
        }
        // '(' insert_column_list ')' OVERRIDING override_kind VALUE_P SelectStmt
        3 => {
            let insert_rest = InsertStmt {
                cols: rhs.list(2)?,
                overriding: rhs.overriding_kind(5)?,
                select_stmt: rhs.node(7)?,
                ..Default::default()
            };
            Ok(SemValue::node(insert_rest))
        }
        // DEFAULT VALUES
        4 => {
            let insert_rest = InsertStmt {
                cols: List::new(),
                select_stmt: None,
                ..Default::default()
            };
            Ok(SemValue::node(insert_rest))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// insert_target:
///       qualified_name
///     | qualified_name AS ColId
/// ```
pub(super) fn insert_target<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualified_name AS ColId
        1 => {
            let mut qualified_name = rhs.take::<RangeVar>(1)?;
            qualified_name.alias = Some(Box::new(make_alias(rhs.string(3)?, List::new())));
            Ok(SemValue::node(qualified_name))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_by_name_or_position:
///       BY NAME_P
///     | BY POSITION
///     | /* empty */
/// ```
pub(super) fn opt_by_name_or_position<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BY NAME_P
        0 => Ok(SemValue::InsertColumnOrder(InsertColumnOrder::ByName)),
        // BY POSITION
        // /* empty */
        1 | 2 => Ok(SemValue::InsertColumnOrder(InsertColumnOrder::ByPosition)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_conf_expr:
///       '(' index_params ')' where_clause
///     | ON CONSTRAINT name
///     | /* empty */
/// ```
pub(super) fn opt_conf_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' index_params ')' where_clause
        0 => {
            let opt_conf_expr = InferClause {
                index_elems: rhs.list(2)?,
                where_clause: rhs.node(4)?,
                conname: None,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(opt_conf_expr))
        }
        // ON CONSTRAINT name
        1 => {
            let opt_conf_expr = InferClause {
                index_elems: List::new(),
                where_clause: None,
                conname: rhs.str(3)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(opt_conf_expr))
        }
        // /* empty */
        2 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_with_clause:
///       with_clause
///     | /* empty */
/// ```
pub(super) fn opt_with_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// insert_column_item:
///       ColId opt_indirection
/// ```
pub(super) fn insert_column_item<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId opt_indirection
        0 => {
            let insert_column_item = ResTarget {
                name: rhs.str(1)?,
                indirection: check_indirection(rhs.list(2)?, rhs.scanner())?,
                val: None,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(insert_column_item))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_clause:
///       set_target '=' a_expr
///     | '(' set_target_list ')' '=' a_expr
/// ```
pub(super) fn set_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // set_target '=' a_expr
        0 => {
            let mut set_target = rhs.take::<ResTarget>(1)?;
            set_target.val = rhs.node(3)?;
            Ok(SemValue::List(vec![set_target.into()]))
        }
        // '(' set_target_list ')' '=' a_expr
        1 => {
            let mut targets = rhs.list(2)?;
            let source = rhs.node(5)?;
            let ncolumns = i32::try_from(targets.len()).map_err(|_| internal("too many columns"))?;
            // every target gets a MultiAssignRef to the same source
            for (colno, target) in (1..).zip(targets.iter_mut()) {
                let Node::ResTarget(target) = target else {
                    return Err(internal("set_target_list holds a non-ResTarget"));
                };
                target.val = Some(
                    MultiAssignRef {
                        source: source.clone(),
                        colno,
                        ncolumns,
                    }
                    .into(),
                );
            }
            Ok(SemValue::List(targets))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_or_action:
///       OR REPLACE
///     | OR IGNORE_P
///     | /* empty */
/// ```
pub(super) fn opt_or_action<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // OR REPLACE
        0 => Ok(SemValue::OnConflictActionAlias(OnConflictActionAlias::Replace)),
        // OR IGNORE_P
        1 => Ok(SemValue::OnConflictActionAlias(OnConflictActionAlias::Ignore)),
        // /* empty */
        2 => Ok(SemValue::OnConflictActionAlias(OnConflictActionAlias::None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_on_conflict:
///       ON CONFLICT opt_conf_expr DO UPDATE SET set_clause_list_opt_comma where_clause
///     | ON CONFLICT opt_conf_expr DO NOTHING
///     | /* empty */
/// ```
pub(super) fn opt_on_conflict<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ON CONFLICT opt_conf_expr DO UPDATE SET set_clause_list_opt_comma where_clause
        0 => {
            let opt_on_conflict = OnConflictClause {
                action: OnConflictAction::Update,
                infer: rhs.ptr::<InferClause>(3)?,
                target_list: rhs.list(7)?,
                where_clause: rhs.node(8)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(opt_on_conflict))
        }
        // ON CONFLICT opt_conf_expr DO NOTHING
        1 => {
            let opt_on_conflict = OnConflictClause {
                action: OnConflictAction::Nothing,
                infer: rhs.ptr::<InferClause>(3)?,
                target_list: List::new(),
                where_clause: None,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(opt_on_conflict))
        }
        // /* empty */
        2 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// index_elem:
///       ColId opt_collate opt_class opt_asc_desc opt_nulls_order
///     | func_expr_windowless opt_collate opt_class opt_asc_desc opt_nulls_order
///     | '(' a_expr ')' opt_collate opt_class opt_asc_desc opt_nulls_order
/// ```
pub(super) fn index_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId opt_collate opt_class opt_asc_desc opt_nulls_order
        0 => {
            let index_elem = IndexElem {
                name: rhs.str(1)?,
                expr: None,
                collation: rhs.list(2)?,
                opclass: rhs.list(3)?,
                ordering: rhs.sort_by_dir(4)?,
                nulls_ordering: rhs.sort_by_nulls(5)?,
            };
            Ok(SemValue::node(index_elem))
        }
        // func_expr_windowless opt_collate opt_class opt_asc_desc opt_nulls_order
        1 => {
            let index_elem = IndexElem {
                name: None,
                expr: rhs.node(1)?,
                collation: rhs.list(2)?,
                opclass: rhs.list(3)?,
                ordering: rhs.sort_by_dir(4)?,
                nulls_ordering: rhs.sort_by_nulls(5)?,
            };
            Ok(SemValue::node(index_elem))
        }
        // '(' a_expr ')' opt_collate opt_class opt_asc_desc opt_nulls_order
        2 => {
            let index_elem = IndexElem {
                name: None,
                expr: rhs.node(2)?,
                collation: rhs.list(4)?,
                opclass: rhs.list(5)?,
                ordering: rhs.sort_by_dir(6)?,
                nulls_ordering: rhs.sort_by_nulls(7)?,
            };
            Ok(SemValue::node(index_elem))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// returning_clause:
///       RETURNING target_list
///     | /* empty */
/// ```
pub(super) fn returning_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// override_kind:
///       USER
///     | SYSTEM_P
/// ```
pub(super) fn override_kind<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // USER
        0 => Ok(SemValue::OverridingKind(OverridingKind::UserValue)),
        // SYSTEM_P
        1 => Ok(SemValue::OverridingKind(OverridingKind::SystemValue)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_target_list:
///       set_target
///     | set_target_list ',' set_target
/// ```
pub(super) fn set_target_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // set_target
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // set_target_list ',' set_target
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_collate:
///       COLLATE any_name
///     | /* empty */
/// ```
pub(super) fn opt_collate<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_class:
///       any_name
///     | /* empty */
/// ```
pub(super) fn opt_class<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// insert_column_list:
///       insert_column_item
///     | insert_column_list ',' insert_column_item
/// ```
pub(super) fn insert_column_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // insert_column_item
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // insert_column_list ',' insert_column_item
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_clause_list:
///       set_clause
///     | set_clause_list ',' set_clause
/// ```
pub(super) fn set_clause_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // set_clause_list ',' set_clause
        1 => Ok(SemValue::List(concat(rhs.list(1)?, rhs.list(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// index_params:
///       index_elem
///     | index_params ',' index_elem
/// ```
pub(super) fn index_params<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // index_elem
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // index_params ',' index_elem
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_target:
///       ColId opt_indirection
/// ```
pub(super) fn set_target<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId opt_indirection
        0 => {
            let set_target = ResTarget {
                name: rhs.str(1)?,
                indirection: check_indirection(rhs.list(2)?, rhs.scanner())?,
                val: None,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(set_target))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
