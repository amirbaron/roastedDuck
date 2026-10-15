use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::FunctionClient;

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

/// Per-call state handed to a kernel.
#[derive(Debug, Default)]
pub struct FunctionState {
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// Thread-local state created on first use by the kernel.
    pub local: Option<Box<dyn Any + Send>>,
}

/// A vectorised scalar kernel: computes `result` from the argument columns of `args`.
pub type ScalarKernel = Arc<dyn Fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()> + Send + Sync>;

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
    /// The calling connection's state (DuckDB's `ClientContext`).
    pub client: &'a Arc<FunctionClient>,
    /// The binder's client context (`ScalarFunctionBindInput::binder`), for functions
    /// that look up catalog entries; the database crate downcasts it.
    pub binder: Option<&'a dyn Any>,
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
}

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
        }
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
