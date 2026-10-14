//! Semantic actions for the rules in `grammar/statements/create_sequence.y`.

use super::prelude::*;

/// ```text
/// CreateSeqStmt:
///       CREATE_P OptTemp SEQUENCE qualified_name OptSeqOptList
///     | CREATE_P OptTemp SEQUENCE IF_P NOT EXISTS qualified_name OptSeqOptList
///     | CREATE_P OR REPLACE OptTemp SEQUENCE qualified_name OptSeqOptList
/// ```
pub(super) fn create_seq_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp SEQUENCE qualified_name OptSeqOptList
        0 => {
            let mut qualified_name = rhs.take::<RangeVar>(4)?;
            let mut n = CreateSeqStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.sequence = Some(qualified_name);
            n.options = rhs.list(5)?;
            n.onconflict = OnCreateConflict::Error;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp SEQUENCE IF_P NOT EXISTS qualified_name OptSeqOptList
        1 => {
            let mut qualified_name = rhs.take::<RangeVar>(7)?;
            let mut n = CreateSeqStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(2)?;
            n.sequence = Some(qualified_name);
            n.options = rhs.list(8)?;
            n.onconflict = OnCreateConflict::Ignore;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OR REPLACE OptTemp SEQUENCE qualified_name OptSeqOptList
        2 => {
            let mut qualified_name = rhs.take::<RangeVar>(6)?;
            let mut n = CreateSeqStmt { ..Default::default() };
            qualified_name.relpersistence = rhs.rel_persistence(4)?;
            n.sequence = Some(qualified_name);
            n.options = rhs.list(7)?;
            n.onconflict = OnCreateConflict::Replace;
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptSeqOptList:
///       SeqOptList
///     | /* empty */
/// ```
pub(super) fn opt_seq_opt_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SeqOptList
        0 => Ok(SemValue::List(rhs.list(1)?)),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
