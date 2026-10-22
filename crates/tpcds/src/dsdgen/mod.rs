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

use std::sync::Arc;

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
use self::context::Context;
use self::schema::SCHEMA_TABLES;
use self::tables::{
    CALL_CENTER, CATALOG_PAGE, CATALOG_SALES, CUSTOMER, CUSTOMER_ADDRESS, CUSTOMER_DEMOGRAPHICS, DATET,
    DBGEN_VERSION, HOUSEHOLD_DEMOGRAPHICS, INCOME_BAND, INVENTORY, ITEM, PROMOTION, REASON, SHIP_MODE, STORE,
    STORE_SALES, TIME, WAREHOUSE, WEB_PAGE, WEB_SALES, WEB_SITE,
};
use self::tdefs::{FL_CHILD, get_simple_tdefs_by_number};

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

/// The appends into the transaction-local storage of the tables (DuckDB's
/// `tpcds_append_information` per table, with the `LocalAppend` of each appender's
/// flush).
#[derive(Debug)]
struct TableAppends {
    appends: Vec<(Arc<DataTable>, LocalAppendState)>,
}

impl TableAppends {
    /// Looks the tables up and starts appending to them.
    fn new(target: &DsdgenTarget, suffix: &str) -> Result<Self> {
        let mut appends = Vec::new();
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
            let state = storage.initialize_local_append(&target.transaction, constraints)?;
            appends.push((storage, state));
        }
        Ok(Self { appends })
    }

    /// `Flush`: publishes the appended rows to the transaction.
    fn finish(self) {
        for (storage, state) in self.appends {
            storage.finalize_local_append(state);
        }
    }
}

impl ChunkSink for TableAppends {
    fn push(&mut self, table: usize, chunk: DataChunk) -> Result<()> {
        let (storage, state) = &mut self.appends[table];
        storage.local_append(state, &chunk)
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

/// Generates every table that is not a child into `out`, in table order.
fn generate(ctx: &mut Context, out: &mut Output<'_>) -> Result<()> {
    for table in CALL_CENTER..DBGEN_VERSION {
        // Child tables are generated with their parents.
        if get_simple_tdefs_by_number(table).is(FL_CHILD) {
            continue;
        }
        let rows = ctx.get_rowcount(table);
        let mut generator = Generator::new(table, ctx)?;
        for index in 1..=rows {
            generator.mk(ctx, out, index)?;
        }
    }
    out.flush()
}

/// `DSDGenWrapper::DSDGen`: generates the data of scale factor `scale` into the tables
/// (named with `suffix`) of `target`.
pub(crate) fn dsdgen(target: &DsdgenTarget, scale: f64, suffix: &str, _threads: usize) -> Result<()> {
    if scale <= 0.0 {
        // Schema only.
        return Ok(());
    }
    let mut ctx = Context::new(scale)?;
    let mut appends = TableAppends::new(target, suffix)?;
    let generated = generate(&mut ctx, &mut Output::new(&mut appends));
    appends.finish();
    generated
}
