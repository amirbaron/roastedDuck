//! Parallel CTID tasks on transaction-owned PostgreSQL snapshots.
use super::super::transaction::{Cursor, Decoder, PgTransaction};
use super::{PgCatalog, PgTable};
use roastedduck_buffer::BufferManager;
use roastedduck_catalog::{Transaction, backend::BackendScan};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{FunctionClient, FunctionData, ParallelTableScan, TableFunction};
use roastedduck_optimizer::{Optimizer, OptimizerRule};
use roastedduck_planner::{
    LogicalKind, LogicalOperator,
    operator::{BoundLimitNode, COLUMN_IDENTIFIER_ROW_ID},
};
use roastedduck_postgres::{QueryParameter, codec::Codec, pool::Pool};
use roastedduck_types::{DataChunk, LogicalType};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug)]
pub(crate) struct Columns {
    pub names: Vec<String>,
    pub codecs: Vec<Codec>,
}
#[derive(Clone, Debug)]
enum Source {
    Attached(Arc<PgCatalog>),
    Standalone { pool: Arc<Pool>, buffer: BufferManager },
}
impl Source {
    fn catalog(&self) -> Option<&Arc<PgCatalog>> {
        match self {
            Self::Attached(c) => Some(c),
            Self::Standalone { .. } => None,
        }
    }
    fn pool(&self) -> &Arc<Pool> {
        match self {
            Self::Attached(c) => &c.pool,
            Self::Standalone { pool, .. } => pool,
        }
    }
    fn buffer(&self) -> &BufferManager {
        match self {
            Self::Attached(c) => &c.buffer_manager,
            Self::Standalone { buffer, .. } => buffer,
        }
    }
    fn isolation(&self) -> &str {
        self.catalog().map_or("REPEATABLE READ", |c| &c.isolation)
    }
    fn readonly(&self) -> bool {
        self.catalog().is_none_or(|c| c.read_only)
    }
    fn new_transaction(
        &self,
        client: &Arc<FunctionClient>,
        lease: roastedduck_postgres::pool::Lease,
    ) -> Arc<PgTransaction> {
        PgTransaction::new(
            lease,
            client.clone(),
            self.buffer().clone(),
            self.isolation(),
            self.readonly(),
        )
    }
}
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent flags of the pinned PostgresBindData scan plan"
)]
pub(crate) struct Data {
    columns: Columns,
    relation: String,
    table_name: String,
    can_emit_ctid: bool,
    source: Source,
    parameters: Vec<QueryParameter>,
    use_transaction: bool,
    client: Arc<FunctionClient>,
    text: bool,
    version: crate::postgres::version::Version,
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
impl Drop for Global {
    fn drop(&mut self) {
        if self.data.source.catalog().is_none() {
            self.main.close();
        }
    }
}
#[derive(Debug)]
struct Local {
    transaction: Option<Arc<PgTransaction>>,
    auxiliary: bool,
    independent: bool,
    pending: Option<(DataChunk, usize)>,
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
            if self.independent {
                transaction.close();
            } else {
                let _ = transaction.finish(false);
            }
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
                if data.emit_ctid && data.can_emit_ctid {
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
                .columns
                .codecs
                .get(index)
                .ok_or_else(|| Error::internal("Invalid Postgres projected column"))?;
            projections.push(codec.projection(&crate::postgres::identifier(&data.columns.names[index]))?);
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
        data.relation,
        data.limit
    );
    if data.text {
        transaction.cursor_query(&sql, &data.parameters, codecs, data.use_transaction, true)
    } else {
        transaction.copy_cursor(
            &format!("COPY ({sql}) TO STDOUT (FORMAT \"binary\");"),
            codecs,
            data.use_transaction,
        )
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
            independent: false,
            pending: None,
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
        let pool = global.data.source.pool();
        let acquire = || {
            Ok(if first {
                Some(pool.force_acquire(global.data.client.interrupt_flag().as_deref())?)
            } else if pool.snapshot().0.max_connections == 0 {
                Some(pool.acquire(global.data.client.interrupt_flag().as_deref())?)
            } else {
                pool.try_acquire()?
            })
        };
        let lease = if global.data.source.catalog().is_some() {
            crate::postgres::auth::with_oauth(&global.data.client, acquire)?
        } else {
            acquire()?
        };
        let Some(lease) = lease else {
            return Ok(Local {
                transaction: None,
                auxiliary: false,
                independent: false,
                pending: None,
                cursor: None,
                batch: 0,
                finished: true,
            });
        };
        let transaction = global.data.source.new_transaction(&global.data.client, lease);
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
        configure_timeouts(&global.data.client, &transaction)?;
        (transaction, true)
    };
    Ok(Local {
        transaction: Some(transaction),
        auxiliary,
        independent: global.data.source.catalog().is_none(),
        pending: None,
        cursor: None,
        batch: 0,
        finished: false,
    })
}

fn next_chunk(global: &Global, local: &mut Local) -> Result<Option<DataChunk>> {
    if local.finished {
        return Ok(None);
    }
    loop {
        if local.cursor.is_none() {
            let Some((batch, range)) = next_range(global) else {
                local.finished = true;
                if let Some(transaction) = local.transaction.take().filter(|_| local.auxiliary) {
                    if local.independent {
                        transaction.close();
                    } else {
                        transaction.finish(false)?;
                    }
                }
                return Ok(None);
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
            return Ok(Some(chunk));
        }
        local.cursor = None;
        if global.materialized.is_some() {
            local.finished = true;
            return Ok(None);
        }
    }
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
    let read_only = readonly(&catalog, transaction)?;
    let data = Data::new(
        Source::Attached(catalog.clone()),
        client.clone(),
        Columns {
            names: table.names.clone(),
            codecs: table.codecs.clone(),
        },
        table.relation.clone(),
        table.table_name.clone(),
        true,
        catalog.version,
        table.pages,
        read_only,
    );
    let function = function(
        "postgres_scan",
        vec![],
        setting(client, "pg_experimental_filter_pushdown").and_then(|v| v.as_bool()) != Some(false),
    );
    Ok(BackendScan {
        function,
        bind_data: Some(Arc::new(data)),
        virtual_columns: vec![(COLUMN_IDENTIFIER_ROW_ID, "rowid".into(), LogicalType::BIGINT)],
    })
}

fn configure_timeouts(client: &FunctionClient, transaction: &PgTransaction) -> Result<()> {
    for (name, setting_name) in [
        ("pg_statement_timeout_millis", "statement_timeout"),
        (
            "pg_idle_in_transaction_timeout_millis",
            "idle_in_transaction_session_timeout",
        ),
    ] {
        if let Some(value) = setting(client, name).and_then(|v| v.as_i128()) {
            transaction.execute(&format!("SET {setting_name}={value}"), true)?;
        }
    }
    Ok(())
}
impl Data {
    #[allow(clippy::too_many_arguments, reason = "The pinned scan bind metadata")]
    fn new(
        source: Source,
        client: Arc<FunctionClient>,
        columns: Columns,
        relation: String,
        table_name: String,
        can_emit_ctid: bool,
        version: crate::postgres::version::Version,
        approx_pages: i64,
        read_only: bool,
    ) -> Self {
        let text = crate::postgres::use_text_protocol(&client);
        let per_task = setting(&client, "pg_pages_per_task")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .unwrap_or(1000);
        let pages = if version.major() >= 14
            && !text
            && setting(&client, "pg_use_ctid_scan").and_then(|v| v.as_bool()) != Some(false)
        {
            approx_pages.max(0) as u64
        } else {
            0
        };
        let max_threads = if read_only && !text {
            (pages / per_task).max(1) as usize
        } else {
            1
        };
        let text = text || version.redshift();
        let materialize = source.catalog().is_some();
        Self {
            source,
            client,
            columns,
            relation,
            table_name,
            can_emit_ctid,
            parameters: Vec::new(),
            use_transaction: true,
            text,
            version,
            pages,
            per_task,
            max_threads,
            read_only,
            materialize,
            main_allowed: true,
            limit: String::new(),
            predicate: String::new(),
            emit_ctid: false,
        }
    }
    pub(crate) fn query(
        catalog: Arc<PgCatalog>,
        client: Arc<FunctionClient>,
        sql: &str,
        columns: Columns,
        parameters: Vec<QueryParameter>,
        use_transaction: bool,
    ) -> Self {
        let version = catalog.version;
        let mut data = Self::new(
            Source::Attached(catalog),
            client,
            columns,
            format!("({sql}) AS __unnamed_subquery"),
            String::new(),
            false,
            version,
            0,
            false,
        );
        data.parameters = parameters;
        data.use_transaction = use_transaction;
        data
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "The pinned standalone scan bind metadata"
    )]
    pub(crate) fn standalone(
        pool: Arc<Pool>,
        buffer: BufferManager,
        client: Arc<FunctionClient>,
        columns: Columns,
        relation: String,
        table_name: String,
        version: crate::postgres::version::Version,
        pages: i64,
    ) -> Self {
        Self::new(
            Source::Standalone { pool, buffer },
            client,
            columns,
            relation,
            table_name,
            true,
            version,
            pages,
            true,
        )
    }
}
pub(crate) fn function(name: &str, arguments: Vec<LogicalType>, filter_pushdown: bool) -> TableFunction {
    let mut function = TableFunction::new(name, arguments);
    function.cardinality = Some(Arc::new(|bind| {
        let data = data(bind).ok()?;
        let rows_per_page = (8169 / (23 + data.columns.codecs.len() as u64 * 8)).max(1);
        Some(data.pages.saturating_mul(rows_per_page))
    }));
    function.projection_pushdown = true;
    function.filter_pushdown = filter_pushdown;
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
                    data.columns
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
            .map_or_else(Vec::new, |d| vec![("Table".into(), d.table_name.as_str().into())])
    }));
    function.parallel_scan = Some(ParallelTableScan {
        init_global: Arc::new(|bind, columns, _, _| {
            let mut data = data(bind)?.clone();
            let main = if let Some(catalog) = data.source.catalog() {
                let current = data
                    .client
                    .active_transaction()
                    .ok_or_else(|| Error::internal("Postgres scan without transaction"))?;
                let transaction = current
                    .downcast_ref::<Transaction>()
                    .ok_or_else(|| Error::internal("Invalid active transaction"))?;
                if !readonly(catalog, transaction)? {
                    data.max_threads = 1;
                    data.materialize = true;
                    data.main_allowed = true;
                }
                catalog.current(&data.client)?
            } else {
                let lease = data
                    .source
                    .pool()
                    .acquire(data.client.interrupt_flag().as_deref())?;
                let main = data.source.new_transaction(&data.client, lease);
                if data.use_transaction {
                    main.start()?;
                    configure_timeouts(&data.client, &main)?;
                }
                main
            };
            let materialized = if data.materialize {
                let range = (data.pages > 0).then_some((0, u64::from(u32::MAX)));
                let cursor = open_cursor(&data, &main, columns, range)?;
                main.materialize_cursor(&cursor)?;
                Some(cursor)
            } else {
                None
            };
            let snapshot =
                if !data.materialize && data.max_threads > 1 && data.source.isolation() != "READ COMMITTED" {
                    main.snapshot(data.version)?
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
            let mut written = 0;
            loop {
                if local.pending.is_none() {
                    let Some(chunk) = next_chunk(global, local)? else {
                        output.set_len(written);
                        return Ok(written > 0);
                    };
                    if written == 0
                        && (chunk.len() == STANDARD_VECTOR_SIZE
                            || global.data.pages == 0
                            || global.materialized.is_some())
                    {
                        *output = chunk;
                        return Ok(true);
                    }
                    local.pending = Some((chunk, 0));
                }
                let (chunk, offset) = local
                    .pending
                    .as_mut()
                    .ok_or_else(|| Error::internal("Missing Postgres chunk"))?;
                if written == 0 {
                    let types = chunk
                        .columns()
                        .iter()
                        .map(|v| v.logical_type().clone())
                        .collect::<Vec<_>>();
                    *output = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
                }
                let count = (STANDARD_VECTOR_SIZE - written).min(chunk.len() - *offset);
                for column in 0..chunk.column_count() {
                    for row in 0..count {
                        output
                            .column_mut(column)
                            .set_value(written + row, &chunk.column(column).get_value(*offset + row))?;
                    }
                }
                written += count;
                *offset += count;
                if *offset == chunk.len() {
                    local.pending = None;
                }
                if written == STANDARD_VECTOR_SIZE {
                    output.set_len(written);
                    return Ok(true);
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
    function
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
        && let Some(catalog) = data.source.catalog()
    {
        *counts.entry(Arc::as_ptr(catalog) as usize).or_default() += 1;
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
        let multiple = data
            .source
            .catalog()
            .is_some_and(|catalog| counts.get(&(Arc::as_ptr(catalog) as usize)).copied().unwrap_or(0) > 1);
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
