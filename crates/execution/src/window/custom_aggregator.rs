//! Aggregates with their own windowed evaluation (DuckDB's
//! `window_custom_aggregator.cpp`), e.g. holistic aggregates like `quantile`.

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{FrameDelta, WindowFrame, WindowPartitionInput};
use rustdb_types::{ValidityMask, Vector};

use super::aggregate_states::WindowAggregateStates;
use super::aggregator::{WindowAggregator, evaluate_subframes, init_subframes};
use super::boundaries_state::{FrameBounds, WindowBounds};
use super::collection::WindowCollection;
use super::mask::WindowMask;

/// Whether the aggregate has a custom window callback usable here (DuckDB's
/// `CanAggregate`; the aggregation mode is always `WINDOW`).
pub(crate) fn can_aggregate(aggregator: &WindowAggregator, has_arg_orders: bool) -> bool {
    aggregator.function.ops.window().is_some() && !has_arg_orders
}

/// The partition data and shared state of a hash group.
#[derive(Debug)]
pub(crate) struct CustomGlobalState {
    /// The argument columns of the whole hash group.
    inputs: Vec<Vector>,
    count: usize,
    all_valid: Vec<bool>,
    filter_packed: ValidityMask,
    stats: [FrameDelta; 2],
    /// The state built by `window_init`, if the function has one.
    shared: Option<WindowAggregateStates>,
}

/// A thread's scratch state, shared by all of its evaluations.
#[derive(Debug)]
pub(crate) struct CustomLocalState {
    state: WindowAggregateStates,
    frames: Vec<FrameBounds>,
}

impl CustomGlobalState {
    /// Collects the partition input and runs `window_init` (DuckDB's `Finalize`).
    pub(crate) fn new(
        aggregator: &WindowAggregator,
        collection: &WindowCollection,
        filter_mask: &WindowMask,
        stats: [FrameDelta; 2],
    ) -> Result<Self> {
        let count = collection.size();
        let inputs: Vec<Vector> = aggregator
            .child_idx
            .iter()
            .map(|&column| collection.column(column).clone())
            .collect();
        let all_valid = aggregator
            .child_idx
            .iter()
            .map(|&column| collection.all_valid(column))
            .collect();
        let filter_packed = if filter_mask.is_all_valid() {
            ValidityMask::all_valid()
        } else {
            let words = (0..count.div_ceil(64))
                .map(|entry| filter_mask.entry(entry))
                .collect();
            ValidityMask::from_words(words)
        };
        let mut state = Self {
            inputs,
            count,
            all_valid,
            filter_packed,
            stats,
            shared: None,
        };
        let window = aggregator
            .function
            .ops
            .window()
            .ok_or_else(|| Error::internal("custom window aggregate without a window callback"))?;
        if window.has_window_init() {
            let shared = WindowAggregateStates::new(aggregator.function.ops.clone(), 1)?;
            // SAFETY: the state was just initialised and is owned here.
            unsafe { window.window_init(aggregator.input(), &state.partition(), shared.state(0))? };
            state.shared = Some(shared);
        }
        Ok(state)
    }

    fn partition(&self) -> WindowPartitionInput<'_> {
        WindowPartitionInput {
            inputs: &self.inputs,
            count: self.count,
            all_valid: &self.all_valid,
            filter_mask: &self.filter_packed,
            stats: self.stats,
        }
    }

    /// Calls the window callback for every row (DuckDB's `Evaluate`).
    pub(crate) fn evaluate(
        &self,
        aggregator: &WindowAggregator,
        local: &mut CustomLocalState,
        bounds: &WindowBounds,
        count: usize,
        row_idx: usize,
    ) -> Result<Vector> {
        let window = aggregator
            .function
            .ops
            .window()
            .ok_or_else(|| Error::internal("custom window aggregate without a window callback"))?;
        let partition = self.partition();
        let shared = self.shared.as_ref().map(|shared| shared.state(0));
        let state = local.state.state(0);
        let mut result = Vector::flat(aggregator.result_type.clone(), count);
        let mut frames = Vec::with_capacity(local.frames.len());
        evaluate_subframes(
            bounds,
            aggregator.exclude_mode,
            count,
            row_idx,
            &mut local.frames,
            |i, subframes| {
                frames.clear();
                frames.extend(subframes.iter().map(|frame| WindowFrame {
                    start: frame.start,
                    end: frame.end,
                }));
                // SAFETY: the local state is initialised and used by this thread only; the
                // shared state is only read.
                unsafe {
                    window.window(
                        aggregator.input(),
                        &partition,
                        shared,
                        state,
                        &frames,
                        &mut result,
                        i,
                    )
                }
            },
        )?;
        Ok(result)
    }
}

impl CustomLocalState {
    pub(crate) fn new(aggregator: &WindowAggregator) -> Result<Self> {
        Ok(Self {
            state: WindowAggregateStates::new(aggregator.function.ops.clone(), 1)?,
            frames: init_subframes(aggregator.exclude_mode),
        })
    }
}
