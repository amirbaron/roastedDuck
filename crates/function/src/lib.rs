//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...).
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk.

pub mod aggregate;
mod scalar;

pub use aggregate::{AggregateFunction, AggregateFunctionSet, AggregateOps, StatePtr, UnaryAggregate};
pub use scalar::{
    FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet, ScalarKernel, Stability,
};
