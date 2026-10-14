//! Semantic actions for the rules in `grammar/statements/alter_table.y`.

use super::prelude::*;

/// ```text
/// AlterTableStmt:
///       ALTER TABLE relation_expr alter_table_cmds
///     | ALTER TABLE IF_P EXISTS relation_expr alter_table_cmds
///     | ALTER INDEX qualified_name alter_table_cmds
///     | ALTER INDEX IF_P EXISTS qualified_name alter_table_cmds
///     | ALTER SEQUENCE qualified_name alter_table_cmds
///     | ALTER SEQUENCE IF_P EXISTS qualified_name alter_table_cmds
///     | ALTER VIEW qualified_name alter_table_cmds
///     | ALTER VIEW IF_P EXISTS qualified_name alter_table_cmds
/// ```
pub(super) fn alter_table_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALTER TABLE relation_expr alter_table_cmds
        0 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                cmds: rhs.list(4)?,
                relkind: ObjectType::Table,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER TABLE IF_P EXISTS relation_expr alter_table_cmds
        1 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(5)?,
                cmds: rhs.list(6)?,
                relkind: ObjectType::Table,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        // ALTER INDEX qualified_name alter_table_cmds
        2 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                cmds: rhs.list(4)?,
                relkind: ObjectType::Index,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER INDEX IF_P EXISTS qualified_name alter_table_cmds
        3 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(5)?,
                cmds: rhs.list(6)?,
                relkind: ObjectType::Index,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        // ALTER SEQUENCE qualified_name alter_table_cmds
        4 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                cmds: rhs.list(4)?,
                relkind: ObjectType::Sequence,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER SEQUENCE IF_P EXISTS qualified_name alter_table_cmds
        5 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(5)?,
                cmds: rhs.list(6)?,
                relkind: ObjectType::Sequence,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        // ALTER VIEW qualified_name alter_table_cmds
        6 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                cmds: rhs.list(4)?,
                relkind: ObjectType::View,
                missing_ok: false,
            };
            Ok(SemValue::node(n))
        }
        // ALTER VIEW IF_P EXISTS qualified_name alter_table_cmds
        7 => {
            let n = AlterTableStmt {
                relation: rhs.ptr::<RangeVar>(5)?,
                cmds: rhs.list(6)?,
                relkind: ObjectType::View,
                missing_ok: true,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_identity_column_option_list:
///       alter_identity_column_option
///     | alter_identity_column_option_list alter_identity_column_option
/// ```
pub(super) fn alter_identity_column_option_list<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // alter_identity_column_option
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // alter_identity_column_option_list alter_identity_column_option
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_column_default:
///       SET DEFAULT a_expr
///     | DROP DEFAULT
/// ```
pub(super) fn alter_column_default<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DROP DEFAULT
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_identity_column_option:
///       RESTART
///     | RESTART opt_with NumericOnly
///     | SET SeqOptElem
///     | SET GENERATED generated_when
/// ```
pub(super) fn alter_identity_column_option<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // RESTART
        0 => Ok(SemValue::node(make_def_elem("restart", None, rhs.loc(1)))),
        // RESTART opt_with NumericOnly
        1 => Ok(SemValue::node(make_def_elem(
            "restart",
            rhs.value(3)?.map(Node::Value),
            rhs.loc(1),
        ))),
        // SET SeqOptElem
        2 => {
            let seq_opt_elem = rhs.take::<DefElem>(2)?;
            if seq_opt_elem.defname.as_deref() == Some("as")
                || seq_opt_elem.defname.as_deref() == Some("restart")
                || seq_opt_elem.defname.as_deref() == Some("owned_by")
            {
                return Err(rhs.error_at(
                    format!(
                        "sequence option \"{}\" not supported here",
                        c_str(seq_opt_elem.defname.as_deref())
                    ),
                    rhs.loc(2),
                ));
            }
            Ok(SemValue::node(seq_opt_elem))
        }
        // SET GENERATED generated_when
        3 => Ok(SemValue::node(make_def_elem(
            "generated",
            Some(Node::Value(Value::Integer(i64::from(rhs.int(3)?)))),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_generic_option_list:
///       alter_generic_option_elem
///     | alter_generic_option_list ',' alter_generic_option_elem
/// ```
pub(super) fn alter_generic_option_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // alter_generic_option_elem
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // alter_generic_option_list ',' alter_generic_option_elem
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualify_list:
///       qualify_el
///     | qualify_list qualify_el
/// ```
pub(super) fn qualify_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualify_el
        0 => Ok(SemValue::List(vec![Node::Str(rhs.string(1)?)])),
        // qualify_list qualify_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::Str(rhs.string(2)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualified_column:
///       ColId
///     | ColId qualify_list
/// ```
pub(super) fn qualified_column<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::List(vec![Node::Str(rhs.string(1)?)])),
        // ColId qualify_list
        1 => Ok(SemValue::List(concat(
            vec![Node::Str(rhs.string(1)?)],
            rhs.list(2)?,
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualifiedColumnDef:
///       qualified_column regularColumnDef
///     | qualified_column generatedColumnDef
/// ```
pub(super) fn qualified_column_def<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualified_column regularColumnDef
        // qualified_column generatedColumnDef
        0 | 1 => {
            let mut n = rhs.take::<ColumnDef>(2)?;
            n.location = rhs.loc(1);
            Ok(SemValue::List(vec![Node::List(rhs.list(1)?), n.into()]))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_table_cmd:
///       ADD_P qualifiedColumnDef
///     | ADD_P IF_P NOT EXISTS qualifiedColumnDef
///     | ADD_P COLUMN qualifiedColumnDef
///     | ADD_P COLUMN IF_P NOT EXISTS qualifiedColumnDef
///     | SET PARTITIONED BY '(' expr_list_opt_comma ')'
///     | RESET PARTITIONED BY
///     | SET SORTED BY '(' sortby_list ')'
///     | RESET SORTED BY
///     | ALTER opt_column ColId alter_column_default
///     | ALTER opt_column ColId DROP NOT NULL_P
///     | ALTER opt_column ColId SET NOT NULL_P
///     | ALTER opt_column ColId SET STATISTICS SignedIconst
///     | ALTER opt_column ColId SET reloptions
///     | ALTER opt_column ColId RESET reloptions
///     | ALTER opt_column ColId SET STORAGE ColId
///     | ALTER opt_column ColId ADD_P GENERATED generated_when AS IDENTITY_P OptParenthesizedSeqOptList
///     | ALTER opt_column ColId alter_identity_column_option_list
///     | ALTER opt_column ColId DROP IDENTITY_P
///     | ALTER opt_column ColId DROP IDENTITY_P IF_P EXISTS
///     | DROP opt_column IF_P EXISTS qualified_column opt_drop_behavior
///     | DROP opt_column qualified_column opt_drop_behavior
///     | ALTER opt_column ColId opt_set_data TYPE_P opt_Typename opt_collate_clause alter_using
///     | ALTER opt_column ColId alter_generic_options
///     | ADD_P TableConstraint
///     | ALTER CONSTRAINT name ConstraintAttributeSpec
///     | VALIDATE CONSTRAINT name
///     | DROP CONSTRAINT IF_P EXISTS name opt_drop_behavior
///     | DROP CONSTRAINT name opt_drop_behavior
///     | SET LOGGED
///     | SET UNLOGGED
///     | SET reloptions
///     | RESET reloptions
///     | alter_generic_options
/// ```
pub(super) fn alter_table_cmd<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ADD_P qualifiedColumnDef
        0 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AddColumn,
                def_list: rhs.list(2)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ADD_P IF_P NOT EXISTS qualifiedColumnDef
        1 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AddColumn,
                def_list: rhs.list(5)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ADD_P COLUMN qualifiedColumnDef
        2 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AddColumn,
                def_list: rhs.list(3)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ADD_P COLUMN IF_P NOT EXISTS qualifiedColumnDef
        3 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AddColumn,
                def_list: rhs.list(6)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SET PARTITIONED BY '(' expr_list_opt_comma ')'
        4 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetPartitionedBy,
                def_list: rhs.list(5)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // RESET PARTITIONED BY
        5 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetPartitionedBy,
                def_list: List::new(),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SET SORTED BY '(' sortby_list ')'
        6 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetSortedBy,
                def_list: rhs.list(5)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // RESET SORTED BY
        7 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetSortedBy,
                def_list: List::new(),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId alter_column_default
        8 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::ColumnDefault,
                name: rhs.str(3)?,
                def: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId DROP NOT NULL_P
        9 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropNotNull,
                name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId SET NOT NULL_P
        10 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetNotNull,
                name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId SET STATISTICS SignedIconst
        11 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetStatistics,
                name: rhs.str(3)?,
                def: Some(Node::Value(Value::Integer(i64::from(rhs.int(6)?)))),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId SET reloptions
        12 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetOptions,
                name: rhs.str(3)?,
                def: Node::from_list(rhs.list(5)?),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId RESET reloptions
        13 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::ResetOptions,
                name: rhs.str(3)?,
                def: Node::from_list(rhs.list(5)?),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId SET STORAGE ColId
        14 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetStorage,
                name: rhs.str(3)?,
                def: Some(Node::string(rhs.string(6)?)),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId ADD_P GENERATED generated_when AS IDENTITY_P OptParenthesizedSeqOptList
        15 => {
            let mut n = AlterTableCmd { ..Default::default() };
            let c = Constraint {
                contype: ConstrType::Identity,
                generated_when: rhs.int(6)? as u8,
                options: rhs.list(9)?,
                location: rhs.loc(5),
                ..Default::default()
            };
            n.subtype = AlterTableType::AddIdentity;
            n.name = rhs.str(3)?;
            n.def = Some(c.into());
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId alter_identity_column_option_list
        16 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetIdentity,
                name: rhs.str(3)?,
                def: Node::from_list(rhs.list(4)?),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId DROP IDENTITY_P
        17 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropIdentity,
                name: rhs.str(3)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId DROP IDENTITY_P IF_P EXISTS
        18 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropIdentity,
                name: rhs.str(3)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DROP opt_column IF_P EXISTS qualified_column opt_drop_behavior
        19 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropColumn,
                def_list: rhs.list(5)?,
                behavior: rhs.drop_behavior(6)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DROP opt_column qualified_column opt_drop_behavior
        20 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropColumn,
                def_list: rhs.list(3)?,
                behavior: rhs.drop_behavior(4)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER opt_column ColId opt_set_data TYPE_P opt_Typename opt_collate_clause alter_using
        21 => {
            // only these fields of the ColumnDef are used
            let def = ColumnDef {
                type_name: rhs.ptr::<TypeName>(6)?,
                coll_clause: rhs.ptr::<CollateClause>(7)?,
                raw_default: rhs.node(8)?,
                location: rhs.loc(3),
                ..Default::default()
            };
            Ok(SemValue::node(AlterTableCmd {
                subtype: AlterTableType::AlterColumnType,
                name: rhs.str(3)?,
                def: Some(def.into()),
                ..Default::default()
            }))
        }
        // ALTER opt_column ColId alter_generic_options
        22 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AlterColumnGenericOptions,
                name: rhs.str(3)?,
                def: Node::from_list(rhs.list(4)?),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ADD_P TableConstraint
        23 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::AddConstraint,
                def: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // ALTER CONSTRAINT name ConstraintAttributeSpec
        24 => {
            let mut constraint = Constraint {
                contype: ConstrType::Foreign, // others not supported, yet
                conname: rhs.str(3)?,
                ..Default::default()
            };
            let targets = CasTargets {
                deferrable: Some(&mut constraint.deferrable),
                initdeferred: Some(&mut constraint.initdeferred),
                ..Default::default()
            };
            process_cas_bits(
                rhs.int(4)?,
                rhs.loc(4),
                "ALTER CONSTRAINT statement",
                targets,
                rhs.scanner(),
            )?;
            Ok(SemValue::node(AlterTableCmd {
                subtype: AlterTableType::AlterConstraint,
                def: Some(constraint.into()),
                ..Default::default()
            }))
        }
        // VALIDATE CONSTRAINT name
        25 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::ValidateConstraint,
                name: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DROP CONSTRAINT IF_P EXISTS name opt_drop_behavior
        26 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropConstraint,
                name: rhs.str(5)?,
                behavior: rhs.drop_behavior(6)?,
                missing_ok: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // DROP CONSTRAINT name opt_drop_behavior
        27 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::DropConstraint,
                name: rhs.str(3)?,
                behavior: rhs.drop_behavior(4)?,
                missing_ok: false,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SET LOGGED
        28 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetLogged,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SET UNLOGGED
        29 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetUnLogged,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SET reloptions
        30 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::SetRelOptions,
                options: rhs.list(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // RESET reloptions
        31 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::ResetRelOptions,
                options: rhs.list(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // alter_generic_options
        32 => {
            let n = AlterTableCmd {
                subtype: AlterTableType::GenericOptions,
                def: Node::from_list(rhs.list(1)?),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_using:
///       USING a_expr
///     | /* empty */
/// ```
pub(super) fn alter_using<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_generic_option_elem:
///       generic_option_elem
///     | SET generic_option_elem
///     | ADD_P generic_option_elem
///     | DROP generic_option_name
/// ```
pub(super) fn alter_generic_option_elem<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SET generic_option_elem
        1 => {
            let mut alter_generic_option_elem = rhs.ptr::<DefElem>(2)?;
            required_mut(&mut alter_generic_option_elem)?.defaction = DefElemAction::Set;
            Ok(SemValue::Node(alter_generic_option_elem.map(Node::from)))
        }
        // ADD_P generic_option_elem
        2 => {
            let mut alter_generic_option_elem = rhs.ptr::<DefElem>(2)?;
            required_mut(&mut alter_generic_option_elem)?.defaction = DefElemAction::Add;
            Ok(SemValue::Node(alter_generic_option_elem.map(Node::from)))
        }
        // DROP generic_option_name
        3 => Ok(SemValue::node(make_def_elem_extended(
            None,
            rhs.string(2)?,
            None,
            DefElemAction::Drop,
            rhs.loc(2),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alter_table_cmds:
///       alter_table_cmd
///     | alter_table_cmds ',' alter_table_cmd
/// ```
pub(super) fn alter_table_cmds<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // alter_table_cmd
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // alter_table_cmds ',' alter_table_cmd
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_set_data:
///       SET DATA_P
///     | SET
///     | /* empty */
/// ```
pub(super) fn opt_set_data<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SET DATA_P
        0 => Ok(SemValue::Int(1)),
        // SET
        // /* empty */
        1 | 2 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}
