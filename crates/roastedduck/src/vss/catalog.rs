//! Actual HNSW catalog inspection and compaction.

use std::any::Any;
use std::sync::Arc;

use roastedduck_catalog::{CatalogType, EntryKind};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE, byte_format};
use roastedduck_function::{
    FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_planner::ClientContext;
use roastedduck_storage::index::hnsw::HnswIndex;
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

/// Resolve execution-time context rather than retaining a prepared statement's
/// original transaction in its bind data.
pub(super) fn current_context(client: &FunctionClient) -> Result<Arc<ClientContext>> {
    client
        .transaction_context()
        .and_then(|context| context.downcast::<ClientContext>().ok())
        .ok_or_else(|| Error::internal("VSS execution without an active transaction context"))
}

pub(crate) fn execute_compact(context: &ClientContext, parameters: &[Value]) -> Result<()> {
    if parameters.len() != 1 {
        return Err(Error::binder("Expected one argument for hnsw_compact_index"));
    }
    if parameters[0].logical_type() != &LogicalType::VARCHAR {
        return Err(Error::binder("Expected a string argument for hnsw_compact_index"));
    }
    let name = parameters[0].to_varchar()?;
    let (catalog, schema, index_name) = crate::sequence::parse_qualified_name(&name)?;
    let (catalog, schema) = context.bind_schema_or_catalog(catalog, schema)?;
    let found = context
        .get_entry(&catalog, &schema, CatalogType::Index, &index_name, false)?
        .ok_or_else(|| Error::internal("VSS index lookup returned no entry"))?;
    let EntryKind::Index(index) = &found.entry.kind else {
        return Err(Error::internal("VSS index lookup returned another entry type"));
    };
    let table = context
        .get_entry(
            &found.catalog,
            &index.schema,
            CatalogType::Table,
            &index.info.table,
            false,
        )?
        .ok_or_else(|| Error::internal("VSS index table lookup returned no entry"))?;
    let EntryKind::Table(table) = &table.entry.kind else {
        return Err(Error::internal("VSS index belongs to a non-table"));
    };
    if let Some(storage) = &table.storage
        && let Some(entry) = storage.indexes().find(found.entry.name())
    {
        let mut state = entry.lock();
        if let Some(index) = state.index.as_secondary_mut()
            && index.as_any().is::<HnswIndex>()
        {
            return index.compact();
        }
    }
    Err(Error::binder(byte_format!("Index {name} not found")))
}

#[derive(Debug)]
struct InfoBindData {
    client: Arc<FunctionClient>,
}

impl FunctionData for InfoBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
struct InfoRows(Vec<Vec<Value>>);

fn info_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for catalog in &context.catalogs {
        for schema in catalog.schemas().scan(&context.transaction) {
            let Some(contents) = schema.as_schema() else {
                continue;
            };
            for entry in contents.indexes.scan(&context.transaction) {
                let EntryKind::Index(definition) = &entry.kind else {
                    continue;
                };
                if !definition.info.index_type_name.eq_ignore_ascii_case("HNSW") {
                    continue;
                }
                let table = catalog
                    .get_entry(
                        &context.transaction,
                        CatalogType::Table,
                        &definition.schema,
                        &definition.info.table,
                    )
                    .ok_or_else(|| Error::internal("HNSW catalog entry has no table"))?;
                let EntryKind::Table(table) = &table.kind else {
                    return Err(Error::internal("HNSW catalog entry belongs to a non-table"));
                };
                let index = table
                    .storage
                    .as_ref()
                    .and_then(|storage| storage.indexes().find(entry.name()))
                    .ok_or_else(|| Error::binder(byte_format!("Index {} not found", entry.name())))?;
                let state = index.lock();
                let graph = state
                    .index
                    .as_secondary()
                    .and_then(|index| index.as_any().downcast_ref::<HnswIndex>())
                    .ok_or_else(|| Error::binder(byte_format!("Index {} not found", entry.name())))?;
                let stats = graph.stats();
                let level_type = LogicalType::struct_type(
                    ["nodes", "edges", "max_edges", "allocated_bytes"]
                        .into_iter()
                        .map(|name| (name.into(), LogicalType::BIGINT))
                        .collect(),
                );
                let levels = stats
                    .levels_stats
                    .iter()
                    .map(|level| {
                        Value::struct_value(vec![
                            ("nodes".into(), Value::bigint(level.nodes as i64)),
                            ("edges".into(), Value::bigint(level.edges as i64)),
                            ("max_edges".into(), Value::bigint(level.max_edges as i64)),
                            (
                                "allocated_bytes".into(),
                                Value::bigint(level.allocated_bytes as i64),
                            ),
                        ])
                    })
                    .collect();
                rows.push(vec![
                    Value::try_varchar(&catalog.name())?,
                    Value::try_varchar(schema.name())?,
                    Value::try_varchar(entry.name())?,
                    Value::try_varchar(&definition.info.table)?,
                    Value::varchar(graph.metric()),
                    Value::bigint(graph.dimensions() as i64),
                    Value::bigint(graph.active_count() as i64),
                    Value::bigint(stats.capacity as i64),
                    Value::bigint(stats.approx_memory_usage as i64),
                    Value::bigint(stats.levels as i64),
                    Value::list(level_type, levels),
                ]);
            }
        }
    }
    Ok(rows)
}

pub(super) fn index_info_function() -> TableFunctionSet {
    let function = TableFunction::new("pragma_hnsw_index_info", Vec::new())
        .with_bind(|input| {
            let context = input
                .binder
                .and_then(|binder| binder.downcast_ref::<ClientContext>())
                .ok_or_else(|| Error::internal("HNSW index info bound without client context"))?;
            let (names, return_types) = super::index_info_schema();
            Ok(TableFunctionBindOutput {
                names,
                return_types,
                bind_data: Some(Arc::new(InfoBindData {
                    client: Arc::clone(&context.function_client),
                })),
            })
        })
        .with_scan(|data, _, state, output| {
            let data = data
                .and_then(|data| data.as_any().downcast_ref::<InfoBindData>())
                .ok_or_else(|| Error::internal("HNSW index info scan without bind data"))?;
            if state.local.is_none() {
                state.local = Some(Box::new(InfoRows(info_rows(
                    current_context(&data.client)?.as_ref(),
                )?)));
            }
            let rows = &state
                .local
                .as_ref()
                .and_then(|local| local.downcast_ref::<InfoRows>())
                .ok_or_else(|| Error::internal("HNSW index info scan lost its cursor"))?
                .0;
            let start = (state.position as usize).min(rows.len());
            let end = (start + STANDARD_VECTOR_SIZE).min(rows.len());
            let (_, types) = super::index_info_schema();
            let mut vectors: Vec<_> = types
                .into_iter()
                .map(|ty| Vector::flat(ty, (end - start).max(1)))
                .collect();
            for (position, row) in rows[start..end].iter().enumerate() {
                for (vector, value) in vectors.iter_mut().zip(row) {
                    vector.set_value(position, value)?;
                }
            }
            *output = DataChunk::new(vectors, end - start);
            state.position = end as u64;
            Ok(end < rows.len())
        });
    let mut set = TableFunctionSet::new("pragma_hnsw_index_info");
    set.add(function);
    set
}
