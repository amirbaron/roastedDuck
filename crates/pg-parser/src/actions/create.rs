//! Semantic actions for the rules in `grammar/statements/create.y`.

use super::prelude::*;

/// ```text
/// CreateStmt:
///       CREATE_P OptTemp TABLE qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
///     | CREATE_P OptTemp TABLE IF_P NOT EXISTS qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
///     | CREATE_P OR REPLACE OptTemp TABLE qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
/// ```
pub(super) fn create_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp TABLE qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
        0 => {
            let mut relation = rhs.take::<RangeVar>(4)?;
            relation.relpersistence = rhs.rel_persistence(2)?;
            let (partition_list, sort_list) = partition_and_sort_lists(rhs.list(8)?)?;
            Ok(SemValue::node(CreateStmt {
                relation: Some(relation),
                table_elts: rhs.list(6)?,
                partition_list,
                sort_list,
                options: rhs.list(9)?,
                oncommit: rhs.on_commit_action(10)?,
                onconflict: OnCreateConflict::Error,
                ..Default::default()
            }))
        }
        // CREATE_P OptTemp TABLE IF_P NOT EXISTS qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
        1 => {
            let mut relation = rhs.take::<RangeVar>(7)?;
            relation.relpersistence = rhs.rel_persistence(2)?;
            let (partition_list, sort_list) = partition_and_sort_lists(rhs.list(11)?)?;
            Ok(SemValue::node(CreateStmt {
                relation: Some(relation),
                table_elts: rhs.list(9)?,
                partition_list,
                sort_list,
                options: rhs.list(12)?,
                oncommit: rhs.on_commit_action(13)?,
                onconflict: OnCreateConflict::Ignore,
                ..Default::default()
            }))
        }
        // CREATE_P OR REPLACE OptTemp TABLE qualified_name '(' OptTableElementList ')' OptPartitionSortedOptions OptWith OnCommitOption
        2 => {
            let mut relation = rhs.take::<RangeVar>(6)?;
            relation.relpersistence = rhs.rel_persistence(4)?;
            let (partition_list, sort_list) = partition_and_sort_lists(rhs.list(10)?)?;
            Ok(SemValue::node(CreateStmt {
                relation: Some(relation),
                table_elts: rhs.list(8)?,
                partition_list,
                sort_list,
                options: rhs.list(11)?,
                oncommit: rhs.on_commit_action(12)?,
                onconflict: OnCreateConflict::Replace,
                ..Default::default()
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstraintAttributeSpec:
///       /* empty */
///     | ConstraintAttributeSpec ConstraintAttributeElem
/// ```
pub(super) fn constraint_attribute_spec<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Int(0)),
        // ConstraintAttributeSpec ConstraintAttributeElem
        1 => {
            let newspec = rhs.int(1)? | rhs.int(2)?;
            if (newspec & (constraint_attr::NOT_DEFERRABLE | constraint_attr::INITIALLY_DEFERRED))
                == constraint_attr::NOT_DEFERRABLE | constraint_attr::INITIALLY_DEFERRED
            {
                return Err(rhs.error_at(
                    "constraint declared INITIALLY DEFERRED must be DEFERRABLE",
                    rhs.loc(2),
                ));
            }
            if (newspec & (constraint_attr::NOT_DEFERRABLE | constraint_attr::DEFERRABLE))
                == constraint_attr::NOT_DEFERRABLE | constraint_attr::DEFERRABLE
                || (newspec & (constraint_attr::INITIALLY_IMMEDIATE | constraint_attr::INITIALLY_DEFERRED))
                    == constraint_attr::INITIALLY_IMMEDIATE | constraint_attr::INITIALLY_DEFERRED
            {
                return Err(rhs.error_at("conflicting constraint properties", rhs.loc(2)));
            }
            Ok(SemValue::Int(newspec))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// def_arg:
///       func_type
///     | reserved_keyword
///     | qual_all_Op
///     | NumericOnly
///     | Sconst
///     | NONE
/// ```
pub(super) fn def_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_type
        0 => Ok(SemValue::Node(rhs.ptr::<TypeName>(1)?.map(Node::from))),
        // reserved_keyword
        1 => Ok(SemValue::node(Node::Value(Value::String(rhs.keyword(1)?.into())))),
        // qual_all_Op
        2 => Ok(SemValue::Node(Node::from_list(rhs.list(1)?))),
        // NumericOnly
        3 => Ok(SemValue::Node(rhs.value(1)?.map(Node::Value))),
        // Sconst
        4 => Ok(SemValue::node(Node::Value(Value::String(
            required(rhs.str(1)?)?.into(),
        )))),
        // NONE
        5 => Ok(SemValue::node(Node::Value(Value::String(rhs.keyword(1)?.into())))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptParenthesizedSeqOptList:
///       '(' SeqOptList ')'
///     | /* empty */
/// ```
pub(super) fn opt_parenthesized_seq_opt_list<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // '(' SeqOptList ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_option_arg:
///       Sconst
/// ```
pub(super) fn generic_option_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        0 => Ok(SemValue::node(Node::Value(Value::String(
            required(rhs.str(1)?)?.into(),
        )))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// key_action:
///       NO ACTION
///     | RESTRICT
///     | CASCADE
///     | SET NULL_P
///     | SET DEFAULT
/// ```
pub(super) fn key_action<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NO ACTION
        0 => Ok(SemValue::Int(i32::from(foreign_key::ACTION_NOACTION))),
        // RESTRICT
        1 => Ok(SemValue::Int(i32::from(foreign_key::ACTION_RESTRICT))),
        // CASCADE
        2 => Ok(SemValue::Int(i32::from(foreign_key::ACTION_CASCADE))),
        // SET NULL_P
        3 => Ok(SemValue::Int(i32::from(foreign_key::ACTION_SETNULL))),
        // SET DEFAULT
        4 => Ok(SemValue::Int(i32::from(foreign_key::ACTION_SETDEFAULT))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ColConstraint:
///       CONSTRAINT name ColConstraintElem
///     | ColConstraintElem
///     | ConstraintAttr
///     | COLLATE any_name
/// ```
pub(super) fn col_constraint<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CONSTRAINT name ColConstraintElem
        0 => {
            let mut n = rhs.take::<Constraint>(3)?;
            n.conname = rhs.str(2)?;
            n.location = rhs.loc(1);
            Ok(SemValue::node(Node::from(n)))
        }
        // ColConstraintElem
        1 => Ok(SemValue::Node(rhs.node(1)?)),
        // ConstraintAttr
        2 => Ok(SemValue::Node(rhs.node(1)?)),
        // COLLATE any_name
        3 => {
            let n = CollateClause {
                arg: None,
                collname: rhs.list(2)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ColConstraintElem:
///       NOT NULL_P
///     | NULL_P
///     | UNIQUE opt_definition
///     | PRIMARY KEY opt_definition
///     | CHECK_P '(' a_expr ')' opt_no_inherit
///     | USING COMPRESSION name
///     | DEFAULT b_expr
///     | REFERENCES qualified_name opt_column_list key_match key_actions
/// ```
pub(super) fn col_constraint_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NOT NULL_P
        0 => {
            let n = Constraint {
                contype: ConstrType::NotNull,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // NULL_P
        1 => {
            let n = Constraint {
                contype: ConstrType::Null,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // UNIQUE opt_definition
        2 => {
            let n = Constraint {
                contype: ConstrType::Unique,
                location: rhs.loc(1),
                keys: List::new(),
                options: rhs.list(2)?,
                indexname: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // PRIMARY KEY opt_definition
        3 => {
            let n = Constraint {
                contype: ConstrType::Primary,
                location: rhs.loc(1),
                keys: List::new(),
                options: rhs.list(3)?,
                indexname: None,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // CHECK_P '(' a_expr ')' opt_no_inherit
        4 => {
            let n = Constraint {
                contype: ConstrType::Check,
                location: rhs.loc(1),
                is_no_inherit: rhs.bool(5)?,
                raw_expr: rhs.node(3)?,
                skip_validation: false,
                initially_valid: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // USING COMPRESSION name
        5 => {
            let n = Constraint {
                contype: ConstrType::Compression,
                location: rhs.loc(1),
                compression_name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // DEFAULT b_expr
        6 => {
            let n = Constraint {
                contype: ConstrType::Default,
                location: rhs.loc(1),
                raw_expr: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // REFERENCES qualified_name opt_column_list key_match key_actions
        7 => {
            let key_actions = rhs.int(5)?;
            let n = Constraint {
                contype: ConstrType::Foreign,
                location: rhs.loc(1),
                pktable: rhs.ptr::<RangeVar>(2)?,
                fk_attrs: List::new(),
                pk_attrs: rhs.list(3)?,
                fk_matchtype: rhs.int(4)? as u8,
                fk_upd_action: (key_actions >> 8) as u8,
                fk_del_action: (key_actions & 255) as u8,
                skip_validation: false,
                initially_valid: true,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// GeneratedColumnType:
///       VIRTUAL
///     | STORED
/// ```
pub(super) fn generated_column_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // VIRTUAL
        0 => Ok(SemValue::ConstrType(ConstrType::GeneratedVirtual)),
        // STORED
        1 => Ok(SemValue::ConstrType(ConstrType::GeneratedStored)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_GeneratedColumnType:
///       GeneratedColumnType
///     | /* empty */
/// ```
pub(super) fn opt_generated_column_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // GeneratedColumnType
        0 => Ok(SemValue::ConstrType(rhs.constr_type(1)?)),
        // /* empty */
        1 => Ok(SemValue::ConstrType(ConstrType::GeneratedVirtual)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// GeneratedConstraintElem:
///       GENERATED generated_when AS IDENTITY_P OptParenthesizedSeqOptList
///     | GENERATED generated_when AS '(' a_expr ')' opt_GeneratedColumnType
///     | AS '(' a_expr ')' opt_GeneratedColumnType
/// ```
pub(super) fn generated_constraint_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // GENERATED generated_when AS IDENTITY_P OptParenthesizedSeqOptList
        0 => {
            let n = Constraint {
                contype: ConstrType::Identity,
                generated_when: rhs.int(2)? as u8,
                options: rhs.list(5)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // GENERATED generated_when AS '(' a_expr ')' opt_GeneratedColumnType
        1 => {
            let generated_when = rhs.int(2)?;
            let n = Constraint {
                contype: rhs.constr_type(7)?,
                generated_when: generated_when as u8,
                raw_expr: rhs.node(5)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            if generated_when != i32::from(identity::ALWAYS) {
                return Err(rhs.error_at(
                    "for a generated column, GENERATED ALWAYS must be specified",
                    rhs.loc(2),
                ));
            }
            Ok(SemValue::node(Node::from(n)))
        }
        // AS '(' a_expr ')' opt_GeneratedColumnType
        2 => {
            let n = Constraint {
                contype: rhs.constr_type(5)?,
                generated_when: identity::ALWAYS,
                raw_expr: rhs.node(3)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_option_elem:
///       generic_option_name generic_option_arg
/// ```
pub(super) fn generic_option_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // generic_option_name generic_option_arg
        0 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// key_update:
///       ON UPDATE key_action
/// ```
pub(super) fn key_update<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ON UPDATE key_action
        0 => Ok(SemValue::Int(rhs.int(3)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// key_actions:
///       key_update
///     | key_delete
///     | key_update key_delete
///     | key_delete key_update
///     | /* empty */
/// ```
pub(super) fn key_actions<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // key_update
        0 => Ok(SemValue::Int(
            (rhs.int(1)? << 8) | (i32::from(foreign_key::ACTION_NOACTION) & 255),
        )),
        // key_delete
        1 => Ok(SemValue::Int(
            (i32::from(foreign_key::ACTION_NOACTION) << 8) | (rhs.int(1)? & 255),
        )),
        // key_update key_delete
        2 => Ok(SemValue::Int((rhs.int(1)? << 8) | (rhs.int(2)? & 255))),
        // key_delete key_update
        3 => Ok(SemValue::Int((rhs.int(2)? << 8) | (rhs.int(1)? & 255))),
        // /* empty */
        4 => Ok(SemValue::Int(
            (i32::from(foreign_key::ACTION_NOACTION) << 8) | (i32::from(foreign_key::ACTION_NOACTION) & 255),
        )),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OnCommitOption:
///       ON COMMIT DROP
///     | ON COMMIT DELETE_P ROWS
///     | ON COMMIT PRESERVE ROWS
///     | /* empty */
/// ```
pub(super) fn on_commit_option<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ON COMMIT DROP
        0 => Ok(SemValue::OnCommitAction(OnCommitAction::Drop)),
        // ON COMMIT DELETE_P ROWS
        1 => Ok(SemValue::OnCommitAction(OnCommitAction::DeleteRows)),
        // ON COMMIT PRESERVE ROWS
        2 => Ok(SemValue::OnCommitAction(OnCommitAction::PreserveRows)),
        // /* empty */
        3 => Ok(SemValue::OnCommitAction(OnCommitAction::Noop)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// reloptions:
///       '(' reloption_list ')'
/// ```
pub(super) fn reloptions<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' reloption_list ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_no_inherit:
///       NO INHERIT
///     | /* empty */
/// ```
pub(super) fn opt_no_inherit<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NO INHERIT
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableConstraint:
///       CONSTRAINT name ConstraintElem
///     | ConstraintElem
/// ```
pub(super) fn table_constraint<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CONSTRAINT name ConstraintElem
        0 => {
            let mut n = rhs.take::<Constraint>(3)?;
            n.conname = rhs.str(2)?;
            n.location = rhs.loc(1);
            Ok(SemValue::node(Node::from(n)))
        }
        // ConstraintElem
        1 => Ok(SemValue::Node(rhs.node(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableLikeOption:
///       COMMENTS
///     | CONSTRAINTS
///     | DEFAULTS
///     | IDENTITY_P
///     | INDEXES
///     | STATISTICS
///     | STORAGE
///     | ALL
/// ```
pub(super) fn table_like_option<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COMMENTS
        0 => Ok(SemValue::Int(table_like_option::COMMENTS)),
        // CONSTRAINTS
        1 => Ok(SemValue::Int(table_like_option::CONSTRAINTS)),
        // DEFAULTS
        2 => Ok(SemValue::Int(table_like_option::DEFAULTS)),
        // IDENTITY_P
        3 => Ok(SemValue::Int(table_like_option::IDENTITY)),
        // INDEXES
        4 => Ok(SemValue::Int(table_like_option::INDEXES)),
        // STATISTICS
        5 => Ok(SemValue::Int(table_like_option::STATISTICS)),
        // STORAGE
        6 => Ok(SemValue::Int(table_like_option::STORAGE)),
        // ALL
        7 => Ok(SemValue::Int(table_like_option::ALL)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// reloption_list:
///       reloption_elem
///     | reloption_list ',' reloption_elem
/// ```
pub(super) fn reloption_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // reloption_elem
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<DefElem>(1)?,
        )?)])),
        // reloption_list ',' reloption_elem
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<DefElem>(3)?)?),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ExistingIndex:
///       USING INDEX index_name
/// ```
pub(super) fn existing_index<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // USING INDEX index_name
        0 => Ok(SemValue::Str(rhs.str(3)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstraintAttr:
///       DEFERRABLE
///     | NOT DEFERRABLE
///     | INITIALLY DEFERRED
///     | INITIALLY IMMEDIATE
/// ```
pub(super) fn constraint_attr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DEFERRABLE
        0 => {
            let n = Constraint {
                contype: ConstrType::AttrDeferrable,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // NOT DEFERRABLE
        1 => {
            let n = Constraint {
                contype: ConstrType::AttrNotDeferrable,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // INITIALLY DEFERRED
        2 => {
            let n = Constraint {
                contype: ConstrType::AttrDeferred,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // INITIALLY IMMEDIATE
        3 => {
            let n = Constraint {
                contype: ConstrType::AttrImmediate,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptPartitionSortedOptions:
///       OptPartitionSortedOptions OptPartitionSortedOption
///     | /* empty */
/// ```
pub(super) fn opt_partition_sorted_options<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // OptPartitionSortedOptions OptPartitionSortedOption
        0 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<DefElem>(2)?)?),
        ))),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptPartitionSortedOption:
///       PARTITIONED BY '(' expr_list_opt_comma ')'
///     | SORTED BY '(' expr_list_opt_comma ')'
/// ```
pub(super) fn opt_partition_sorted_option<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // PARTITIONED BY '(' expr_list_opt_comma ')'
        0 => Ok(SemValue::node(make_def_elem(
            "partitioned_by",
            Node::from_list(rhs.list(4)?),
            rhs.loc(1),
        ))),
        // SORTED BY '(' expr_list_opt_comma ')'
        1 => Ok(SemValue::node(make_def_elem(
            "sorted_by",
            Node::from_list(rhs.list(4)?),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptWith:
///       WITH reloptions
///     | WITH OIDS
///     | WITHOUT OIDS
///     | /* empty */
/// ```
pub(super) fn opt_with<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH reloptions
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // WITH OIDS
        1 => Ok(SemValue::List(vec![
            make_def_elem(
                "oids",
                Some(Node::Value(Value::Integer(i64::from(true)))),
                rhs.loc(1),
            )
            .into(),
        ])),
        // WITHOUT OIDS
        2 => Ok(SemValue::List(vec![
            make_def_elem(
                "oids",
                Some(Node::Value(Value::Integer(i64::from(false)))),
                rhs.loc(1),
            )
            .into(),
        ])),
        // /* empty */
        3 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// definition:
///       '(' def_list ')'
/// ```
pub(super) fn definition<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' def_list ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableLikeOptionList:
///       TableLikeOptionList INCLUDING TableLikeOption
///     | TableLikeOptionList EXCLUDING TableLikeOption
///     | /* empty */
/// ```
pub(super) fn table_like_option_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TableLikeOptionList INCLUDING TableLikeOption
        0 => Ok(SemValue::Int(rhs.int(1)? | rhs.int(3)?)),
        // TableLikeOptionList EXCLUDING TableLikeOption
        1 => Ok(SemValue::Int(rhs.int(1)? & (!rhs.int(3)?))),
        // /* empty */
        2 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generic_option_name:
///       ColLabel
/// ```
pub(super) fn generic_option_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColLabel
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstraintAttributeElem:
///       NOT DEFERRABLE
///     | DEFERRABLE
///     | INITIALLY IMMEDIATE
///     | INITIALLY DEFERRED
///     | NOT VALID
///     | NO INHERIT
/// ```
pub(super) fn constraint_attribute_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NOT DEFERRABLE
        0 => Ok(SemValue::Int(constraint_attr::NOT_DEFERRABLE)),
        // DEFERRABLE
        1 => Ok(SemValue::Int(constraint_attr::DEFERRABLE)),
        // INITIALLY IMMEDIATE
        2 => Ok(SemValue::Int(constraint_attr::INITIALLY_IMMEDIATE)),
        // INITIALLY DEFERRED
        3 => Ok(SemValue::Int(constraint_attr::INITIALLY_DEFERRED)),
        // NOT VALID
        4 => Ok(SemValue::Int(constraint_attr::NOT_VALID)),
        // NO INHERIT
        5 => Ok(SemValue::Int(constraint_attr::NO_INHERIT)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// regularColumnDef:
///       Typename ColQualList
/// ```
pub(super) fn regular_column_def<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Typename ColQualList
        0 => {
            let (constraints, coll_clause) = split_col_qual_list(rhs.list(2)?, rhs.scanner())?;
            Ok(SemValue::node(ColumnDef {
                category: ColumnCategory::Standard,
                type_name: rhs.ptr::<TypeName>(1)?,
                constraints,
                coll_clause,
                ..Default::default()
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generatedColumnDef:
///       opt_Typename GeneratedConstraintElem ColQualList
/// ```
pub(super) fn generated_column_def<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_Typename GeneratedConstraintElem ColQualList
        0 => {
            // merge the constraints with the generated column constraint
            let mut quals = rhs.list(3)?;
            quals.push(rhs.required_node(2)?);
            let (constraints, coll_clause) = split_col_qual_list(quals, rhs.scanner())?;
            Ok(SemValue::node(ColumnDef {
                category: ColumnCategory::Generated,
                type_name: rhs.ptr::<TypeName>(1)?,
                constraints,
                coll_clause,
                ..Default::default()
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnDef:
///       ColId regularColumnDef
///     | ColId generatedColumnDef
/// ```
pub(super) fn column_def<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId regularColumnDef
        0 => {
            let mut n = rhs.take::<ColumnDef>(2)?;
            n.colname = rhs.str(1)?;
            n.location = rhs.loc(1);
            Ok(SemValue::node(Node::from(n)))
        }
        // ColId generatedColumnDef
        1 => {
            let mut n = rhs.take::<ColumnDef>(2)?;
            n.colname = rhs.str(1)?;
            n.location = rhs.loc(1);
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// def_list:
///       def_elem
///     | def_list ',' def_elem
/// ```
pub(super) fn def_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // def_elem
        0 => Ok(SemValue::List(vec![Node::from(required(
            rhs.ptr::<DefElem>(1)?,
        )?)])),
        // def_list ',' def_elem
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::from(required(rhs.ptr::<DefElem>(3)?)?),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// index_name:
///       ColId
/// ```
pub(super) fn index_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableElement:
///       columnDef
///     | TableLikeClause
///     | TableConstraint
/// ```
pub(super) fn table_element<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // columnDef
        0 => Ok(SemValue::Node(rhs.node(1)?)),
        // TableLikeClause
        1 => Ok(SemValue::Node(rhs.node(1)?)),
        // TableConstraint
        2 => Ok(SemValue::Node(rhs.node(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// def_elem:
///       ColLabel '=' def_arg
///     | ColLabel
/// ```
pub(super) fn def_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColLabel '=' def_arg
        0 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            rhs.node(3)?,
            rhs.loc(1),
        ))),
        // ColLabel
        1 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            None,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_definition:
///       WITH definition
///     | /* empty */
/// ```
pub(super) fn opt_definition<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH definition
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptTableElementList:
///       TableElementList
///     | TableElementList ','
///     | /* empty */
/// ```
pub(super) fn opt_table_element_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TableElementList
        0 => Ok(SemValue::List(rhs.list(1)?)),
        // TableElementList ','
        1 => Ok(SemValue::List(rhs.list(1)?)),
        // /* empty */
        2 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnElem:
///       ColId
/// ```
pub(super) fn column_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::node(Node::Value(Value::String(
            required(rhs.str(1)?)?.into(),
        )))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_column_list:
///       '(' columnList ')'
///     | /* empty */
/// ```
pub(super) fn opt_column_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' columnList ')'
        0 => Ok(SemValue::List(rhs.list(2)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ColQualList:
///       ColQualList ColConstraint
///     | /* empty */
/// ```
pub(super) fn col_qual_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColQualList ColConstraint
        0 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(2)?)?))),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// key_delete:
///       ON DELETE_P key_action
/// ```
pub(super) fn key_delete<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ON DELETE_P key_action
        0 => Ok(SemValue::Int(rhs.int(3)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// reloption_elem:
///       ColLabel '=' a_expr
///     | ColLabel
///     | ColLabel '.' ColLabel '=' def_arg
///     | ColLabel '.' ColLabel
///     | Sconst '=' a_expr
///     | Sconst
/// ```
pub(super) fn reloption_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColLabel '=' a_expr
        0 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            rhs.node(3)?,
            rhs.loc(1),
        ))),
        // ColLabel
        1 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            None,
            rhs.loc(1),
        ))),
        // ColLabel '.' ColLabel '=' def_arg
        2 => Ok(SemValue::node(make_def_elem_extended(
            rhs.str(1)?,
            required(rhs.str(3)?)?,
            rhs.node(5)?,
            DefElemAction::Unspec,
            rhs.loc(1),
        ))),
        // ColLabel '.' ColLabel
        3 => Ok(SemValue::node(make_def_elem_extended(
            rhs.str(1)?,
            required(rhs.str(3)?)?,
            None,
            DefElemAction::Unspec,
            rhs.loc(1),
        ))),
        // Sconst '=' a_expr
        4 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            rhs.node(3)?,
            rhs.loc(1),
        ))),
        // Sconst
        5 => Ok(SemValue::node(make_def_elem(
            required(rhs.str(1)?)?,
            None,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnList:
///       columnElem
///     | columnList ',' columnElem
/// ```
pub(super) fn column_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // columnElem
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // columnList ',' columnElem
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnList_opt_comma:
///       columnList
///     | columnList ','
/// ```
pub(super) fn column_list_opt_comma<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // columnList
        0 => Ok(SemValue::List(rhs.list(1)?)),
        // columnList ','
        1 => Ok(SemValue::List(rhs.list(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_type:
///       Typename
///     | type_function_name attrs '%' TYPE_P
///     | SETOF type_function_name attrs '%' TYPE_P
/// ```
pub(super) fn func_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Typename
        0 => Ok(SemValue::Node((rhs.ptr::<TypeName>(1)?).map(Node::from))),
        // type_function_name attrs '%' TYPE_P
        1 => {
            let mut func_type = make_type_name_from_name_list(prepend(
                Node::Value(Value::String(required(rhs.str(1)?)?.into())),
                rhs.list(2)?,
            ));
            func_type.pct_type = true;
            func_type.location = rhs.loc(1);
            Ok(SemValue::node(func_type))
        }
        // SETOF type_function_name attrs '%' TYPE_P
        2 => {
            let mut func_type = make_type_name_from_name_list(prepend(
                Node::Value(Value::String(required(rhs.str(2)?)?.into())),
                rhs.list(3)?,
            ));
            func_type.pct_type = true;
            func_type.setof = true;
            func_type.location = rhs.loc(2);
            Ok(SemValue::node(func_type))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstraintElem:
///       CHECK_P '(' a_expr ')' ConstraintAttributeSpec
///     | UNIQUE '(' columnList_opt_comma ')' opt_definition ConstraintAttributeSpec
///     | UNIQUE ExistingIndex ConstraintAttributeSpec
///     | PRIMARY KEY '(' columnList_opt_comma ')' opt_definition ConstraintAttributeSpec
///     | PRIMARY KEY ExistingIndex ConstraintAttributeSpec
///     | FOREIGN KEY '(' columnList_opt_comma ')' REFERENCES qualified_name opt_column_list key_match key_actions ConstraintAttributeSpec
/// ```
pub(super) fn constraint_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CHECK_P '(' a_expr ')' ConstraintAttributeSpec
        0 => {
            let mut constraint = Constraint {
                contype: ConstrType::Check,
                location: rhs.loc(1),
                raw_expr: rhs.node(3)?,
                ..Default::default()
            };
            let targets = CasTargets {
                not_valid: Some(&mut constraint.skip_validation),
                no_inherit: Some(&mut constraint.is_no_inherit),
                ..Default::default()
            };
            process_cas_bits(rhs.int(5)?, rhs.loc(5), "CHECK", targets, rhs.scanner())?;
            constraint.initially_valid = !constraint.skip_validation;
            Ok(SemValue::node(constraint))
        }
        // UNIQUE '(' columnList_opt_comma ')' opt_definition ConstraintAttributeSpec
        1 => {
            let mut constraint = Constraint {
                contype: ConstrType::Unique,
                location: rhs.loc(1),
                keys: rhs.list(3)?,
                options: rhs.list(5)?,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                ..Default::default()
            };
            process_cas_bits(rhs.int(6)?, rhs.loc(6), "UNIQUE", targets, rhs.scanner())?;
            Ok(SemValue::node(constraint))
        }
        // UNIQUE ExistingIndex ConstraintAttributeSpec
        2 => {
            let mut constraint = Constraint {
                contype: ConstrType::Unique,
                location: rhs.loc(1),
                indexname: rhs.str(2)?,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                ..Default::default()
            };
            process_cas_bits(rhs.int(3)?, rhs.loc(3), "UNIQUE", targets, rhs.scanner())?;
            Ok(SemValue::node(constraint))
        }
        // PRIMARY KEY '(' columnList_opt_comma ')' opt_definition ConstraintAttributeSpec
        3 => {
            let mut constraint = Constraint {
                contype: ConstrType::Primary,
                location: rhs.loc(1),
                keys: rhs.list(4)?,
                options: rhs.list(6)?,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                ..Default::default()
            };
            process_cas_bits(rhs.int(7)?, rhs.loc(7), "PRIMARY KEY", targets, rhs.scanner())?;
            Ok(SemValue::node(constraint))
        }
        // PRIMARY KEY ExistingIndex ConstraintAttributeSpec
        4 => {
            let mut constraint = Constraint {
                contype: ConstrType::Primary,
                location: rhs.loc(1),
                indexname: rhs.str(3)?,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                ..Default::default()
            };
            process_cas_bits(rhs.int(4)?, rhs.loc(4), "PRIMARY KEY", targets, rhs.scanner())?;
            Ok(SemValue::node(constraint))
        }
        // FOREIGN KEY '(' columnList_opt_comma ')' REFERENCES qualified_name opt_column_list key_match key_actions ConstraintAttributeSpec
        5 => {
            let key_actions = rhs.int(10)?;
            let mut constraint = Constraint {
                contype: ConstrType::Foreign,
                location: rhs.loc(1),
                pktable: rhs.ptr::<RangeVar>(7)?,
                fk_attrs: rhs.list(4)?,
                pk_attrs: rhs.list(8)?,
                fk_matchtype: rhs.int(9)? as u8,
                // key_actions packs the ON UPDATE action in the second byte
                fk_upd_action: (key_actions >> 8) as u8,
                fk_del_action: (key_actions & 0xFF) as u8,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                not_valid: Some(&mut constraint.skip_validation),
                ..Default::default()
            };
            process_cas_bits(rhs.int(11)?, rhs.loc(11), "FOREIGN KEY", targets, rhs.scanner())?;
            constraint.initially_valid = !constraint.skip_validation;
            Ok(SemValue::node(constraint))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableElementList:
///       TableElement
///     | TableElementList ',' TableElement
/// ```
pub(super) fn table_element_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TableElement
        0 => Ok(SemValue::List(vec![required(rhs.node(1)?)?])),
        // TableElementList ',' TableElement
        1 => Ok(SemValue::List(append(rhs.list(1)?, required(rhs.node(3)?)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// key_match:
///       MATCH FULL
///     | MATCH PARTIAL
///     | MATCH SIMPLE
///     | /* empty */
/// ```
pub(super) fn key_match<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // MATCH FULL
        0 => Ok(SemValue::Int(i32::from(foreign_key::MATCH_FULL))),
        // MATCH PARTIAL
        1 => {
            return Err(rhs.error_at("MATCH PARTIAL not yet implemented", rhs.loc(1)));
        }
        // MATCH SIMPLE
        2 => Ok(SemValue::Int(i32::from(foreign_key::MATCH_SIMPLE))),
        // /* empty */
        3 => Ok(SemValue::Int(i32::from(foreign_key::MATCH_SIMPLE))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableLikeClause:
///       LIKE qualified_name TableLikeOptionList
/// ```
pub(super) fn table_like_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // LIKE qualified_name TableLikeOptionList
        0 => {
            let n = TableLikeClause {
                relation: rhs.ptr::<RangeVar>(2)?,
                options: rhs.int(3)? as u32,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptTemp:
///       TEMPORARY
///     | TEMP
///     | LOCAL TEMPORARY
///     | LOCAL TEMP
///     | GLOBAL TEMPORARY
///     | GLOBAL TEMP
///     | UNLOGGED
///     | /* empty */
/// ```
pub(super) fn opt_temp<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TEMPORARY
        0 => Ok(SemValue::RelPersistence(RelPersistence::Temp)),
        // TEMP
        1 => Ok(SemValue::RelPersistence(RelPersistence::Temp)),
        // LOCAL TEMPORARY
        2 => Ok(SemValue::RelPersistence(RelPersistence::Temp)),
        // LOCAL TEMP
        3 => Ok(SemValue::RelPersistence(RelPersistence::Temp)),
        // GLOBAL TEMPORARY
        4 => {
            return Err(rhs.error_at("GLOBAL is deprecated in temporary table creation", rhs.loc(1)));
        }
        // GLOBAL TEMP
        5 => {
            return Err(rhs.error_at("GLOBAL is deprecated in temporary table creation", rhs.loc(1)));
        }
        // UNLOGGED
        6 => Ok(SemValue::RelPersistence(RelPersistence::Unlogged)),
        // /* empty */
        7 => Ok(SemValue::RelPersistence(RelPersistence::Permanent)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// generated_when:
///       ALWAYS
///     | BY DEFAULT
/// ```
pub(super) fn generated_when<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALWAYS
        0 => Ok(SemValue::Int(i32::from(identity::ALWAYS))),
        // BY DEFAULT
        1 => Ok(SemValue::Int(i32::from(identity::BY_DEFAULT))),
        _ => Err(rhs.bad_alternative()),
    }
}
