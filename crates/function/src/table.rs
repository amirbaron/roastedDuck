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

/// The estimated row count of a call from its bind data (DuckDB's
/// `table_function_cardinality_t`, whose estimate and maximum are the same here).
pub type TableFunctionCardinality = Arc<dyn Fn(Option<&dyn FunctionData>) -> Option<u64> + Send + Sync>;

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

/// Applies pushed-down table filters (`rustdb_storage::TableFilterSet`, keyed by the
/// position of the filtered column in the scan's column ids, passed as `Any`) to a table
/// function's bind data: the scan of the returned bind data only produces rows passing
/// the filters, so the planner drops the filters themselves.
pub type TableFunctionPushdownFilters =
    Arc<dyn Fn(&dyn FunctionData, &[u64], &dyn std::any::Any) -> Result<Arc<dyn FunctionData>> + Send + Sync>;

/// The virtual columns of a call (DuckDB's `table_function_get_virtual_columns_t`):
/// `(column id, name, type)` for columns such as `filename` that can be selected by name
/// without being part of the function's columns.
pub type TableFunctionVirtualColumns =
    Arc<dyn Fn(Option<&dyn FunctionData>) -> Vec<(u64, String, LogicalType)> + Send + Sync>;

/// The statistics of column `column` of a call (DuckDB's `table_statistics_t`):
/// `rustdb_storage::statistics::BaseStatistics`, boxed as `Any`.
pub type TableFunctionStatistics =
    Arc<dyn Fn(Option<&dyn FunctionData>, u64) -> Option<Box<dyn std::any::Any + Send>> + Send + Sync>;

/// The statistics of one partition (row group) of a table function's data, for
/// statistics-based query answering (DuckDB's `PartitionRowGroup`).
pub trait PartitionRowGroup: Send + Sync + fmt::Debug {
    /// The statistics of column `column` (`rustdb_storage::statistics::BaseStatistics`,
    /// boxed as `Any`), if known.
    fn column_statistics(&self, column: u64) -> Option<Box<dyn std::any::Any + Send>>;
    /// Whether the min/max of these statistics are values of the column, not bounds.
    fn min_max_is_exact(&self, column: u64) -> bool;
}

/// A partition of a table function's data (DuckDB's `PartitionStatistics`).
#[derive(Clone, Debug)]
pub struct PartitionStatistics {
    pub count: u64,
    /// Whether `count` is exact.
    pub exact: bool,
    pub row_group: Option<Arc<dyn PartitionRowGroup>>,
}

/// The partitions of a call (DuckDB's `table_function_get_partition_stats_t`); empty when
/// unknown.
pub type TableFunctionPartitionStats =
    Arc<dyn Fn(Option<&dyn FunctionData>) -> Vec<PartitionStatistics> + Send + Sync>;

/// How a call's data is partitioned on a set of columns (DuckDB's `TablePartitionInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TablePartitionInfo {
    /// The data is not partitioned by the given set of columns.
    NotPartitioned,
    /// Each partition has exactly one unique value of the columns (e.g. bounds
    /// `[1,1][2,2][3,3]`).
    SingleValuePartitions,
}

/// How a call's data is partitioned on its columns `partition_ids` (DuckDB's
/// `table_function_get_partition_info_t`).
pub type TableFunctionPartitionInfo =
    Arc<dyn Fn(Option<&dyn FunctionData>, &[u64]) -> TablePartitionInfo + Send + Sync>;

/// Filter pushdown into the file list of a multi-file scan whose paths determine column
/// values: hive partitions and the `filename` column (DuckDB's `pushdown_complex_filter`
/// of `MultiFileFunction`, `MultiFileList::ComplexFilterPushdown`).
#[derive(Clone)]
#[allow(clippy::type_complexity, reason = "callback signatures")]
pub struct FileFilterPushdown {
    /// The scanned files, if their paths determine any column (`hive_partitioning` or
    /// `filename` is set).
    pub files: Arc<dyn Fn(&dyn FunctionData) -> Option<Vec<String>> + Send + Sync>,
    /// The value of the column named `name` (of type `ty`) in the file at `path`, if the
    /// path determines it (`GetKnownColumnValues` + `HivePartitioning::GetValue`).
    pub known_value: Arc<
        dyn Fn(&dyn FunctionData, &str, &str, &LogicalType) -> Result<Option<rustdb_types::Value>>
            + Send
            + Sync,
    >,
    /// The bind data scanning only the files at positions `keep` of `files`.
    pub with_files: Arc<dyn Fn(&dyn FunctionData, &[usize]) -> Result<Arc<dyn FunctionData>> + Send + Sync>,
}

impl fmt::Debug for FileFilterPushdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileFilterPushdown").finish_non_exhaustive()
    }
}

/// The (virtual) columns that identify a row of a call (`table_function_get_row_id_columns`).
pub type TableFunctionRowIdColumns = Arc<dyn Fn(Option<&dyn FunctionData>) -> Vec<u64> + Send + Sync>;

/// The shared state of a parallel table function scan.
pub type ParallelScanGlobal = Arc<dyn std::any::Any + Send + Sync>;
/// The state of one thread of a parallel table function scan.
pub type ParallelScanLocal = Box<dyn std::any::Any + Send>;

/// A table function that several threads scan at once, each with its own local state
/// (DuckDB's `init_global` / `init_local` and a `function` that runs on many threads).
#[derive(Clone)]
#[allow(clippy::type_complexity, reason = "callback signatures")]
pub struct ParallelTableScan {
    /// The shared state of a scan of `column_ids`.
    pub init_global:
        Arc<dyn Fn(Option<&dyn FunctionData>, &[u64]) -> Result<ParallelScanGlobal> + Send + Sync>,
    /// The state of a new thread.
    pub init_local: Arc<dyn Fn(&ParallelScanGlobal) -> Result<ParallelScanLocal> + Send + Sync>,
    /// Fills the next chunk of this thread; `false` once the thread has no more work.
    pub scan: Arc<
        dyn Fn(
                Option<&dyn FunctionData>,
                &[u64],
                &ParallelScanGlobal,
                &mut ParallelScanLocal,
                &mut DataChunk,
            ) -> Result<bool>
            + Send
            + Sync,
    >,
    /// How many threads can usefully scan (`MaxThreads`).
    pub max_threads: Arc<dyn Fn(&ParallelScanGlobal) -> usize + Send + Sync>,
    /// The batch index of the chunk the thread produced last, increasing in the
    /// function's row order (`GetPartitionData`).
    pub batch_index: Arc<dyn Fn(&ParallelScanGlobal, &ParallelScanLocal) -> u64 + Send + Sync>,
    /// The values of the scanned columns at positions `partition_columns` of the column
    /// ids in the chunk the thread produced last, on which the function reports single
    /// value partitions (the partition data of `GetPartitionData`).
    pub partition_data: Option<
        Arc<dyn Fn(&ParallelScanGlobal, &ParallelScanLocal, &[u64]) -> Result<Vec<Value>> + Send + Sync>,
    >,
    /// What `EXPLAIN ANALYZE` shows about the finished scan (`dynamic_to_string`).
    pub dynamic_to_string: Option<
        Arc<dyn Fn(Option<&dyn FunctionData>, &ParallelScanGlobal) -> Vec<(String, String)> + Send + Sync>,
    >,
    /// The row groups the thread read and the row groups handed out to all threads
    /// (`get_metrics`: `OPERATOR_ROW_GROUPS_SCANNED`, `OPERATOR_TOTAL_ROW_GROUPS_TO_SCAN`).
    pub row_group_metrics:
        Option<Arc<dyn Fn(&ParallelScanGlobal, &ParallelScanLocal) -> (u64, u64) + Send + Sync>>,
}

impl fmt::Debug for ParallelTableScan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParallelTableScan").finish_non_exhaustive()
    }
}

/// What a [`TableInOutFunction`] call did with its input (DuckDB's `OperatorResultType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InOutResult {
    /// The input chunk is consumed: call again with the next one.
    NeedMoreInput,
    /// Call again with the same input for more output.
    HaveMoreOutput,
    /// No more output, whatever the input.
    Finished,
}

/// A table-in/table-out function (DuckDB's `table_in_out_function_t`): turns the rows of
/// `input` (one column per argument) into rows of the function's columns, at most
/// `STANDARD_VECTOR_SIZE` per call, keeping its progress in the state.
pub type TableInOutFunction = Arc<
    dyn Fn(Option<&dyn FunctionData>, &mut TableScanState, &DataChunk, &mut DataChunk) -> Result<InOutResult>
        + Send
        + Sync,
>;

/// Per-scan state of a [`TableFunctionScan`] (cursor position and similar).
#[derive(Debug, Default)]
pub struct TableScanState {
    pub position: u64,
    pub local: Option<Box<dyn std::any::Any + Send>>,
    /// The columns an in-out function produces, virtual ones included
    /// (`TableFunctionInitInput::column_ids`); empty for all of its columns.
    pub column_ids: Vec<u64>,
}

/// Named parameters in the iteration order of DuckDB's `named_parameter_type_map_t` (a
/// `case_insensitive_map_t` filled in declaration order), which `Function::CallToString`
/// and `duckdb_functions()` list them in.
pub fn named_parameters_in_map_order(
    named_parameters: &[(String, LogicalType)],
) -> impl Iterator<Item = &(String, LogicalType)> {
    let mut map = rustdb_common::libcxx::UnorderedOrder::default();
    for (index, (name, _)) in named_parameters.iter().enumerate() {
        map.insert(rustdb_common::libcxx::ci_hash(name), index, |a, b| {
            named_parameters[*a]
                .0
                .eq_ignore_ascii_case(&named_parameters[*b].0)
        });
    }
    let indices: Vec<usize> = map.order().into_iter().map(|node| map.keys()[node]).collect();
    indices.into_iter().map(|index| &named_parameters[index])
}

/// One overload of a table function.
#[derive(Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "DuckDB's TableFunction capability flags"
)]
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
    /// Whether the scan can leave out columns read only for its filters (DuckDB's
    /// `filter_prune`).
    pub filter_prune: bool,
    /// Whether this is a table-in/table-out function (DuckDB's `in_out_function`):
    /// constant arguments form its input row, and every output column is scanned.
    pub in_out: bool,
    /// Reads columns directly as the types an INSERT needs (`type_pushdown`).
    pub type_pushdown: Option<TableFunctionTypePushdown>,
    /// The table-in/table-out form (DuckDB's `in_out_function`): used when the arguments
    /// are not constants (e.g. LATERAL column references); they are then bound as a
    /// subquery whose rows are the function's input, and `bind` sees their types in
    /// `input_table_types` instead of `inputs`.
    pub in_out_function: Option<TableInOutFunction>,
    /// The estimated row count of a call (`cardinality`).
    pub cardinality: Option<TableFunctionCardinality>,
    /// Whether the function has DuckDB's empty virtual column (`COLUMN_IDENTIFIER_EMPTY`,
    /// which the multi-file readers provide): a scan needing no column reads it.
    pub empty_column: bool,
    /// Hands the filters pushed into the scan to the function (DuckDB's table filters of
    /// a `filter_pushdown` function): returns the bind data that applies them.
    pub pushdown_filters: Option<TableFunctionPushdownFilters>,
    /// The function's virtual columns (`get_virtual_columns`).
    pub get_virtual_columns: Option<TableFunctionVirtualColumns>,
    /// Column statistics (`statistics`).
    pub statistics: Option<TableFunctionStatistics>,
    /// Partition statistics (`get_partition_stats`).
    pub get_partition_stats: Option<TableFunctionPartitionStats>,
    /// How the data is partitioned on some of its columns (`get_partition_info`).
    pub get_partition_info: Option<TableFunctionPartitionInfo>,
    /// A scan by several threads at once; used instead of `scan` when set.
    pub parallel_scan: Option<ParallelTableScan>,
    /// Filter pushdown into the scanned files (`pushdown_complex_filter`).
    pub file_filter_pushdown: Option<FileFilterPushdown>,
    /// The (virtual) columns that identify a row, for late materialization
    /// (`late_materialization` + `get_row_id_columns`).
    pub get_row_id_columns: Option<TableFunctionRowIdColumns>,
    /// Whether the scan fills one output column per scanned column id, in order
    /// (DuckDB's scan contract), instead of all of the function's columns.
    pub projected_output: bool,
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
            filter_prune: false,
            in_out: false,
            type_pushdown: None,
            in_out_function: None,
            cardinality: None,
            empty_column: false,
            pushdown_filters: None,
            get_virtual_columns: None,
            statistics: None,
            get_partition_stats: None,
            get_partition_info: None,
            parallel_scan: None,
            file_filter_pushdown: None,
            get_row_id_columns: None,
            projected_output: false,
        }
    }

    /// Sets the table-in/table-out callback.
    #[must_use]
    pub fn with_in_out(
        mut self,
        function: impl Fn(
            Option<&dyn FunctionData>,
            &mut TableScanState,
            &DataChunk,
            &mut DataChunk,
        ) -> Result<InOutResult>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.in_out_function = Some(Arc::new(function));
        self
    }

    /// Sets the cardinality callback.
    #[must_use]
    pub fn with_cardinality(
        mut self,
        cardinality: impl Fn(Option<&dyn FunctionData>) -> Option<u64> + Send + Sync + 'static,
    ) -> Self {
        self.cardinality = Some(Arc::new(cardinality));
        self
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

    /// Adds the overloads of `new_functions` (DuckDB's `FunctionSet::MergeFunctionSet`
    /// without override): fails, leaving the set unchanged and returning `false`, if one
    /// has the same arguments and varargs as an existing overload (`TableFunction::Equal`).
    pub fn merge_function_set(&mut self, new_functions: Self) -> bool {
        if new_functions.functions.iter().any(|function| {
            self.functions.iter().any(|existing| {
                existing.arguments == function.arguments && existing.varargs == function.varargs
            })
        }) {
            return false;
        }
        self.functions.extend(new_functions.functions);
        true
    }
}
