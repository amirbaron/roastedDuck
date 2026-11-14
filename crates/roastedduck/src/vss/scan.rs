//! HNSW graph candidates followed by transactional table fetches.

use std::any::Any;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_catalog::{CatalogEntry, EntryKind};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{FunctionClient, FunctionData, ParallelTableScan, TableFunction};
use roastedduck_planner::ClientContext;
use roastedduck_planner::operator::COLUMN_IDENTIFIER_ROW_ID;
use roastedduck_storage::checkpoint_lock::SharedCheckpointGuard;
use roastedduck_storage::index::IndexEntry;
use roastedduck_storage::index::hnsw::HnswIndex;
use roastedduck_storage::table::{StorageIndex, TableScanState};
use roastedduck_storage::{DataTable, TableFilterSet};
use roastedduck_types::{DataChunk, SelectionVector, Vector};

pub(super) use super::search::search_rows;

#[derive(Clone, Debug)]
pub(super) struct ScanData {
    pub table_entry: Arc<CatalogEntry>,
    pub table: Arc<DataTable>,
    pub index: Arc<IndexEntry>,
    pub query: Vec<f32>,
    pub limit: usize,
    pub client: Arc<FunctionClient>,
    pub filters: Option<TableFilterSet>,
}

impl FunctionData for ScanData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn data(bind: Option<&dyn FunctionData>) -> Result<&ScanData> {
    bind.and_then(|bind| bind.as_any().downcast_ref::<ScanData>())
        .ok_or_else(|| Error::internal("HNSW scan without optimizer bind data"))
}

pub(super) fn ef_search(context: &ClientContext) -> Option<usize> {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting("hnsw_ef_search").ok())
        .and_then(|value| value.as_i64())
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
}

/// Real graph search with adaptive overfetch only for MVCC-invisible rows.
/// This primitive does not consider user predicates; filtered scans expand its
/// requested budget separately. The caller holds the table's index-scan vacuum
/// guard until these row IDs have been fetched.
pub(super) fn visible_candidates(
    context: &ClientContext,
    table: &Arc<DataTable>,
    index: &Arc<IndexEntry>,
    query: &[f32],
    limit: usize,
) -> Result<Vec<i64>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let override_ef = ef_search(context);
    let mut requested = limit;
    loop {
        let (neighbors, available) = {
            let state = index.lock();
            let graph = state
                .index
                .as_secondary()
                .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
                .ok_or_else(|| Error::internal("HNSW scan references another index type"))?;
            let available = graph.searchable_count();
            let neighbors = graph
                .search(query, requested.min(available), override_ef)?
                .neighbors;
            (neighbors, available)
        };
        let row_ids: Vec<i64> = neighbors.iter().map(|neighbor| neighbor.label).collect();
        let mut visible = Vec::with_capacity(limit.min(row_ids.len()));
        for batch in row_ids.chunks(STANDARD_VECTOR_SIZE) {
            let mut fetched = DataChunk::default();
            table.fetch(
                &context.transaction,
                &mut fetched,
                &[StorageIndex::row_id()],
                batch,
            )?;
            for row in 0..fetched.len() {
                let row_id = fetched
                    .column(0)
                    .get_value(row)
                    .as_i64()
                    .ok_or_else(|| Error::internal("HNSW visibility fetch returned a non-rowid"))?;
                visible.push(row_id);
                if visible.len() == limit {
                    return Ok(visible);
                }
            }
        }
        if requested >= available || row_ids.len() < requested {
            return Ok(visible);
        }
        requested = requested.saturating_mul(2).min(available);
    }
}

/// Expand genuine graph search until the SQL predicate admits LIMIT visible
/// rows, or the index's initial searchable population has been requested. This
/// avoids tie-dependent underfill without scanning the table as an exact search.
fn filtered_candidates(
    context: &ClientContext,
    bind: &ScanData,
    columns: &[StorageIndex],
) -> Result<(Vec<i64>, bool)> {
    let Some(filters) = bind.filters.as_ref().filter(|filters| !filters.is_empty()) else {
        return visible_candidates(context, &bind.table, &bind.index, &bind.query, bind.limit)
            .map(|rows| (rows, false));
    };
    if bind.limit == 0 {
        return Ok((Vec::new(), false));
    }
    let bound = {
        let state = bind.index.lock();
        state
            .index
            .as_secondary()
            .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
            .ok_or_else(|| Error::internal("HNSW filtered scan references another index type"))?
            .searchable_count()
    };
    let mut requested = bind.limit.min(bound);
    let mut expanded = false;
    let mut fetch_columns = columns.to_vec();
    fetch_columns.push(StorageIndex::row_id());
    loop {
        let candidates = visible_candidates(context, &bind.table, &bind.index, &bind.query, requested)?;
        let mut eligible = HashSet::new();
        for batch in candidates.chunks(STANDARD_VECTOR_SIZE) {
            let mut fetched = DataChunk::default();
            bind.table
                .fetch(&context.transaction, &mut fetched, &fetch_columns, batch)?;
            apply_filters(&mut fetched, Some(filters))?;
            for row in 0..fetched.len() {
                let row_id = fetched
                    .column(columns.len())
                    .get_value(row)
                    .as_i64()
                    .ok_or_else(|| Error::internal("HNSW predicate fetch returned a non-rowid"))?;
                eligible.insert(row_id);
            }
        }
        // Fetch can group row IDs internally; preserve graph distance order by
        // selecting from the original candidate list, not the fetch's order.
        let selected: Vec<i64> = candidates
            .into_iter()
            .filter(|id| eligible.contains(id))
            .take(bind.limit)
            .collect();
        if selected.len() == bind.limit || requested >= bound {
            return Ok((selected, expanded));
        }
        requested = requested
            .saturating_mul(2)
            .max(requested.saturating_add(1))
            .min(bound);
        expanded = true;
    }
}

pub(super) fn storage_columns(entry: &CatalogEntry, column_ids: &[u64]) -> Result<Vec<StorageIndex>> {
    let EntryKind::Table(table) = &entry.kind else {
        return Err(Error::internal("HNSW scan table is not a table"));
    };
    column_ids
        .iter()
        .map(|&column| {
            if column == COLUMN_IDENTIFIER_ROW_ID {
                return Ok(StorageIndex::row_id());
            }
            let physical = usize::try_from(column)
                .ok()
                .and_then(|column| table.logical_to_physical(column))
                .ok_or_else(|| Error::internal("HNSW scan of an unknown/generated column"))?;
            Ok(StorageIndex::from(physical))
        })
        .collect()
}

#[derive(Debug)]
enum Cursor {
    Graph { row_ids: Vec<i64>, position: usize },
    Exact(Box<TableScanState>),
}

#[derive(Debug)]
struct ScanGlobal {
    context: Arc<ClientContext>,
    table: Arc<DataTable>,
    columns: Vec<StorageIndex>,
    cursor: Mutex<Cursor>,
    mode: &'static str,
    filters: Option<TableFilterSet>,
    _vacuum_guard: Option<SharedCheckpointGuard>,
}

fn apply_filters(chunk: &mut DataChunk, filters: Option<&TableFilterSet>) -> Result<()> {
    let Some(filters) = filters else { return Ok(()) };
    let mut selected: Vec<u32> = (0..chunk.len() as u32).collect();
    for (&column, filter) in &filters.filters {
        let vector = chunk
            .columns()
            .get(column)
            .ok_or_else(|| Error::internal("HNSW residual filter column out of range"))?;
        filter.select(vector, chunk.len(), &mut selected)?;
    }
    if selected.len() != chunk.len() {
        let selected = SelectionVector::new(selected);
        chunk.set_columns(Vector::slice_all(chunk.columns(), &selected), selected.len());
    }
    Ok(())
}

/// The internal scan is optimizer-bound; there is intentionally no SQL bind callback.
pub(super) fn scan_function() -> TableFunction {
    let mut function = TableFunction::new("hnsw_index_scan", Vec::new());
    function.static_explain = Some(Arc::new(|bind| {
        data(bind).map_or_else(
            |_| Vec::new(),
            |bind| {
                vec![
                    ("Table".into(), bind.table_entry.name().into()),
                    ("HNSW Index".into(), bind.index.name().clone()),
                ]
            },
        )
    }));
    function.projection_pushdown = true;
    function.projected_output = true;
    function.filter_pushdown = false;
    // Preserve the original predicates. The real graph candidate budget grows
    // when they would otherwise underfill LIMIT; exact fallbacks apply them to
    // the visible table scan before the retained TopN/min_by operator.
    function.pushdown_filters = Some(Arc::new(|bind, _, filters| {
        let mut bind = data(Some(bind))?.clone();
        bind.filters = Some(
            filters
                .downcast_ref::<TableFilterSet>()
                .ok_or_else(|| Error::internal("HNSW residual filters have the wrong type"))?
                .clone(),
        );
        Ok(Arc::new(bind))
    }));
    function.cardinality = Some(Arc::new(|bind| data(bind).ok().map(|bind| bind.limit as u64)));
    function.parallel_scan = Some(ParallelTableScan {
        init_global: Arc::new(|bind, column_ids, column_indexes, _| {
            let bind = data(bind)?;
            if column_indexes
                .iter()
                .any(|column| !column.child_indexes.is_empty())
            {
                return Err(Error::internal("HNSW scan does not push down nested fields"));
            }
            let context = super::catalog::current_context(&bind.client)?;
            let vacuum_guard = bind.table.io().index_scan_vacuum_guard();
            let columns = storage_columns(&bind.table_entry, column_ids)?;
            let local_changes = context.transaction.local_storage().storage(&bind.table).is_some();
            let stale_index = !super::search::current_index(&bind.table, &bind.index);
            let (cursor, mode) = if local_changes || stale_index {
                // Prepared plans may outlive transaction-local writes. Keeping the
                // enclosing TopN/min_by operator lets this explicit fallback use
                // the ordinary SQL metric and include all local/visible rows.
                let mut state = TableScanState::default();
                bind.table
                    .initialize_scan(&context.transaction, &mut state, columns.clone(), None)?;
                (
                    Cursor::Exact(Box::new(state)),
                    if local_changes {
                        "Exact scan (transaction-local changes)"
                    } else {
                        "Exact scan (index replaced or dropped)"
                    },
                )
            } else {
                let (row_ids, expanded) = filtered_candidates(&context, bind, &columns)?;
                if super::search::current_index(&bind.table, &bind.index) {
                    (
                        Cursor::Graph { row_ids, position: 0 },
                        if expanded {
                            "HNSW graph traversal (predicate expansion)"
                        } else {
                            "HNSW graph traversal"
                        },
                    )
                } else {
                    let mut state = TableScanState::default();
                    bind.table
                        .initialize_scan(&context.transaction, &mut state, columns.clone(), None)?;
                    (
                        Cursor::Exact(Box::new(state)),
                        "Exact scan (index replaced or dropped)",
                    )
                }
            };
            // Graph IDs have already passed these predicates. Do not evaluate
            // them twice; the exact fallback still needs ordinary filtering.
            let filters = matches!(&cursor, Cursor::Exact(_))
                .then(|| bind.filters.clone())
                .flatten();
            Ok(Arc::new(ScanGlobal {
                context,
                table: Arc::clone(&bind.table),
                columns,
                cursor: Mutex::new(cursor),
                mode,
                filters,
                _vacuum_guard: vacuum_guard,
            }))
        }),
        init_local: Arc::new(|_| Ok(Box::new(0_u64))),
        scan: Arc::new(|_, _, global, local, output| {
            let global = global
                .downcast_ref::<ScanGlobal>()
                .ok_or_else(|| Error::internal("HNSW scan lost global state"))?;
            let batch_index = local
                .downcast_mut::<u64>()
                .ok_or_else(|| Error::internal("HNSW scan lost local state"))?;
            let mut cursor = global.cursor.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                match &mut *cursor {
                    Cursor::Graph { row_ids, position } => {
                        if *position == row_ids.len() {
                            return Ok(false);
                        }
                        let end = (*position + STANDARD_VECTOR_SIZE).min(row_ids.len());
                        global.table.fetch(
                            &global.context.transaction,
                            output,
                            &global.columns,
                            &row_ids[*position..end],
                        )?;
                        *position = end;
                    }
                    Cursor::Exact(state) => {
                        global.table.scan(&global.context.transaction, output, state)?;
                        if output.is_empty() {
                            return Ok(false);
                        }
                    }
                }
                apply_filters(output, global.filters.as_ref())?;
                if !output.is_empty() {
                    *batch_index += 1;
                    return Ok(true);
                }
            }
        }),
        max_threads: Arc::new(|_| 1),
        batch_index: Arc::new(|_, local| local.downcast_ref::<u64>().copied().unwrap_or(0)),
        partition_data: None,
        dynamic_to_string: Some(Arc::new(|_, global| {
            global
                .downcast_ref::<ScanGlobal>()
                .map_or_else(Vec::new, |global| vec![("Execution".into(), global.mode.into())])
        })),
        row_group_metrics: None,
    });
    function
}
