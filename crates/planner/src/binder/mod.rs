//! The binder (DuckDB's `Binder`): resolves names against the catalog and the tables in
//! scope, types every expression, and plans each query node into logical operators.
//!
//! # Ownership
//!
//! DuckDB's binders point to their parent binder, and expression binders of enclosing
//! queries stay "active" while a subquery is bound, so that the subquery can resolve
//! correlated columns (and aggregates) in them. Here a [`Binder`] owns only its own
//! state; the enclosing levels are reached through a chain of [`Scope`]s passed down the
//! call stack as `&mut dyn Scope`: the binder of a FROM-clause subquery sees its parent
//! binder, the binder of a subquery expression sees the expression binder that is binding
//! it. Binding code runs on a [`BindScope`], a binder together with its enclosing scopes.

mod bind_context;
mod binding;
pub(crate) mod expression;
pub(crate) mod query_node;
pub(crate) mod statement;
pub(crate) mod tableref;

use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{ParsedExpression, SetOperationType};
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use crate::ColumnBinding;
use crate::context::ClientContext;
use crate::expression::Expression;
use crate::expression_binder::ExpressionBinder;
use crate::operator::LogicalOperator;
use crate::prepared::{BoundParameterMap, StatementProperties};

pub use bind_context::{BindContext, UsingColumnSet, UsingSetRef};
pub use binding::{Binding, BindingAlias, BindingKind, CteBindState, DUMMY_NAME};

/// A bound statement or query node: its plan and result columns (DuckDB's `BoundStatement`).
#[derive(Clone, Debug)]
pub struct BoundStatement {
    pub plan: LogicalOperator,
    pub types: Vec<LogicalType>,
    pub names: Vec<String>,
    pub extra_info: ExtraBoundInfo,
}

impl BoundStatement {
    pub fn new(plan: LogicalOperator, names: Vec<String>, types: Vec<LogicalType>) -> Self {
        Self {
            plan,
            types,
            names,
            extra_info: ExtraBoundInfo::default(),
        }
    }
}

/// Binding information some callers need besides the plan (DuckDB's `ExtraBoundInfo`).
#[derive(Clone, Debug, Default)]
pub struct ExtraBoundInfo {
    /// Set for set operations.
    pub setop_type: Option<SetOperationType>,
    /// The binders of a set operation's operands.
    pub child_binders: Vec<Binder>,
    /// A set operation's operands (their plans are part of the set operation's plan).
    pub bound_children: Vec<BoundChildInfo>,
    /// The select list before binding (used by set operations to bind ORDER BY).
    pub original_expressions: Vec<ParsedExpression>,
}

/// What a set operation keeps of a bound operand once its plan is taken.
#[derive(Clone, Debug, Default)]
pub struct BoundChildInfo {
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub extra_info: ExtraBoundInfo,
}

impl BoundChildInfo {
    pub fn from_statement(statement: &BoundStatement) -> Self {
        Self {
            names: statement.names.clone(),
            types: statement.types.clone(),
            extra_info: statement.extra_info.clone(),
        }
    }
}

/// A column of an enclosing query referenced by a subquery (DuckDB's `CorrelatedColumnInfo`).
#[derive(Clone, Debug)]
pub struct CorrelatedColumnInfo {
    pub binding: ColumnBinding,
    pub logical_type: LogicalType,
    pub name: String,
    pub depth: u64,
}

impl CorrelatedColumnInfo {
    /// The correlated column a bound column reference stands for.
    pub fn from_expression(expression: &Expression) -> Option<Self> {
        let column = expression.as_column_ref()?;
        Some(Self {
            binding: column.binding,
            logical_type: expression.return_type.clone(),
            name: expression.name(),
            depth: column.depth,
        })
    }
}

/// The correlated columns of a subquery (DuckDB's `CorrelatedColumns`): new columns are
/// added at the front, and `delim_index` counts how many there were when a delim join
/// was planned.
#[derive(Clone, Debug)]
pub struct CorrelatedColumns {
    columns: Vec<CorrelatedColumnInfo>,
    delim_index: u64,
}

impl Default for CorrelatedColumns {
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            delim_index: 1 << 63,
        }
    }
}

impl CorrelatedColumns {
    /// Adds a column at the front.
    pub fn add_column(&mut self, info: CorrelatedColumnInfo) {
        self.columns.insert(0, info);
        self.delim_index = self.delim_index.wrapping_add(1);
    }

    /// Adds a column at the back.
    pub fn add_column_to_back(&mut self, info: CorrelatedColumnInfo) {
        self.columns.push(info);
    }

    pub fn set_delim_index_to_zero(&mut self) {
        self.delim_index = 0;
    }

    pub const fn delim_index(&self) -> u64 {
        self.delim_index
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    pub fn clear(&mut self) {
        self.columns.clear();
    }

    pub fn iter(&self) -> std::slice::Iter<'_, CorrelatedColumnInfo> {
        self.columns.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, CorrelatedColumnInfo> {
        self.columns.iter_mut()
    }

    /// The column at `index`.
    pub fn get(&self, index: usize) -> Option<&CorrelatedColumnInfo> {
        self.columns.get(index)
    }

    pub fn contains(&self, binding: ColumnBinding) -> bool {
        self.columns.iter().any(|column| column.binding == binding)
    }
}

impl<'a> IntoIterator for &'a CorrelatedColumns {
    type Item = &'a CorrelatedColumnInfo;
    type IntoIter = std::slice::Iter<'a, CorrelatedColumnInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.columns.iter()
    }
}

impl<'a> IntoIterator for &'a mut CorrelatedColumns {
    type Item = &'a mut CorrelatedColumnInfo;
    type IntoIter = std::slice::IterMut<'a, CorrelatedColumnInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.columns.iter_mut()
    }
}

/// Whether a binder resolves a query or a view's definition (DuckDB's `BinderType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BinderType {
    #[default]
    Regular,
    View,
}

/// What binding a statement is for (DuckDB's `BindingMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BindingMode {
    /// Binding to plan the statement.
    #[default]
    Standard,
    /// Binding only to find the tables the statement uses, by name.
    ExtractNames,
    /// Binding only to find the tables the statement uses, by qualified name.
    ExtractQualifiedNames,
}

impl BindingMode {
    /// Whether the binding only finds table names: columns and function calls bind as
    /// NULL, and missing tables as dummy scans.
    pub fn extracts_names(self) -> bool {
        matches!(self, Self::ExtractNames | Self::ExtractQualifiedNames)
    }
}

/// State shared by every binder of one statement (DuckDB's `GlobalBinderState`).
#[derive(Debug, Default)]
pub struct GlobalBinderState {
    bound_tables: AtomicU64,
    properties: Mutex<StatementProperties>,
    parameters: Mutex<Option<BoundParameterMap>>,
    mode: Mutex<BindingMode>,
    /// The tables found when extracting names (`table_names`).
    table_names: Mutex<BTreeSet<String>>,
}

/// The binder of one query level (DuckDB's `Binder` members).
#[derive(Clone, Debug)]
pub struct Binder {
    pub context: Arc<ClientContext>,
    global: Arc<GlobalBinderState>,
    pub bind_context: BindContext,
    pub correlated_columns: CorrelatedColumns,
    /// The alias of the subquery being bound, if any.
    pub alias: String,
    /// Macro parameters in scope.
    pub macro_binding: Option<Binding>,
    pub binder_type: BinderType,
    /// Whether dependent joins remain to be flattened.
    pub has_unplanned_dependent_joins: bool,
    /// Whether dependent joins of enclosing queries have been flattened.
    pub is_outside_flattened: bool,
    /// Whether expressions may keep the `NULL` type (instead of becoming INTEGER).
    pub can_contain_nulls: bool,
    pub(crate) unnamed_subquery_index: u64,
    depth: u64,
    /// The views being bound by this binder and its parents (DuckDB's `bound_views` up
    /// the parent chain), to detect a view that references itself.
    pub(crate) bound_views: Vec<Arc<rustdb_catalog::CatalogEntry>>,
}

impl Binder {
    /// The binder of a new statement (DuckDB's `Binder::CreateBinder` without parent).
    pub fn new(context: Arc<ClientContext>) -> Result<Self> {
        let mut binder = Self {
            context,
            global: Arc::default(),
            bind_context: BindContext::default(),
            correlated_columns: CorrelatedColumns::default(),
            alias: String::new(),
            macro_binding: None,
            binder_type: BinderType::Regular,
            has_unplanned_dependent_joins: false,
            is_outside_flattened: true,
            can_contain_nulls: false,
            unnamed_subquery_index: 1,
            depth: 1,
            bound_views: Vec::new(),
        };
        binder.increase_depth()?;
        Ok(binder)
    }

    /// A binder for a nested query (DuckDB's `Binder::CreateBinder(context, this)`).
    pub fn child(&self) -> Result<Self> {
        self.child_of_type(BinderType::Regular)
    }

    /// A child binder of the given type.
    pub fn child_of_type(&self, binder_type: BinderType) -> Result<Self> {
        let mut binder = Self {
            context: Arc::clone(&self.context),
            global: Arc::clone(&self.global),
            bind_context: BindContext::default(),
            correlated_columns: CorrelatedColumns::default(),
            alias: String::new(),
            macro_binding: self.macro_binding.clone(),
            binder_type,
            has_unplanned_dependent_joins: false,
            is_outside_flattened: true,
            can_contain_nulls: false,
            unnamed_subquery_index: 1,
            depth: self.depth,
            bound_views: self.bound_views.clone(),
        };
        binder.increase_depth()?;
        Ok(binder)
    }

    /// Records that this binder binds `view`; fails if a parent binder is already binding
    /// it (`Binder::AddBoundView`).
    pub(crate) fn add_bound_view(&mut self, view: &Arc<rustdb_catalog::CatalogEntry>) -> Result<()> {
        if self.bound_views.iter().any(|bound| Arc::ptr_eq(bound, view)) {
            return Err(Error::binder(format!(
                "infinite recursion detected: attempting to recursively bind view \"{}\"",
                view.name()
            )));
        }
        self.bound_views.push(Arc::clone(view));
        Ok(())
    }

    pub(crate) fn increase_depth(&mut self) -> Result<()> {
        self.depth += 1;
        let max = self.context.config.max_expression_depth;
        if self.depth > max {
            return Err(Error::binder(format!(
                "Max expression depth limit of {max} exceeded. Use \"SET max_expression_depth TO x\" to increase \
                 the maximum expression depth."
            )));
        }
        Ok(())
    }

    /// An unused table index (DuckDB's `GenerateTableIndex`).
    pub fn generate_table_index(&self) -> u64 {
        self.global.bound_tables.fetch_add(1, Ordering::Relaxed)
    }

    /// The number of table indexes generated so far, the next one included none.
    pub fn table_index_count(&self) -> u64 {
        self.global.bound_tables.load(Ordering::Relaxed)
    }

    /// The statement's properties.
    pub fn with_properties<T>(&self, f: impl FnOnce(&mut StatementProperties) -> T) -> T {
        f(&mut self
            .global
            .properties
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
    }

    /// Sets what the statement is bound for (`SetBindingMode`).
    pub fn set_binding_mode(&self, mode: BindingMode) {
        *self.global.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    /// What the statement is bound for (`GetBindingMode`).
    pub fn binding_mode(&self) -> BindingMode {
        *self.global.mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a table the statement uses (`AddTableName`).
    pub fn add_table_name(&self, name: String) {
        self.global
            .table_names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name);
    }

    /// The tables recorded while extracting names (`GetTableNames`).
    pub fn take_table_names(&self) -> BTreeSet<String> {
        std::mem::take(
            &mut *self
                .global
                .table_names
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// A copy of the statement's properties.
    pub fn properties(&self) -> StatementProperties {
        self.with_properties(|properties| properties.clone())
    }

    /// Installs the parameter map of a statement being prepared or executed.
    pub fn set_parameters(&self, parameters: BoundParameterMap) {
        *self
            .global
            .parameters
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(parameters);
    }

    /// Removes and returns the parameter map.
    pub fn take_parameters(&self) -> Option<BoundParameterMap> {
        self.global
            .parameters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Runs `f` on the parameter map; `None` if the statement cannot have parameters.
    pub fn with_parameters<T>(&self, f: impl FnOnce(&mut BoundParameterMap) -> T) -> Option<T> {
        self.global
            .parameters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .map(f)
    }

    /// Requires binding again before every execution (DuckDB's `SetAlwaysRequireRebind`).
    pub fn set_always_require_rebind(&self) {
        self.with_properties(|properties| properties.always_require_rebind = true);
    }

    /// The next name for an unnamed subquery (`unnamed_subquery`, `unnamed_subquery2`, ...).
    pub fn next_unnamed_subquery_name(&mut self) -> String {
        let index = self.unnamed_subquery_index;
        self.unnamed_subquery_index += 1;
        if index == 1 {
            "unnamed_subquery".to_owned()
        } else {
            format!("unnamed_subquery{index}")
        }
    }

    /// Adds a correlated column unless it is already there (DuckDB's `AddCorrelatedColumn`).
    pub fn add_correlated_column(&mut self, info: CorrelatedColumnInfo) {
        if !self.correlated_columns.contains(info.binding) {
            self.correlated_columns.add_column(info);
        }
    }

    /// Takes over the correlated columns of a child binder (DuckDB's
    /// `MoveCorrelatedExpressions`).
    pub fn move_correlated_expressions(&mut self, other: &mut Self) {
        let columns = std::mem::take(&mut other.correlated_columns);
        self.merge_correlated_columns(&columns);
        other.correlated_columns.clear();
    }

    /// Adds every column of `other` (DuckDB's `MergeCorrelatedColumns`).
    pub fn merge_correlated_columns(&mut self, other: &CorrelatedColumns) {
        for column in other {
            self.add_correlated_column(column.clone());
        }
    }

    /// Collects the correlated columns referenced in `expression` (DuckDB's
    /// `ExpressionBinder::ExtractCorrelatedExpressions`).
    pub fn extract_correlated_expressions(&mut self, expression: &Expression) {
        if let Some(column) = expression.as_column_ref()
            && column.depth > 0
            && let Some(info) = CorrelatedColumnInfo::from_expression(expression)
        {
            self.add_correlated_column(info);
        }
        for child in expression.children() {
            self.extract_correlated_expressions(child);
        }
    }
}

/// An enclosing query level, as seen from a subquery bound inside it.
///
/// A level is either a binder that is binding a FROM-clause item (a plain
/// [`BindScope`]) or an expression binder that is binding an expression containing the
/// subquery. Correlated columns are resolved by the expression binders of enclosing
/// levels, outermost last.
pub trait Scope {
    /// The level's binder.
    fn scope_binder(&self) -> &Binder;
    /// The level's binder and the levels enclosing it.
    fn parts(&mut self) -> (&mut Binder, Option<&mut dyn Scope>);
    /// The expression binder active at this level, if any.
    fn expression_binder(&mut self) -> Option<&mut dyn ExpressionBinder>;
}

/// A binder together with the levels enclosing it; the binding methods run on this.
pub struct BindScope<'a> {
    pub binder: &'a mut Binder,
    pub outer: Option<&'a mut dyn Scope>,
}

impl std::fmt::Debug for BindScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindScope")
            .field("binder", &self.binder)
            .finish_non_exhaustive()
    }
}

impl<'a> BindScope<'a> {
    /// The top-level scope of a statement.
    pub fn root(binder: &'a mut Binder) -> Self {
        Self { binder, outer: None }
    }

    /// The enclosing levels, reborrowed.
    pub fn outer_mut(&mut self) -> Option<&mut dyn Scope> {
        match &mut self.outer {
            Some(outer) => Some(&mut **outer),
            None => None,
        }
    }

    /// The binder and the enclosing levels, reborrowed separately (to create an expression
    /// binder for this level).
    pub fn split(&mut self) -> (&mut Binder, Option<&mut dyn Scope>) {
        let outer = self.outer.as_mut().map(|outer| &mut **outer as &mut dyn Scope);
        (&mut *self.binder, outer)
    }
}

impl Deref for BindScope<'_> {
    type Target = Binder;

    fn deref(&self) -> &Binder {
        self.binder
    }
}

impl DerefMut for BindScope<'_> {
    fn deref_mut(&mut self) -> &mut Binder {
        self.binder
    }
}

impl Scope for BindScope<'_> {
    fn scope_binder(&self) -> &Binder {
        self.binder
    }

    fn parts(&mut self) -> (&mut Binder, Option<&mut dyn Scope>) {
        let outer = self.outer.as_mut().map(|outer| &mut **outer as &mut dyn Scope);
        (&mut *self.binder, outer)
    }

    fn expression_binder(&mut self) -> Option<&mut dyn ExpressionBinder> {
        None
    }
}
