use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_serde::Serializer;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::FunctionClient;
use crate::aggregate::AggregateFunction;

/// How a function treats NULL inputs (DuckDB's `FunctionNullHandling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NullHandling {
    /// Any NULL argument yields NULL; the kernel need not look at NULL rows.
    #[default]
    Default,
    /// The kernel handles NULLs itself (e.g. `coalesce`).
    Special,
}

/// What binding does with VARCHAR collations of the arguments (DuckDB's
/// `FunctionCollationHandling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollationHandling {
    /// Collations are ignored.
    IgnoreCollations,
    /// A VARCHAR result carries the collation of the arguments.
    #[default]
    PropagateCollations,
    /// The combinable collations are applied to the arguments (`like`, `contains`, ...).
    PushCombinableCollations,
}

/// Whether a function's result may change for the same input (DuckDB's `FunctionStability`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stability {
    #[default]
    Consistent,
    Volatile,
    ConsistentWithinQuery,
}

/// Data computed when a function is bound, shared by every execution of that expression.
pub trait FunctionData: Any + Send + Sync + fmt::Debug {
    fn as_any(&self) -> &dyn Any;

    /// Whether `other` is the same bind data (DuckDB's `FunctionData::Equals`); by
    /// default only data that is shared is known to be equal.
    fn equals(&self, other: &dyn FunctionData) -> bool {
        let _ = other;
        false
    }

    /// The database a function call writes to, which its statement then modifies
    /// (DuckDB's `StatementProperties::RegisterDBModify` in a table function's bind or a
    /// scalar function's `get_modified_databases`).
    fn modified_database(&self) -> Option<rustdb_common::ByteString> {
        None
    }
}

/// The bind data of DECIMAL `+`, `-`, `*` and `%` (DuckDB's `DecimalArithmeticBindData`):
/// whether the kernel checks for overflow. Statistics propagation clears it when the
/// operands' ranges rule an overflow out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalArithmeticBindData {
    pub check_overflow: bool,
}

impl FunctionData for DecimalArithmeticBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// The bind data of `list_transform`, `list_filter` and `list_reduce` (DuckDB's
/// `ListLambdaBindData`). The lambda body stays on the bound function expression (its
/// `lambda`), which writes it with this data in serialized plans.
#[derive(Debug, Clone)]
pub struct ListLambdaBindData {
    pub return_type: LogicalType,
    /// Whether the call keeps its lambda (DuckDB's `lambda_expr`, null for a NULL list).
    pub has_lambda: bool,
    /// Whether the last lambda parameter is the element's index.
    pub has_index: bool,
    /// Whether `list_reduce` has an initial value.
    pub has_initial: bool,
}

impl FunctionData for ListLambdaBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The bind data of `stats(x)` (DuckDB's `StatsBindData`): the statistics of `x` as
/// text, filled in by the optimizer's statistics propagation.
#[derive(Debug, Default)]
pub struct StatsBindData {
    pub stats: std::sync::Mutex<String>,
}

impl FunctionData for StatsBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Evaluates the bound lambda of a lambda function call (DuckDB's `ExpressionExecutor` over
/// the lambda expression of `ListLambdaBindData`), supplied by the executor.
pub trait LambdaExecutor: Send + fmt::Debug {
    /// Evaluates the lambda body for every row of `input`, whose columns are the lambda
    /// parameters (in DuckDB's order: index first, then the element(s)) followed by the
    /// captured columns.
    fn execute(&mut self, input: &DataChunk) -> Result<Vector>;

    /// Whether the lambda body is volatile (then a constant input does not give a
    /// constant result).
    fn is_volatile(&self) -> bool;
}

/// Per-call state handed to a kernel.
#[derive(Debug, Default)]
pub struct FunctionState {
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// The alias of the call (DuckDB's `state.expr.GetAlias()`), empty if none.
    pub alias: rustdb_common::ByteString,
    /// Thread-local state created on first use by the kernel.
    pub local: Option<Box<dyn Any + Send>>,
    /// The lambda of a lambda function call (`list_transform(l, x -> ...)`).
    pub lambda: Option<Box<dyn LambdaExecutor>>,
}

/// A vectorised scalar kernel: computes `result` from the argument columns of `args`.
pub type ScalarKernel = Arc<dyn Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync>;

/// A builtin aggregate resolved and bound for a scalar function that runs it internally
/// (such as `list_aggregate`).
#[derive(Clone)]
pub struct BoundAggregateFunction {
    /// The chosen overload after its bind callback.
    pub function: AggregateFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// The number of arguments left after the bind (it may consume constant ones).
    pub argument_count: usize,
    /// The bound call as SQL text, e.g. `arg_max(NULL, 1)`.
    pub display: String,
    /// Writes the bound call as the binder's aggregate expression (a nullable value), for
    /// the bind data of a serialized plan.
    pub serialize: ExpressionWriter,
}

/// Writes an expression the binder made (see [`BoundAggregateFunction::serialize`]).
pub type ExpressionWriter = Arc<dyn Fn(&mut dyn Serializer) -> Result<()> + Send + Sync>;

impl fmt::Debug for BoundAggregateFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundAggregateFunction")
            .field("function", &self.function)
            .field("argument_count", &self.argument_count)
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

/// Resolves a builtin aggregate by name and binds it (DuckDB's lookup in the system
/// catalog, `FunctionBinder::BindFunction` and `FunctionBinder::BindAggregateFunction`).
pub trait AggregateResolver {
    /// Binds aggregate `name` for arguments of the given types; constant arguments carry
    /// their value, the others are bound as NULL constants of their type.
    fn bind_aggregate(
        &self,
        name: &str,
        arguments: &[(LogicalType, Option<Value>)],
    ) -> Result<BoundAggregateFunction>;
}

impl fmt::Debug for dyn AggregateResolver + '_ {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AggregateResolver")
    }
}

/// The lambda argument of a lambda function call, as its bind callback sees it.
#[derive(Clone, Debug)]
pub struct LambdaInput {
    /// The number of lambda parameters (`x -> ...` has 1, `(x, i) -> ...` has 2).
    pub parameter_count: usize,
    /// The type of the lambda body.
    pub return_type: LogicalType,
}

/// The type of lambda parameter `parameter` given the call's argument types (DuckDB's
/// `bind_lambda_function_t`; the lambda argument has type `LAMBDA`).
pub type BindLambda = fn(&[LogicalType], usize) -> Result<LogicalType>;

/// What a bind callback learns about a call (DuckDB's `ScalarFunctionBindInput` plus the
/// bound argument expressions, reduced to what functions actually inspect).
#[derive(Clone, Copy, Debug)]
pub struct BindInput<'a> {
    /// Types of the bound argument expressions, before casting to the overload.
    pub argument_types: &'a [LogicalType],
    /// The value of each argument that is a foldable constant, `None` otherwise.
    pub constants: &'a [Option<Value>],
    /// The alias of each argument expression (`a := x` names, or the column name of a
    /// column reference).
    pub aliases: &'a [rustdb_common::ByteString],
    /// The name of each argument expression (DuckDB's `Expression::GetName`): its alias, or
    /// its SQL text when it has none.
    pub names: &'a [rustdb_common::ByteString],
    /// The calling connection's state (DuckDB's `ClientContext`).
    pub client: &'a Arc<FunctionClient>,
    /// The binder's client context (`ScalarFunctionBindInput::binder`), for functions
    /// that look up catalog entries; the database crate downcasts it.
    pub binder: Option<&'a dyn Any>,
    /// The bound argument expressions (the planner's `Vec<Expression>`, the `arguments`
    /// of DuckDB's `bind_scalar_function_t`), for functions that inspect them; the C API
    /// downcasts them.
    pub arguments: Option<&'a dyn Any>,
    /// Aggregate resolution for functions that run an aggregate (`None` outside a binder).
    pub aggregates: Option<&'a dyn AggregateResolver>,
    /// The lambda argument, for lambda functions.
    pub lambda: Option<&'a LambdaInput>,
}

/// Specialises a chosen overload for a call (DuckDB's `bind_scalar_function_t`).
///
/// The callback may rewrite the function's `arguments` (the binder then casts the call's
/// arguments to them) and `return_type`, and may return data shared by every execution.
pub type ScalarBind =
    Arc<dyn Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync>;

/// One overload of a scalar function.
#[derive(Clone)]
pub struct ScalarFunction {
    pub name: rustdb_common::ByteString,
    pub arguments: Vec<LogicalType>,
    /// Type of any additional trailing arguments, for variadic functions.
    pub varargs: Option<LogicalType>,
    pub return_type: LogicalType,
    pub kernel: ScalarKernel,
    pub null_handling: NullHandling,
    pub stability: Stability,
    /// Called once per call site after overload resolution.
    pub bind: Option<ScalarBind>,
    /// The lambda parameter types, for functions taking a lambda (DuckDB's
    /// `bind_lambda_function`).
    pub bind_lambda: Option<BindLambda>,
    /// Set by the bind of a lambda function: the type the lambda body is cast to (DuckDB
    /// wraps the lambda expression in a cast, e.g. to BOOLEAN for `list_filter`).
    pub lambda_cast: Option<LogicalType>,
    /// How argument collations are handled.
    pub collation_handling: CollationHandling,
    /// Replaces the bound call by a constant computed from its bind data and argument
    /// types (DuckDB's `bind_expression` callback, for functions such as `getvariable` or
    /// `typeof` that resolve completely while binding).
    pub bind_expression: Option<BindExpression>,
    /// Replaces the bound call by another expression (DuckDB's `bind_expression` callback
    /// returning an expression, e.g. `switch` becoming a CASE). Planner expressions are not
    /// known here: the planner downcasts this to its `RewriteExpression`.
    pub rewrite_expression: Option<Arc<dyn Any + Send + Sync>>,
    /// The kernel statistics propagation switches to when the first argument cannot
    /// contain unicode (the `SetFunctionCallback` of DuckDB's `ILikePropagateStats`).
    pub ascii_kernel: Option<ScalarKernel>,
    /// The arguments before the bind removed some (DuckDB's `original_arguments`, set by
    /// `Function::EraseArgument`); empty when it removed none.
    pub original_arguments: Vec<LogicalType>,
    /// The positions `erase_argument` removed while binding, in order: the binder drops
    /// the call's arguments there too.
    pub erased_arguments: Vec<usize>,
    /// Casts the bind adds to the call's arguments, by position and in order, keeping
    /// the declared argument types (DuckDB's binds wrap `arguments[i]` in a cast, e.g.
    /// `AddArrayCastToList`).
    pub argument_casts: Vec<(usize, LogicalType)>,
    /// The catalog and schema of the function's catalog entry (DuckDB's `catalog_name`
    /// and `schema_name`, set by the function catalog entries); empty for a function not
    /// taken from a catalog.
    pub catalog_name: Arc<[u8]>,
    pub schema_name: Arc<[u8]>,
    /// Writes the bind data, so that a serialized plan can bind the function again
    /// (DuckDB's `serialize` callback).
    pub serialize: Option<ScalarSerialize>,
}

/// See [`ScalarFunction::serialize`] (DuckDB's `scalar_serialize_t`).
pub type ScalarSerialize =
    Arc<dyn Fn(&mut dyn Serializer, Option<&dyn FunctionData>, &ScalarFunction) -> Result<()> + Send + Sync>;

/// See [`ScalarFunction::bind_expression`].
pub type BindExpression =
    Arc<dyn Fn(Option<&dyn FunctionData>, &[LogicalType]) -> Result<Option<Value>> + Send + Sync>;

impl ScalarFunction {
    pub fn new(
        name: impl Into<rustdb_common::ByteString>,
        arguments: Vec<LogicalType>,
        return_type: LogicalType,
        kernel: impl Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            arguments,
            varargs: None,
            return_type,
            kernel: Arc::new(kernel),
            null_handling: NullHandling::Default,
            stability: Stability::Consistent,
            bind: None,
            bind_lambda: None,
            lambda_cast: None,
            collation_handling: CollationHandling::PropagateCollations,
            bind_expression: None,
            rewrite_expression: None,
            ascii_kernel: None,
            original_arguments: Vec::new(),
            erased_arguments: Vec::new(),
            argument_casts: Vec::new(),
            catalog_name: Arc::from(b"".as_slice()),
            schema_name: Arc::from(b"".as_slice()),
            serialize: None,
        }
    }

    /// Sets the catalog and schema of the function's catalog entry (see
    /// [`ScalarFunction::catalog_name`]); empty ones for a function the engine uses
    /// directly rather than through the catalog (DuckDB's `XFun::GetFunction()`).
    pub fn set_catalog_entry(&mut self, catalog: &Arc<[u8]>, schema: &Arc<[u8]>) {
        self.catalog_name = Arc::clone(catalog);
        self.schema_name = Arc::clone(schema);
    }

    /// Forgets the catalog entry: the bind made a new function object (DuckDB's
    /// `function = GetX(...)` binds and `XFun::GetFunction()` callers).
    pub fn clear_catalog_entry(&mut self) {
        let empty: Arc<[u8]> = Arc::from(b"".as_slice());
        self.set_catalog_entry(&empty, &empty);
    }

    /// Casts argument `index` of the call to `target` (see
    /// [`ScalarFunction::argument_casts`]).
    pub fn cast_argument(&mut self, index: usize, target: LogicalType) {
        self.argument_casts.push((index, target));
    }

    /// Removes argument `index`, which the bind consumed, keeping the arguments as they
    /// were before the first removal (DuckDB's `Function::EraseArgument`).
    pub fn erase_argument(&mut self, index: usize) {
        if self.original_arguments.is_empty() {
            self.original_arguments.clone_from(&self.arguments);
        }
        self.arguments.remove(index);
        self.erased_arguments.push(index);
    }

    /// Sets the kernel for arguments without unicode (see [`ScalarFunction::ascii_kernel`]).
    #[must_use]
    pub fn with_ascii_kernel(
        mut self,
        kernel: impl Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.ascii_kernel = Some(Arc::new(kernel));
        self
    }

    /// Sets the bind-expression callback (see [`ScalarFunction::bind_expression`]).
    #[must_use]
    pub fn with_bind_expression(
        mut self,
        bind_expression: impl Fn(Option<&dyn FunctionData>, &[LogicalType]) -> Option<Value>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.bind_expression = Some(Arc::new(move |data, types| Ok(bind_expression(data, types))));
        self
    }

    /// Sets a bind-time expression callback that can raise a native binding error.
    #[must_use]
    pub fn with_fallible_bind_expression(
        mut self,
        bind_expression: impl Fn(Option<&dyn FunctionData>, &[LogicalType]) -> Result<Option<Value>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.bind_expression = Some(Arc::new(bind_expression));
        self
    }

    /// Sets the bind data writer (see [`ScalarFunction::serialize`]).
    #[must_use]
    pub fn with_serialize(
        mut self,
        serialize: impl Fn(&mut dyn Serializer, Option<&dyn FunctionData>, &Self) -> Result<()>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.serialize = Some(Arc::new(serialize));
        self
    }

    /// Sets the bind callback.
    #[must_use]
    pub fn with_bind(
        mut self,
        bind: impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.bind = Some(Arc::new(bind));
        self
    }
}

impl fmt::Debug for ScalarFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScalarFunction")
            .field("name", &self.name)
            .field("arguments", &self.arguments)
            .field("varargs", &self.varargs)
            .field("return_type", &self.return_type)
            .field("null_handling", &self.null_handling)
            .field("stability", &self.stability)
            .field("has_bind", &self.bind.is_some())
            .finish_non_exhaustive()
    }
}

/// All overloads registered under one name.
#[derive(Clone, Debug)]
pub struct ScalarFunctionSet {
    pub name: rustdb_common::ByteString,
    pub functions: Vec<ScalarFunction>,
}

impl ScalarFunctionSet {
    pub fn new(name: impl Into<rustdb_common::ByteString>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
        }
    }

    pub fn add(&mut self, function: ScalarFunction) {
        self.functions.push(function);
    }

    /// Adds the overloads of `new_functions` (DuckDB's `FunctionSet::MergeFunctionSet`): an
    /// overload with the same arguments, return type and varargs (`ScalarFunction::Equal`)
    /// replaces the existing one in place if `override_existing`, otherwise the merge fails,
    /// leaving the set unchanged, and `false` is returned; the others are appended.
    pub fn merge_function_set(&mut self, new_functions: Self, override_existing: bool) -> bool {
        let same = |existing: &ScalarFunction, function: &ScalarFunction| {
            existing.arguments == function.arguments
                && existing.return_type == function.return_type
                && existing.varargs == function.varargs
        };
        if !override_existing
            && new_functions
                .functions
                .iter()
                .any(|function| self.functions.iter().any(|existing| same(existing, function)))
        {
            return false;
        }
        for function in new_functions.functions {
            match self
                .functions
                .iter_mut()
                .find(|existing| same(existing, &function))
            {
                Some(existing) => *existing = function,
                None => self.functions.push(function),
            }
        }
        true
    }
}
