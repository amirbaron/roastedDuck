//! `product(DOUBLE)` (DuckDB's `product.cpp`).

use rustdb_common::Result;
use rustdb_function::aggregate::{Unary, UnaryAggregate};
use rustdb_function::{AggregateFunction, AggregateFunctionSet};
use rustdb_types::LogicalType;

/// Whether any value was seen, and the running product.
#[derive(Clone, Copy, Debug)]
struct ProductState {
    empty: bool,
    value: f64,
}

impl Default for ProductState {
    fn default() -> Self {
        Self {
            empty: true,
            value: 1.0,
        }
    }
}

struct Product;

impl UnaryAggregate for Product {
    type Input = f64;
    type State = ProductState;
    type Output = f64;

    #[inline]
    fn update(state: &mut ProductState, value: f64) -> Result<()> {
        state.empty = false;
        state.value *= value;
        Ok(())
    }

    fn combine(source: &ProductState, target: &mut ProductState) -> Result<()> {
        target.value *= source.value;
        target.empty = target.empty && source.empty;
        Ok(())
    }

    fn finalize(state: &ProductState) -> Result<Option<f64>> {
        Ok((!state.empty).then_some(state.value))
    }
}

/// `product(DOUBLE) -> DOUBLE`.
pub fn product_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "product",
        AggregateFunction::new(
            "product",
            vec![LogicalType::DOUBLE],
            LogicalType::DOUBLE,
            Unary::<Product>::default(),
        ),
    )
}
