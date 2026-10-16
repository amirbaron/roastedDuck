//! Table functions (DuckDB's `TableFunction`): functions in the FROM clause that produce
//! rows, such as `range(10)` or `read_csv(...)`.
//!
//! Binding a call resolves the overload, evaluates the constant arguments and calls the
//! function's bind callback, which decides the output columns. Execution is attached by
//! the function's implementation through [`TableFunction::scan`].

use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::FunctionData;

/// What a table function's bind callback receives (DuckDB's `TableFunctionBindInput`).
#[derive(Clone, Copy, Debug)]
pub struct TableFunctionBindInput<'a> {
    /// The positional arguments, cast to the overload's argument types.
    pub inputs: &'a [Value],
    /// `name := value` arguments, cast to the declared parameter types.
    pub named_parameters: &'a [(String, Value)],
    /// For table-in/table-out functions: the input table's column types and names.
    pub input_table_types: &'a [LogicalType],
    pub input_table_names: &'a [String],
    /// The binder's client context (the planner's `ClientContext`), for functions that
    /// read the catalog.
    pub binder: Option<&'a dyn std::any::Any>,
}

/// The columns a table function call produces, and its bind data.
#[derive(Clone, Debug, Default)]
pub struct TableFunctionBindOutput {
    pub return_types: Vec<LogicalType>,
    pub names: Vec<String>,
    pub bind_data: Option<Arc<dyn FunctionData>>,
}

/// A table function's bind callback (DuckDB's `table_function_bind_t`).
pub type TableFunctionBind =
    Arc<dyn Fn(&TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> + Send + Sync>;

/// What a `bind_replace` callback substitutes for the call: a query the binder parses
/// and binds as a subquery in place of the function (DuckDB returns the parsed
/// `TableRef`; the function library only builds the SQL).
#[derive(Clone, Debug)]
pub struct TableFunctionReplacement {
    /// The SQL of a single SELECT statement.
    pub query: String,
    /// The parser error raised when `query` is not a single SELECT statement.
    pub error: String,
}

/// A table function's `bind_replace` callback: `Some` replaces the call by a subquery.
pub type TableFunctionBindReplace =
    Arc<dyn Fn(&TableFunctionBindInput<'_>) -> Result<Option<TableFunctionReplacement>> + Send + Sync>;

/// Produces the function's rows: called repeatedly with the bind data and the projected
/// column ids, it fills the chunk (all of the function's columns, at most
/// `STANDARD_VECTOR_SIZE` rows) and returns whether more rows may follow (a
/// single-threaded `table_function_t`).
pub type TableFunctionScan = Arc<
    dyn Fn(Option<&dyn FunctionData>, &[u64], &mut TableScanState, &mut DataChunk) -> Result<bool>
        + Send
        + Sync,
>;

/// Makes the function produce other types for some of its columns (DuckDB's
/// `table_function_type_pushdown_t`): the bind data with column index -> type applied.
pub type TableFunctionTypePushdown =
    Arc<dyn Fn(&dyn FunctionData, &[(usize, LogicalType)]) -> Result<Arc<dyn FunctionData>> + Send + Sync>;

/// Per-scan state of a [`TableFunctionScan`] (cursor position and similar).
#[derive(Debug, Default)]
pub struct TableScanState {
    pub position: u64,
    pub local: Option<Box<dyn std::any::Any + Send>>,
}

/// One overload of a table function.
#[derive(Clone)]
pub struct TableFunction {
    pub name: Arc<str>,
    pub arguments: Vec<LogicalType>,
    /// Type of any additional trailing arguments, for variadic functions.
    pub varargs: Option<LogicalType>,
    /// The accepted `name := value` parameters and their types.
    pub named_parameters: Vec<(String, LogicalType)>,
    pub bind: Option<TableFunctionBind>,
    /// Replaces the call by a subquery before `bind` (DuckDB's `bind_replace`).
    pub bind_replace: Option<TableFunctionBindReplace>,
    pub scan: Option<TableFunctionScan>,
    /// Whether the scan can produce only the requested columns.
    pub projection_pushdown: bool,
    /// Whether the scan can apply filters itself.
    pub filter_pushdown: bool,
    /// Whether this is a table-in/table-out function (DuckDB's `in_out_function`):
    /// constant arguments form its input row, and every output column is scanned.
    pub in_out: bool,
    /// Reads columns directly as the types an INSERT needs (`type_pushdown`).
    pub type_pushdown: Option<TableFunctionTypePushdown>,
}

impl TableFunction {
    pub fn new(name: impl Into<Arc<str>>, arguments: Vec<LogicalType>) -> Self {
        Self {
            name: name.into(),
            arguments,
            varargs: None,
            named_parameters: Vec::new(),
            bind: None,
            bind_replace: None,
            scan: None,
            projection_pushdown: false,
            filter_pushdown: false,
            in_out: false,
            type_pushdown: None,
        }
    }

    /// Sets the scan callback.
    #[must_use]
    pub fn with_scan(
        mut self,
        scan: impl Fn(Option<&dyn FunctionData>, &[u64], &mut TableScanState, &mut DataChunk) -> Result<bool>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.scan = Some(Arc::new(scan));
        self
    }

    /// Sets the bind callback.
    #[must_use]
    pub fn with_bind(
        mut self,
        bind: impl Fn(&TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> + Send + Sync + 'static,
    ) -> Self {
        self.bind = Some(Arc::new(bind));
        self
    }
}

impl fmt::Debug for TableFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableFunction")
            .field("name", &self.name)
            .field("arguments", &self.arguments)
            .field("varargs", &self.varargs)
            .field("named_parameters", &self.named_parameters)
            .finish_non_exhaustive()
    }
}

/// All overloads of a table function.
#[derive(Clone, Debug)]
pub struct TableFunctionSet {
    pub name: Arc<str>,
    pub functions: Vec<TableFunction>,
}

impl TableFunctionSet {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
        }
    }

    pub fn add(&mut self, function: TableFunction) {
        self.functions.push(function);
    }
}
