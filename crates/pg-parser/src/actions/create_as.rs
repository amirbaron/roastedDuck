//! Semantic actions for the rules in `grammar/statements/create_as.y`.

use super::prelude::*;

/// ```text
/// CreateAsStmt:
///       CREATE_P OptTemp TABLE create_as_target AS SelectStmt opt_with_data
///     | CREATE_P OptTemp TABLE IF_P NOT EXISTS create_as_target AS SelectStmt opt_with_data
///     | CREATE_P OR REPLACE OptTemp TABLE create_as_target AS SelectStmt opt_with_data
/// ```
pub(super) fn create_as_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp TABLE create_as_target AS SelectStmt opt_with_data
        0 => {
            let mut into = rhs.take::<IntoClause>(4)?;
            // cram additional flags into the IntoClause
            required_mut(&mut into.rel)?.relpersistence = rhs.rel_persistence(2)?;
            into.skip_data = !rhs.bool(7)?;
            Ok(SemValue::node(CreateTableAsStmt {
                query: rhs.node(6)?,
                into: Some(into),
                relkind: ObjectType::Table,
                is_select_into: false,
                onconflict: OnCreateConflict::Error,
            }))
        }
        // CREATE_P OptTemp TABLE IF_P NOT EXISTS create_as_target AS SelectStmt opt_with_data
        1 => {
            let mut into = rhs.take::<IntoClause>(7)?;
            required_mut(&mut into.rel)?.relpersistence = rhs.rel_persistence(2)?;
            into.skip_data = !rhs.bool(10)?;
            Ok(SemValue::node(CreateTableAsStmt {
                query: rhs.node(9)?,
                into: Some(into),
                relkind: ObjectType::Table,
                is_select_into: false,
                onconflict: OnCreateConflict::Ignore,
            }))
        }
        // CREATE_P OR REPLACE OptTemp TABLE create_as_target AS SelectStmt opt_with_data
        2 => {
            let mut into = rhs.take::<IntoClause>(6)?;
            required_mut(&mut into.rel)?.relpersistence = rhs.rel_persistence(4)?;
            into.skip_data = !rhs.bool(9)?;
            Ok(SemValue::node(CreateTableAsStmt {
                query: rhs.node(8)?,
                into: Some(into),
                relkind: ObjectType::Table,
                is_select_into: false,
                onconflict: OnCreateConflict::Replace,
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_with_data:
///       WITH DATA_P
///     | WITH NO DATA_P
///     | /* empty */
/// ```
pub(super) fn opt_with_data<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH DATA_P
        0 => Ok(SemValue::Bool(true)),
        // WITH NO DATA_P
        1 => Ok(SemValue::Bool(false)),
        // /* empty */
        2 => Ok(SemValue::Bool(true)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// create_as_target:
///       qualified_name opt_column_list OptPartitionSortedOptions OptWith OnCommitOption
/// ```
pub(super) fn create_as_target<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualified_name opt_column_list OptPartitionSortedOptions OptWith OnCommitOption
        0 => {
            let (partition_list, sort_list) = partition_and_sort_lists(rhs.list(3)?)?;
            Ok(SemValue::node(IntoClause {
                rel: rhs.ptr::<RangeVar>(1)?,
                col_names: rhs.list(2)?,
                partition_list,
                sort_list,
                options: rhs.list(4)?,
                on_commit: rhs.on_commit_action(5)?,
                skip_data: false, // might get changed later
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}
