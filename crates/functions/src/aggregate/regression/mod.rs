//! Linear regression aggregates `regr_*(y, x)` (DuckDB's `core_functions/aggregate/
//! regression`).

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::BinaryAggregate;
use rustdb_function::{AggregateFunctionSet, NullHandling};
use rustdb_types::LogicalType;

use crate::aggregate::algebraic::stddev::{CorrState, CovarState, StddevState, binary_double};

/// Sum and count of one side (DuckDB's `RegrState`).
#[derive(Clone, Copy, Debug, Default)]
struct RegrAvgState {
    sum: f64,
    count: u64,
}

/// `regr_avgx` (`Y = false`) and `regr_avgy` (`Y = true`).
struct RegrAvg<const Y: bool>;

impl<const Y: bool> BinaryAggregate for RegrAvg<Y> {
    type A = f64;
    type B = f64;
    type State = RegrAvgState;
    type Output = f64;

    #[inline]
    fn update(state: &mut RegrAvgState, y: f64, x: f64) -> Result<()> {
        state.sum += if Y { y } else { x };
        state.count += 1;
        Ok(())
    }

    fn combine(source: &RegrAvgState, target: &mut RegrAvgState) -> Result<()> {
        target.sum += source.sum;
        target.count += source.count;
        Ok(())
    }

    fn finalize(state: &RegrAvgState) -> Result<Option<f64>> {
        Ok((state.count > 0).then(|| state.sum / state.count as f64))
    }
}

/// `regr_count(y, x) -> UINTEGER`.
struct RegrCount;

impl BinaryAggregate for RegrCount {
    type A = f64;
    type B = f64;
    type State = u64;
    type Output = u32;

    #[inline]
    fn update(state: &mut u64, _: f64, _: f64) -> Result<()> {
        *state += 1;
        Ok(())
    }

    fn combine(source: &u64, target: &mut u64) -> Result<()> {
        *target += *source;
        Ok(())
    }

    fn finalize(state: &u64) -> Result<Option<u32>> {
        Ok(Some(*state as u32))
    }
}

/// The state of `regr_slope` (DuckDB's `RegrSlopeState`).
#[derive(Clone, Copy, Debug, Default)]
struct SlopeState {
    cov_pop: CovarState,
    var_pop: StddevState,
}

impl SlopeState {
    fn add(&mut self, y: f64, x: f64) {
        self.cov_pop.add(y, x);
        self.var_pop.add(x);
    }

    fn merge(&mut self, source: &Self) {
        self.cov_pop.merge(&source.cov_pop);
        self.var_pop.merge(&source.var_pop);
    }

    /// DuckDB's `RegrSlopeOperation::Finalize`.
    fn slope(&self) -> Result<Option<f64>> {
        if self.cov_pop.count == 0 || self.var_pop.count == 0 {
            return Ok(None);
        }
        let cov = self.cov_pop.co_moment / self.cov_pop.count as f64;
        let var_pop = self.var_pop.var_pop();
        if !var_pop.is_finite() {
            return Err(Error::out_of_range("VARPOP is out of range!"));
        }
        Ok(Some(if var_pop != 0.0 { cov / var_pop } else { f64::NAN }))
    }
}

struct RegrSlope;

impl BinaryAggregate for RegrSlope {
    type A = f64;
    type B = f64;
    type State = SlopeState;
    type Output = f64;

    fn update(state: &mut SlopeState, y: f64, x: f64) -> Result<()> {
        state.add(y, x);
        Ok(())
    }

    fn combine(source: &SlopeState, target: &mut SlopeState) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &SlopeState) -> Result<Option<f64>> {
        state.slope()
    }
}

/// The state of `regr_intercept`.
#[derive(Clone, Copy, Debug, Default)]
struct InterceptState {
    count: u64,
    sum_x: f64,
    sum_y: f64,
    slope: SlopeState,
}

struct RegrIntercept;

impl BinaryAggregate for RegrIntercept {
    type A = f64;
    type B = f64;
    type State = InterceptState;
    type Output = f64;

    fn update(state: &mut InterceptState, y: f64, x: f64) -> Result<()> {
        state.count += 1;
        state.sum_x += x;
        state.sum_y += y;
        state.slope.add(y, x);
        Ok(())
    }

    fn combine(source: &InterceptState, target: &mut InterceptState) -> Result<()> {
        target.count += source.count;
        target.sum_x += source.sum_x;
        target.sum_y += source.sum_y;
        target.slope.merge(&source.slope);
        Ok(())
    }

    fn finalize(state: &InterceptState) -> Result<Option<f64>> {
        if state.count == 0 {
            return Ok(None);
        }
        let Some(slope) = state.slope.slope()? else {
            return Ok(None);
        };
        if slope.is_nan() {
            return Ok(None);
        }
        let x_avg = state.sum_x / state.count as f64;
        let y_avg = state.sum_y / state.count as f64;
        Ok(Some((-slope).mul_add(x_avg, y_avg)))
    }
}

/// The state of `regr_r2`.
#[derive(Clone, Copy, Debug, Default)]
struct R2State {
    corr: CorrState,
    var_pop_x: StddevState,
    var_pop_y: StddevState,
}

struct RegrR2;

impl BinaryAggregate for RegrR2 {
    type A = f64;
    type B = f64;
    type State = R2State;
    type Output = f64;

    fn update(state: &mut R2State, y: f64, x: f64) -> Result<()> {
        state.corr.add(y, x);
        state.var_pop_x.add(x);
        state.var_pop_y.add(y);
        Ok(())
    }

    fn combine(source: &R2State, target: &mut R2State) -> Result<()> {
        target.corr.merge(&source.corr);
        target.var_pop_x.merge(&source.var_pop_x);
        target.var_pop_y.merge(&source.var_pop_y);
        Ok(())
    }

    fn finalize(state: &R2State) -> Result<Option<f64>> {
        let var_pop_x = state.var_pop_x.var_pop();
        if !var_pop_x.is_finite() {
            return Err(Error::out_of_range("VARPOP(X) is out of range!"));
        }
        if var_pop_x == 0.0 {
            return Ok(None);
        }
        let var_pop_y = state.var_pop_y.var_pop();
        if !var_pop_y.is_finite() {
            return Err(Error::out_of_range("VARPOP(Y) is out of range!"));
        }
        if var_pop_y == 0.0 {
            return Ok(Some(1.0));
        }
        // DuckDB squares whatever the correlation produced, NULL included.
        Ok(state.corr.corr()?.map(|corr| corr.powf(2.0)))
    }
}

/// The state of `regr_sxx`/`regr_syy`.
#[derive(Clone, Copy, Debug, Default)]
struct SState {
    count: u64,
    var_pop: StddevState,
}

/// `regr_sxx` (`Y = false`) and `regr_syy` (`Y = true`).
struct RegrS<const Y: bool>;

impl<const Y: bool> BinaryAggregate for RegrS<Y> {
    type A = f64;
    type B = f64;
    type State = SState;
    type Output = f64;

    fn update(state: &mut SState, y: f64, x: f64) -> Result<()> {
        state.count += 1;
        state.var_pop.add(if Y { y } else { x });
        Ok(())
    }

    fn combine(source: &SState, target: &mut SState) -> Result<()> {
        target.count += source.count;
        target.var_pop.merge(&source.var_pop);
        Ok(())
    }

    fn finalize(state: &SState) -> Result<Option<f64>> {
        if state.var_pop.count == 0 {
            return Ok(None);
        }
        let var_pop = state.var_pop.var_pop();
        if !var_pop.is_finite() {
            return Err(Error::out_of_range("VARPOP is out of range!"));
        }
        Ok(Some(state.count as f64 * var_pop))
    }
}

/// The state of `regr_sxy`.
#[derive(Clone, Copy, Debug, Default)]
struct SxyState {
    count: u64,
    cov_pop: CovarState,
}

struct RegrSxy;

impl BinaryAggregate for RegrSxy {
    type A = f64;
    type B = f64;
    type State = SxyState;
    type Output = f64;

    fn update(state: &mut SxyState, y: f64, x: f64) -> Result<()> {
        state.count += 1;
        state.cov_pop.add(y, x);
        Ok(())
    }

    fn combine(source: &SxyState, target: &mut SxyState) -> Result<()> {
        target.cov_pop.merge(&source.cov_pop);
        target.count += source.count;
        Ok(())
    }

    fn finalize(state: &SxyState) -> Result<Option<f64>> {
        Ok(state
            .cov_pop
            .covar_pop()
            .map(|cov_pop| state.count as f64 * cov_pop))
    }
}

/// Every `regr_*` aggregate.
pub fn regression_functions() -> Vec<AggregateFunctionSet> {
    let mut count = binary_double::<RegrCount>("regr_count", LogicalType::UINTEGER);
    for function in &mut count.functions {
        function.null_handling = NullHandling::Special;
    }
    vec![
        binary_double::<RegrAvg<false>>("regr_avgx", LogicalType::DOUBLE),
        binary_double::<RegrAvg<true>>("regr_avgy", LogicalType::DOUBLE),
        count,
        binary_double::<RegrIntercept>("regr_intercept", LogicalType::DOUBLE),
        binary_double::<RegrR2>("regr_r2", LogicalType::DOUBLE),
        binary_double::<RegrSlope>("regr_slope", LogicalType::DOUBLE),
        binary_double::<RegrS<false>>("regr_sxx", LogicalType::DOUBLE),
        binary_double::<RegrSxy>("regr_sxy", LogicalType::DOUBLE),
        binary_double::<RegrS<true>>("regr_syy", LogicalType::DOUBLE),
    ]
}
