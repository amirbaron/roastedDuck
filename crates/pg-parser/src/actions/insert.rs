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
            Ok(SemValue::node(Node::from(insert_rest)))
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
            let mut insert_rest = InsertStmt::default();
            insert_rest.cols = List::new();
            insert_rest.select_stmt = rhs.node(1)?;
            Ok(SemValue::node(insert_rest))
        }
        // OVERRIDING override_kind VALUE_P SelectStmt
        1 => {
            let mut insert_rest = InsertStmt::default();
            insert_rest.cols = List::new();
            insert_rest.overriding = rhs.overriding_kind(2)?;
            insert_rest.select_stmt = rhs.node(4)?;
            Ok(SemValue::node(insert_rest))
        }
        // '(' insert_column_list ')' SelectStmt
        2 => {
            let mut insert_rest = InsertStmt::default();
            insert_rest.cols = rhs.list(2)?;
            insert_rest.select_stmt = rhs.node(4)?;
            Ok(SemValue::node(insert_rest))
        }
        // '(' insert_column_list ')' OVERRIDING override_kind VALUE_P SelectStmt
        3 => {
            let mut insert_rest = InsertStmt::default();
            insert_rest.cols = rhs.list(2)?;
            insert_rest.overriding = rhs.overriding_kind(5)?;
            insert_rest.select_stmt = rhs.node(7)?;
            Ok(SemValue::node(insert_rest))
        }
        // DEFAULT VALUES
        4 => {
            let mut insert_rest = InsertStmt::default();
            insert_rest.cols = List::new();
            insert_rest.select_stmt = None;
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
        // qualified_name
        0 => Ok(SemValue::Node((rhs.ptr::<RangeVar>(1)?).map(Node::from))),
        // qualified_name AS ColId
        1 => {
            let mut qualified_name = rhs.take::<RangeVar>(1)?;
            qualified_name.alias = Some(Box::new(make_alias(required(rhs.str(3)?)?, List::new())));
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
        1 => Ok(SemValue::InsertColumnOrder(InsertColumnOrder::ByPosition)),
        // /* empty */
        2 => Ok(SemValue::InsertColumnOrder(InsertColumnOrder::ByPosition)),
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
            let mut opt_conf_expr = InferClause::default();
            opt_conf_expr.index_elems = rhs.list(2)?;
            opt_conf_expr.where_clause = rhs.node(4)?;
            opt_conf_expr.conname = None;
            opt_conf_expr.location = rhs.loc(1);
            Ok(SemValue::node(opt_conf_expr))
        }
        // ON CONSTRAINT name
        1 => {
            let mut opt_conf_expr = InferClause::default();
            opt_conf_expr.index_elems = List::new();
            opt_conf_expr.where_clause = None;
            opt_conf_expr.conname = rhs.str(3)?;
            opt_conf_expr.location = rhs.loc(1);
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
        // with_clause
        0 => Ok(SemValue::Node((rhs.ptr::<WithClause>(1)?).map(Node::from))),
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
            let mut insert_column_item = ResTarget::default();
            insert_column_item.name = rhs.str(1)?;
            insert_column_item.indirection = check_indirection(rhs.list(2)?, rhs.scanner())?;
            insert_column_item.val = None;
            insert_column_item.location = rhs.loc(1);
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
            let mut opt_on_conflict = OnConflictClause::default();
            opt_on_conflict.action = OnConflictAction::Update;
            opt_on_conflict.infer = rhs.ptr::<InferClause>(3)?;
            opt_on_conflict.target_list = rhs.list(7)?;
            opt_on_conflict.where_clause = rhs.node(8)?;
            opt_on_conflict.location = rhs.loc(1);
            Ok(SemValue::node(opt_on_conflict))
        }
        // ON CONFLICT opt_conf_expr DO NOTHING
        1 => {
            let mut opt_on_conflict = OnConflictClause::default();
            opt_on_conflict.action = OnConflictAction::Nothing;
            opt_on_conflict.infer = rhs.ptr::<InferClause>(3)?;
            opt_on_conflict.target_list = List::new();
            opt_on_conflict.where_clause = None;
            opt_on_conflict.location = rhs.loc(1);
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
            let mut index_elem = IndexElem::default();
            index_elem.name = rhs.str(1)?;
            index_elem.expr = None;
            index_elem.collation = rhs.list(2)?;
            index_elem.opclass = rhs.list(3)?;
            index_elem.ordering = rhs.sort_by_dir(4)?;
            index_elem.nulls_ordering = rhs.sort_by_nulls(5)?;
            Ok(SemValue::node(index_elem))
        }
        // func_expr_windowless opt_collate opt_class opt_asc_desc opt_nulls_order
        1 => {
            let mut index_elem = IndexElem::default();
            index_elem.name = None;
            index_elem.expr = rhs.node(1)?;
            index_elem.collation = rhs.list(2)?;
            index_elem.opclass = rhs.list(3)?;
            index_elem.ordering = rhs.sort_by_dir(4)?;
            index_elem.nulls_ordering = rhs.sort_by_nulls(5)?;
            Ok(SemValue::node(index_elem))
        }
        // '(' a_expr ')' opt_collate opt_class opt_asc_desc opt_nulls_order
        2 => {
            let mut index_elem = IndexElem::default();
            index_elem.name = None;
            index_elem.expr = rhs.node(2)?;
            index_elem.collation = rhs.list(4)?;
            index_elem.opclass = rhs.list(5)?;
            index_elem.ordering = rhs.sort_by_dir(6)?;
            index_elem.nulls_ordering = rhs.sort_by_nulls(7)?;
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
        // RETURNING target_list
        0 => Ok(SemValue::List(rhs.list(2)?)),
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
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<ResTarget>(1)?,
        )?)])),
        // set_target_list ',' set_target
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<ResTarget>(3)?)?),
        ))),
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
        // COLLATE any_name
        0 => Ok(SemValue::List(rhs.list(2)?)),
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
        // any_name
        0 => Ok(SemValue::List(rhs.list(1)?)),
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
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<ResTarget>(1)?,
        )?)])),
        // insert_column_list ',' insert_column_item
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<ResTarget>(3)?)?),
        ))),
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
        // set_clause
        0 => Ok(SemValue::List(rhs.list(1)?)),
        // set_clause_list ',' set_clause
        1 => Ok(SemValue::List(concat(rhs.list(1)?, rhs.list(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// set_clause_list_opt_comma:
///       set_clause_list
///     | set_clause_list ','
/// ```
pub(super) fn set_clause_list_opt_comma<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // set_clause_list
        0 => Ok(SemValue::List(rhs.list(1)?)),
        // set_clause_list ','
        1 => Ok(SemValue::List(rhs.list(1)?)),
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
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<IndexElem>(1)?,
        )?)])),
        // index_params ',' index_elem
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<IndexElem>(3)?)?),
        ))),
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
            let mut set_target = ResTarget::default();
            set_target.name = rhs.str(1)?;
            set_target.indirection = check_indirection(rhs.list(2)?, rhs.scanner())?;
            set_target.val = None;
            set_target.location = rhs.loc(1);
            Ok(SemValue::node(set_target))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
