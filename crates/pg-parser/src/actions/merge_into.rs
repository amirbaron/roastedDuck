//! Semantic actions for the rules in `grammar/statements/merge_into.y`.

use super::prelude::*;

/// ```text
/// MergeIntoStmt:
///       opt_with_clause MERGE INTO relation_expr_opt_alias USING table_ref join_qual merge_match_list returning_clause
/// ```
pub(super) fn merge_into_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_with_clause MERGE INTO relation_expr_opt_alias USING table_ref join_qual merge_match_list returning_clause
        0 => {
            let join_qual = rhs.node(7)?;
            let mut n = MergeIntoStmt {
                target_table: rhs.ptr::<RangeVar>(4)?,
                source: rhs.node(6)?,
                ..Default::default()
            };
            if true && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.join_condition = join_qual;
            }
            n.match_actions = rhs.list(8)?;
            n.with_clause = rhs.ptr::<WithClause>(1)?;
            n.returning_list = rhs.list(9)?;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_and_clause:
///       AND a_expr
///     | /* empty */
/// ```
pub(super) fn opt_and_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_insert_column_list:
///       '(' insert_column_list ')'
///     | /* empty */
/// ```
pub(super) fn opt_insert_column_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// matched_clause_action:
///       UPDATE SET set_clause_list_opt_comma
///     | UPDATE SET '*'
///     | UPDATE opt_by_name_or_position
///     | DELETE_P
///     | INSERT opt_insert_column_list VALUES '(' expr_list_opt_comma ')'
///     | INSERT opt_by_name_or_position opt_star_expr
///     | INSERT DEFAULT VALUES
///     | DO NOTHING
///     | ERROR_P opt_error_message
/// ```
pub(super) fn matched_clause_action<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // UPDATE SET set_clause_list_opt_comma
        0 => {
            let n = MatchAction {
                action_type: MergeActionType::Update,
                insert_column_order: InsertColumnOrder::ByPosition,
                update_targets: rhs.list(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // UPDATE SET '*'
        1 => {
            let n = MatchAction {
                action_type: MergeActionType::Update,
                insert_column_order: InsertColumnOrder::ByPosition,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // UPDATE opt_by_name_or_position
        2 => {
            let n = MatchAction {
                action_type: MergeActionType::Update,
                insert_column_order: rhs.insert_column_order(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DELETE_P
        3 => {
            let n = MatchAction {
                action_type: MergeActionType::Delete,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // INSERT opt_insert_column_list VALUES '(' expr_list_opt_comma ')'
        4 => {
            let n = MatchAction {
                action_type: MergeActionType::Insert,
                insert_column_order: InsertColumnOrder::ByPosition,
                insert_cols: rhs.list(2)?,
                insert_values: rhs.list(5)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // INSERT opt_by_name_or_position opt_star_expr
        5 => {
            let n = MatchAction {
                action_type: MergeActionType::Insert,
                insert_column_order: rhs.insert_column_order(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // INSERT DEFAULT VALUES
        6 => {
            let n = MatchAction {
                action_type: MergeActionType::Insert,
                insert_column_order: InsertColumnOrder::ByPosition,
                default_values: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DO NOTHING
        7 => {
            let n = MatchAction {
                action_type: MergeActionType::DoNothing,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ERROR_P opt_error_message
        8 => {
            let n = MatchAction {
                action_type: MergeActionType::Error,
                error_message: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_error_message:
///       a_expr
///     | /* empty */
/// ```
pub(super) fn opt_error_message<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// matched_clause:
///       WHEN MATCHED opt_and_clause THEN matched_clause_action
/// ```
pub(super) fn matched_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WHEN MATCHED opt_and_clause THEN matched_clause_action
        0 => {
            let mut n = rhs.take::<MatchAction>(5)?;
            n.when = MergeAction::Matched;
            n.and_clause = rhs.node(3)?;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_source_or_target:
///       BY SOURCE_P
///     | BY TARGET_P
///     | /* empty */
/// ```
pub(super) fn opt_source_or_target<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BY SOURCE_P
        0 => Ok(SemValue::MergeAction(MergeAction::NotMatchedBySource)),
        // BY TARGET_P
        // /* empty */
        1 | 2 => Ok(SemValue::MergeAction(MergeAction::NotMatchedByTarget)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// not_matched_clause:
///       WHEN NOT MATCHED opt_source_or_target opt_and_clause THEN matched_clause_action
/// ```
pub(super) fn not_matched_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WHEN NOT MATCHED opt_source_or_target opt_and_clause THEN matched_clause_action
        0 => {
            let mut n = rhs.take::<MatchAction>(7)?;
            n.when = rhs.merge_action(4)?;
            n.and_clause = rhs.node(5)?;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// merge_match_list:
///       matched_or_not_matched_clause
///     | matched_or_not_matched_clause merge_match_list
/// ```
pub(super) fn merge_match_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // matched_or_not_matched_clause
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // matched_or_not_matched_clause merge_match_list
        1 => Ok(SemValue::List(concat(vec![rhs.required_node(1)?], rhs.list(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}
