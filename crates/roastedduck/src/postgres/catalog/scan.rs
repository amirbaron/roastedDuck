//! Parallel CTID tasks on transaction-owned PostgreSQL snapshots.
use super::super::transaction::{Cursor, Decoder, PgTransaction};
use super::{PgCatalog, PgTable};
use roastedduck_catalog::{Transaction, backend::BackendScan};
use roastedduck_common::{Error, Result};
use roastedduck_function::{FunctionClient, FunctionData, ParallelTableScan, TableFunction};
use roastedduck_optimizer::{Optimizer, OptimizerRule};
use roastedduck_planner::{
    LogicalKind, LogicalOperator,
    operator::{BoundLimitNode, COLUMN_IDENTIFIER_ROW_ID},
};
use roastedduck_types::LogicalType;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent flags of the pinned PostgresBindData scan plan"
)]
struct Data {
    table: PgTable,
    catalog: Arc<PgCatalog>,
    client: Arc<FunctionClient>,
    text: bool,
    pages: u64,
    per_task: u64,
    max_threads: usize,
    read_only: bool,
    materialize: bool,
    main_allowed: bool,
    limit: String,
    predicate: String,
    emit_ctid: bool,
}
impl FunctionData for Data {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[derive(Debug, Default)]
struct Tasks {
    next: u64,
    batch: u64,
    main_used: bool,
    serial_used: bool,
}
#[derive(Debug)]
struct Global {
    data: Data,
    main: Arc<PgTransaction>,
    snapshot: String,
    tasks: Mutex<Tasks>,
    columns: Vec<u64>,
    materialized: Option<Arc<Mutex<Cursor>>>,
}
#[derive(Debug)]
struct Local {
    transaction: Option<Arc<PgTransaction>>,
    auxiliary: bool,
    cursor: Option<Arc<Mutex<Cursor>>>,
    batch: u64,
    finished: bool,
}
impl Drop for Local {
    fn drop(&mut self) {
        self.cursor = None;
        if self.auxiliary
            && let Some(transaction) = self.transaction.take()
        {
            let _ = transaction.finish(false);
        }
    }
}
fn setting(client: &FunctionClient, name: &str) -> Option<roastedduck_types::Value> {
    client.settings().and_then(|s| s.current_setting(name).ok())
}
fn readonly(catalog: &PgCatalog, transaction: &Transaction) -> Result<bool> {
    let name = catalog.native()?.name();
    Ok(!transaction
        .modified_database()
        .is_some_and(|modified| modified.eq_ignore_ascii_case(&name)))
}
fn data(bind: Option<&dyn FunctionData>) -> Result<&Data> {
    bind.and_then(|d| d.as_any().downcast_ref::<Data>())
        .ok_or_else(|| Error::internal("Missing Postgres scan data"))
}
fn global(state: &roastedduck_function::ParallelScanGlobal) -> Result<&Global> {
    state
        .as_ref()
        .downcast_ref::<Global>()
        .ok_or_else(|| Error::internal("Invalid Postgres scan state"))
}
fn open_cursor(
    data: &Data,
    transaction: &PgTransaction,
    columns: &[u64],
    range: Option<(u64, u64)>,
) -> Result<Arc<Mutex<Cursor>>> {
    let mut projections = Vec::new();
    let mut codecs = Vec::new();
    for &id in columns {
        if id == COLUMN_IDENTIFIER_ROW_ID {
            projections.push(
                if data.emit_ctid && data.table.emit_ctid {
                    "ctid"
                } else {
                    "NULL::bigint"
                }
                .to_owned(),
            );
            codecs.push(if data.text {
                Decoder::TextCtid
            } else {
                Decoder::Ctid
            });
        } else {
            let index = id as usize;
            let codec = data
                .table
                .codecs
                .get(index)
                .ok_or_else(|| Error::internal("Invalid Postgres projected column"))?;
            projections.push(codec.projection(&crate::postgres::identifier(&data.table.names[index]))?);
            codecs.push(if data.text {
                Decoder::Text(codec.logical_type.clone())
            } else {
                Decoder::Value(codec.clone())
            });
        }
    }
    if projections.is_empty() {
        projections.push("NULL".into());
    }
    let mut predicate = range.map_or_else(String::new, |(start, end)| {
        format!(" WHERE ctid BETWEEN '({start},0)'::tid AND '({end},0)'::tid")
    });
    if !data.predicate.is_empty() {
        predicate += if predicate.is_empty() { " WHERE " } else { " AND " };
        predicate += &data.predicate;
    }
    let sql = format!(
        "SELECT {} FROM {}{predicate}{}",
        projections.join(", "),
        data.table.relation,
        data.limit
    );
    if data.text {
        transaction.cursor(&sql, &[], codecs, true, true)
    } else {
        transaction.copy_cursor(&format!("COPY ({sql}) TO STDOUT (FORMAT \"binary\");"), codecs)
    }
}
fn next_range(global: &Global) -> Option<(u64, Option<(u64, u64)>)> {
    let mut tasks = global.tasks.lock().unwrap_or_else(PoisonError::into_inner);
    if global.data.pages == 0 {
        if tasks.serial_used {
            return None;
        }
        tasks.serial_used = true;
        return Some((0, None));
    }
    if tasks.next >= global.data.pages {
        return None;
    }
    let start = tasks.next;
    let mut end = start.saturating_add(global.data.per_task);
    if end >= global.data.pages || end > u64::from(u32::MAX) {
        end = u64::from(u32::MAX);
    }
    tasks.next = end;
    let batch = tasks.batch;
    tasks.batch += 1;
    Some((batch, Some((start, end))))
}
fn initialize_local(global: &Global) -> Result<Local> {
    if let Some(cursor) = &global.materialized {
        return Ok(Local {
            transaction: None,
            auxiliary: false,
            cursor: Some(cursor.clone()),
            batch: 0,
            finished: false,
        });
    }
    let first = {
        let mut tasks = global.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        let first = !tasks.main_used;
        tasks.main_used = true;
        first
    };
    let (transaction, auxiliary) = if first && global.data.main_allowed {
        (global.main.clone(), false)
    } else {
        let pool = &global.data.catalog.pool;
        let lease = if first {
            Some(pool.force_acquire(global.data.client.interrupt_flag().as_deref())?)
        } else if pool.snapshot().0.max_connections == 0 {
            Some(pool.acquire(global.data.client.interrupt_flag().as_deref())?)
        } else {
            pool.try_acquire()?
        };
        let Some(lease) = lease else {
            return Ok(Local {
                transaction: None,
                auxiliary: false,
                cursor: None,
                batch: 0,
                finished: true,
            });
        };
        let catalog = &global.data.catalog;
        let transaction = PgTransaction::new(
            lease,
            global.data.client.clone(),
            catalog.buffer_manager.clone(),
            &catalog.isolation,
            catalog.read_only,
        );
        transaction.start()?;
        if !global.snapshot.is_empty() {
            transaction.execute(
                &format!(
                    "SET TRANSACTION SNAPSHOT {}",
                    crate::postgres::literal(&global.snapshot)
                ),
                true,
            )?;
        }
        for (name, setting_name) in [
            ("pg_statement_timeout_millis", "statement_timeout"),
            (
                "pg_idle_in_transaction_timeout_millis",
                "idle_in_transaction_session_timeout",
            ),
        ] {
            if let Some(value) = setting(&global.data.client, name).and_then(|v| v.as_i128()) {
                transaction.execute(&format!("SET {setting_name}={value}"), true)?;
            }
        }
        (transaction, true)
    };
    Ok(Local {
        transaction: Some(transaction),
        auxiliary,
        cursor: None,
        batch: 0,
        finished: false,
    })
}

pub(super) fn bind(
    table: &PgTable,
    client: &Arc<FunctionClient>,
    transaction: &Transaction,
) -> Result<BackendScan> {
    let catalog = table
        .catalog
        .upgrade()
        .ok_or_else(|| Error::internal("Postgres catalog closed"))?;
    let text = crate::postgres::use_text_protocol(client);
    let per_task = setting(client, "pg_pages_per_task")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or(1000);
    let pages = if catalog.version >= 140_000
        && !text
        && setting(client, "pg_use_ctid_scan").and_then(|v| v.as_bool()) != Some(false)
    {
        table.pages.max(0) as u64
    } else {
        0
    };
    let read_only = readonly(&catalog, transaction)?;
    let max_threads = if read_only && !text {
        (pages / per_task).max(1) as usize
    } else {
        1
    };
    let bind = Data {
        table: table.clone(),
        catalog,
        client: client.clone(),
        text,
        pages,
        per_task,
        max_threads,
        read_only,
        materialize: true,
        main_allowed: true,
        limit: String::new(),
        predicate: String::new(),
        emit_ctid: false,
    };
    let mut function = TableFunction::new("postgres_scan", vec![]);
    function.cardinality = Some(Arc::new(|bind| {
        let data = data(bind).ok()?;
        let rows_per_page = (8169 / (23 + data.table.codecs.len() as u64 * 8)).max(1);
        Some(data.pages.saturating_mul(rows_per_page))
    }));
    function.projection_pushdown = true;
    function.filter_pushdown =
        setting(client, "pg_experimental_filter_pushdown").and_then(|v| v.as_bool()) != Some(false);
    function.filter_support = Some(Arc::new(|_, _, filter| {
        filter
            .downcast_ref::<roastedduck_storage::table_filter::TableFilter>()
            .is_some_and(super::filter::supported)
    }));
    function.pushdown_filters = Some(Arc::new(|bind, columns, filters| {
        let mut data = data(Some(bind))?.clone();
        let filters = filters
            .downcast_ref::<roastedduck_storage::table_filter::TableFilterSet>()
            .ok_or_else(|| Error::internal("Invalid Postgres table filters"))?;
        let mut predicates = Vec::new();
        for (&position, filter) in &filters.filters {
            let id = *columns
                .get(position)
                .ok_or_else(|| Error::internal("Invalid Postgres filter column"))?;
            let virtual_column = id == COLUMN_IDENTIFIER_ROW_ID;
            let column = if virtual_column {
                "ctid".to_owned()
            } else {
                crate::postgres::identifier(
                    data.table
                        .names
                        .get(id as usize)
                        .ok_or_else(|| Error::internal("Invalid Postgres column"))?,
                )
            };
            let expression = super::filter::expression(&column, filter, virtual_column)?;
            if !expression.is_empty() {
                predicates.push(expression);
            }
        }
        data.predicate = predicates.join(" AND ");
        Ok(Arc::new(data) as Arc<dyn FunctionData>)
    }));
    function.prepare_modify = Some(Arc::new(|bind| {
        let mut data = data(Some(bind))?.clone();
        data.materialize = true;
        data.max_threads = 1;
        data.main_allowed = true;
        data.emit_ctid = true;
        Ok(Arc::new(data) as Arc<dyn FunctionData>)
    }));
    function.projected_output = true;
    function.static_explain = Some(Arc::new(|data| {
        data.and_then(|d| d.as_any().downcast_ref::<Data>())
            .map_or_else(Vec::new, |d| {
                vec![("Table".into(), d.table.table_name.as_str().into())]
            })
    }));
    function.parallel_scan = Some(ParallelTableScan {
        init_global: Arc::new(|bind, columns, _, _| {
            let mut data = data(bind)?.clone();
            let current = data
                .client
                .active_transaction()
                .ok_or_else(|| Error::internal("Postgres scan without transaction"))?;
            let transaction = current
                .downcast_ref::<Transaction>()
                .ok_or_else(|| Error::internal("Invalid active transaction"))?;
            if !readonly(&data.catalog, transaction)? {
                data.max_threads = 1;
                data.materialize = true;
                data.main_allowed = true;
            }
            let main = data.catalog.current(&data.client)?;
            let materialized = if data.materialize {
                let range = (data.pages > 0).then_some((0, u64::from(u32::MAX)));
                let cursor = open_cursor(&data, &main, columns, range)?;
                main.materialize_cursor(&cursor)?;
                Some(cursor)
            } else {
                None
            };
            let snapshot =
                if !data.materialize && data.max_threads > 1 && data.catalog.isolation != "READ COMMITTED" {
                    main.snapshot()?
                } else {
                    String::new()
                };
            Ok(Arc::new(Global {
                data,
                main,
                snapshot,
                tasks: Mutex::default(),
                columns: columns.to_vec(),
                materialized,
            }) as roastedduck_function::ParallelScanGlobal)
        }),
        init_local: Arc::new(|state| {
            Ok(Box::new(initialize_local(global(state)?)?) as roastedduck_function::ParallelScanLocal)
        }),
        scan: Arc::new(|_, _, state, local, output| {
            let global = global(state)?;
            let local = local
                .downcast_mut::<Local>()
                .ok_or_else(|| Error::internal("Invalid local Postgres scan"))?;
            if local.finished {
                output.set_len(0);
                return Ok(false);
            }
            loop {
                if local.cursor.is_none() {
                    let Some((batch, range)) = next_range(global) else {
                        local.finished = true;
                        local
                            .transaction
                            .take()
                            .filter(|_| local.auxiliary)
                            .map(|t| t.finish(false))
                            .transpose()?;
                        output.set_len(0);
                        return Ok(false);
                    };
                    local.batch = batch;
                    local.cursor = Some(open_cursor(
                        &global.data,
                        local
                            .transaction
                            .as_deref()
                            .ok_or_else(|| Error::internal("Postgres scan lost connection"))?,
                        &global.columns,
                        range,
                    )?);
                }
                let chunk = local
                    .cursor
                    .as_ref()
                    .ok_or_else(|| Error::internal("Missing scan cursor"))?
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .next()?;
                if let Some(chunk) = chunk {
                    *output = chunk;
                    return Ok(true);
                }
                local.cursor = None;
                if global.materialized.is_some() {
                    local.finished = true;
                    output.set_len(0);
                    return Ok(false);
                }
            }
        }),
        max_threads: Arc::new(|state| {
            global(state).map_or(1, |g| if g.data.materialize { 1 } else { g.data.max_threads })
        }),
        batch_index: Arc::new(|_, local| local.downcast_ref::<Local>().map_or(0, |l| l.batch)),
        partition_data: None,
        row_group_metrics: None,
        dynamic_to_string: None,
    });
    Ok(BackendScan {
        function,
        bind_data: Some(Arc::new(bind)),
        virtual_columns: vec![(COLUMN_IDENTIFIER_ROW_ID, "rowid".into(), LogicalType::BIGINT)],
    })
}

#[derive(Debug)]
pub(crate) struct Rule;
impl OptimizerRule for Rule {
    fn name(&self) -> &'static str {
        "postgres_scan"
    }
    fn post_optimize(&self, _: &mut Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
        push_limit(&mut plan);
        let mut counts = HashMap::new();
        count(&plan, &mut counts);
        configure(&mut plan, &counts);
        Ok(plan)
    }
}
fn count(plan: &LogicalOperator, counts: &mut HashMap<usize, usize>) {
    if let LogicalKind::Get(get) = &plan.kind
        && let Some(data) = get
            .bind_data
            .as_ref()
            .and_then(|d| d.as_any().downcast_ref::<Data>())
    {
        *counts.entry(Arc::as_ptr(&data.catalog) as usize).or_default() += 1;
    }
    for child in &plan.children {
        count(child, counts);
    }
}
fn configure(plan: &mut LogicalOperator, counts: &HashMap<usize, usize>) {
    if let LogicalKind::Get(get) = &mut plan.kind
        && let Some(data) = get
            .bind_data
            .as_ref()
            .and_then(|d| d.as_any().downcast_ref::<Data>())
    {
        let mut data = data.clone();
        let multiple = counts
            .get(&(Arc::as_ptr(&data.catalog) as usize))
            .copied()
            .unwrap_or(0)
            > 1;
        data.materialize = multiple && !(data.max_threads > 1 && data.read_only);
        data.main_allowed = !multiple || data.materialize;
        get.bind_data = Some(Arc::new(data));
    }
    for child in &mut plan.children {
        configure(child, counts);
    }
}
fn push_limit(plan: &mut LogicalOperator) {
    if let LogicalKind::Limit { limit, offset } = &plan.kind
        && matches!(limit, BoundLimitNode::Unset | BoundLimitNode::ConstantValue(_))
        && matches!(offset, BoundLimitNode::Unset | BoundLimitNode::ConstantValue(_))
    {
        let mut clause = String::new();
        if let BoundLimitNode::ConstantValue(n) = limit {
            let _ = write!(clause, " LIMIT {n}");
        }
        if let BoundLimitNode::ConstantValue(n) = offset {
            let _ = write!(clause, " OFFSET {n}");
        }
        if !clause.is_empty()
            && let Some(child) = plan.children.first_mut()
            && set_limit(child, &clause)
        {
            *plan = plan.children.remove(0);
            return;
        }
    }
    for child in &mut plan.children {
        push_limit(child);
    }
}
fn set_limit(plan: &mut LogicalOperator, clause: &str) -> bool {
    if matches!(plan.kind, LogicalKind::Projection { .. }) {
        return plan.children.first_mut().is_some_and(|c| set_limit(c, clause));
    }
    if let LogicalKind::Get(get) = &mut plan.kind
        && let Some(data) = get
            .bind_data
            .as_ref()
            .and_then(|d| d.as_any().downcast_ref::<Data>())
    {
        let mut data = data.clone();
        data.limit = clause.into();
        data.pages = 0;
        data.max_threads = 1;
        get.bind_data = Some(Arc::new(data));
        return true;
    }
    false
}
