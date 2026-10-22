//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...) and casts.
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk. Casts are
//! bound the same way through [`cast::CastFunctionSet`].

pub mod aggregate;
pub mod bignum;
pub mod caching_file_system;
pub mod cast;
pub mod client;
pub use client::ObjectCache;
pub mod copy;
pub mod extension_entries;
pub mod external_file_cache;
pub mod file_system;
pub mod function_descriptions;
pub mod logging;
pub mod pragma;
pub mod random;
mod scalar;
mod table;

pub use aggregate::{
    AggregateCollation, AggregateFunction, AggregateFunctionSet, AggregateOps, AggregateSerialize,
    OwnedStates, StatePtr, UnaryAggregate,
};
pub use client::{
    ClientRequest, ExtensionOption, ExtensionOptionSet, FunctionClient, SettingInfo, SettingScope,
    SettingsSource, TempCatalog,
};
pub use copy::{CopyFunction, CopyOption, CopyOptionMode};
pub use pragma::{PragmaFunction, PragmaFunctionSet, PragmaType};
pub use scalar::{
    AggregateResolver, BindInput, BindLambda, BoundAggregateFunction, CollationHandling,
    DecimalArithmeticBindData, ExpressionWriter, FunctionData, FunctionState, LambdaExecutor, LambdaInput,
    NullHandling, ScalarBind, ScalarFunction, ScalarFunctionSet, ScalarKernel, ScalarSerialize, Stability,
    StatsBindData,
};
pub use table::{
    FileFilterPushdown, InOutResult, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan,
    PartitionRowGroup, PartitionStatistics, TableFunction, TableFunctionBind, TableFunctionBindInput,
    TableFunctionBindOutput, TableFunctionBindReplace, TableFunctionPartitionInfo,
    TableFunctionPartitionStats, TableFunctionPushdownFilters, TableFunctionReplacement, TableFunctionScan,
    TableFunctionSerialize, TableFunctionSet, TableFunctionStatistics, TableFunctionTypePushdown,
    TableFunctionVirtualColumns, TableInOutFunction, TablePartitionInfo, TableScanState,
    named_parameters_in_map_order,
};
