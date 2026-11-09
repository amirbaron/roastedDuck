//! The order in which a scan evaluates its filters, adapted to their measured cost
//! (DuckDB's `AdaptiveFilter`, `execution/adaptive_filter.cpp`).
//!
//! Filters start in the order of their estimated cost (`ExpressionHeuristics::
//! GetInitialOrder`). After a warm-up, every so often two neighbouring filters swap and
//! the swap is kept if the following vectors are filtered faster; swaps that did not pay
//! off become less likely.

use std::time::{Duration, Instant};

use roastedduck_function::random::RandomEngine;

use crate::table_filter::TableFilter;

/// Vectors measured after a swap before deciding whether to keep it.
const OBSERVE_INTERVAL: usize = 10;
/// Vectors measured between swaps.
const EXECUTE_INTERVAL: usize = 20;
/// Vectors measured before the first swap.
const WARMUP_INTERVAL: usize = 5;

/// An order of a scan's filters that adapts to their runtime.
#[derive(Debug)]
pub struct AdaptiveFilter {
    /// The filters in the order to evaluate them.
    pub permutation: Vec<usize>,
    iteration_count: usize,
    swap_index: usize,
    /// The likeliness (out of 100) of swapping each filter with the next one.
    swap_likeliness: Vec<u32>,
    right_random_border: u32,
    runtime_sum: Duration,
    previous_mean: f64,
    observe: bool,
    warmup: bool,
    generator: Option<RandomEngine>,
}

impl AdaptiveFilter {
    /// An order of `filters` by their estimated cost.
    pub fn new<'a>(filters: impl ExactSizeIterator<Item = &'a TableFilter>) -> Self {
        let count = filters.len();
        let costs: Vec<u64> = filters.map(TableFilter::cost).collect();
        let mut permutation: Vec<usize> = (0..count).collect();
        permutation.sort_by_key(|&index| costs[index]);
        Self {
            permutation,
            iteration_count: 0,
            swap_index: 0,
            swap_likeliness: vec![100; count.saturating_sub(1)],
            right_random_border: 100 * count.saturating_sub(1) as u32,
            runtime_sum: Duration::ZERO,
            previous_mean: 0.0,
            observe: false,
            warmup: true,
            generator: None,
        }
    }

    /// Starts measuring the filtering of a vector (`BeginFilter`); `None` when there is
    /// nothing to reorder.
    pub fn begin_filter(&self) -> Option<Instant> {
        (self.permutation.len() > 1).then(Instant::now)
    }

    /// Records the time since [`AdaptiveFilter::begin_filter`] and maybe reorders the
    /// filters (`EndFilter`).
    pub fn end_filter(&mut self, start: Option<Instant>) {
        if let Some(start) = start {
            self.adapt_runtime_statistics(start.elapsed());
        }
    }

    /// `AdaptRuntimeStatistics`.
    #[allow(clippy::cast_precision_loss, reason = "a mean over a few vectors")]
    fn adapt_runtime_statistics(&mut self, duration: Duration) {
        self.iteration_count += 1;
        self.runtime_sum += duration;
        if self.warmup {
            if self.iteration_count == WARMUP_INTERVAL {
                self.reset();
                self.observe = false;
                self.warmup = false;
            }
            return;
        }
        let mean = || self.runtime_sum.as_secs_f64() / self.iteration_count as f64;
        if self.observe && self.iteration_count == OBSERVE_INTERVAL {
            // Keep the swap if the runtime decreased, else undo it.
            if self.previous_mean - mean() <= 0.0 {
                self.permutation.swap(self.swap_index, self.swap_index + 1);
                // Less likely to try again, but always a little.
                if self.swap_likeliness[self.swap_index] > 1 {
                    self.swap_likeliness[self.swap_index] /= 2;
                }
            } else {
                self.swap_likeliness[self.swap_index] = 100;
            }
            self.observe = false;
            self.reset();
        } else if !self.observe && self.iteration_count == EXECUTE_INTERVAL {
            self.previous_mean = mean();
            let border = self.right_random_border;
            let random = self
                .generator
                .get_or_insert_with(RandomEngine::default)
                .next_random_integer_in(1, border);
            self.swap_index = (random / 100) as usize;
            let likeliness = random - 100 * self.swap_index as u32;
            if self.swap_likeliness[self.swap_index] > likeliness {
                self.permutation.swap(self.swap_index, self.swap_index + 1);
                self.observe = true;
            }
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.iteration_count = 0;
        self.runtime_sum = Duration::ZERO;
    }
}

impl TableFilter {
    /// The estimated cost of evaluating the filter (`ExpressionHeuristics::Cost`): cheap
    /// comparisons first, optional and dynamic filters first of all (they stop filtering
    /// when they do not pay off), unknown filters (IN lists, expressions) last.
    pub fn cost(&self) -> u64 {
        use roastedduck_types::PhysicalType;
        match self {
            Self::Dynamic(_) | Self::Optional(_) | Self::SelectivityOptional(_) => 0,
            Self::And(children) | Self::Or(children) => 5 + children.iter().map(Self::cost).sum::<u64>(),
            Self::ConstantComparison { constant, .. } => match constant.logical_type().physical_type() {
                PhysicalType::Varchar => 5,
                PhysicalType::Float | PhysicalType::Double => 2,
                _ => 1,
            },
            Self::IsNull | Self::IsNotNull => 5,
            Self::StructExtract { child_filter, .. } => child_filter.cost(),
            Self::In(_) | Self::Expression(_) => 1000,
        }
    }
}
