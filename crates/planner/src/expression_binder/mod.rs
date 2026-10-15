//! Expression binders (DuckDB's `ExpressionBinder` and its specialisations).
//!
//! An expression binder turns a parsed expression into a bound [`Expression`] in the
//! context of one query level. The base behaviour lives in the default methods of the
//! [`ExpressionBinder`] trait; each clause of a query has its own binder overriding what
//! differs there (aggregates in `SELECT`, aliases in `WHERE`, groups in `HAVING`, ...).
//!
//! Binding works in place like DuckDB's: each successfully bound sub-tree of the parse
//! tree is replaced by a [`rustdb_ast::BoundExpression`] node. When a sub-tree fails to
//! bind, the error is returned *softly* ([`BindResult::Unbound`]) so that the binders of
//! enclosing queries can retry it as a correlated reference, keeping what already bound.
//! Hard errors (DuckDB's thrown exceptions) are `Err`.

mod base;
mod base_select_binder;
mod column_alias_binder;
mod group_binder;
mod lateral_binder;
mod order_binder;
mod qualify;
mod simple_binders;

use rustdb_ast::{
    BoundExpression, ExpressionClass, ExpressionType, FunctionExpression, OperatorExpression,
    ParsedExpression, ParsedExpressionKind,
};
use rustdb_common::string_util::top_n_strings;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::binder::{Binder, Binding, Scope};
use crate::expression::Expression;
use crate::function_binder::add_cast_to_type;

pub use base::BinderBase;
pub use base_select_binder::{
    BoundGroupInformation, HavingBinder, QualifyBinder, SelectBindState, SelectBinder, SelectRole,
};
pub use column_alias_binder::ColumnAliasBinder;
pub use group_binder::GroupBinder;
pub use lateral_binder::{LateralBinder, reduce_expression_depth};
pub use order_binder::OrderBinder;
pub use qualify::{
    ColumnQualifier, QualifyContext, bind_and_qualify_function, create_struct_extract, for_each_child,
    is_lambda_function, qualify_column_names,
};
pub use simple_binders::{
    AggregateBinder, BaseBinder, ConstantBinder, InsertBinder, TableFunctionBinder, WhereBinder,
};

/// A column-not-found error, kept apart so that errors of several query levels can be
/// merged into one candidate list (DuckDB's `COLUMN_NOT_FOUND` error subtype).
#[derive(Clone, Debug)]
pub struct MissingColumn {
    pub name: String,
    pub candidates: Vec<String>,
}

/// A soft binding failure (DuckDB's `ErrorData` inside a `BindResult`).
#[derive(Clone, Debug)]
pub struct BindError {
    pub error: Error,
    pub missing_column: Option<MissingColumn>,
}

impl From<Error> for BindError {
    fn from(error: Error) -> Self {
        Self {
            error,
            missing_column: None,
        }
    }
}

impl BindError {
    /// A Binder Error.
    pub fn binder(message: impl Into<String>) -> Self {
        Error::binder(message.into()).into()
    }

    /// "Referenced column ... not found" (DuckDB's `BinderException::ColumnNotFound`).
    pub fn column_not_found(name: &str, candidates: Vec<String>, location: Option<u64>) -> Self {
        let message = if candidates.is_empty() {
            format!("Referenced column \"{name}\" was not found because the FROM clause is missing")
        } else {
            let quoted: Vec<String> = candidates.iter().map(|c| format!("\"{c}\"")).collect();
            format!(
                "Referenced column \"{name}\" not found in FROM clause!\nCandidate bindings: {}",
                quoted.join(", ")
            )
        };
        Self {
            error: Error::binder(message).with_position(location),
            missing_column: Some(MissingColumn {
                name: name.to_owned(),
                candidates,
            }),
        }
    }

    /// Attaches a query location unless one is set (DuckDB's `AddQueryLocation`).
    #[must_use]
    pub fn with_position(mut self, location: Option<u64>) -> Self {
        self.error = self.error.with_position(location);
        self
    }

    /// Merges two "column not found" errors about the same column into one with the best
    /// candidates of both (DuckDB's `CombineMissingColumns`); otherwise the new error wins
    /// (DuckDB's `CombineErrors`).
    #[must_use]
    pub fn combine(self, new: Self) -> Self {
        let (Some(current), Some(other)) = (&self.missing_column, &new.missing_column) else {
            return new;
        };
        if current.name != other.name {
            return new;
        }
        if current.candidates.is_empty() {
            return new;
        }
        if other.candidates.is_empty() {
            return self;
        }
        let name = current.name.clone();
        let mut seen: Vec<String> = Vec::new();
        let mut scores = Vec::new();
        for candidate in current.candidates.iter().chain(&other.candidates) {
            let Some(column) = candidate.split('.').next_back() else {
                continue;
            };
            if seen.contains(candidate) {
                continue;
            }
            let score = rustdb_common::string_util::similarity_rating(column, &name);
            seen.push(candidate.clone());
            scores.push((candidate.clone(), score));
        }
        let location = self.error.position().or(new.error.position());
        Self::column_not_found(&name, top_n_strings(scores, 5, 0.5), location)
    }
}

/// The outcome of binding an expression at one query level (DuckDB's `BindResult`).
#[derive(Debug)]
pub enum BindResult {
    Bound(Expression),
    Unbound(BindError),
}

impl From<Expression> for BindResult {
    fn from(expression: Expression) -> Self {
        Self::Bound(expression)
    }
}

impl BindResult {
    /// A soft Binder Error.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Unbound(BindError::binder(message))
    }
}

/// A column reference that bound successfully (DuckDB's `BoundColumnReferenceInfo`).
#[derive(Clone, Debug)]
pub struct BoundColumnReferenceInfo {
    pub name: String,
    pub query_location: Option<u64>,
}

/// The members every expression binder has (DuckDB's `ExpressionBinder` fields).
#[derive(Clone, Debug, Default)]
pub struct ExpressionBinderState {
    /// The type results are cast to, if any.
    pub target_type: Option<LogicalType>,
    /// Columns this binder bound (used to detect non-aggregated columns).
    pub bound_columns: Vec<BoundColumnReferenceInfo>,
    /// Parameters of the lambdas being bound, innermost last.
    pub lambda_bindings: Vec<Binding>,
}

/// "The parsed expression was not bound" (an internal invariant violation).
fn not_bound() -> Error {
    Error::internal("expected a bound expression")
}

/// Replaces a parsed node by its bound form (DuckDB's `make_uniq<BoundExpression>`).
pub fn set_bound(expr: &mut ParsedExpression, bound: Expression) {
    let text = bound.to_string();
    let alias = std::mem::take(&mut expr.alias);
    *expr = ParsedExpression {
        expression_type: ExpressionType::Invalid,
        alias,
        query_location: expr.query_location,
        kind: ParsedExpressionKind::Bound(BoundExpression::new(bound, text)),
    };
}

/// The bound expression of a node replaced by [`set_bound`]
/// (DuckDB's `BoundExpression::GetExpression`).
pub fn take_bound(expr: &mut ParsedExpression) -> Result<Expression> {
    let placeholder = ParsedExpression::new(ExpressionType::Invalid, ParsedExpressionKind::Default);
    let ParsedExpressionKind::Bound(bound) = std::mem::replace(expr, placeholder).kind else {
        return Err(not_bound());
    };
    let expression = bound.payload.downcast::<Expression>().map_err(|_| not_bound())?;
    Ok(std::sync::Arc::try_unwrap(expression).unwrap_or_else(|shared| (*shared).clone()))
}

/// The bound expression of a node replaced by [`set_bound`], without taking it.
pub fn peek_bound(expr: &ParsedExpression) -> Option<&Expression> {
    match &expr.kind {
        ParsedExpressionKind::Bound(bound) => bound.payload.downcast_ref::<Expression>(),
        _ => None,
    }
}

/// Whether a parsed node has been replaced by its bound form.
pub fn is_bound(expr: &ParsedExpression) -> bool {
    matches!(&expr.kind, ParsedExpressionKind::Bound(bound) if bound.payload.is::<Expression>())
}

/// The type the binder reasons with for `expression`: constants keep their literal-ness
/// so that `1 + x` can bind to `x`'s type (DuckDB's `GetExpressionReturnType`).
pub fn expression_return_type(expression: &Expression) -> LogicalType {
    if let Some(value) = expression.as_constant() {
        let ty = &expression.return_type;
        if ty.id() == LogicalTypeId::Varchar && ty.alias().is_none() && collation(ty).is_none() {
            return LogicalType::STRING_LITERAL;
        }
        if ty.is_integral() && !value.is_null() {
            return LogicalType::integer_literal(value.clone());
        }
    }
    expression.return_type.clone()
}

/// The collation of a VARCHAR type, if any.
pub fn collation(ty: &LogicalType) -> Option<&str> {
    match ty.info() {
        Some(rustdb_types::TypeInfo::String(info)) if !info.collation.is_empty() => Some(&info.collation),
        _ => None,
    }
}

/// Whether `ty` contains `target` anywhere (DuckDB's `ExpressionBinder::ContainsType`).
pub fn contains_type(ty: &LogicalType, target: LogicalTypeId) -> bool {
    if ty.id() == target {
        return true;
    }
    match ty.id() {
        LogicalTypeId::Struct | LogicalTypeId::Union => ty
            .struct_children()
            .unwrap_or_default()
            .iter()
            .skip(usize::from(ty.id() == LogicalTypeId::Union))
            .any(|(_, child)| contains_type(child, target)),
        LogicalTypeId::List | LogicalTypeId::Map | LogicalTypeId::Array => {
            ty.child_type().is_some_and(|child| contains_type(child, target))
        }
        _ => false,
    }
}

/// `ty` with every `target` replaced by `new_type` (DuckDB's `ExchangeType`).
pub fn exchange_type(ty: &LogicalType, target: LogicalTypeId, new_type: &LogicalType) -> LogicalType {
    if ty.id() == target {
        return new_type.clone();
    }
    match ty.id() {
        LogicalTypeId::Struct => LogicalType::struct_type(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| (name.clone(), exchange_type(child, target, new_type)))
                .collect(),
        ),
        LogicalTypeId::Union => LogicalType::union_type(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .skip(1)
                .map(|(name, child)| (name.clone(), exchange_type(child, target, new_type)))
                .collect(),
        ),
        LogicalTypeId::List => ty.child_type().map_or_else(
            || ty.clone(),
            |child| LogicalType::list(exchange_type(child, target, new_type)),
        ),
        LogicalTypeId::Map => match ty.map_key_value() {
            Some((key, value)) => LogicalType::map(
                exchange_type(key, target, new_type),
                exchange_type(value, target, new_type),
            ),
            None => ty.clone(),
        },
        LogicalTypeId::Array => match ty.array_child() {
            Some((child, size)) => LogicalType::array(exchange_type(child, target, new_type), size),
            None => ty.clone(),
        },
        _ => ty.clone(),
    }
}

/// "Parameter types could not be resolved" (DuckDB's `ParameterNotResolvedException`).
pub fn parameter_not_resolved() -> Error {
    Error::new(
        ErrorKind::ParameterNotResolved,
        "Parameter types could not be resolved",
    )
}

/// The unsupported-construct error at an expression's location (DuckDB's
/// `BinderException::Unsupported`).
pub fn unsupported(expr: &ParsedExpression, message: impl Into<String>) -> BindError {
    Error::binder(message.into())
        .with_position(expr.query_location)
        .into()
}

/// Whether a function name is UNNEST (DuckDB's `IsUnnestFunction`).
pub fn is_unnest_function(name: &str) -> bool {
    name == "unnest" || name == "unlist"
}

/// The function a SQL value keyword stands for (`current_date` → `current_date()`)
/// (DuckDB's `GetSQLValueFunctionName`).
pub fn sql_value_function_name(column_name: &str) -> Option<&'static str> {
    Some(match column_name.to_lowercase().as_str() {
        "current_catalog" => "current_catalog",
        "current_date" => "current_date",
        "current_schema" => "current_schema",
        "current_role" => "current_role",
        "current_time" => "get_current_time",
        "current_timestamp" => "get_current_timestamp",
        "current_user" => "current_user",
        "localtime" => "current_localtime",
        "localtimestamp" => "current_localtimestamp",
        "session_user" => "session_user",
        "user" => "user",
        _ => return None,
    })
}

/// A call with no arguments.
pub fn function_call(name: &str, children: Vec<ParsedExpression>) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Function,
        ParsedExpressionKind::Function(Box::new(FunctionExpression {
            function_name: name.to_owned(),
            schema: String::new(),
            children,
            filter: None,
            order_bys: rustdb_ast::OrderModifier { orders: Vec::new() },
            distinct: false,
            is_operator: false,
            export_state: false,
            catalog: String::new(),
        })),
    )
}

/// An operator expression with the given children.
pub fn operator_expression(
    expression_type: ExpressionType,
    children: Vec<ParsedExpression>,
) -> ParsedExpression {
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Operator(OperatorExpression { children }),
    )
}

/// The behaviour of an expression binder. Default methods are DuckDB's base
/// `ExpressionBinder`; specialised binders override the virtual parts.
pub trait ExpressionBinder: ColumnQualifier {
    /// The binder of the query level this expression binder works in.
    fn binder(&mut self) -> &mut Binder;
    /// The query levels enclosing this one.
    fn outer(&mut self) -> Option<&mut dyn Scope>;
    /// This binder as an enclosing level of subqueries bound inside it.
    fn as_scope(&mut self) -> &mut dyn Scope;
    fn state(&mut self) -> &mut ExpressionBinderState;
    fn state_ref(&self) -> &ExpressionBinderState;

    /// Binds one parsed node (DuckDB's virtual `BindExpression`).
    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        crate::binder::expression::bind_expression_base(self, expr, depth, root_expression)
    }

    /// Resolves a column reference that names a select-list alias (DuckDB's
    /// `TryResolveAliasReference`); `None` if it is not one.
    fn try_resolve_alias_reference(
        &mut self,
        _expr: &mut ParsedExpression,
        _depth: u64,
        _root_expression: bool,
    ) -> Result<Option<BindResult>> {
        Ok(None)
    }

    /// `GROUPING(...)` (DuckDB's `BindGroupingFunction`).
    fn bind_grouping_function(&mut self, _expr: &mut ParsedExpression, _depth: u64) -> Result<BindResult> {
        Ok(BindResult::error("GROUPING function is not supported here"))
    }

    /// An aggregate call (DuckDB's `BindAggregate`); unsupported by default.
    fn bind_aggregate(
        &mut self,
        expr: &mut ParsedExpression,
        _functions: &rustdb_function::AggregateFunctionSet,
        depth: u64,
    ) -> Result<BindResult> {
        let message = self.unsupported_aggregate_message();
        crate::binder::expression::bind_unsupported_expression(self, expr, depth, &message)
    }

    /// `UNNEST(...)` (DuckDB's `BindUnnest`); unsupported by default.
    fn bind_unnest(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        _root_expression: bool,
    ) -> Result<BindResult> {
        let message = self.unsupported_unnest_message();
        crate::binder::expression::bind_unsupported_expression(self, expr, depth, &message)
    }

    /// A window function call; only some binders allow them.
    fn bind_window(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        crate::binder::expression::bind_unsupported_expression(
            self,
            expr,
            depth,
            "window functions are not allowed here",
        )
    }

    fn unsupported_aggregate_message(&self) -> String {
        "Aggregate functions are not supported here".to_owned()
    }

    fn unsupported_unnest_message(&self) -> String {
        "UNNEST not supported here".to_owned()
    }

    /// The SQL value function a bare name stands for (DuckDB's `GetSQLValueFunction`).
    fn sql_value_function(&self, column_name: &str) -> Option<ParsedExpression> {
        sql_value_function_name(column_name).map(|name| function_call(name, Vec::new()))
    }

    /// Binds `expr` in place at `depth`; returns the soft error if it fails
    /// (DuckDB's `ExpressionBinder::Bind(expr, depth, root)`).
    fn bind_in_place(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<Option<BindError>> {
        if is_bound(expr) {
            return Ok(None);
        }
        let query_location = expr.query_location;
        let alias = expr.alias.clone();
        match self.bind_expression(expr, depth, root_expression)? {
            BindResult::Bound(mut bound) => {
                bound.query_location = query_location;
                if !alias.is_empty() {
                    bound.alias.clone_from(&alias);
                }
                expr.alias = alias;
                set_bound(expr, bound);
                Ok(None)
            }
            BindResult::Unbound(error) => Ok(Some(error)),
        }
    }

    /// Binds a child, keeping the first error in `error` (DuckDB's `BindChild`).
    fn bind_child(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        error: &mut Option<BindError>,
    ) -> Result<()> {
        let child_error = self.bind_in_place(expr, depth, false)?;
        if error.is_none() {
            *error = child_error;
        }
        Ok(())
    }

    /// Retries binding `expr` in the expression binders of enclosing queries, one level
    /// further out each time (DuckDB's `BindCorrelatedColumns`).
    fn bind_correlated_columns(
        &mut self,
        expr: &mut ParsedExpression,
        error: BindError,
    ) -> Result<Option<BindError>> {
        let mut bind_error = error;
        let mut depth = 1;
        let mut current = self.outer();
        while let Some(scope) = current {
            if scope.expression_binder().is_none() {
                current = scope.parts().1;
                continue;
            }
            let Some(outer_binder) = scope.expression_binder() else {
                return Err(Error::internal("expression binder disappeared"));
            };
            qualify_column_names(outer_binder.binder_ref(), expr)?;
            match outer_binder.bind_in_place(expr, depth, false)? {
                None => return Ok(None),
                Some(next_error) => bind_error = bind_error.combine(next_error),
            }
            depth += 1;
            current = outer_binder.outer();
        }
        Ok(Some(bind_error))
    }

    /// Binds a whole expression (DuckDB's `ExpressionBinder::Bind(expr, result_type, root)`):
    /// retries failures as correlated references, adds the cast to the target type, and
    /// replaces the `NULL` type by INTEGER outside of contexts that keep it.
    fn bind(&mut self, expr: &mut ParsedExpression, root_expression: bool) -> Result<Expression> {
        if let Some(error) = self.bind_in_place(expr, 0, root_expression)? {
            if let Some(correlated_error) = self.bind_correlated_columns(expr, error.clone())? {
                return Err(error.combine(correlated_error).error);
            }
            let bound = peek_bound(expr).ok_or_else(not_bound)?.clone();
            self.binder().extract_correlated_expressions(&bound);
        }
        let mut result = take_bound(expr)?;
        let target_type = self.state_ref().target_type.clone();
        if let Some(target) = target_type {
            let context = self.binder_ref().context.clone();
            result = add_cast_to_type(&context, result, &target, false)?;
        } else {
            if !self.binder_ref().can_contain_nulls
                && contains_type(&result.return_type, LogicalTypeId::SqlNull)
            {
                let exchanged =
                    exchange_type(&result.return_type, LogicalTypeId::SqlNull, &LogicalType::INTEGER);
                let context = self.binder_ref().context.clone();
                result = add_cast_to_type(&context, result, &exchanged, false)?;
            }
            if result.return_type.id() == LogicalTypeId::Unknown {
                return Err(parameter_not_resolved());
            }
        }
        Ok(result)
    }

    /// Whether any column was bound (DuckDB's `HasBoundColumns`).
    fn has_bound_columns(&self) -> bool {
        !self.state_ref().bound_columns.is_empty()
    }
}

/// DuckDB's `ParsedExpression::IsScalar`: no column references, subqueries, DEFAULT or
/// positional references.
pub fn parsed_is_scalar(expr: &mut ParsedExpression) -> bool {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::Subquery(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::PositionalReference(_) => false,
        ParsedExpressionKind::Parameter(_) => true,
        _ => {
            let mut scalar = true;
            let _ = for_each_child(expr, &mut |child| {
                scalar &= parsed_is_scalar(child);
                Ok(())
            });
            scalar
        }
    }
}

/// DuckDB's `ParsedExpression::HasParameter`.
pub fn parsed_has_parameter(expr: &mut ParsedExpression) -> bool {
    if matches!(expr.kind, ParsedExpressionKind::Parameter(_)) {
        return true;
    }
    let mut found = false;
    let _ = for_each_child(expr, &mut |child| {
        found |= parsed_has_parameter(child);
        Ok(())
    });
    found
}

/// A constant of type `NULL`.
pub fn null_constant() -> Expression {
    Expression::constant(Value::null(LogicalType::SQLNULL))
}

/// Whether a parsed expression is a column reference that could be a select-list alias:
/// unqualified, or qualified with the table name `alias` (DuckDB's `IsPotentialAlias`).
pub fn is_potential_alias(names: &[String]) -> bool {
    match names {
        [_] => true,
        [table, _] => table.eq_ignore_ascii_case("alias"),
        _ => false,
    }
}

/// Whether the expression class is a column reference.
pub const fn is_column_ref(expr: &ParsedExpression) -> bool {
    matches!(expr.kind.expression_class(), ExpressionClass::ColumnRef)
}
