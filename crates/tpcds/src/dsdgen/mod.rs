//! The TPC-DS data generator (DuckDB's `extension/tpcds/dsdgen`, a port of the TPC's
//! `dsdgen`) and its glue to the database (`dsdgen.cpp`): creating the 24 tables and
//! appending the generated rows to them.
//!
//! The data is the same as DuckDB's, value for value: the same random number streams,
//! distributions and arithmetic, and the rows are appended in the same order.

mod address;
mod append_info;
mod build_support;
#[allow(dead_code, reason = "generated: every column of `columns.h`")]
mod columns;
mod constants;
mod context;
mod date;
mod decimal;
mod dist;
mod genrand;
mod join;
mod pricing;
mod scaling;
mod scd;
mod schema;
mod skip_days;
#[allow(dead_code, reason = "generated: every table of `tables.h`")]
mod tables;
mod tdefs;
mod tdefs_data;
mod text;
mod w_call_center;
mod w_catalog_page;
mod w_catalog_sales;
mod w_customer;
mod w_customer_address;
mod w_customer_demographics;
mod w_datetbl;
mod w_household_demographics;
mod w_inventory;
mod w_item;
mod w_promotion;
mod w_store;
mod w_store_sales;
mod w_timetbl;
mod w_web_page;
mod w_web_sales;
mod w_web_site;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use rustdb_ast::{
    ColumnDefinition, ColumnList, CompressionType, Constraint, CreateInfo, CreateInfoKind, CreateTableInfo,
    LogicalDependencyList, NotNullConstraint, OnCreateConflict, TableColumnType, UniqueConstraint,
};
use rustdb_catalog::{Catalog, CatalogType, EntryKind, Transaction};
use rustdb_common::{CaseInsensitiveMap, Error, LogicalIndex, OrderedMap, Result};
use rustdb_execution::operators::{CatalogTableCreator, TableCreator};
use rustdb_storage::{DataTable, LocalAppendState, TableConstraints};
use rustdb_types::{DataChunk, Value};

use self::append_info::{ChunkSink, RowAppender};
use self::columns::{CD_NULLS, INV_NULLS, INV_QUANTITY_ON_HAND};
use self::context::Context;
use self::schema::SCHEMA_TABLES;
use self::tables::{
    CALL_CENTER, CATALOG_PAGE, CATALOG_SALES, CUSTOMER, CUSTOMER_ADDRESS, CUSTOMER_DEMOGRAPHICS, DATET,
    DBGEN_VERSION, HOUSEHOLD_DEMOGRAPHICS, INCOME_BAND, INVENTORY, ITEM, PROMOTION, REASON, SHIP_MODE, STORE,
    STORE_SALES, TIME, WAREHOUSE, WEB_PAGE, WEB_SALES, WEB_SITE,
};
use self::tdefs::{FL_CHILD, FL_PARENT, get_simple_tdefs_by_number};

/// The type of a TPC-DS column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnType {
    BigInt,
    Integer,
    Varchar,
    Date,
    /// `DECIMAL(width, scale)`, appended as its scaled integer.
    Decimal(u8, u8),
}

/// The name, columns and primary key of a TPC-DS table (DuckDB's `CallCenterInfo`, ...).
#[derive(Debug)]
pub(crate) struct TableInfo {
    name: &'static str,
    columns: &'static [(&'static str, ColumnType)],
    primary_key: &'static [&'static str],
}

/// Where `dsdgen` writes: a schema of a catalog, in a transaction.
#[derive(Debug, Clone)]
pub(crate) struct DsdgenTarget {
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) schema: String,
    pub(crate) transaction: Arc<Transaction>,
}

/// `CreateTPCDSTable`.
fn create_tpcds_table(
    target: &DsdgenTarget,
    table: &TableInfo,
    suffix: &str,
    keys: bool,
    overwrite: bool,
) -> Result<()> {
    let columns = table
        .columns
        .iter()
        .map(|&(name, ty)| ColumnDefinition {
            name: name.to_owned(),
            logical_type: ty.logical_type(),
            expression: None,
            category: TableColumnType::Standard,
            compression_type: CompressionType::CompressionAuto,
            comment: Value::default(),
            tags: OrderedMap::default(),
        })
        .collect();
    let mut constraints = Vec::new();
    if keys {
        constraints.push(Constraint::Unique(UniqueConstraint {
            index: LogicalIndex(u64::MAX),
            columns: table.primary_key.iter().map(|&name| name.to_owned()).collect(),
            is_primary_key: true,
        }));
        // `Binder::BindCreateTableInfo`: a PRIMARY KEY makes its columns NOT NULL.
        for &key in table.primary_key {
            if let Some(index) = table.columns.iter().position(|&(name, _)| name == key) {
                constraints.push(Constraint::NotNull(NotNullConstraint {
                    index: LogicalIndex(index as u64),
                }));
            }
        }
    }
    let info = CreateInfo {
        catalog: target.catalog.name().to_string(),
        schema: target.schema.clone(),
        temporary: false,
        internal: false,
        on_conflict: if overwrite {
            OnCreateConflict::ReplaceOnConflict
        } else {
            OnCreateConflict::ErrorOnConflict
        },
        sql: String::new(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind: CreateInfoKind::Table(Box::new(CreateTableInfo {
            table: format!("{}{suffix}", table.name),
            columns: ColumnList { columns },
            constraints,
            query: None,
            partition_keys: Vec::new(),
            sort_keys: Vec::new(),
            options: CaseInsensitiveMap::default(),
        })),
    };
    let creator = CatalogTableCreator {
        catalog: Arc::clone(&target.catalog),
        schema: target.schema.clone(),
        info,
        io: Arc::clone(target.catalog.io()?),
    };
    creator.create(&target.transaction)?;
    Ok(())
}

/// `DSDGenWrapper::CreateTPCDSSchema`: creates the tables (named with `suffix`), with
/// their primary keys if `keys`, replacing existing ones if `overwrite`.
pub(crate) fn create_tpcds_schema(
    target: &DsdgenTarget,
    suffix: &str,
    keys: bool,
    overwrite: bool,
) -> Result<()> {
    for table in SCHEMA_TABLES {
        create_tpcds_table(target, table, suffix, keys, overwrite)?;
    }
    Ok(())
}

/// The appends into the transaction-local storage of a table (DuckDB's
/// `tpcds_append_information`, with the `LocalAppend` of its appender's flushes), in the
/// order of the parts the table is generated in.
#[derive(Debug)]
struct TableWriter {
    state: Mutex<WriterState>,
    /// Signalled when a part was appended or the generation stopped.
    turn: Condvar,
}

#[derive(Debug)]
struct WriterState {
    storage: Arc<DataTable>,
    append: LocalAppendState,
    /// The next part to append.
    next_part: usize,
    /// Parts generated before their turn.
    pending: BTreeMap<usize, Vec<DataChunk>>,
}

impl WriterState {
    fn append(&mut self, chunk: &DataChunk) -> Result<()> {
        self.storage.local_append(&mut self.append, chunk)
    }
}

impl TableWriter {
    /// Looks the tables up and starts appending to them (`DSDGen`'s lookups before the
    /// generation).
    fn all(target: &DsdgenTarget, suffix: &str) -> Result<Vec<Self>> {
        let mut writers = Vec::new();
        for table in CALL_CENTER..DBGEN_VERSION {
            let name = format!("{}{suffix}", get_simple_tdefs_by_number(table).name);
            let entry = target
                .catalog
                .get_entry(&target.transaction, CatalogType::Table, &target.schema, &name)
                .ok_or_else(|| Error::catalog(format!("Table with name {name} does not exist!")))?;
            let EntryKind::Table(table_entry) = &entry.kind else {
                return Err(Error::catalog(format!("{name} is not a table")));
            };
            let storage = table_entry
                .storage
                .clone()
                .ok_or_else(|| Error::invalid_input("dsdgen is only supported for DuckDB database files"))?;
            let constraints = TableConstraints {
                not_null: table_entry
                    .not_null_columns
                    .iter()
                    .filter_map(|&logical| table_entry.logical_to_physical(logical))
                    .collect(),
                column_names: table_entry
                    .physical_columns()
                    .map(|(_, _, column)| column.name.clone())
                    .collect(),
                ..TableConstraints::default()
            };
            let append = storage.initialize_local_append(&target.transaction, constraints)?;
            writers.push(Self {
                state: Mutex::new(WriterState {
                    storage,
                    append,
                    next_part: 0,
                    pending: BTreeMap::new(),
                }),
                turn: Condvar::new(),
            });
        }
        Ok(writers)
    }

    fn lock(&self) -> MutexGuard<'_, WriterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends a chunk of the first part.
    fn append(&self, chunk: &DataChunk) -> Result<()> {
        self.lock().append(chunk)
    }

    /// Waits until part `part` may be generated: at most `window` parts ahead of the
    /// appends. `false` once the generation stopped.
    fn wait_for_turn(&self, part: usize, window: usize, stop: &AtomicBool) -> bool {
        let mut state = self.lock();
        while part >= state.next_part + window && !stop.load(Ordering::Acquire) {
            state = self.turn.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        !stop.load(Ordering::Acquire)
    }

    /// Hands over the chunks of part `part`, appended once the parts before it are.
    fn finish_part(&self, part: usize, chunks: Vec<DataChunk>) -> Result<()> {
        let mut state = self.lock();
        if part != state.next_part {
            state.pending.insert(part, chunks);
            return Ok(());
        }
        let mut chunks = chunks;
        loop {
            for chunk in &chunks {
                state.append(chunk)?;
            }
            state.next_part += 1;
            self.turn.notify_all();
            let next = state.next_part;
            match state.pending.remove(&next) {
                Some(next_chunks) => chunks = next_chunks,
                None => return Ok(()),
            }
        }
    }

    /// Wakes the generation tasks waiting for their turn.
    fn stop(&self) {
        let _state = self.lock();
        self.turn.notify_all();
    }

    /// `Flush`: publishes the appended rows to the transaction.
    fn finish(self) {
        let state = self.state.into_inner().unwrap_or_else(PoisonError::into_inner);
        state.storage.finalize_local_append(state.append);
    }
}

/// Where the chunks of one generation task go: straight to the tables for the first part
/// of a table, kept until the end of the task for the later parts.
#[derive(Debug)]
struct JobSink<'a> {
    writers: &'a [TableWriter],
    part: usize,
    kept: Vec<(usize, DataChunk)>,
}

impl ChunkSink for JobSink<'_> {
    fn push(&mut self, table: usize, chunk: DataChunk) -> Result<()> {
        if self.part == 0 {
            self.writers[table].append(&chunk)
        } else {
            self.kept.push((table, chunk));
            Ok(())
        }
    }
}

/// The row appenders of the tables and where their chunks go (DuckDB's `append_info`
/// array).
pub(crate) struct Output<'a> {
    rows: Vec<RowAppender>,
    sink: &'a mut dyn ChunkSink,
}

impl std::fmt::Debug for Output<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl<'a> Output<'a> {
    fn new(sink: &'a mut dyn ChunkSink) -> Self {
        Self {
            rows: SCHEMA_TABLES
                .iter()
                .enumerate()
                .map(|(table, info)| RowAppender::new(table, info))
                .collect(),
            sink,
        }
    }

    /// The appender of `table`.
    pub(crate) fn row(&mut self, table: usize) -> &mut RowAppender {
        &mut self.rows[table]
    }

    /// `append_row_end` of `table`.
    pub(crate) fn end(&mut self, table: usize) -> Result<()> {
        self.rows[table].end(self.sink)
    }

    /// `appender.Close()` of every table: hands over the last rows.
    fn flush(&mut self) -> Result<()> {
        for row in &mut self.rows {
            row.flush(self.sink)?;
        }
        Ok(())
    }
}

/// The generator of a table that is not a child (`GetTDefFunctionByNumber`'s builder).
#[derive(Debug)]
enum Generator {
    CallCenter(Box<w_call_center::CallCenter>),
    CatalogPage(w_catalog_page::CatalogPage),
    CatalogSales(Box<w_catalog_sales::CatalogSales>),
    Customer(w_customer::Customer),
    CustomerAddress(w_customer_address::CustomerAddress),
    CustomerDemographics(w_customer_demographics::CustomerDemographics),
    Date(w_datetbl::DateDim),
    HouseholdDemographics(w_household_demographics::HouseholdDemographics),
    IncomeBand(w_household_demographics::IncomeBand),
    Inventory(w_inventory::Inventory),
    Item(Box<w_item::Item>),
    Promotion(w_promotion::Promotion),
    Reason(w_promotion::Reason),
    ShipMode(w_promotion::ShipMode),
    Store(Box<w_store::Store>),
    StoreSales(Box<w_store_sales::StoreSales>),
    Time(w_timetbl::TimeDim),
    Warehouse(w_timetbl::Warehouse),
    WebPage(Box<w_web_page::WebPage>),
    WebSales(Box<w_web_sales::WebSales>),
    WebSite(Box<w_web_site::WebSite>),
}

impl Generator {
    /// The generator of `table`, in the state of its first row.
    fn new(table: usize, ctx: &mut Context) -> Result<Self> {
        Ok(match table {
            CALL_CENTER => Self::CallCenter(Box::new(w_call_center::CallCenter::new())),
            CATALOG_PAGE => Self::CatalogPage(w_catalog_page::CatalogPage::new()),
            CATALOG_SALES => Self::CatalogSales(Box::new(w_catalog_sales::CatalogSales::new(ctx))),
            CUSTOMER => Self::Customer(w_customer::Customer::new()),
            CUSTOMER_ADDRESS => Self::CustomerAddress(w_customer_address::CustomerAddress::new()),
            CUSTOMER_DEMOGRAPHICS => {
                Self::CustomerDemographics(w_customer_demographics::CustomerDemographics::new())
            }
            DATET => Self::Date(w_datetbl::DateDim::new()),
            HOUSEHOLD_DEMOGRAPHICS => {
                Self::HouseholdDemographics(w_household_demographics::HouseholdDemographics::new())
            }
            INCOME_BAND => Self::IncomeBand(w_household_demographics::IncomeBand::new()),
            INVENTORY => Self::Inventory(w_inventory::Inventory::new(ctx)),
            ITEM => Self::Item(Box::new(w_item::Item::new())),
            PROMOTION => Self::Promotion(w_promotion::Promotion::new()),
            REASON => Self::Reason(w_promotion::Reason::new()),
            SHIP_MODE => Self::ShipMode(w_promotion::ShipMode::new()),
            STORE => Self::Store(Box::new(w_store::Store::new())),
            STORE_SALES => Self::StoreSales(Box::new(w_store_sales::StoreSales::new(ctx))),
            TIME => Self::Time(w_timetbl::TimeDim::new()),
            WAREHOUSE => Self::Warehouse(w_timetbl::Warehouse::default()),
            WEB_PAGE => Self::WebPage(Box::new(w_web_page::WebPage::new())),
            WEB_SALES => Self::WebSales(Box::new(w_web_sales::WebSales::new(ctx))),
            WEB_SITE => Self::WebSite(Box::new(w_web_site::WebSite::new())),
            _ => return Err(Error::internal("Table generation failed")),
        })
    }

    /// Generates row `index` of the table (and its children's rows).
    fn mk(&mut self, ctx: &mut Context, out: &mut Output<'_>, index: i64) -> Result<()> {
        match self {
            Self::CallCenter(generator) => generator.mk(ctx, out, index),
            Self::CatalogPage(generator) => generator.mk(ctx, out, index),
            Self::CatalogSales(generator) => generator.mk(ctx, out, index),
            Self::Customer(generator) => generator.mk(ctx, out, index),
            Self::CustomerAddress(generator) => generator.mk(ctx, out, index),
            Self::CustomerDemographics(generator) => generator.mk(ctx, out, index),
            Self::Date(generator) => generator.mk(ctx, out, index),
            Self::HouseholdDemographics(generator) => generator.mk(ctx, out, index),
            Self::IncomeBand(generator) => generator.mk(ctx, out, index),
            Self::Inventory(generator) => generator.mk(ctx, out, index),
            Self::Item(generator) => generator.mk(ctx, out, index),
            Self::Promotion(generator) => generator.mk(ctx, out, index),
            Self::Reason(generator) => generator.mk(ctx, out, index),
            Self::ShipMode(generator) => generator.mk(ctx, out, index),
            Self::Store(generator) => generator.mk(ctx, out, index),
            Self::StoreSales(generator) => generator.mk(ctx, out, index),
            Self::Time(generator) => generator.mk(ctx, out, index),
            Self::Warehouse(generator) => generator.mk(ctx, out, index),
            Self::WebPage(generator) => generator.mk(ctx, out, index),
            Self::WebSales(generator) => generator.mk(ctx, out, index),
            Self::WebSite(generator) => generator.mk(ctx, out, index),
        }
    }
}

/// The rows of a table one generation task makes: all of them, or a part of a table whose
/// rows draw the same amount of numbers from each stream.
#[derive(Clone, Copy, Debug)]
struct Job {
    table: usize,
    part: usize,
    first: i64,
    count: i64,
}

/// The rows of a part of the tables generated in parts.
const PART_ROWS: i64 = 1 << 17;

/// The numbers each row of `table` draws from each of its streams, for the tables whose
/// parts can start at any row (`row_skip` with their `nUsedPerRow`, which `nullSet` and
/// the column draws use up exactly).
fn row_draws(table: usize) -> Option<&'static [(usize, i64)]> {
    match table {
        INVENTORY => Some(&[(INV_NULLS, 2), (INV_QUANTITY_ON_HAND, 1)]),
        CUSTOMER_DEMOGRAPHICS => Some(&[(CD_NULLS, 2)]),
        _ => None,
    }
}

/// The generation tasks: every table that is not a child (child tables are generated with
/// their parents), the fact tables first as they take longest.
///
/// The tables draw from streams of their own (the one stream two tables share, `CC_HOURS`,
/// picks `item`'s container from a distribution of one value), so every table, and every
/// part of a table, starts from the streams of a new generation, skipped ahead for a part.
fn jobs(ctx: &Context) -> Vec<Job> {
    const FACT_TABLES: [usize; 3] = [STORE_SALES, CATALOG_SALES, WEB_SALES];
    let order = FACT_TABLES
        .into_iter()
        .chain((CALL_CENTER..DBGEN_VERSION).filter(|table| !FACT_TABLES.contains(table)));
    let mut jobs = Vec::new();
    for table in order {
        if get_simple_tdefs_by_number(table).is(FL_CHILD) {
            continue;
        }
        let rows = ctx.get_rowcount(table);
        if row_draws(table).is_none() {
            jobs.push(Job {
                table,
                part: 0,
                first: 1,
                count: rows,
            });
            continue;
        }
        let mut first = 1;
        let mut part = 0;
        while first <= rows {
            let count = PART_ROWS.min(rows - first + 1);
            jobs.push(Job {
                table,
                part,
                first,
                count,
            });
            first += count;
            part += 1;
        }
    }
    jobs
}

/// Generates the rows of `job` (and of its table's child) into `writers`.
fn run_job(job: Job, scale: f64, writers: &[TableWriter]) -> Result<()> {
    let mut ctx = Context::new(scale)?;
    for &(stream, draws) in row_draws(job.table).unwrap_or_default() {
        ctx.rng.skip_random(stream, draws * (job.first - 1));
    }
    let mut sink = JobSink {
        writers,
        part: job.part,
        kept: Vec::new(),
    };
    let mut out = Output::new(&mut sink);
    let mut generator = Generator::new(job.table, &mut ctx)?;
    for index in job.first..job.first + job.count {
        generator.mk(&mut ctx, &mut out, index)?;
    }
    out.flush()?;
    // The part is done: append it in its turn.
    let tdef = get_simple_tdefs_by_number(job.table);
    let mut tables = vec![job.table];
    if tdef.is(FL_PARENT) {
        tables.push(tdef.param as usize);
    }
    for table in tables {
        let chunks = sink
            .kept
            .extract_if(.., |(kept, _)| *kept == table)
            .map(|(_, chunk)| chunk)
            .collect();
        writers[table].finish_part(job.part, chunks)?;
    }
    Ok(())
}

/// Runs `jobs` on `threads` threads. The parts of a table are appended in order, at most
/// `threads` of them generated ahead of the appends.
fn run_jobs(jobs: &[Job], scale: f64, writers: &[TableWriter], threads: usize) -> Result<()> {
    let threads = threads.clamp(1, jobs.len().max(1));
    let next_job = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let error = Mutex::new(None);
    let worker = || {
        loop {
            let i = next_job.fetch_add(1, Ordering::Relaxed);
            if i >= jobs.len() || stop.load(Ordering::Acquire) {
                break;
            }
            let job = jobs[i];
            if !writers[job.table].wait_for_turn(job.part, threads, &stop) {
                break;
            }
            if let Err(err) = run_job(job, scale, writers) {
                stop.store(true, Ordering::Release);
                for writer in writers {
                    writer.stop();
                }
                error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert(err);
                break;
            }
        }
    };
    if threads == 1 {
        worker();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(worker);
            }
        });
    }
    match error.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// `DSDGenWrapper::DSDGen`: generates the data of scale factor `scale` into the tables
/// (named with `suffix`) of `target`, on `threads` threads. DuckDB generates the tables
/// one after the other; their rows and the order of the rows are the same.
pub(crate) fn dsdgen(target: &DsdgenTarget, scale: f64, suffix: &str, threads: usize) -> Result<()> {
    if scale <= 0.0 {
        // Schema only.
        return Ok(());
    }
    let ctx = Context::new(scale)?;
    let writers = TableWriter::all(target, suffix)?;
    let generated = run_jobs(&jobs(&ctx), scale, &writers, threads);
    for writer in writers {
        writer.finish();
    }
    generated
}
