//! Semantic values (bison's `%union`) and the context a rule's action runs in.

use rustdb_common::ByteString;
use rustdb_common::{Error, Result};

use crate::error::internal;
use crate::nodes::{
    Alias, ConstrType, CteMaterialize, DropBehavior, IgnoreNulls, InsertColumnOrder, JoinType, List,
    LoadInstallType, LockClauseStrength, LockWaitPolicy, MergeAction, Node, NodeStruct, ObjectType,
    OnCommitAction, OnConflictActionAlias, OverridingKind, RelPersistence, SortByDir, SortByNulls,
    SubLinkType, TransactionStmtType, Value, ViewCheckOption, drop_tree,
};
use crate::scanner::Scanner;

/// `LIMIT`/`OFFSET` as collected by `select_limit` (C: a three-element list).
#[derive(Debug, Default)]
pub(crate) struct SelectLimit {
    pub(crate) offset: Option<Node>,
    pub(crate) count: Option<Node>,
    /// Whether `OFFSET` was written before `LIMIT` (or there is no `OFFSET`), which
    /// the C code detects by comparing the offset with the list's third element.
    pub(crate) offset_first: bool,
}

impl SelectLimit {
    /// No `LIMIT` and no `OFFSET`.
    pub(crate) fn none() -> Self {
        Self {
            offset: None,
            count: None,
            offset_first: true,
        }
    }
}

/// An alias with an optional column definition list (C: `list_make2(alias, coldeflist)`).
#[derive(Debug, Default)]
pub(crate) struct FuncAlias {
    pub(crate) alias: Option<Box<Alias>>,
    pub(crate) coldeflist: List,
}

macro_rules! semantic_value {
    (
        $( $(#[$doc:meta])* $variant:ident($ty:ty) => $getter:ident ),* $(,)?
    ) => {
        /// A grammar symbol's semantic value: the Rust form of bison's `%union`.
        ///
        /// Every typed pointer member of the C union (`range`, `typnam`, `with`, ...)
        /// is a [`Node`] here; the typed accessors of [`Reduction`] downcast it.
        #[derive(Debug, Default)]
        pub(crate) enum SemValue<'a> {
            /// No value: tokens without one, and empty rules that set none.
            #[default]
            None,
            /// A keyword's text as written in the query.
            Keyword(&'a [u8]),
            $( $(#[$doc])* $variant($ty), )*
        }

        impl<'a> Reduction<'_, 'a> {
            $(
                #[doc = concat!("Takes `$k` as `", stringify!($ty), "`.")]
                pub(crate) fn $getter(&mut self, k: usize) -> Result<$ty> {
                    match self.take_value(k) {
                        SemValue::$variant(value) => Ok(value),
                        SemValue::None => Ok(<$ty>::default()),
                        other => Err(self.type_error(k, stringify!($variant), &other)),
                    }
                }
            )*
        }
    };
}

semantic_value! {
    /// `node` and every typed node pointer (`range`, `typnam`, `alias`, ...).
    Node(Option<Node>) => node,
    /// `list`.
    List(List) => list,
    /// `str`: an owned (possibly absent) string.
    Str(Option<ByteString>) => str,
    /// `conststr`.
    ConstStr(Option<&'static str>) => const_str,
    /// `ival`.
    Int(i32) => int,
    /// `boolean`.
    Bool(bool) => bool,
    /// A list that distinguishes "absent" from "empty" (C: a list holding `NULL`).
    OptList(Option<List>) => opt_list,
    /// `select_limit`.
    SelectLimit(Box<SelectLimit>) => select_limit,
    /// `func_alias_clause`.
    FuncAlias(Box<FuncAlias>) => func_alias,
    /// `OptTemp`.
    RelPersistence(RelPersistence) => rel_persistence,
    /// `jtype`.
    JoinType(JoinType) => join_type,
    /// `dbehavior`.
    DropBehavior(DropBehavior) => drop_behavior,
    /// `oncommit`.
    OnCommitAction(OnCommitAction) => on_commit_action,
    /// `objtype`.
    ObjectType(ObjectType) => object_type,
    /// `ctematerialize`.
    CteMaterialize(CteMaterialize) => cte_materialize,
    /// `onconflictshorthand`.
    OnConflictActionAlias(OnConflictActionAlias) => on_conflict_action_alias,
    /// `override`.
    OverridingKind(OverridingKind) => overriding_kind,
    /// `sortorder`.
    SortByDir(SortByDir) => sort_by_dir,
    /// `nullorder`.
    SortByNulls(SortByNulls) => sort_by_nulls,
    /// `ignorenulls`.
    IgnoreNulls(IgnoreNulls) => ignore_nulls,
    /// `constr`.
    ConstrType(ConstrType) => constr_type,
    /// `lockstrength`.
    LockClauseStrength(LockClauseStrength) => lock_clause_strength,
    /// `lockwaitpolicy`.
    LockWaitPolicy(LockWaitPolicy) => lock_wait_policy,
    /// `subquerytype`.
    SubLinkType(SubLinkType) => sub_link_type,
    /// `viewcheckoption`.
    ViewCheckOption(ViewCheckOption) => view_check_option,
    /// `bynameorposition`.
    InsertColumnOrder(InsertColumnOrder) => insert_column_order,
    /// `loadinstalltype`.
    LoadInstallType(LoadInstallType) => load_install_type,
    /// `transactiontype`.
    TransactionStmtType(TransactionStmtType) => transaction_stmt_type,
    /// `mergeaction`.
    MergeAction(MergeAction) => merge_action,
}

impl SemValue<'_> {
    /// A non-null node value.
    pub(crate) fn node(node: impl Into<Node>) -> Self {
        SemValue::Node(Some(node.into()))
    }

    /// Drops the value, tearing any node trees down without recursion.
    pub(crate) fn drop_tree(self) {
        match self {
            SemValue::Node(Some(node)) => drop_tree(node),
            SemValue::List(list) | SemValue::OptList(Some(list)) => list.into_iter().for_each(drop_tree),
            SemValue::SelectLimit(limit) => limit.offset.into_iter().chain(limit.count).for_each(drop_tree),
            SemValue::FuncAlias(alias) => alias.coldeflist.into_iter().for_each(drop_tree),
            _ => {}
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            SemValue::None => "None",
            SemValue::Keyword(_) => "Keyword",
            SemValue::Node(_) => "Node",
            SemValue::List(_) => "List",
            SemValue::Str(_) => "Str",
            SemValue::Int(_) => "Int",
            SemValue::Bool(_) => "Bool",
            _ => "other",
        }
    }
}

/// The signature shared by all semantic actions: the nonterminal's function plus
/// the index of the alternative being reduced.
pub(crate) type Action = for<'s, 'a> fn(&mut Reduction<'s, 'a>, usize) -> Result<SemValue<'a>>;

/// What to do when reducing a rule.
#[derive(Clone, Copy)]
pub(crate) enum RuleAction {
    /// Bison's default action `$$ = $1` (no value for an empty rule). Rules whose
    /// action is empty also end up here: bison sets `$$ = $1` before any action.
    Default,
    /// An action that just passes `$k` up: `$$ = $k`.
    Forward(usize),
    /// Run the nonterminal's action function for the given alternative.
    Call(Action, usize),
}

/// One entry of the parser stack: a state, plus the value and location of the
/// symbol whose shift or reduction entered it (bison keeps three parallel stacks).
pub(crate) struct StackEntry<'a> {
    pub(crate) state: usize,
    pub(crate) location: i32,
    pub(crate) value: SemValue<'a>,
}

/// The right-hand side of the rule being reduced: `$1..$n`, `@1..@n` and `@$`.
pub(crate) struct Reduction<'s, 'a> {
    rhs: &'s mut [StackEntry<'a>],
    /// `@$`, initialised to `@1` (or -1 for an empty rule) like `YYLLOC_DEFAULT`.
    pub(crate) location: i32,
    scanner: &'s Scanner<'a>,
    rule: usize,
}

impl<'s, 'a> Reduction<'s, 'a> {
    pub(crate) fn new(rhs: &'s mut [StackEntry<'a>], scanner: &'s Scanner<'a>, rule: usize) -> Self {
        let location = rhs.first().map_or(-1, |entry| entry.location);
        Self {
            rhs,
            location,
            scanner,
            rule,
        }
    }

    /// `@k`: the location of the k-th right-hand-side symbol.
    pub(crate) fn loc(&self, k: usize) -> i32 {
        self.rhs.get(k - 1).map_or(-1, |entry| entry.location)
    }

    /// The scanner, for helpers that report errors at the current token.
    pub(crate) fn scanner(&self) -> &Scanner<'a> {
        self.scanner
    }

    fn take_value(&mut self, k: usize) -> SemValue<'a> {
        self.rhs
            .get_mut(k - 1)
            .map(|entry| std::mem::take(&mut entry.value))
            .unwrap_or_default()
    }

    /// Takes `$k` as-is (bison's default action `$$ = $1`).
    pub(crate) fn take_raw(&mut self, k: usize) -> SemValue<'a> {
        self.take_value(k)
    }

    /// `$k` as a keyword's text.
    pub(crate) fn keyword(&mut self, k: usize) -> Result<&'a [u8]> {
        match self.take_value(k) {
            SemValue::Keyword(text) => Ok(text),
            other => Err(self.type_error(k, "Keyword", &other)),
        }
    }

    /// `$k` as a string that must be present.
    pub(crate) fn string(&mut self, k: usize) -> Result<rustdb_common::ByteString> {
        match self.take_value(k) {
            SemValue::Str(Some(s)) => Ok(s),
            SemValue::Keyword(text) => Ok(text.into()),
            other => Err(self.type_error(k, "Str", &other)),
        }
    }

    /// `$k` as a node that must be present.
    pub(crate) fn required_node(&mut self, k: usize) -> Result<Node> {
        match self.node(k)? {
            Some(node) => Ok(node),
            None => Err(internal(format!("rule {}: ${k} is NULL", self.rule))),
        }
    }

    /// `$k` as a `PGValue *`.
    pub(crate) fn value(&mut self, k: usize) -> Result<Option<Value>> {
        match self.node(k)? {
            None => Ok(None),
            Some(Node::Value(value)) => Ok(Some(value)),
            Some(other) => Err(internal(format!(
                "rule {}: ${k} is {} not a value",
                self.rule,
                other.name()
            ))),
        }
    }

    /// `$k` as a typed node pointer that may be absent.
    pub(crate) fn ptr<T: NodeStruct>(&mut self, k: usize) -> Result<Option<Box<T>>> {
        match self.node(k)? {
            None => Ok(None),
            Some(node) => T::from_node(node)
                .map(Some)
                .map_err(|node| self.cast_error::<T>(k, &node)),
        }
    }

    /// `$k` as a typed node pointer that must be present.
    pub(crate) fn take<T: NodeStruct>(&mut self, k: usize) -> Result<Box<T>> {
        let node = self.required_node(k)?;
        T::from_node(node).map_err(|node| self.cast_error::<T>(k, &node))
    }

    fn type_error(&self, k: usize, expected: &str, found: &SemValue<'_>) -> Error {
        internal(format!(
            "rule {}: ${k} is {} where {expected} was expected",
            self.rule,
            found.kind()
        ))
    }

    fn cast_error<T: NodeStruct>(&self, k: usize, found: &Node) -> Error {
        internal(format!(
            "rule {}: ${k} is {} where {} was expected",
            self.rule,
            found.name(),
            T::NAME
        ))
    }

    /// An error for an alternative index the action function does not know.
    pub(crate) fn bad_alternative(&self) -> Error {
        internal(format!("no semantic action for rule {}", self.rule))
    }

    /// `ereport(ERROR, errmsg(...), parser_errposition(location))`.
    pub(crate) fn error_at(&self, message: impl Into<ByteString>, location: i32) -> Error {
        self.scanner.error_at(message.into(), location)
    }
}
