use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::{DataChunk, LogicalType, Vector};

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
        }
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
