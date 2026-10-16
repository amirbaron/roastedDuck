//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...) and casts.
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk. Casts are
//! bound the same way through [`cast::CastFunctionSet`].

pub mod aggregate;
pub mod bignum;
pub mod cast;
pub mod client;
pub mod copy;
pub mod extension_entries;
pub mod function_descriptions;
pub mod logging;
pub mod random;
mod scalar;
mod table;

pub use aggregate::{
    AggregateCollation, AggregateFunction, AggregateFunctionSet, AggregateOps, OwnedStates, StatePtr,
    UnaryAggregate,
};
pub use client::{FunctionClient, SettingInfo, SettingsSource};
pub use copy::{CopyFunction, CopyOption, CopyOptionMode};
pub use scalar::{
    AggregateResolver, BindInput, BindLambda, BoundAggregateFunction, CollationHandling, FunctionData,
    FunctionState, LambdaExecutor, LambdaInput, NullHandling, ScalarBind, ScalarFunction, ScalarFunctionSet,
    ScalarKernel, Stability, StatsBindData,
};
pub use table::{
    InOutResult, TableFunction, TableFunctionBind, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionBindReplace, TableFunctionPushdownFilters, TableFunctionReplacement, TableFunctionScan, TableFunctionSet,
    TableFunctionTypePushdown, TableInOutFunction, TableScanState,
};
