use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;
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
    /// Thread-local state created on first use by the kernel.
    pub local: Option<Box<dyn Any + Send>>,
    /// The lambda of a lambda function call (`list_transform(l, x -> ...)`).
    pub lambda: Option<Box<dyn LambdaExecutor>>,
}

/// A vectorised scalar kernel: computes `result` from the argument columns of `args`.
pub type ScalarKernel = Arc<dyn Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync>;

/// A builtin aggregate resolved and bound for a scalar function that runs it internally
/// (such as `list_aggregate`).
#[derive(Clone, Debug)]
pub struct BoundAggregateFunction {
    /// The chosen overload after its bind callback.
    pub function: AggregateFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// The number of arguments left after the bind (it may consume constant ones).
    pub argument_count: usize,
    /// The bound call as SQL text, e.g. `arg_max(NULL, 1)`.
    pub display: String,
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
    pub aliases: &'a [String],
    /// The name of each argument expression (DuckDB's `Expression::GetName`): its alias, or
    /// its SQL text when it has none.
    pub names: &'a [String],
    /// The calling connection's state (DuckDB's `ClientContext`).
    pub client: &'a Arc<FunctionClient>,
    /// The binder's client context (`ScalarFunctionBindInput::binder`), for functions
    /// that look up catalog entries; the database crate downcasts it.
    pub binder: Option<&'a dyn Any>,
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
    pub name: Arc<str>,
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
    /// Replaces the bound call by a constant computed from its bind data (DuckDB's
    /// `bind_expression` callback, for functions such as `getvariable` that resolve
    /// completely while binding).
    pub bind_expression: Option<BindExpression>,
}

/// See [`ScalarFunction::bind_expression`].
pub type BindExpression = Arc<dyn Fn(Option<&dyn FunctionData>) -> Option<Value> + Send + Sync>;

impl ScalarFunction {
    pub fn new(
        name: impl Into<Arc<str>>,
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
            bind_expression: None,
        }
    }

    /// Sets the bind-expression callback (see [`ScalarFunction::bind_expression`]).
    #[must_use]
    pub fn with_bind_expression(
        mut self,
        bind_expression: impl Fn(Option<&dyn FunctionData>) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        self.bind_expression = Some(Arc::new(bind_expression));
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
    pub name: Arc<str>,
    pub functions: Vec<ScalarFunction>,
}

impl ScalarFunctionSet {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
        }
    }

    pub fn add(&mut self, function: ScalarFunction) {
        self.functions.push(function);
    }
}
