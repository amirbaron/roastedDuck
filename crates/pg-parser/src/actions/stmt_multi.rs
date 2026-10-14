//! Semantic actions for the rules in `grammar/grammar.y`.

use super::prelude::*;

/// ```text
/// stmtblock:
///       stmtmulti
/// ```
pub(super) fn stmtblock<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // stmtmulti
        0 => Ok(SemValue::List(rhs.list(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// stmtmulti:
///       stmtmulti ';' stmt
///     | stmt
/// ```
pub(super) fn stmtmulti<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // stmtmulti ';' stmt
        0 => {
            let mut stmts = rhs.list(1)?;
            if let Some(Node::RawStmt(last)) = stmts.last_mut() {
                // the previous statement ends at this semicolon
                update_raw_stmt_end(last, rhs.loc(2));
            }
            if let Some(stmt) = rhs.node(3)? {
                stmts.push(make_raw_stmt(Some(stmt), rhs.loc(2) + 1).into());
            }
            Ok(SemValue::List(stmts))
        }
        // stmt
        1 => {
            let stmts = match rhs.node(1)? {
                Some(stmt) => vec![make_raw_stmt(Some(stmt), 0).into()],
                None => List::new(),
            };
            Ok(SemValue::List(stmts))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// stmt:
///       AlterDatabaseStmt
///     | AlterObjectSchemaStmt
///     | AlterSeqStmt
///     | AlterTableStmt
///     | AnalyzeStmt
///     | AttachStmt
///     | CallStmt
///     | CheckPointStmt
///     | CommentOnStmt
///     | CopyStmt
///     | CreateAsStmt
///     | CreateFunctionStmt
///     | CreateSchemaStmt
///     | CreateSecretStmt
///     | CreateSeqStmt
///     | CreateStmt
///     | CreateTypeStmt
///     | DeallocateStmt
///     | DeleteStmt
///     | DetachStmt
///     | DropSecretStmt
///     | DropStmt
///     | ExecuteStmt
///     | ExplainStmt
///     | ExportStmt
///     | ImportStmt
///     | IndexStmt
///     | InsertStmt
///     | LoadStmt
///     | MergeIntoStmt
///     | PragmaStmt
///     | PrepareStmt
///     | RenameStmt
///     | SelectStmt
///     | TransactionStmt
///     | UpdateExtensionsStmt
///     | UpdateStmt
///     | UseStmt
///     | VacuumStmt
///     | VariableResetStmt
///     | VariableSetStmt
///     | VariableShowStmt
///     | ViewStmt
///     | /* empty */
/// ```
pub(super) fn stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        43 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}
