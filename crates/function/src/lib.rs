//! Function definitions (DuckDB's `ScalarFunction`, `FunctionSet`, ...) and casts.
//!
//! A function couples a signature with a vectorised kernel. The binder picks an overload
//! from a [`ScalarFunctionSet`]; the executor calls the kernel once per chunk. Casts are
//! bound the same way through [`cast::CastFunctionSet`].

pub mod aggregate;
pub mod cast;
mod scalar;

pub use aggregate::{AggregateFunction, AggregateFunctionSet, AggregateOps, StatePtr, UnaryAggregate};
pub use scalar::{
    FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet, ScalarKernel, Stability,
};
