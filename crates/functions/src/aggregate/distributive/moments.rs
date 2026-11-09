//! `skewness`, `kurtosis` and `kurtosis_pop` from power sums (DuckDB's `skew.cpp` and
//! `kurtosis.cpp`).
//!
//! The formulas and the order of floating point operations follow DuckDB exactly, with
//! `pow` from the C library. The reference build contracts `a * b + c` inside one C++
//! expression into a fused multiply-add (clang's default `-ffp-contract=on`), so those
//! spots use `mul_add`; results are then bit-identical.

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::{Unary, UnaryAggregate};
use roastedduck_function::{AggregateFunction, AggregateFunctionSet};
use roastedduck_types::LogicalType;

/// Count and power sums up to the fourth power.
#[derive(Clone, Copy, Debug, Default)]
struct PowerSums {
    n: u64,
    sum: f64,
    sum_sqr: f64,
    sum_cub: f64,
    sum_four: f64,
}

impl PowerSums {
    #[inline]
    fn add(&mut self, value: f64, fourth: bool) {
        self.n += 1;
        self.sum += value;
        self.sum_sqr += value.powf(2.0);
        self.sum_cub += value.powf(3.0);
        if fourth {
            self.sum_four += value.powf(4.0);
        }
    }

    fn merge(&mut self, other: &Self) {
        if other.n == 0 {
            return;
        }
        self.n += other.n;
        self.sum += other.sum;
        self.sum_sqr += other.sum_sqr;
        self.sum_cub += other.sum_cub;
        self.sum_four += other.sum_four;
    }
}

/// `skewness(DOUBLE)` (DuckDB's `SkewnessOperation`).
struct Skewness;

impl UnaryAggregate for Skewness {
    type Input = f64;
    type State = PowerSums;
    type Output = f64;

    #[inline]
    fn update(state: &mut PowerSums, value: f64) -> Result<()> {
        state.add(value, false);
        Ok(())
    }

    fn combine(source: &PowerSums, target: &mut PowerSums) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &PowerSums) -> Result<Option<f64>> {
        if state.n <= 2 {
            return Ok(None);
        }
        let n = state.n as f64;
        let temp = 1.0 / n;
        let mut p = (temp * (-(state.sum * state.sum)).mul_add(temp, state.sum_sqr)).powf(3.0);
        if p < 0.0 {
            p = 0.0;
        }
        let div = p.sqrt();
        if div == 0.0 {
            return Ok(Some(f64::NAN));
        }
        let temp1 = (n * (n - 1.0)).sqrt() / (n - 2.0);
        let inner = (2.0 * state.sum.powf(3.0) * temp).mul_add(
            temp,
            (-(3.0 * state.sum_sqr * state.sum)).mul_add(temp, state.sum_cub),
        );
        let result = temp1 * temp * inner / div;
        if !result.is_finite() {
            return Err(Error::out_of_range("SKEW is out of range!"));
        }
        Ok(Some(result))
    }
}

/// `kurtosis` (bias corrected) and `kurtosis_pop` (DuckDB's `KurtosisOperation`).
struct Kurtosis<const BIAS_CORRECTION: bool>;

impl<const BIAS_CORRECTION: bool> UnaryAggregate for Kurtosis<BIAS_CORRECTION> {
    type Input = f64;
    type State = PowerSums;
    type Output = f64;

    #[inline]
    fn update(state: &mut PowerSums, value: f64) -> Result<()> {
        state.add(value, true);
        Ok(())
    }

    fn combine(source: &PowerSums, target: &mut PowerSums) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &PowerSums) -> Result<Option<f64>> {
        let n = state.n as f64;
        if n <= 1.0 || (BIAS_CORRECTION && n <= 3.0) {
            return Ok(None);
        }
        let temp = 1.0 / n;
        // DuckDB also checks with `long double`, which is `double` on the reference build.
        let centered = (-(state.sum * state.sum)).mul_add(temp, state.sum_sqr);
        if centered == 0.0 {
            return Ok(None);
        }
        let m4 = temp
            * (-(3.0 * state.sum.powf(4.0))).mul_add(
                temp.powf(3.0),
                (6.0 * state.sum_sqr * state.sum * state.sum * temp).mul_add(
                    temp,
                    (-(4.0 * state.sum_cub * state.sum)).mul_add(temp, state.sum_four),
                ),
            );
        let m2 = temp * centered;
        if m2 <= 0.0 {
            return Ok(None);
        }
        let result = if BIAS_CORRECTION {
            (n - 1.0) * (-3.0f64).mul_add(n - 1.0, (n + 1.0) * m4 / (m2 * m2)) / ((n - 2.0) * (n - 3.0))
        } else {
            m4 / (m2 * m2) - 3.0
        };
        if !result.is_finite() {
            return Err(Error::out_of_range("Kurtosis is out of range!"));
        }
        Ok(Some(result))
    }
}

fn double_aggregate<A: UnaryAggregate<Input = f64, Output = f64>>(name: &str) -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        name,
        AggregateFunction::new(
            name,
            vec![LogicalType::DOUBLE],
            LogicalType::DOUBLE,
            Unary::<A>::default(),
        ),
    )
}

/// `skewness(DOUBLE) -> DOUBLE`.
pub fn skewness_functions() -> AggregateFunctionSet {
    double_aggregate::<Skewness>("skewness")
}

/// `kurtosis(DOUBLE) -> DOUBLE`.
pub fn kurtosis_functions() -> AggregateFunctionSet {
    double_aggregate::<Kurtosis<true>>("kurtosis")
}

/// `kurtosis_pop(DOUBLE) -> DOUBLE`.
pub fn kurtosis_pop_functions() -> AggregateFunctionSet {
    double_aggregate::<Kurtosis<false>>("kurtosis_pop")
}
