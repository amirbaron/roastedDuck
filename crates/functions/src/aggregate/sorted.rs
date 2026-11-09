//! Aggregates with `ORDER BY` arguments (DuckDB's `sorted_aggregate_function.cpp`): the
//! wrapper buffers each group's rows, sorts them when finalizing and feeds them to the
//! wrapped aggregate in order.
//!
//! Like DuckDB, the rows of a batch of states are sorted together with the state's index as
//! the leading key, using the engine's sort, so rows with equal keys come out in the same
//! order as in DuckDB.

use std::sync::Arc;

use roastedduck_buffer::{BufferManager, BufferManagerConfig};
use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::{Aggregate, AggregateInput, AggregateState, States, Typed};
use roastedduck_function::{AggregateFunction, FunctionData};
pub use roastedduck_row::sort::{NullOrder, OrderModifiers, OrderType};
use roastedduck_row::sort::{Sort, SortColumn, SortConfig};
use roastedduck_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

/// Rows per sort batch before the batch is sorted (DuckDB's `ordered_aggregate_threshold`).
const DEFAULT_THRESHOLD: usize = 262_144;

/// One `ORDER BY` of a sorted aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortedOrder {
    /// The type of the ordering expression.
    pub ty: LogicalType,
    pub modifiers: OrderModifiers,
    /// The argument the ordering expression equals, if any (it is then not buffered twice).
    pub argument: Option<usize>,
}

/// The bind data of a sorted aggregate (DuckDB's `SortedAggregateBindData`).
#[derive(Debug)]
struct SortedAggregateData {
    function: AggregateFunction,
    bind_info: Option<Arc<dyn FunctionData>>,
    /// Input columns that are buffered: the arguments, then the unmatched orderings.
    buffered: Vec<usize>,
    orders: Vec<SortedOrder>,
    arguments: usize,
    sort: Sort,
    threshold: usize,
}

impl FunctionData for SortedAggregateData {
    fn equals(&self, other: &dyn FunctionData) -> bool {
        let Some(other) = other.as_any().downcast_ref::<Self>() else {
            return false;
        };
        let bind_equal = match (&self.bind_info, &other.bind_info) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right) || left.equals(right.as_ref()),
            (None, None) => true,
            _ => false,
        };
        bind_equal
            && self.function.ops.callback_identity() == other.function.ops.callback_identity()
            && self.orders == other.orders
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl SortedAggregateData {
    fn inner_input(&self) -> AggregateInput<'_> {
        AggregateInput {
            bind_data: self.bind_info.as_deref(),
        }
    }
}

/// The rows buffered by one state: shared input columns and the selected rows.
#[derive(Debug, Default)]
struct SortedState {
    segments: Vec<(Arc<Vec<Vector>>, Vec<u32>)>,
    count: usize,
}

impl SortedState {
    fn push(&mut self, columns: &Arc<Vec<Vector>>, row: usize) {
        match self.segments.last_mut() {
            Some((last, rows)) if Arc::ptr_eq(last, columns) => rows.push(row as u32),
            _ => self.segments.push((Arc::clone(columns), vec![row as u32])),
        }
        self.count += 1;
    }
}

struct SortedAggregate;

impl Aggregate for SortedAggregate {
    type State = SortedState;

    fn initialize(&self) -> SortedState {
        SortedState::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, SortedState>,
        count: usize,
    ) -> Result<()> {
        let data = input.bind_data::<SortedAggregateData>()?;
        let columns = Arc::new(
            data.buffered
                .iter()
                .map(|&column| inputs[column].clone())
                .collect::<Vec<_>>(),
        );
        if let Some(state) = states.as_single() {
            if count > 0 {
                state.segments.push((columns, (0..count as u32).collect()));
                state.count += count;
            }
            return Ok(());
        }
        for row in 0..count {
            states.get(row).push(&columns, row);
        }
        Ok(())
    }

    fn combine(&self, source: &SortedState, target: &mut SortedState, _: AggregateInput<'_>) -> Result<()> {
        target.segments.extend(source.segments.iter().cloned());
        target.count += source.count;
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut SortedState,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        self.finalize_all(&mut States::single(state), 1, input, result, row)
    }

    fn finalize_all(
        &self,
        states: &mut States<'_, SortedState>,
        count: usize,
        input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let data = input.bind_data::<SortedAggregateData>()?;
        let mut first = 0;
        while first < count {
            // Collect states until the batch holds `threshold` rows (DuckDB's batching).
            let mut last = first;
            let mut rows = 0;
            while last < count && (last == first || rows < data.threshold) {
                rows += states.get(last).count;
                last += 1;
            }
            finalize_batch(data, states, first, last, result, offset)?;
            first = last;
        }
        Ok(())
    }
}

/// Sorts the rows of states `first..last` together and finalizes each state.
fn finalize_batch(
    data: &SortedAggregateData,
    states: &mut States<'_, SortedState>,
    first: usize,
    last: usize,
    result: &mut Vector,
    offset: usize,
) -> Result<()> {
    let global = data.sort.global_sink_state(SortConfig::default());
    let mut local = data.sort.local_sink_state();
    let mut counts = Vec::with_capacity(last - first);
    for index in first..last {
        let state = states.get(index);
        counts.push(state.count);
        let prefix = Vector::constant(&Value::usmallint((index - first) as u16))?;
        for (columns, rows) in &state.segments {
            let selection = SelectionVector::new(rows.clone());
            let mut sliced = Vec::with_capacity(columns.len() + 1);
            sliced.push(prefix.clone());
            sliced.extend(columns.iter().map(|column| column.slice(&selection)));
            data.sort
                .sink(&DataChunk::new(sliced, rows.len()), &global, &mut local)?;
        }
    }
    data.sort.combine(&global, local)?;
    let chunks = match data.sort.finalize(global)? {
        Some(source) => source.collect()?,
        None => Vec::new(),
    };
    let mut chunks = chunks.into_iter();
    let mut current: Option<(DataChunk, usize)> = None;
    for (index, &rows) in counts.iter().enumerate() {
        let mut state = AggregateState::new(&data.function);
        let mut remaining = rows;
        while remaining > 0 {
            let (chunk, consumed) = match current.take() {
                Some(entry) => entry,
                None => (
                    chunks
                        .next()
                        .ok_or_else(|| Error::internal("sorted aggregate lost rows"))?,
                    0,
                ),
            };
            let take = remaining.min(chunk.len() - consumed);
            let selection =
                SelectionVector::new((consumed as u32..(consumed + take) as u32).collect::<Vec<_>>());
            let arguments: Vec<Vector> = chunk
                .columns()
                .iter()
                .take(data.arguments)
                .map(|column| column.slice(&selection))
                .collect();
            state.update(&arguments, data.inner_input(), take)?;
            remaining -= take;
            if consumed + take < chunk.len() {
                current = Some((chunk, consumed + take));
            }
        }
        state.finalize(data.inner_input(), result, offset + first + index)?;
    }
    Ok(())
}

/// The result of [`bind_sorted_aggregate`].
#[derive(Debug)]
pub struct SortedBinding {
    /// The wrapper aggregate.
    pub function: AggregateFunction,
    /// Its bind data.
    pub bind_data: Arc<dyn FunctionData>,
    /// Whether the ordering expressions must be appended to the aggregate's arguments.
    pub append_orders: bool,
}

/// Wraps `function` so its input is sorted by `orders` first (DuckDB's
/// `FunctionBinder::BindSortedAggregate`).
///
/// `argument_types` are the types of the aggregate's argument expressions. The wrapper's
/// inputs are the aggregate's arguments followed — when any ordering does not
/// match an argument ([`SortedBinding::append_orders`]) — by every ordering expression, in
/// order.
pub fn bind_sorted_aggregate(
    function: AggregateFunction,
    bind_info: Option<Arc<dyn FunctionData>>,
    argument_types: &[LogicalType],
    orders: &[SortedOrder],
) -> Result<SortedBinding> {
    let arguments = argument_types.len();
    let sorted_on_args = orders.iter().all(|order| order.argument.is_some());
    let mut buffered: Vec<usize> = (0..arguments).collect();
    let mut buffered_types = argument_types.to_vec();
    // The sort chunk is the state index followed by the buffered columns.
    let mut sort_columns = vec![SortColumn {
        column: 0,
        modifiers: OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst),
        stats: None,
    }];
    for (index, order) in orders.iter().enumerate() {
        let column = if let Some(argument) = order.argument {
            argument + 1
        } else {
            buffered.push(arguments + index);
            buffered_types.push(order.ty.clone());
            buffered.len()
        };
        sort_columns.push(SortColumn {
            column,
            modifiers: order.modifiers,
            stats: None,
        });
    }
    let mut sort_types = vec![LogicalType::USMALLINT];
    sort_types.extend(buffered_types);
    let buffer_manager = BufferManager::new(BufferManagerConfig::default())?;
    let sort = Sort::new(
        buffer_manager,
        sort_columns,
        &sort_types,
        Some((1..=arguments).collect()),
    )?;
    let mut input_types = argument_types.to_vec();
    if !sorted_on_args {
        input_types.extend(orders.iter().map(|order| order.ty.clone()));
    }
    let mut wrapper = AggregateFunction::new(
        Arc::clone(&function.name),
        input_types,
        function.return_type.clone(),
        Typed(SortedAggregate),
    );
    wrapper.null_handling = function.null_handling;
    let data = SortedAggregateData {
        function,
        bind_info,
        buffered,
        orders: orders.to_vec(),
        arguments,
        sort,
        threshold: DEFAULT_THRESHOLD,
    };
    Ok(SortedBinding {
        function: wrapper,
        bind_data: Arc::new(data),
        append_orders: !sorted_on_args,
    })
}
