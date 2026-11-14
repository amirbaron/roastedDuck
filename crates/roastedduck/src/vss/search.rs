//! Shared execution for vector joins, with a bounded exact fallback when required.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_planner::{ClientContext, Expression};
use roastedduck_storage::DataTable;
use roastedduck_storage::index::IndexEntry;
use roastedduck_storage::index::hnsw::HnswIndex;
use roastedduck_storage::table::{StorageIndex, TableScanState};
use roastedduck_types::{DataChunk, LogicalType, Value};

pub(super) fn current_index(table: &DataTable, index: &Arc<IndexEntry>) -> bool {
    table
        .indexes()
        .find(index.name())
        .is_some_and(|current| Arc::ptr_eq(&current, index))
}

#[derive(Debug)]
struct RankedRow {
    distance: Option<f64>,
    row_id: i64,
}
impl PartialEq for RankedRow {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for RankedRow {}
impl PartialOrd for RankedRow {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RankedRow {
    fn cmp(&self, other: &Self) -> Ordering {
        let distance = match (self.distance, other.distance) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) if left.is_nan() || right.is_nan() => {
                left.is_nan().cmp(&right.is_nan())
            }
            (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        };
        distance.then_with(|| self.row_id.cmp(&other.row_id))
    }
}

fn exact_candidates(
    context: &ClientContext,
    table: &Arc<DataTable>,
    index: &Arc<IndexEntry>,
    query: &[f32],
    limit: usize,
) -> Result<Vec<i64>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let (spec, function) = {
        let state = index.lock();
        let graph = state
            .index
            .as_secondary()
            .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
            .ok_or_else(|| Error::internal("VSS exact fallback references another index type"))?;
        let function = match graph.metric() {
            "l2sq" => "array_distance",
            "cosine" => "array_cosine_distance",
            "ip" => "array_negative_inner_product",
            _ => return Err(Error::internal("VSS exact fallback has an unknown metric")),
        };
        (graph.spec().clone(), function)
    };
    let client = super::catalog::current_context(&context.function_client)?;
    let binder =
        roastedduck_optimizer::Optimizer::new(client, roastedduck_optimizer::OptimizerConfig::default());
    let query = Value::array(
        LogicalType::FLOAT,
        query.iter().copied().map(Value::float).collect(),
    );
    let physical_count = table.columns().len();
    let columns: Vec<_> = (0..physical_count)
        .map(StorageIndex::from)
        .chain(std::iter::once(StorageIndex::row_id()))
        .collect();
    let mut scan = TableScanState::default();
    table.initialize_scan(&context.transaction, &mut scan, columns, None)?;
    let mut heap = BinaryHeap::new();
    let mut chunk = DataChunk::default();
    loop {
        table.scan(&context.transaction, &mut chunk, &mut scan)?;
        if chunk.is_empty() {
            break;
        }
        let table_chunk = DataChunk::new(chunk.columns()[..physical_count].to_vec(), chunk.len());
        let keys = spec.execute_expressions(&table_chunk)?;
        for row in 0..chunk.len() {
            let expression = binder.bind_scalar_function(
                function,
                vec![
                    Expression::constant(keys.column(0).get_value(row)),
                    Expression::constant(query.clone()),
                ],
            )?;
            let value = context.evaluator.evaluate_scalar(&expression)?;
            let distance = if value.is_null() {
                None
            } else {
                Some(
                    value
                        .as_f32()
                        .map(f64::from)
                        .or_else(|| value.as_f64())
                        .ok_or_else(|| {
                            Error::internal("VSS exact SQL distance returned a nonnumeric value")
                        })?,
                )
            };
            let row_id = chunk
                .column(physical_count)
                .get_value(row)
                .as_i64()
                .ok_or_else(|| Error::internal("VSS exact scan returned a non-rowid"))?;
            let ranked = RankedRow { distance, row_id };
            if heap.len() < limit {
                heap.push(ranked);
            } else if heap.peek().is_some_and(|worst| ranked < *worst) {
                heap.pop();
                heap.push(ranked);
            }
        }
    }
    Ok(heap.into_sorted_vec().into_iter().map(|row| row.row_id).collect())
}

/// Actual graph traversal and transactional fetch, or an explicitly reported
/// exact fallback when local writes/stale index handles require one. Returned
/// rows retain candidate rank order, including mixed local and base row IDs.
pub(super) fn search_rows(
    context: &ClientContext,
    table: &Arc<DataTable>,
    index: &Arc<IndexEntry>,
    query: &[f32],
    limit: usize,
    columns: &[StorageIndex],
) -> Result<(DataChunk, bool)> {
    let _guard = table.io().index_scan_vacuum_guard();
    let mut exact =
        context.transaction.local_storage().storage(table).is_some() || !current_index(table, index);
    let mut row_ids = if exact {
        exact_candidates(context, table, index, query, limit)?
    } else {
        super::scan::visible_candidates(context, table, index, query, limit)?
    };
    // DROP may have reset the retained index handle between the initial lookup
    // and traversal. Already copied row IDs survive later drops, but a reset
    // during traversal must not turn this snapshot's result into an empty set.
    if !exact && !current_index(table, index) {
        exact = true;
        row_ids = exact_candidates(context, table, index, query, limit)?;
    }
    let mut fetch_columns = columns.to_vec();
    fetch_columns.push(StorageIndex::row_id());
    let mut output = DataChunk::default();
    let mut emitted = 0;
    for ids in row_ids.chunks(STANDARD_VECTOR_SIZE) {
        let mut fetched = DataChunk::default();
        table.fetch(&context.transaction, &mut fetched, &fetch_columns, ids)?;
        if output.column_count() == 0 {
            let types = fetched.columns()[..columns.len()]
                .iter()
                .map(|column| column.logical_type().clone())
                .collect::<Vec<_>>();
            output = DataChunk::with_types(&types, row_ids.len().max(1));
        }
        let mut positions = HashMap::new();
        for row in 0..fetched.len() {
            let row_id = fetched
                .column(columns.len())
                .get_value(row)
                .as_i64()
                .ok_or_else(|| Error::internal("HNSW aligned fetch returned a non-rowid"))?;
            positions.insert(row_id, row);
        }
        for id in ids {
            let Some(&position) = positions.get(id) else {
                continue;
            };
            for column in 0..columns.len() {
                output
                    .column_mut(column)
                    .set_value(emitted, &fetched.column(column).get_value(position))?;
            }
            emitted += 1;
        }
    }
    output.set_len(emitted);
    Ok((output, exact))
}
