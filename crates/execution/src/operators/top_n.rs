//! `ORDER BY ... LIMIT n` as a heap of the best `n + offset` rows (DuckDB's
//! `PhysicalTopN` and `TopNHeap`).
//!
//! Rows are compared by their normalized sort keys. Among equal keys the earlier row
//! wins, so the result equals a stable sort followed by the limit.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{OrderByNullType, OrderType};
use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_row::sort::{
    NullOrder, OrderModifiers, OrderType as SortOrder, create_sort_keys, decode_sort_key,
};
use rustdb_storage::table_filter::DynamicFilterData;
use rustdb_types::compare::CompareOp;
use rustdb_types::{DataChunk, LogicalType, PhysicalType, SelectionVector, StringT, Vector};

use crate::operator::{
    ErasedSink, ErasedSource, OrderPreservationType, PhysicalOperator, PhysicalRef, SinkState, order_names,
};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// `ORDER BY` with a constant `LIMIT` (and `OFFSET`) (DuckDB's `PhysicalTopN`).
#[derive(Debug)]
pub struct PhysicalTopN {
    types: Vec<LogicalType>,
    orders: Vec<BoundOrderByNode>,
    limit: u64,
    offset: u64,
    /// The run-time filter pushed into the scans below and its comparison: rows beyond
    /// the boundary of a full heap cannot be in the result.
    dynamic_filter: Option<(Arc<DynamicFilterData>, CompareOp)>,
    children: [PhysicalRef; 1],
}

impl PhysicalTopN {
    pub fn new(
        types: Vec<LogicalType>,
        orders: Vec<BoundOrderByNode>,
        limit: u64,
        offset: u64,
        dynamic_filter: Option<Arc<DynamicFilterData>>,
        child: PhysicalRef,
    ) -> Self {
        // With a single key, rows equal to the boundary cannot enter the heap either.
        let single = orders.len() == 1;
        let descending = orders
            .first()
            .is_some_and(|order| order.order_type == OrderType::Descending);
        let op = match (descending, single) {
            (false, true) => CompareOp::LessThan,
            (false, false) => CompareOp::LessThanOrEqual,
            (true, true) => CompareOp::GreaterThan,
            (true, false) => CompareOp::GreaterThanOrEqual,
        };
        Self {
            types,
            orders,
            limit,
            offset,
            dynamic_filter: dynamic_filter.map(|data| (data, op)),
            children: [child],
        }
    }

    /// Tightens the dynamic filter to the boundary of a full heap (DuckDB's
    /// `TopNBoundaryValue::UpdateValue`).
    fn update_boundary(&self, global: &TopNGlobalState, heap: &TopNHeap) -> Result<()> {
        let Some((data, op)) = &self.dynamic_filter else {
            return Ok(());
        };
        if heap.heap_size == 0 || heap.heap.len() < heap.heap_size {
            return Ok(());
        }
        let Some(top) = heap.heap.peek() else {
            return Ok(());
        };
        let mut boundary = global.boundary.lock().unwrap_or_else(PoisonError::into_inner);
        if boundary.as_ref().is_some_and(|current| **current <= *top.key) {
            return Ok(());
        }
        *boundary = Some(top.key.clone());
        let Some(first) = self.orders.first() else {
            return Ok(());
        };
        let mut columns = [Vector::flat(first.expression.return_type.clone(), 1)];
        decode_sort_key(&top.key, &mut columns, 0, &heap.modifiers[..1])?;
        data.set(*op, columns[0].get_value(0));
        Ok(())
    }

    fn modifiers(&self) -> Result<Vec<OrderModifiers>> {
        self.orders
            .iter()
            .map(|order| {
                let order_type = match order.order_type {
                    OrderType::Ascending => SortOrder::Ascending,
                    OrderType::Descending => SortOrder::Descending,
                    other => return Err(Error::internal(format!("unresolved order type {other:?}"))),
                };
                let nulls = match order.null_order {
                    OrderByNullType::NullsFirst => NullOrder::NullsFirst,
                    OrderByNullType::NullsLast => NullOrder::NullsLast,
                    other => return Err(Error::internal(format!("unresolved null order {other:?}"))),
                };
                Ok(OrderModifiers::new(order_type, nulls))
            })
            .collect()
    }

    fn heap(&self) -> Result<TopNHeap> {
        let heap_size = usize::try_from(self.limit.saturating_add(self.offset)).unwrap_or(usize::MAX);
        Ok(TopNHeap {
            types: self.types.clone(),
            modifiers: self.modifiers()?,
            executor: ExpressionExecutor::new(
                self.orders.iter().map(|order| order.expression.clone()).collect(),
            ),
            heap_size,
            heap: BinaryHeap::new(),
            data: Payload::default(),
            sequence: 0,
        })
    }
}

impl PhysicalOperator for PhysicalTopN {
    fn name(&self) -> &'static str {
        "TOP_N"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn parallel_sink(&self) -> bool {
        true
    }
    fn source_order(&self) -> OrderPreservationType {
        OrderPreservationType::FixedOrder
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("Top".to_owned(), self.limit.to_string());
        if self.offset > 0 {
            result.insert("Offset".to_owned(), self.offset.to_string());
        }
        result.insert("Order By".to_owned(), order_names(&self.orders));
        result
    }
}

/// A row in the heap: its sort key, then its arrival order, which breaks ties in favor
/// of earlier rows (DuckDB's `TopNEntry`).
#[derive(Debug, PartialEq, Eq)]
struct TopNEntry {
    key: Box<[u8]>,
    sequence: u64,
    /// The row in the payload.
    index: usize,
}

impl Ord for TopNEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key).then(self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for TopNEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The payload rows of the heap: compact chunks addressed by a running row number.
#[derive(Debug, Default)]
struct Payload {
    chunks: Vec<DataChunk>,
    count: usize,
}

impl Payload {
    /// Appends rows `sel` of `chunk`; returns the row number of the first.
    fn append(&mut self, chunk: &DataChunk, sel: &[u32]) -> usize {
        let start = self.count;
        let selection = SelectionVector::new(sel.to_vec());
        let columns = chunk
            .columns()
            .iter()
            .map(|column| column.slice(&selection).flattened(sel.len()))
            .collect();
        self.chunks.push(DataChunk::new(columns, sel.len()));
        self.count += sel.len();
        start
    }

    /// One chunk holding every row, in row number order.
    fn concatenate(&self, types: &[LogicalType]) -> Result<DataChunk> {
        let columns = types
            .iter()
            .enumerate()
            .map(|(column, ty)| {
                let parts: Vec<&Vector> = self.chunks.iter().map(|chunk| chunk.column(column)).collect();
                let counts: Vec<usize> = self.chunks.iter().map(DataChunk::len).collect();
                concat_vectors(ty, &parts, &counts)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(DataChunk::new(columns, self.count))
    }
}

/// Concatenates flat vectors of `ty` with `counts` rows each.
fn concat_vectors(ty: &LogicalType, parts: &[&Vector], counts: &[usize]) -> Result<Vector> {
    let total: usize = counts.iter().sum();
    let mut result = Vector::flat(ty.clone(), total.max(1));
    macro_rules! copy_values {
        ($native:ty) => {{
            let mut offset = 0;
            for (part, &count) in parts.iter().zip(counts) {
                let source = part.flattened(count);
                result.values_mut::<$native>()[offset..offset + count]
                    .copy_from_slice(&source.values::<$native>()[..count]);
                for row in 0..count {
                    if !source.validity().is_valid(row) {
                        result.validity_mut().set_invalid(offset + row, total.max(1));
                    }
                }
                if std::any::TypeId::of::<$native>() == std::any::TypeId::of::<StringT>() {
                    result.add_heap_references(&source);
                }
                offset += count;
            }
        }};
    }
    match ty.physical_type() {
        PhysicalType::Bool => copy_values!(bool),
        PhysicalType::Int8 => copy_values!(i8),
        PhysicalType::Int16 => copy_values!(i16),
        PhysicalType::Int32 => copy_values!(i32),
        PhysicalType::Int64 => copy_values!(i64),
        PhysicalType::Int128 => copy_values!(i128),
        PhysicalType::UInt8 => copy_values!(u8),
        PhysicalType::UInt16 => copy_values!(u16),
        PhysicalType::UInt32 => copy_values!(u32),
        PhysicalType::UInt64 => copy_values!(u64),
        PhysicalType::UInt128 => copy_values!(u128),
        PhysicalType::Float => copy_values!(f32),
        PhysicalType::Double => copy_values!(f64),
        PhysicalType::Varchar => copy_values!(StringT),
        _ => {
            let mut offset = 0;
            for (part, &count) in parts.iter().zip(counts) {
                for row in 0..count {
                    result.set_value(offset + row, &part.get_value(row))?;
                }
                offset += count;
            }
        }
    }
    Ok(result)
}

/// The best rows seen so far (DuckDB's `TopNHeap`): a max-heap of at most
/// `heap_size` entries over the payload.
#[derive(Debug)]
pub struct TopNHeap {
    types: Vec<LogicalType>,
    modifiers: Vec<OrderModifiers>,
    executor: ExpressionExecutor,
    heap_size: usize,
    heap: BinaryHeap<TopNEntry>,
    data: Payload,
    /// Arrival counter for tie-breaking.
    sequence: u64,
}

impl TopNHeap {
    /// When the payload holds this many rows, it is compacted (DuckDB's `ReduceThreshold`).
    fn reduce_threshold(&self) -> usize {
        (STANDARD_VECTOR_SIZE * 5).max(self.heap_size.saturating_mul(2))
    }

    /// Whether a row with `key` (arriving after every row in the heap) belongs in it
    /// (DuckDB's `EntryShouldBeAdded`).
    fn should_add(&self, key: &[u8]) -> bool {
        if self.heap_size == 0 {
            return false;
        }
        if self.heap.len() < self.heap_size {
            return true;
        }
        self.heap.peek().is_some_and(|top| key < &*top.key)
    }

    fn add_entry(&mut self, entry: TopNEntry) {
        if self.heap.len() >= self.heap_size {
            self.heap.pop();
        }
        self.heap.push(entry);
    }

    /// DuckDB's `TopNHeap::Sink`.
    fn sink(&mut self, input: &DataChunk) -> Result<()> {
        if self.heap_size == 0 || input.is_empty() {
            return Ok(());
        }
        let mut sort_chunk = DataChunk::default();
        self.executor.execute(input, &mut sort_chunk)?;
        let columns: Vec<&Vector> = sort_chunk.columns().iter().collect();
        let keys = create_sort_keys(&columns, &self.modifiers, input.len())?;
        let mut selected: Vec<u32> = Vec::new();
        for row in 0..input.len() {
            let key = keys.key(row);
            if !self.should_add(key) {
                continue;
            }
            let entry = TopNEntry {
                key: key.into(),
                sequence: self.sequence,
                index: self.data.count + selected.len(),
            };
            self.sequence += 1;
            selected.push(row as u32);
            // Entries are added one by one so later rows of the chunk see the new top.
            self.add_entry(entry);
        }
        if !selected.is_empty() {
            self.data.append(input, &selected);
        }
        Ok(())
    }

    /// Drops the payload rows that are no longer in the heap (DuckDB's `Reduce`).
    fn reduce(&mut self) -> Result<()> {
        if self.data.count < self.reduce_threshold() {
            return Ok(());
        }
        self.compact()
    }

    /// Rebuilds the payload with only the heap's rows.
    fn compact(&mut self) -> Result<()> {
        let all = self.data.concatenate(&self.types)?;
        let mut entries = std::mem::take(&mut self.heap).into_vec();
        let sel: Vec<u32> = entries.iter().map(|entry| entry.index as u32).collect();
        for (position, entry) in entries.iter_mut().enumerate() {
            entry.index = position;
        }
        self.data = Payload::default();
        self.data.append(&all, &sel);
        self.heap = entries.into();
        Ok(())
    }

    /// Merges `other` (DuckDB's `Combine`).
    fn combine(&mut self, other: Self) -> Result<()> {
        let all = other.data.concatenate(&other.types)?;
        let mut selected: Vec<u32> = Vec::new();
        for mut entry in other.heap.into_sorted_vec() {
            if !self.should_add(&entry.key) {
                break;
            }
            selected.push(entry.index as u32);
            entry.index = self.data.count + selected.len() - 1;
            entry.sequence = self.sequence;
            self.sequence += 1;
            self.add_entry(entry);
        }
        if !selected.is_empty() {
            self.data.append(&all, &selected);
        }
        self.reduce()
    }

    /// The rows in order (DuckDB's `Finalize` + `InitializeScan`), skipping `offset`.
    fn finish(&mut self, offset: usize) -> Result<TopNResult> {
        let all = self.data.concatenate(&self.types)?;
        let order: Vec<u32> = std::mem::take(&mut self.heap)
            .into_sorted_vec()
            .into_iter()
            .skip(offset)
            .map(|entry| entry.index as u32)
            .collect();
        Ok(TopNResult { data: all, order })
    }
}

/// The finished result: the payload and the order to emit its rows in.
#[derive(Debug)]
struct TopNResult {
    data: DataChunk,
    order: Vec<u32>,
}

/// The global heap and, once finalized, the result.
#[derive(Debug)]
pub struct TopNGlobalState {
    heap: Mutex<Option<TopNHeap>>,
    /// The smallest boundary key of any full heap so far.
    boundary: Mutex<Option<Box<[u8]>>>,
    result: Option<TopNResult>,
}

impl Sink for PhysicalTopN {
    type Global = TopNGlobalState;
    type Local = TopNHeap;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<TopNGlobalState> {
        if let Some((data, _)) = &self.dynamic_filter {
            data.reset();
        }
        Ok(TopNGlobalState {
            heap: Mutex::new(Some(self.heap()?)),
            boundary: Mutex::new(None),
            result: None,
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &TopNGlobalState) -> Result<TopNHeap> {
        self.heap()
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &TopNGlobalState,
        local: &mut TopNHeap,
    ) -> Result<SinkResult> {
        local.sink(chunk)?;
        self.update_boundary(global, local)?;
        local.reduce()?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &TopNGlobalState, local: TopNHeap) -> Result<()> {
        let mut heap = global.heap.lock().unwrap_or_else(PoisonError::into_inner);
        let heap = heap
            .as_mut()
            .ok_or_else(|| Error::internal("TOP_N combine after finalize"))?;
        heap.combine(local)
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut TopNGlobalState) -> Result<SinkFinalize> {
        let mut heap = global
            .heap
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| Error::internal("TOP_N finalized twice"))?;
        let offset = usize::try_from(self.offset).unwrap_or(usize::MAX);
        global.result = Some(heap.finish(offset)?);
        Ok(SinkFinalize::Ready)
    }
}

/// The finalized sink state and the next row to emit.
#[derive(Debug)]
pub struct TopNSourceState {
    sink: SinkState,
    position: Mutex<usize>,
}

impl Source for PhysicalTopN {
    type Global = TopNSourceState;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<TopNSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("TOP_N source requires its sink state"))?;
        Ok(TopNSourceState {
            sink: Arc::clone(sink),
            position: Mutex::new(0),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &TopNSourceState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &TopNSourceState,
        (): &mut (),
    ) -> Result<SourceResult> {
        if self.limit == 0 {
            return Ok(SourceResult::Finished);
        }
        let state = global
            .sink
            .downcast_ref::<TopNGlobalState>()
            .ok_or_else(|| Error::internal("TOP_N sink state has the wrong type"))?;
        let Some(result) = &state.result else {
            return Ok(SourceResult::Finished);
        };
        let mut position = global.position.lock().unwrap_or_else(PoisonError::into_inner);
        if *position >= result.order.len() {
            return Ok(SourceResult::Finished);
        }
        let count = STANDARD_VECTOR_SIZE.min(result.order.len() - *position);
        let sel = SelectionVector::new(result.order[*position..*position + count].to_vec());
        *position += count;
        let columns = result
            .data
            .columns()
            .iter()
            .map(|column| column.slice(&sel))
            .collect();
        chunk.set_columns(columns, count);
        Ok(SourceResult::HaveMoreOutput)
    }
}
