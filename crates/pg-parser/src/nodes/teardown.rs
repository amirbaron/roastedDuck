//! Dropping parse trees without deep recursion.
//!
//! Left-recursive grammar rules build trees whose depth grows with the input
//! (`1+1+...+1` nests one `AExpr` per operator) without growing the parser stack,
//! so the compiler-generated recursive `Drop` could overflow the thread's stack.
//! [`RawStmt`] therefore tears its tree down with an explicit work list, and the
//! parser does the same for values it discards on error.

use super::{List, Node, NodeStruct, RawStmt, Value};

/// Moves the child nodes out of a node or field.
pub(crate) trait TakeChildren {
    /// Passes every child node owned by `self` to `out`, leaving `self` shallow.
    fn take_children(&mut self, out: &mut dyn FnMut(Node));
}

impl TakeChildren for Option<Node> {
    fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
        if let Some(node) = self.take() {
            out(node);
        }
    }
}

impl TakeChildren for List {
    fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
        self.drain(..).for_each(out);
    }
}

impl TakeChildren for Option<List> {
    fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
        if let Some(mut list) = self.take() {
            list.take_children(out);
        }
    }
}

impl<T: NodeStruct> TakeChildren for Option<Box<T>> {
    fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
        if let Some(node) = self.take() {
            out(T::into_node(node));
        }
    }
}

macro_rules! no_children {
    ($($ty:ty),* $(,)?) => {$(
        impl TakeChildren for $ty {
            fn take_children(&mut self, _: &mut dyn FnMut(Node)) {}
        }
    )*};
}
no_children!(bool, i32, i64, u8, u32, Option<String>, Value);
no_children!(
    super::AExprKind,
    super::AlterDatabaseType,
    super::AlterTableType,
    super::BoolExprType,
    super::BoolTestType,
    super::CteMaterialize,
    super::ColumnCategory,
    super::ConstrType,
    super::DefElemAction,
    super::DropBehavior,
    super::GroupingSetKind,
    super::IgnoreNulls,
    super::InsertColumnOrder,
    super::JoinRefType,
    super::JoinType,
    super::LoadInstallType,
    super::LockClauseStrength,
    super::LockWaitPolicy,
    super::MergeAction,
    super::MergeActionType,
    super::NewTypeKind,
    super::NullTestType,
    super::ObjectType,
    super::OnCommitAction,
    super::OnConflictAction,
    super::OnConflictActionAlias,
    super::OnCreateConflict,
    super::OverridingKind,
    super::PragmaKind,
    super::SetOperation,
    super::SortByDir,
    super::SortByNulls,
    super::SubLinkType,
    super::TransactionStmtKind,
    super::TransactionStmtType,
    super::ViewCheckOption,
    super::VariableSetKind,
    super::VariableSetScope,
    super::RelPersistence,
);

/// Drops a tree of any depth using heap memory instead of the call stack.
///
/// Dropping a [`Node`] normally recurses once per level of nesting; use this for
/// trees taken out of a [`RawStmt`] that may be arbitrarily deep.
pub fn drop_tree(node: Node) {
    let mut deferred = Vec::new();
    drop_bounded(node, 0, &mut deferred);
    while let Some(node) = deferred.pop() {
        drop_bounded(node, 0, &mut deferred);
    }
}

/// Nesting depth up to which subtrees are dropped by plain recursion.
const RECURSION_LIMIT: usize = 64;

/// Drops `node` recursively up to [`RECURSION_LIMIT`] levels; deeper subtrees are
/// moved to `deferred` and dropped by the caller's loop.
fn drop_bounded(mut node: Node, depth: usize, deferred: &mut Vec<Node>) {
    if depth == RECURSION_LIMIT {
        deferred.push(node);
        return;
    }
    node.take_children(&mut |child| drop_bounded(child, depth + 1, deferred));
}

impl Drop for RawStmt {
    fn drop(&mut self) {
        if let Some(stmt) = self.stmt.take() {
            drop_tree(stmt);
        }
    }
}
