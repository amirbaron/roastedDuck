//! Variance, standard deviation, covariance and correlation (DuckDB's `stddev.hpp`,
//! `covar.hpp`, `corr.hpp` and their `.cpp` registrations): Welford-style running moments
//! with the parallel merge formulas of DuckDB, operation for operation (including the fused
//! multiply-adds the reference build's compiler forms from `a * b + c` expressions).

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Binary, BinaryAggregate, Unary, UnaryAggregate};
use rustdb_function::{AggregateFunction, AggregateFunctionSet};
use rustdb_types::LogicalType;

/// Count, mean and sum of squared deviations (DuckDB's `StddevState`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StddevState {
    pub(crate) count: u64,
    pub(crate) mean: f64,
    pub(crate) dsquared: f64,
}

impl StddevState {
    /// DuckDB's `STDDevBaseOperation::Execute`.
    #[inline]
    pub(crate) fn add(&mut self, input: f64) {
        self.count += 1;
        let mean_differential = (input - self.mean) / self.count as f64;
        let new_mean = self.mean + mean_differential;
        let dsquared_increment = (input - new_mean) * (input - self.mean);
        self.dsquared += dsquared_increment;
        self.mean = new_mean;
    }

    /// DuckDB's `STDDevBaseOperation::Combine`.
    pub(crate) fn merge(&mut self, source: &Self) {
        if self.count == 0 {
            *self = *source;
        } else if source.count > 0 {
            let count = self.count + source.count;
            let target_count = self.count as f64;
            let source_count = source.count as f64;
            let total_count = count as f64;
            let delta = source.mean - self.mean;
            let mean = (source_count / total_count).mul_add(delta, self.mean);
            self.dsquared =
                source.dsquared + self.dsquared + delta * delta * source_count * target_count / total_count;
            self.mean = mean;
            self.count = count;
        }
    }

    /// The population variance, or 0 for a single value (`VarPopOperation`).
    pub(crate) fn var_pop(&self) -> f64 {
        if self.count > 1 {
            self.dsquared / self.count as f64
        } else {
            0.0
        }
    }
}

/// Fails with `<NAME> is out of range!` unless `value` is finite.
fn finite(value: f64, name: &str) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::out_of_range(format!("{name} is out of range!")))
    }
}

/// Which statistic a [`Moment`] aggregate finalizes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Statistic {
    VarSamp,
    VarPop,
    StddevSamp,
    StddevPop,
    StandardError,
}

impl Statistic {
    const fn from_tag(tag: u8) -> Self {
        match tag {
            0 => Self::VarSamp,
            1 => Self::VarPop,
            2 => Self::StddevSamp,
            3 => Self::StddevPop,
            _ => Self::StandardError,
        }
    }
}

/// A unary aggregate over [`StddevState`]; `TAG` selects the [`Statistic`].
struct Moment<const TAG: u8>;

impl<const TAG: u8> UnaryAggregate for Moment<TAG> {
    type Input = f64;
    type State = StddevState;
    type Output = f64;

    #[inline]
    fn update(state: &mut StddevState, value: f64) -> Result<()> {
        state.add(value);
        Ok(())
    }

    fn combine(source: &StddevState, target: &mut StddevState) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &StddevState) -> Result<Option<f64>> {
        let count = state.count as f64;
        Ok(Some(match Statistic::from_tag(TAG) {
            Statistic::VarSamp => {
                if state.count <= 1 {
                    return Ok(None);
                }
                finite(state.dsquared / (count - 1.0), "VARSAMP")?
            }
            Statistic::VarPop => {
                if state.count == 0 {
                    return Ok(None);
                }
                finite(state.var_pop(), "VARPOP")?
            }
            Statistic::StddevSamp => {
                if state.count <= 1 {
                    return Ok(None);
                }
                finite((state.dsquared / (count - 1.0)).sqrt(), "STDDEV_SAMP")?
            }
            Statistic::StddevPop => {
                if state.count == 0 {
                    return Ok(None);
                }
                let value = if state.count > 1 {
                    (state.dsquared / count).sqrt()
                } else {
                    0.0
                };
                finite(value, "STDDEV_POP")?
            }
            Statistic::StandardError => {
                if state.count == 0 {
                    return Ok(None);
                }
                finite((state.dsquared / count).sqrt() / count.sqrt(), "SEM")?
            }
        }))
    }
}

fn moment<const TAG: u8>(name: &str) -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        name,
        AggregateFunction::new(
            name,
            vec![LogicalType::DOUBLE],
            LogicalType::DOUBLE,
            Unary::<Moment<TAG>>::default(),
        ),
    )
}

/// `var_samp` (alias `variance`).
pub fn var_samp_functions() -> AggregateFunctionSet {
    moment::<0>("var_samp")
}

/// `var_pop`.
pub fn var_pop_functions() -> AggregateFunctionSet {
    moment::<1>("var_pop")
}

/// `stddev_samp` (alias `stddev`).
pub fn stddev_samp_functions() -> AggregateFunctionSet {
    moment::<2>("stddev_samp")
}

/// `stddev_pop`.
pub fn stddev_pop_functions() -> AggregateFunctionSet {
    moment::<3>("stddev_pop")
}

/// `sem` (standard error of the mean).
pub fn sem_functions() -> AggregateFunctionSet {
    moment::<4>("sem")
}

/// Running means and co-moment of `(y, x)` (DuckDB's `CovarState`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CovarState {
    pub(crate) count: u64,
    pub(crate) meanx: f64,
    pub(crate) meany: f64,
    pub(crate) co_moment: f64,
}

impl CovarState {
    /// DuckDB's `CovarOperation::Operation(state, y, x)`.
    #[inline]
    pub(crate) fn add(&mut self, y: f64, x: f64) {
        self.count += 1;
        let n = self.count as f64;
        let dx = x - self.meanx;
        let meanx = self.meanx + dx / n;
        let dy = y - self.meany;
        let meany = self.meany + dy / n;
        let co_moment = dx.mul_add(y - meany, self.co_moment);
        self.meanx = meanx;
        self.meany = meany;
        self.co_moment = co_moment;
    }

    /// DuckDB's `CovarOperation::Combine`.
    pub(crate) fn merge(&mut self, source: &Self) {
        if self.count == 0 {
            *self = *source;
        } else if source.count > 0 {
            let count = self.count + source.count;
            let target_count = self.count as f64;
            let source_count = source.count as f64;
            let total_count = count as f64;
            let meanx = source_count.mul_add(source.meanx, target_count * self.meanx) / total_count;
            let meany = source_count.mul_add(source.meany, target_count * self.meany) / total_count;
            let deltax = self.meanx - source.meanx;
            let deltay = self.meany - source.meany;
            self.co_moment = source.co_moment
                + self.co_moment
                + deltax * deltay * source_count * target_count / total_count;
            self.meanx = meanx;
            self.meany = meany;
            self.count = count;
        }
    }

    /// `covar_pop`, or `None` without input.
    pub(crate) fn covar_pop(&self) -> Option<f64> {
        (self.count > 0).then(|| self.co_moment / self.count as f64)
    }
}

/// `covar_pop`/`covar_samp`.
struct Covar<const SAMPLE: bool>;

impl<const SAMPLE: bool> BinaryAggregate for Covar<SAMPLE> {
    type A = f64;
    type B = f64;
    type State = CovarState;
    type Output = f64;

    #[inline]
    fn update(state: &mut CovarState, y: f64, x: f64) -> Result<()> {
        state.add(y, x);
        Ok(())
    }

    fn combine(source: &CovarState, target: &mut CovarState) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &CovarState) -> Result<Option<f64>> {
        if SAMPLE {
            Ok((state.count >= 2).then(|| state.co_moment / (state.count - 1) as f64))
        } else {
            Ok(state.covar_pop())
        }
    }
}

/// The state of `corr` (DuckDB's `CorrState`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CorrState {
    cov_pop: CovarState,
    dev_pop_x: StddevState,
    dev_pop_y: StddevState,
}

impl CorrState {
    #[inline]
    pub(crate) fn add(&mut self, y: f64, x: f64) {
        self.cov_pop.add(y, x);
        self.dev_pop_x.add(x);
        self.dev_pop_y.add(y);
    }

    pub(crate) fn merge(&mut self, source: &Self) {
        self.cov_pop.merge(&source.cov_pop);
        self.dev_pop_x.merge(&source.dev_pop_x);
        self.dev_pop_y.merge(&source.dev_pop_y);
    }

    /// DuckDB's `CorrOperation::Finalize`.
    pub(crate) fn corr(&self) -> Result<Option<f64>> {
        if self.cov_pop.count == 0 || self.dev_pop_x.count == 0 || self.dev_pop_y.count == 0 {
            return Ok(None);
        }
        let cov = self.cov_pop.co_moment / self.cov_pop.count as f64;
        let std = |state: &StddevState, name: &str| {
            let value = if state.count > 1 {
                (state.dsquared / state.count as f64).sqrt()
            } else {
                0.0
            };
            finite(value, name)
        };
        let std_x = std(&self.dev_pop_x, "STDDEV_POP for X")?;
        let std_y = std(&self.dev_pop_y, "STDDEV_POP for Y")?;
        Ok(Some(if std_x * std_y != 0.0 {
            cov / (std_x * std_y)
        } else {
            f64::NAN
        }))
    }
}

struct Corr;

impl BinaryAggregate for Corr {
    type A = f64;
    type B = f64;
    type State = CorrState;
    type Output = f64;

    #[inline]
    fn update(state: &mut CorrState, y: f64, x: f64) -> Result<()> {
        state.add(y, x);
        Ok(())
    }

    fn combine(source: &CorrState, target: &mut CorrState) -> Result<()> {
        target.merge(source);
        Ok(())
    }

    fn finalize(state: &CorrState) -> Result<Option<f64>> {
        state.corr()
    }
}

pub(crate) fn binary_double<A: BinaryAggregate<A = f64, B = f64>>(
    name: &str,
    output: LogicalType,
) -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        name,
        AggregateFunction::new(
            name,
            vec![LogicalType::DOUBLE, LogicalType::DOUBLE],
            output,
            Binary::<A>::default(),
        ),
    )
}

/// `covar_pop(DOUBLE, DOUBLE)`.
pub fn covar_pop_functions() -> AggregateFunctionSet {
    binary_double::<Covar<false>>("covar_pop", LogicalType::DOUBLE)
}

/// `covar_samp(DOUBLE, DOUBLE)`.
pub fn covar_samp_functions() -> AggregateFunctionSet {
    binary_double::<Covar<true>>("covar_samp", LogicalType::DOUBLE)
}

/// `corr(DOUBLE, DOUBLE)`.
pub fn corr_functions() -> AggregateFunctionSet {
    binary_double::<Corr>("corr", LogicalType::DOUBLE)
}
