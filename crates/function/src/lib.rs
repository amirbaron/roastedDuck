//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...) and casts.
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk. Casts are
//! bound the same way through [`cast::CastFunctionSet`].

pub mod aggregate;
pub mod bignum;
pub mod cast;
pub mod client;
pub use client::ObjectCache;
pub mod copy;
pub mod extension_entries;
pub mod file_system;
pub mod function_descriptions;
pub mod logging;
pub mod random;
mod scalar;
mod table;

pub use aggregate::{
    AggregateCollation, AggregateFunction, AggregateFunctionSet, AggregateOps, OwnedStates, StatePtr,
    UnaryAggregate,
};
pub use client::{ClientRequest, ExtensionOption, ExtensionOptionSet, FunctionClient, SettingInfo, SettingsSource};
pub use copy::{CopyFunction, CopyOption, CopyOptionMode};
pub use scalar::{
    AggregateResolver, BindInput, BindLambda, BoundAggregateFunction, CollationHandling, FunctionData,
    FunctionState, LambdaExecutor, LambdaInput, NullHandling, ScalarBind, ScalarFunction, ScalarFunctionSet,
    ScalarKernel, Stability, StatsBindData,
};
pub use table::{
    FileFilterPushdown, InOutResult, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan,
    PartitionRowGroup, PartitionStatistics, TableFunction, TableFunctionBind, TableFunctionBindInput,
    TableFunctionBindOutput, TableFunctionBindReplace, TableFunctionPartitionStats,
    TableFunctionPushdownFilters, TableFunctionReplacement, TableFunctionScan, TableFunctionSet,
    TableFunctionStatistics, TableFunctionTypePushdown, TableFunctionVirtualColumns, TableInOutFunction,
    TableScanState,
};
