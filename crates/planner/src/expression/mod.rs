//! Bound expressions (DuckDB's `Expression` hierarchy in `planner/expression`).
//!
//! Like the parse tree, an [`Expression`] holds the members every expression has (type,
//! alias, query location, result type) and its class-specific data in
//! [`ExpressionKind`]. The `expression_type` refines the class: a
//! [`ExpressionKind::Comparison`] is `COMPARE_EQUAL`, `COMPARE_LESSTHAN`, ...

mod display;
mod equality;
mod serialize;

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{
    ExpressionClass, ExpressionType, OrderByNullType, OrderType, SubqueryType, WindowBoundary,
    WindowExcludeMode,
};
use rustdb_common::Result;
use rustdb_function::cast::BoundCastInfo;
use rustdb_function::{AggregateFunction, FunctionData, NullHandling, ScalarFunction, Stability};
use rustdb_types::{LogicalType, Value};

use crate::ColumnBinding;
use crate::binder::{Binder, BoundStatement};

pub use display::expression_list_to_string;

/// A bound, typed expression (DuckDB's `Expression`).
#[derive(Clone, Debug)]
pub struct Expression {
    pub expression_type: ExpressionType,
    /// The name given with `AS`, or the name the binder derived.
    pub alias: String,
    /// Byte offset in the query text, for error messages.
    pub query_location: Option<u64>,
    /// The type the expression evaluates to.
    pub return_type: LogicalType,
    pub kind: ExpressionKind,
}

/// The class-specific part of an [`Expression`].
#[derive(Clone, Debug)]
pub enum ExpressionKind {
    Aggregate(Box<BoundAggregateExpression>),
    Between(Box<BoundBetweenExpression>),
    Case(Box<BoundCaseExpression>),
    Cast(Box<BoundCastExpression>),
    ColumnRef(BoundColumnRefExpression),
    Comparison(Box<BoundComparisonExpression>),
    Conjunction(BoundConjunctionExpression),
    Constant(BoundConstantExpression),
    /// `DEFAULT` in an `INSERT` or `UPDATE`.
    Default,
    Function(Box<BoundFunctionExpression>),
    Lambda(Box<BoundLambdaExpression>),
    LambdaRef(BoundLambdaRefExpression),
    Operator(BoundOperatorExpression),
    Parameter(BoundParameterExpression),
    /// A column of the operator's input chunk, after column bindings are resolved.
    Reference(BoundReferenceExpression),
    Subquery(Box<BoundSubqueryExpression>),
    Unnest(Box<BoundUnnestExpression>),
    Window(Box<BoundWindowExpression>),
    /// Several expressions a single select-list entry expands into.
    Expanded(BoundExpandedExpression),
}

/// Whether an aggregate removes duplicate inputs.
pub use crate::generated::enums::AggregateType;

impl Default for crate::TupleDataValidityType {
    /// `LogicalAggregate`'s until statistics propagation knows better.
    fn default() -> Self {
        Self::CanHaveNullValues
    }
}

/// An aggregate call (DuckDB's `BoundAggregateExpression`).
#[derive(Clone, Debug)]
pub struct BoundAggregateExpression {
    pub function: AggregateFunction,
    pub children: Vec<Expression>,
    pub bind_info: Option<Arc<dyn FunctionData>>,
    pub aggr_type: AggregateType,
    /// `FILTER (WHERE ...)`.
    pub filter: Option<Expression>,
    /// `ORDER BY` inside the call.
    pub order_bys: Option<BoundOrderModifier>,
}

impl BoundAggregateExpression {
    pub fn is_distinct(&self) -> bool {
        self.aggr_type == AggregateType::Distinct
    }
}

/// `input BETWEEN lower AND upper` (DuckDB's `BoundBetweenExpression`).
#[derive(Clone, Debug)]
pub struct BoundBetweenExpression {
    pub input: Expression,
    pub lower: Expression,
    pub upper: Expression,
    pub lower_inclusive: bool,
    pub upper_inclusive: bool,
}

impl BoundBetweenExpression {
    pub const fn lower_comparison_type(&self) -> ExpressionType {
        if self.lower_inclusive {
            ExpressionType::CompareGreaterthanorequalto
        } else {
            ExpressionType::CompareGreaterthan
        }
    }

    pub const fn upper_comparison_type(&self) -> ExpressionType {
        if self.upper_inclusive {
            ExpressionType::CompareLessthanorequalto
        } else {
            ExpressionType::CompareLessthan
        }
    }
}

/// One `WHEN ... THEN ...` of a `CASE`.
#[derive(Clone, Debug)]
pub struct BoundCaseCheck {
    pub when_expr: Expression,
    pub then_expr: Expression,
}

/// `CASE WHEN ... THEN ... ELSE ... END` (DuckDB's `BoundCaseExpression`).
#[derive(Clone, Debug)]
pub struct BoundCaseExpression {
    pub case_checks: Vec<BoundCaseCheck>,
    pub else_expr: Expression,
}

/// A cast to the expression's return type (DuckDB's `BoundCastExpression`).
#[derive(Clone, Debug)]
pub struct BoundCastExpression {
    pub child: Expression,
    pub try_cast: bool,
    pub bound_cast: BoundCastInfo,
}

/// A column of some operator's output, before resolution to a
/// [`ExpressionKind::Reference`] (DuckDB's `BoundColumnRefExpression`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundColumnRefExpression {
    pub binding: ColumnBinding,
    /// How many subquery levels up the column comes from (0: this query).
    pub depth: u64,
}

/// A comparison; the operator is the expression type (DuckDB's `BoundComparisonExpression`).
#[derive(Clone, Debug)]
pub struct BoundComparisonExpression {
    pub left: Expression,
    pub right: Expression,
}

/// `AND` / `OR` over two or more children (DuckDB's `BoundConjunctionExpression`).
#[derive(Clone, Debug)]
pub struct BoundConjunctionExpression {
    pub children: Vec<Expression>,
}

/// A constant (DuckDB's `BoundConstantExpression`).
#[derive(Clone, Debug)]
pub struct BoundConstantExpression {
    pub value: Value,
}

/// A scalar function call (DuckDB's `BoundFunctionExpression`).
#[derive(Clone, Debug)]
pub struct BoundFunctionExpression {
    pub function: ScalarFunction,
    pub children: Vec<Expression>,
    pub bind_info: Option<Arc<dyn FunctionData>>,
    /// Whether this came from an operator (`a + b`) rather than a call (`add(a, b)`).
    pub is_operator: bool,
    /// The bound lambda body of a lambda function call (DuckDB keeps it in the
    /// `ListLambdaBindData`); its inputs are references into the lambda's input chunk.
    pub lambda: Option<Box<Expression>>,
}

/// A lambda passed to a list function (DuckDB's `BoundLambdaExpression`).
#[derive(Clone, Debug)]
pub struct BoundLambdaExpression {
    pub lambda_expr: Expression,
    /// Columns of the enclosing query the lambda body uses.
    pub captures: Vec<Expression>,
    pub parameter_count: u64,
}

/// A lambda parameter inside a lambda body (DuckDB's `BoundLambdaRefExpression`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundLambdaRefExpression {
    pub binding: ColumnBinding,
    pub lambda_index: u64,
    pub depth: u64,
}

/// An operator that is not a function: `NOT`, `IS NULL`, `IN`, `COALESCE`, `TRY`, ...
/// (DuckDB's `BoundOperatorExpression`).
#[derive(Clone, Debug)]
pub struct BoundOperatorExpression {
    pub children: Vec<Expression>,
}

/// The value and type of a prepared statement parameter, shared between the plan and the
/// statement's parameter map (DuckDB's `BoundParameterData`).
#[derive(Clone, Debug, Default)]
pub struct BoundParameterData {
    pub value: Value,
    pub return_type: LogicalType,
}

impl BoundParameterData {
    /// A parameter with a known value; its type is the value's, with strings typed as
    /// string literals so that they can still be implicitly cast.
    pub fn from_value(value: Value) -> Self {
        let return_type = if value.logical_type().id() == rustdb_types::LogicalTypeId::Varchar {
            LogicalType::STRING_LITERAL
        } else {
            value.logical_type().clone()
        };
        Self { value, return_type }
    }
}

/// A shared, updatable [`BoundParameterData`].
pub type SharedParameterData = Arc<Mutex<BoundParameterData>>;

/// A prepared statement parameter `$n` / `?` (DuckDB's `BoundParameterExpression`).
#[derive(Clone, Debug)]
pub struct BoundParameterExpression {
    pub identifier: String,
    pub parameter_data: SharedParameterData,
}

impl BoundParameterExpression {
    /// The parameter's current value.
    pub fn value(&self) -> Value {
        self.parameter_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .value
            .clone()
    }
}

/// Column `index` of the input chunk (DuckDB's `BoundReferenceExpression`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundReferenceExpression {
    pub index: u64,
}

/// A subquery used as an expression (DuckDB's `BoundSubqueryExpression`).
#[derive(Clone, Debug)]
pub struct BoundSubqueryExpression {
    /// The binder that bound the subquery (its correlated columns are needed to plan it).
    pub binder: Box<Binder>,
    /// The bound and planned subquery.
    pub subquery: BoundStatement,
    pub subquery_type: SubqueryType,
    /// The left side of `x IN (...)` / `x = ANY(...)`.
    pub children: Vec<Expression>,
    /// The comparison of an `ANY` subquery.
    pub comparison_type: ExpressionType,
    /// The types of `children` before casting.
    pub child_types: Vec<LogicalType>,
    /// The types `children` are compared at.
    pub child_targets: Vec<LogicalType>,
}

impl BoundSubqueryExpression {
    /// Whether the subquery references columns of enclosing queries.
    pub fn is_correlated(&self) -> bool {
        !self.binder.correlated_columns.is_empty()
    }
}

/// `UNNEST(child)` in a select list (DuckDB's `BoundUnnestExpression`).
#[derive(Clone, Debug)]
pub struct BoundUnnestExpression {
    pub child: Expression,
}

/// A window function call (DuckDB's `BoundWindowExpression`).
#[derive(Clone, Debug)]
pub struct BoundWindowExpression {
    /// The aggregate, for aggregates used as window functions.
    pub aggregate: Option<AggregateFunction>,
    pub bind_info: Option<Arc<dyn FunctionData>>,
    pub children: Vec<Expression>,
    pub partitions: Vec<Expression>,
    pub orders: Vec<BoundOrderByNode>,
    pub filter_expr: Option<Expression>,
    pub ignore_nulls: bool,
    pub distinct: bool,
    pub start: WindowBoundary,
    pub end: WindowBoundary,
    pub exclude_clause: WindowExcludeMode,
    pub start_expr: Option<Expression>,
    pub end_expr: Option<Expression>,
    pub offset_expr: Option<Expression>,
    pub default_expr: Option<Expression>,
    /// `ORDER BY` inside the call (`first_value(x ORDER BY y)`).
    pub arg_orders: Vec<BoundOrderByNode>,
}

/// The expressions a single select-list entry expands into (DuckDB's
/// `BoundExpandedExpression`).
#[derive(Clone, Debug)]
pub struct BoundExpandedExpression {
    pub expanded_expressions: Vec<Expression>,
}

/// One `ORDER BY` term (DuckDB's `BoundOrderByNode`).
#[derive(Clone, Debug)]
pub struct BoundOrderByNode {
    pub order_type: OrderType,
    pub null_order: OrderByNullType,
    pub expression: Expression,
}

impl BoundOrderByNode {
    pub fn new(order_type: OrderType, null_order: OrderByNullType, expression: Expression) -> Self {
        Self {
            order_type,
            null_order,
            expression,
        }
    }
}

/// A bound `ORDER BY` (DuckDB's `BoundOrderModifier`).
#[derive(Clone, Debug, Default)]
pub struct BoundOrderModifier {
    pub orders: Vec<BoundOrderByNode>,
}

impl Expression {
    /// An expression with the given type and kind, without alias or location.
    pub fn new(expression_type: ExpressionType, return_type: LogicalType, kind: ExpressionKind) -> Self {
        Self {
            expression_type,
            alias: String::new(),
            query_location: None,
            return_type,
            kind,
        }
    }

    /// A constant (`VALUE_CONSTANT`).
    pub fn constant(value: Value) -> Self {
        let return_type = value.logical_type().clone();
        Self::new(
            ExpressionType::ValueConstant,
            return_type,
            ExpressionKind::Constant(BoundConstantExpression { value }),
        )
    }

    /// A reference to `binding` (`BOUND_COLUMN_REF`).
    pub fn column_ref(
        alias: impl Into<String>,
        return_type: LogicalType,
        binding: ColumnBinding,
        depth: u64,
    ) -> Self {
        let mut expression = Self::new(
            ExpressionType::BoundColumnRef,
            return_type,
            ExpressionKind::ColumnRef(BoundColumnRefExpression { binding, depth }),
        );
        expression.alias = alias.into();
        expression
    }

    /// Column `index` of the input chunk (`BOUND_REF`).
    pub fn reference(alias: impl Into<String>, return_type: LogicalType, index: u64) -> Self {
        let mut expression = Self::new(
            ExpressionType::BoundRef,
            return_type,
            ExpressionKind::Reference(BoundReferenceExpression { index }),
        );
        expression.alias = alias.into();
        expression
    }

    /// A comparison of `left` and `right`.
    pub fn comparison(expression_type: ExpressionType, left: Self, right: Self) -> Self {
        Self::new(
            expression_type,
            LogicalType::BOOLEAN,
            ExpressionKind::Comparison(Box::new(BoundComparisonExpression { left, right })),
        )
    }

    /// `AND` / `OR` of `children`.
    pub fn conjunction(expression_type: ExpressionType, children: Vec<Self>) -> Self {
        Self::new(
            expression_type,
            LogicalType::BOOLEAN,
            ExpressionKind::Conjunction(BoundConjunctionExpression { children }),
        )
    }

    /// An operator expression.
    pub fn operator(expression_type: ExpressionType, return_type: LogicalType, children: Vec<Self>) -> Self {
        Self::new(
            expression_type,
            return_type,
            ExpressionKind::Operator(BoundOperatorExpression { children }),
        )
    }

    /// A scalar function call.
    pub fn function(
        function: ScalarFunction,
        children: Vec<Self>,
        bind_info: Option<Arc<dyn FunctionData>>,
        is_operator: bool,
    ) -> Self {
        let return_type = function.return_type.clone();
        Self::new(
            ExpressionType::BoundFunction,
            return_type,
            ExpressionKind::Function(Box::new(BoundFunctionExpression {
                function,
                children,
                bind_info,
                is_operator,
                lambda: None,
            })),
        )
    }

    /// The same expression under an alias.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }

    /// The expression's class (DuckDB's `GetExpressionClass`).
    pub const fn expression_class(&self) -> ExpressionClass {
        match &self.kind {
            ExpressionKind::Aggregate(_) => ExpressionClass::BoundAggregate,
            ExpressionKind::Between(_) => ExpressionClass::BoundBetween,
            ExpressionKind::Case(_) => ExpressionClass::BoundCase,
            ExpressionKind::Cast(_) => ExpressionClass::BoundCast,
            ExpressionKind::ColumnRef(_) => ExpressionClass::BoundColumnRef,
            ExpressionKind::Comparison(_) => ExpressionClass::BoundComparison,
            ExpressionKind::Conjunction(_) => ExpressionClass::BoundConjunction,
            ExpressionKind::Constant(_) => ExpressionClass::BoundConstant,
            ExpressionKind::Default => ExpressionClass::BoundDefault,
            ExpressionKind::Function(_) => ExpressionClass::BoundFunction,
            ExpressionKind::Lambda(_) => ExpressionClass::BoundLambda,
            ExpressionKind::LambdaRef(_) => ExpressionClass::BoundLambdaRef,
            ExpressionKind::Operator(_) => ExpressionClass::BoundOperator,
            ExpressionKind::Parameter(_) => ExpressionClass::BoundParameter,
            ExpressionKind::Reference(_) => ExpressionClass::BoundRef,
            ExpressionKind::Subquery(_) => ExpressionClass::BoundSubquery,
            ExpressionKind::Unnest(_) => ExpressionClass::BoundUnnest,
            ExpressionKind::Window(_) => ExpressionClass::BoundWindow,
            ExpressionKind::Expanded(_) => ExpressionClass::BoundExpanded,
        }
    }

    /// The alias, or else the expression's text (DuckDB's `GetName`).
    pub fn name(&self) -> String {
        if self.alias.is_empty() {
            self.to_string()
        } else {
            self.alias.clone()
        }
    }

    /// The constant value, if this is a constant.
    pub fn as_constant(&self) -> Option<&Value> {
        match &self.kind {
            ExpressionKind::Constant(constant) => Some(&constant.value),
            _ => None,
        }
    }

    /// The column binding and depth, if this is a column reference.
    pub fn as_column_ref(&self) -> Option<&BoundColumnRefExpression> {
        match &self.kind {
            ExpressionKind::ColumnRef(column) => Some(column),
            _ => None,
        }
    }

    /// Calls `callback` on every direct child, in DuckDB's `ExpressionIterator` order.
    pub fn try_for_each_child_mut<E>(
        &mut self,
        mut callback: impl FnMut(&mut Self) -> Result<(), E>,
    ) -> Result<(), E> {
        match &mut self.kind {
            ExpressionKind::Aggregate(aggregate) => {
                for child in &mut aggregate.children {
                    callback(child)?;
                }
                if let Some(filter) = &mut aggregate.filter {
                    callback(filter)?;
                }
                if let Some(order_bys) = &mut aggregate.order_bys {
                    for order in &mut order_bys.orders {
                        callback(&mut order.expression)?;
                    }
                }
            }
            ExpressionKind::Between(between) => {
                callback(&mut between.input)?;
                callback(&mut between.lower)?;
                callback(&mut between.upper)?;
            }
            ExpressionKind::Case(case) => {
                for check in &mut case.case_checks {
                    callback(&mut check.when_expr)?;
                    callback(&mut check.then_expr)?;
                }
                callback(&mut case.else_expr)?;
            }
            ExpressionKind::Cast(cast) => callback(&mut cast.child)?,
            ExpressionKind::Comparison(comparison) => {
                callback(&mut comparison.left)?;
                callback(&mut comparison.right)?;
            }
            ExpressionKind::Conjunction(BoundConjunctionExpression { children })
            | ExpressionKind::Operator(BoundOperatorExpression { children }) => {
                for child in children {
                    callback(child)?;
                }
            }
            ExpressionKind::Function(function) => {
                for child in &mut function.children {
                    callback(child)?;
                }
            }
            ExpressionKind::Subquery(subquery) => {
                for child in &mut subquery.children {
                    callback(child)?;
                }
            }
            ExpressionKind::Window(window) => {
                for partition in &mut window.partitions {
                    callback(partition)?;
                }
                for order in &mut window.orders {
                    callback(&mut order.expression)?;
                }
                for child in &mut window.children {
                    callback(child)?;
                }
                for bound in [
                    &mut window.filter_expr,
                    &mut window.start_expr,
                    &mut window.end_expr,
                    &mut window.offset_expr,
                    &mut window.default_expr,
                ]
                .into_iter()
                .flatten()
                {
                    callback(bound)?;
                }
                for order in &mut window.arg_orders {
                    callback(&mut order.expression)?;
                }
            }
            ExpressionKind::Unnest(unnest) => callback(&mut unnest.child)?,
            ExpressionKind::ColumnRef(_)
            | ExpressionKind::LambdaRef(_)
            | ExpressionKind::Constant(_)
            | ExpressionKind::Default
            | ExpressionKind::Parameter(_)
            | ExpressionKind::Reference(_)
            | ExpressionKind::Lambda(_)
            | ExpressionKind::Expanded(_) => {}
        }
        Ok(())
    }

    /// A copy as DuckDB's `Expression::Copy` makes it: column references, references and
    /// lambda parameters lose their query location, lambdas, UNNESTs and DEFAULTs their
    /// alias too (their `Copy` does not call `CopyProperties`).
    #[must_use]
    pub fn copy(&self) -> Self {
        fn drop_uncopied_properties(expression: &mut Expression) {
            match &expression.kind {
                ExpressionKind::ColumnRef(_)
                | ExpressionKind::Reference(_)
                | ExpressionKind::LambdaRef(_) => {
                    expression.query_location = None;
                }
                ExpressionKind::Lambda(_) | ExpressionKind::Unnest(_) | ExpressionKind::Default => {
                    expression.query_location = None;
                    expression.alias.clear();
                }
                _ => {}
            }
            expression.for_each_child_mut(drop_uncopied_properties);
        }
        let mut copy = self.clone();
        drop_uncopied_properties(&mut copy);
        copy
    }

    /// Calls `callback` on every direct child.
    pub fn for_each_child_mut(&mut self, mut callback: impl FnMut(&mut Self)) {
        let _: Result<(), ()> = self.try_for_each_child_mut(|child| {
            callback(child);
            Ok(())
        });
    }

    /// The direct children, in DuckDB's `ExpressionIterator` order.
    pub fn children(&self) -> Vec<&Self> {
        let mut children = Vec::new();
        match &self.kind {
            ExpressionKind::Aggregate(aggregate) => {
                children.extend(&aggregate.children);
                children.extend(&aggregate.filter);
                if let Some(order_bys) = &aggregate.order_bys {
                    children.extend(order_bys.orders.iter().map(|order| &order.expression));
                }
            }
            ExpressionKind::Between(between) => {
                children.extend([&between.input, &between.lower, &between.upper]);
            }
            ExpressionKind::Case(case) => {
                for check in &case.case_checks {
                    children.extend([&check.when_expr, &check.then_expr]);
                }
                children.push(&case.else_expr);
            }
            ExpressionKind::Cast(cast) => children.push(&cast.child),
            ExpressionKind::Comparison(comparison) => children.extend([&comparison.left, &comparison.right]),
            ExpressionKind::Conjunction(BoundConjunctionExpression { children: list })
            | ExpressionKind::Operator(BoundOperatorExpression { children: list }) => children.extend(list),
            ExpressionKind::Function(function) => children.extend(&function.children),
            ExpressionKind::Subquery(subquery) => children.extend(&subquery.children),
            ExpressionKind::Window(window) => {
                children.extend(&window.partitions);
                children.extend(window.orders.iter().map(|order| &order.expression));
                children.extend(&window.children);
                children.extend(
                    [
                        &window.filter_expr,
                        &window.start_expr,
                        &window.end_expr,
                        &window.offset_expr,
                        &window.default_expr,
                    ]
                    .into_iter()
                    .flatten(),
                );
                children.extend(window.arg_orders.iter().map(|order| &order.expression));
            }
            ExpressionKind::Unnest(unnest) => children.push(&unnest.child),
            ExpressionKind::ColumnRef(_)
            | ExpressionKind::LambdaRef(_)
            | ExpressionKind::Constant(_)
            | ExpressionKind::Default
            | ExpressionKind::Parameter(_)
            | ExpressionKind::Reference(_)
            | ExpressionKind::Lambda(_)
            | ExpressionKind::Expanded(_) => {}
        }
        children
    }

    /// Whether `predicate` holds for this expression or any expression below it.
    pub fn any(&self, predicate: &impl Fn(&Self) -> bool) -> bool {
        predicate(self) || self.children().into_iter().any(|child| child.any(predicate))
    }

    /// Whether the expression contains an aggregate (DuckDB's `IsAggregate`).
    pub fn is_aggregate(&self) -> bool {
        self.any(&|expression| matches!(expression.kind, ExpressionKind::Aggregate(_)))
    }

    /// Whether the expression contains a window function (DuckDB's `IsWindow`).
    pub fn is_window(&self) -> bool {
        self.any(&|expression| matches!(expression.kind, ExpressionKind::Window(_)))
    }

    /// Whether the expression contains a subquery (DuckDB's `HasSubquery`).
    pub fn has_subquery(&self) -> bool {
        self.any(&|expression| matches!(expression.kind, ExpressionKind::Subquery(_)))
    }

    /// Whether the expression contains a parameter (DuckDB's `HasParameter`).
    pub fn has_parameter(&self) -> bool {
        self.any(&|expression| matches!(expression.kind, ExpressionKind::Parameter(_)))
    }

    /// Whether the expression yields one value regardless of the input (DuckDB's `IsScalar`).
    pub fn is_scalar(&self) -> bool {
        let scalar_node = !matches!(
            self.kind,
            ExpressionKind::ColumnRef(_)
                | ExpressionKind::LambdaRef(_)
                | ExpressionKind::Default
                | ExpressionKind::Reference(_)
                | ExpressionKind::Subquery(_)
        );
        scalar_node && self.children().into_iter().all(Self::is_scalar)
    }

    /// Whether the expression can produce different results for the same input
    /// (DuckDB's `IsVolatile`).
    pub fn is_volatile(&self) -> bool {
        // The body of a lambda function counts too (`BoundFunctionExpression::IsVolatile`).
        self.any(&|expression| match &expression.kind {
            ExpressionKind::Function(function) => {
                function.function.stability == Stability::Volatile
                    || function
                        .lambda
                        .as_ref()
                        .is_some_and(|lambda| lambda.is_volatile())
            }
            _ => false,
        })
    }

    /// Whether the expression produces the same result within and across queries
    /// (DuckDB's `IsConsistent`).
    pub fn is_consistent(&self) -> bool {
        !self.any(&|expression| match &expression.kind {
            ExpressionKind::Function(function) => function.function.stability != Stability::Consistent,
            _ => false,
        })
    }

    /// Whether the expression can be evaluated once, without input rows
    /// (DuckDB's `IsFoldable`).
    pub fn is_foldable(&self) -> bool {
        let foldable_node = match &self.kind {
            ExpressionKind::Aggregate(_)
            | ExpressionKind::ColumnRef(_)
            | ExpressionKind::LambdaRef(_)
            | ExpressionKind::Default
            | ExpressionKind::Parameter(_)
            | ExpressionKind::Reference(_)
            | ExpressionKind::Subquery(_)
            | ExpressionKind::Unnest(_)
            | ExpressionKind::Window(_) => false,
            // A lambda function with a volatile body cannot be folded
            // (`BoundFunctionExpression::IsFoldable`).
            ExpressionKind::Function(function) => {
                function.function.stability != Stability::Volatile
                    && !function
                        .lambda
                        .as_ref()
                        .is_some_and(|lambda| lambda.is_volatile())
            }
            _ => true,
        };
        foldable_node && self.children().into_iter().all(Self::is_foldable)
    }

    /// Whether a NULL input always produces a NULL output (DuckDB's `PropagatesNullValues`).
    pub fn propagates_null_values(&self) -> bool {
        let propagating_type = !matches!(
            self.expression_type,
            ExpressionType::OperatorIsNull
                | ExpressionType::OperatorIsNotNull
                | ExpressionType::CompareNotDistinctFrom
                | ExpressionType::CompareDistinctFrom
                | ExpressionType::ConjunctionOr
                | ExpressionType::ConjunctionAnd
                | ExpressionType::OperatorCoalesce
                | ExpressionType::CaseExpr
        );
        let propagating_node = match &self.kind {
            ExpressionKind::Function(function) => function.function.null_handling != NullHandling::Special,
            ExpressionKind::Aggregate(aggregate) => aggregate.function.null_handling != NullHandling::Special,
            ExpressionKind::Subquery(_) => false,
            _ => true,
        };
        propagating_type && propagating_node && self.children().into_iter().all(Self::propagates_null_values)
    }
}
