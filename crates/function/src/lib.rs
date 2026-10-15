//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...) and casts.
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk. Casts are
//! bound the same way through [`cast::CastFunctionSet`].

pub mod aggregate;
pub mod cast;
pub mod client;
pub mod random;
mod scalar;
mod table;

pub use aggregate::{
    AggregateFunction, AggregateFunctionSet, AggregateOps, OwnedStates, StatePtr, UnaryAggregate,
};
pub use client::FunctionClient;
pub use scalar::{
    AggregateResolver, BindInput, BoundAggregateFunction, FunctionData, FunctionState, NullHandling,
    ScalarBind, ScalarFunction, ScalarFunctionSet, ScalarKernel, Stability,
};
pub use table::{
    TableFunction, TableFunctionBind, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionScan,
    TableFunctionSet, TableScanState,
};
