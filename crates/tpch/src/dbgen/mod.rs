//! The TPC-H data generator (DuckDB's `extension/tpch/dbgen`, a port of the TPC's
//! `dbgen` 2.x) and its glue to the database (`dbgen/dbgen.cpp`): creating the eight
//! tables and appending the generated rows to them.
//!
//! The data is the same as DuckDB's, byte for byte: the same random number streams,
//! text pool, distributions and formatting, and the rows are appended in the same order,
//! also when several threads generate them.

mod bm_utils;
mod build;
mod dists;
mod dss;
mod rnd;
mod speed_seed;
mod text;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};

use roastedduck_ast::{
    ColumnDefinition, ColumnList, CompressionType, Constraint, CreateInfo, CreateInfoKind, CreateTableInfo,
    LogicalDependencyList, NotNullConstraint, OnCreateConflict, TableColumnType,
};
use roastedduck_catalog::{Catalog, CatalogType, EntryKind, Transaction};
use roastedduck_common::{CaseInsensitiveMap, Error, LogicalIndex, OrderedMap, Result, STANDARD_VECTOR_SIZE};
use roastedduck_execution::operators::{CatalogTableCreator, TableCreator};
use roastedduck_storage::{DataTable, LocalAppendState, TableConstraints};
use roastedduck_types::{DataChunk, LogicalType, StringHeap, StringT, Value, Vector};

use self::bm_utils::asc_dates;
use self::build::{mk_cust, mk_nation, mk_order, mk_part, mk_region, mk_supp};
use self::dists::distributions;
use self::dss::{
    Code, Customer, DbgenContext, MAX_CHILDREN, MIN_SCALE, ORDERS_PER_CUST, Order, Part, SUPP_PER_PART,
    Supplier, Table,
};
use self::rnd::{row_start, row_stop_h};
use self::speed_seed::{sd_cust, sd_line, sd_nation, sd_order, sd_part, sd_psupp, sd_region, sd_supp};

/// The type of a TPC-H column.
#[derive(Clone, Copy, Debug)]
enum ColumnType {
    Integer,
    BigInt,
    Varchar,
    /// `DECIMAL(15,2)`, appended as its scaled integer.
    Decimal,
    Date,
}

impl ColumnType {
    fn logical_type(self) -> LogicalType {
        match self {
            Self::Integer => LogicalType::INTEGER,
            Self::BigInt => LogicalType::BIGINT,
            Self::Varchar => LogicalType::VARCHAR,
            Self::Decimal => LogicalType::decimal(15, 2),
            Self::Date => LogicalType::DATE,
        }
    }
}

/// The name and columns of a TPC-H table (DuckDB's `RegionInfo`, `NationInfo`, ...).
#[derive(Debug)]
struct TableInfo {
    name: &'static str,
    columns: &'static [(&'static str, ColumnType)],
}

const REGION_INFO: TableInfo = TableInfo {
    name: "region",
    columns: &[
        ("r_regionkey", ColumnType::Integer),
        ("r_name", ColumnType::Varchar),
        ("r_comment", ColumnType::Varchar),
    ],
};

const NATION_INFO: TableInfo = TableInfo {
    name: "nation",
    columns: &[
        ("n_nationkey", ColumnType::Integer),
        ("n_name", ColumnType::Varchar),
        ("n_regionkey", ColumnType::Integer),
        ("n_comment", ColumnType::Varchar),
    ],
};

const SUPPLIER_INFO: TableInfo = TableInfo {
    name: "supplier",
    columns: &[
        ("s_suppkey", ColumnType::BigInt),
        ("s_name", ColumnType::Varchar),
        ("s_address", ColumnType::Varchar),
        ("s_nationkey", ColumnType::Integer),
        ("s_phone", ColumnType::Varchar),
        ("s_acctbal", ColumnType::Decimal),
        ("s_comment", ColumnType::Varchar),
    ],
};

const CUSTOMER_INFO: TableInfo = TableInfo {
    name: "customer",
    columns: &[
        ("c_custkey", ColumnType::BigInt),
        ("c_name", ColumnType::Varchar),
        ("c_address", ColumnType::Varchar),
        ("c_nationkey", ColumnType::Integer),
        ("c_phone", ColumnType::Varchar),
        ("c_acctbal", ColumnType::Decimal),
        ("c_mktsegment", ColumnType::Varchar),
        ("c_comment", ColumnType::Varchar),
    ],
};

const PART_INFO: TableInfo = TableInfo {
    name: "part",
    columns: &[
        ("p_partkey", ColumnType::BigInt),
        ("p_name", ColumnType::Varchar),
        ("p_mfgr", ColumnType::Varchar),
        ("p_brand", ColumnType::Varchar),
        ("p_type", ColumnType::Varchar),
        ("p_size", ColumnType::Integer),
        ("p_container", ColumnType::Varchar),
        ("p_retailprice", ColumnType::Decimal),
        ("p_comment", ColumnType::Varchar),
    ],
};

const PARTSUPP_INFO: TableInfo = TableInfo {
    name: "partsupp",
    columns: &[
        ("ps_partkey", ColumnType::BigInt),
        ("ps_suppkey", ColumnType::BigInt),
        ("ps_availqty", ColumnType::BigInt),
        ("ps_supplycost", ColumnType::Decimal),
        ("ps_comment", ColumnType::Varchar),
    ],
};

const ORDERS_INFO: TableInfo = TableInfo {
    name: "orders",
    columns: &[
        ("o_orderkey", ColumnType::BigInt),
        ("o_custkey", ColumnType::BigInt),
        ("o_orderstatus", ColumnType::Varchar),
        ("o_totalprice", ColumnType::Decimal),
        ("o_orderdate", ColumnType::Date),
        ("o_orderpriority", ColumnType::Varchar),
        ("o_clerk", ColumnType::Varchar),
        ("o_shippriority", ColumnType::Integer),
        ("o_comment", ColumnType::Varchar),
    ],
};

const LINEITEM_INFO: TableInfo = TableInfo {
    name: "lineitem",
    columns: &[
        ("l_orderkey", ColumnType::BigInt),
        ("l_partkey", ColumnType::BigInt),
        ("l_suppkey", ColumnType::BigInt),
        ("l_linenumber", ColumnType::BigInt),
        ("l_quantity", ColumnType::Decimal),
        ("l_extendedprice", ColumnType::Decimal),
        ("l_discount", ColumnType::Decimal),
        ("l_tax", ColumnType::Decimal),
        ("l_returnflag", ColumnType::Varchar),
        ("l_linestatus", ColumnType::Varchar),
        ("l_shipdate", ColumnType::Date),
        ("l_commitdate", ColumnType::Date),
        ("l_receiptdate", ColumnType::Date),
        ("l_shipinstruct", ColumnType::Varchar),
        ("l_shipmode", ColumnType::Varchar),
        ("l_comment", ColumnType::Varchar),
    ],
};

/// The tables in the order `CreateTPCHSchema` creates them.
const SCHEMA_TABLES: [&TableInfo; 8] = [
    &REGION_INFO,
    &NATION_INFO,
    &SUPPLIER_INFO,
    &CUSTOMER_INFO,
    &PART_INFO,
    &PARTSUPP_INFO,
    &ORDERS_INFO,
    &LINEITEM_INFO,
];

/// `get_table_name`: the table a table number stores into, if any.
const fn table_info(table: Table) -> Option<&'static TableInfo> {
    match table {
        Table::Part => Some(&PART_INFO),
        Table::Psupp => Some(&PARTSUPP_INFO),
        Table::Supp => Some(&SUPPLIER_INFO),
        Table::Cust => Some(&CUSTOMER_INFO),
        Table::Order => Some(&ORDERS_INFO),
        Table::Line => Some(&LINEITEM_INFO),
        Table::Nation => Some(&NATION_INFO),
        Table::Region => Some(&REGION_INFO),
        Table::OrderLine | Table::PartPsupp => None,
    }
}

/// The tables `LoadTPCHData` generates (the `table` bit mask), by number.
const GENERATED_TABLES: [Table; 6] = [
    Table::Supp,
    Table::Cust,
    Table::OrderLine,
    Table::PartPsupp,
    Table::Nation,
    Table::Region,
];

/// Where `dbgen` writes: a schema of a catalog, in a transaction.
#[derive(Debug, Clone)]
pub(crate) struct DbgenTarget {
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) schema: roastedduck_common::ByteString,
    pub(crate) transaction: Arc<Transaction>,
}

/// `DBGenWrapper::CreateTPCHSchema`: creates the tables (named with `suffix`) unless
/// they exist.
pub(crate) fn create_tpch_schema(target: &DbgenTarget, suffix: &[u8]) -> Result<()> {
    for table in SCHEMA_TABLES {
        let columns = table
            .columns
            .iter()
            .map(|&(name, ty)| ColumnDefinition {
                name: name.into(),
                logical_type: ty.logical_type(),
                expression: None,
                category: TableColumnType::Standard,
                compression_type: CompressionType::CompressionAuto,
                comment: Value::default(),
                tags: OrderedMap::default(),
            })
            .collect();
        let constraints = (0..table.columns.len())
            .map(|i| {
                Constraint::NotNull(NotNullConstraint {
                    index: LogicalIndex(i as u64),
                })
            })
            .collect();
        let info = CreateInfo {
            catalog: (&*target.catalog.name()).into(),
            schema: target.schema.clone(),
            temporary: false,
            internal: false,
            on_conflict: OnCreateConflict::IgnoreOnConflict,
            sql: roastedduck_common::ByteString::new(),
            comment: Value::default(),
            tags: CaseInsensitiveMap::default(),
            dependencies: LogicalDependencyList::default(),
            kind: CreateInfoKind::Table(Box::new(CreateTableInfo {
                table: roastedduck_common::byte_format!("{}{suffix}", table.name),
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
    }
    Ok(())
}

/// The values of one column of the chunk being filled.
#[derive(Debug)]
enum ColumnBuffer {
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    Varchar(Vec<StringT>, StringHeap),
}

impl ColumnBuffer {
    fn new(ty: ColumnType) -> Self {
        match ty {
            ColumnType::Integer | ColumnType::Date => Self::Int32(Vec::with_capacity(STANDARD_VECTOR_SIZE)),
            ColumnType::BigInt | ColumnType::Decimal => Self::Int64(Vec::with_capacity(STANDARD_VECTOR_SIZE)),
            ColumnType::Varchar => {
                Self::Varchar(Vec::with_capacity(STANDARD_VECTOR_SIZE), StringHeap::default())
            }
        }
    }

    /// The column as a vector of type `ty`; the buffer starts over.
    fn take_vector(&mut self, ty: LogicalType) -> Vector {
        match self {
            Self::Int32(values) => {
                let mut vector = Vector::flat_for_overwrite(ty, values.len());
                vector.values_mut::<i32>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
            Self::Int64(values) => {
                let mut vector = Vector::flat_for_overwrite(ty, values.len());
                vector.values_mut::<i64>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
            Self::Varchar(values, heap) => {
                let mut vector = Vector::flat(ty, values.len());
                vector.add_heap(std::mem::take(heap));
                vector.values_mut::<StringT>()[..values.len()].copy_from_slice(values);
                values.clear();
                vector
            }
        }
    }
}

/// Collects the rows of one table into chunks (the row interface of DuckDB's
/// `InternalAppender`, with `tpch_append_information`'s `append_*` helpers).
#[derive(Debug)]
struct RowAppender {
    table: Table,
    types: Vec<LogicalType>,
    columns: Vec<ColumnBuffer>,
    /// The next column of the current row.
    column: usize,
    rows: usize,
}

impl RowAppender {
    fn new(table: Table, info: &TableInfo) -> Self {
        Self {
            table,
            types: info.columns.iter().map(|&(_, ty)| ty.logical_type()).collect(),
            columns: info
                .columns
                .iter()
                .map(|&(_, ty)| ColumnBuffer::new(ty))
                .collect(),
            column: 0,
            rows: 0,
        }
    }

    fn next_column(&mut self) -> &mut ColumnBuffer {
        let column = &mut self.columns[self.column];
        self.column += 1;
        column
    }

    /// `append_int32`.
    fn int32(&mut self, value: i64) {
        match self.next_column() {
            ColumnBuffer::Int32(values) => values.push(value as i32),
            _ => unreachable!("an INTEGER value for a column of another type"),
        }
    }

    /// `append_int64` / `append_decimal`.
    fn int64(&mut self, value: i64) {
        match self.next_column() {
            ColumnBuffer::Int64(values) => values.push(value),
            _ => unreachable!("a BIGINT value for a column of another type"),
        }
    }

    /// `append_date`: day `index` of the generated date range.
    fn date(&mut self, index: usize) {
        self.int32(i64::from(asc_dates()[index].0));
    }

    /// `append_string`.
    fn string(&mut self, value: &[u8]) {
        match self.next_column() {
            ColumnBuffer::Varchar(values, heap) => values.push(heap.add(value)),
            _ => unreachable!("a VARCHAR value for a column of another type"),
        }
    }

    /// `append_char`.
    fn char(&mut self, value: u8) {
        self.string(&[value]);
    }

    /// `EndRow`: hands a full chunk to `sink`.
    fn end_row(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        debug_assert_eq!(self.column, self.columns.len(), "a row was not filled");
        self.column = 0;
        self.rows += 1;
        if self.rows == STANDARD_VECTOR_SIZE {
            self.flush(sink)?;
        }
        Ok(())
    }

    /// Hands the rows collected so far to `sink`.
    fn flush(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        if self.rows == 0 {
            return Ok(());
        }
        let columns = self
            .columns
            .iter_mut()
            .zip(&self.types)
            .map(|(column, ty)| column.take_vector(ty.clone()))
            .collect();
        let chunk = DataChunk::new(columns, self.rows);
        self.rows = 0;
        sink.push(self.table, chunk)
    }
}

/// Where generated chunks go.
trait ChunkSink {
    fn push(&mut self, table: Table, chunk: DataChunk) -> Result<()>;
}

/// The chunks of one part of the tables, in generation order, kept until the parts
/// before it are appended.
#[derive(Debug, Default)]
struct CollectedChunks {
    chunks: Vec<(Table, DataChunk)>,
}

impl ChunkSink for CollectedChunks {
    fn push(&mut self, table: Table, chunk: DataChunk) -> Result<()> {
        self.chunks.push((table, chunk));
        Ok(())
    }
}

/// The appends into the transaction-local storage of the tables (DuckDB's
/// `TPCHDBgenParameters` with the `LocalAppend` of each appender's flush).
#[derive(Debug)]
struct TableAppends {
    appends: Vec<(Table, Arc<DataTable>, LocalAppendState)>,
}

impl TableAppends {
    /// Looks the tables up and starts appending to them.
    fn new(target: &DbgenTarget, suffix: &[u8]) -> Result<Self> {
        let mut appends = Vec::new();
        for table in Table::ALL {
            let Some(info) = table_info(table) else {
                continue;
            };
            let name = roastedduck_common::byte_format!("{}{suffix}", info.name);
            let entry = target
                .catalog
                .get_entry(&target.transaction, CatalogType::Table, &target.schema, &name)
                .ok_or_else(|| {
                    Error::catalog(roastedduck_common::byte_format!(
                        "Table with name {name} does not exist!"
                    ))
                })?;
            let EntryKind::Table(table_entry) = &entry.kind else {
                return Err(Error::catalog(roastedduck_common::byte_format!(
                    "{name} is not a table"
                )));
            };
            let storage = table_entry
                .storage
                .clone()
                .ok_or_else(|| Error::invalid_input("dbgen is only supported for DuckDB database files"))?;
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
            appends.push((table, storage, state));
        }
        Ok(Self { appends })
    }

    /// `Flush`: publishes the appended rows to the transaction.
    fn finish(self) {
        for (_, storage, state) in self.appends {
            storage.finalize_local_append(state);
        }
    }
}

impl ChunkSink for TableAppends {
    fn push(&mut self, table: Table, chunk: DataChunk) -> Result<()> {
        let (_, storage, state) = self
            .appends
            .iter_mut()
            .find(|(appended, _, _)| *appended == table)
            .ok_or_else(|| Error::internal("dbgen appended to an unknown table"))?;
        storage.local_append(state, &chunk)
    }
}

/// The row appenders of the eight tables (`tpch_append_information` per table).
#[derive(Debug)]
struct RowAppenders {
    part: RowAppender,
    psupp: RowAppender,
    supp: RowAppender,
    cust: RowAppender,
    order: RowAppender,
    line: RowAppender,
    nation: RowAppender,
    region: RowAppender,
}

impl RowAppenders {
    fn new() -> Self {
        Self {
            part: RowAppender::new(Table::Part, &PART_INFO),
            psupp: RowAppender::new(Table::Psupp, &PARTSUPP_INFO),
            supp: RowAppender::new(Table::Supp, &SUPPLIER_INFO),
            cust: RowAppender::new(Table::Cust, &CUSTOMER_INFO),
            order: RowAppender::new(Table::Order, &ORDERS_INFO),
            line: RowAppender::new(Table::Line, &LINEITEM_INFO),
            nation: RowAppender::new(Table::Nation, &NATION_INFO),
            region: RowAppender::new(Table::Region, &REGION_INFO),
        }
    }

    fn all_mut(&mut self) -> [&mut RowAppender; 8] {
        [
            &mut self.part,
            &mut self.psupp,
            &mut self.supp,
            &mut self.cust,
            &mut self.order,
            &mut self.line,
            &mut self.nation,
            &mut self.region,
        ]
    }
}

/// The row buffers and appenders of one generator (DuckDB's `TPCHDataAppender`).
#[derive(Debug)]
struct DataAppender {
    ctx: DbgenContext,
    order: Box<Order>,
    customer: Customer,
    part: Part,
    supplier: Supplier,
    code: Code,
    appenders: RowAppenders,
}

impl DataAppender {
    fn new(ctx: DbgenContext) -> Self {
        Self {
            ctx,
            order: Box::default(),
            customer: Customer::default(),
            part: Part::default(),
            supplier: Supplier::default(),
            code: Code::default(),
            appenders: RowAppenders::new(),
        }
    }

    /// `append_order` and `append_line`.
    fn append_order_line(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let o = &*self.order;
        let appender = &mut self.appenders.order;
        appender.int64(o.okey);
        appender.int64(o.custkey);
        appender.char(o.orderstatus);
        appender.int64(o.totalprice);
        appender.date(o.odate);
        appender.string(o.opriority);
        appender.string(&o.clerk);
        appender.int32(i64::from(o.spriority));
        appender.string(o.comment);
        appender.end_row(sink)?;

        let appender = &mut self.appenders.line;
        for line in &o.l[..o.lines] {
            appender.int64(line.okey);
            appender.int64(line.partkey);
            appender.int64(line.suppkey);
            appender.int64(line.lcnt);
            appender.int64(line.quantity);
            appender.int64(line.eprice);
            appender.int64(line.discount);
            appender.int64(line.tax);
            appender.char(line.rflag);
            appender.char(line.lstatus);
            appender.date(line.sdate);
            appender.date(line.cdate);
            appender.date(line.rdate);
            appender.string(line.shipinstruct);
            appender.string(line.shipmode);
            appender.string(line.comment);
            appender.end_row(sink)?;
        }
        Ok(())
    }

    /// `append_supp`.
    fn append_supp(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let s = &self.supplier;
        let appender = &mut self.appenders.supp;
        appender.int64(s.suppkey);
        appender.string(&s.name);
        appender.string(&s.address);
        appender.int32(s.nation_code);
        appender.string(&s.phone);
        appender.int64(s.acctbal);
        appender.string(&s.comment);
        appender.end_row(sink)
    }

    /// `append_cust`.
    fn append_cust(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let c = &self.customer;
        let appender = &mut self.appenders.cust;
        appender.int64(c.custkey);
        appender.string(&c.name);
        appender.string(&c.address);
        appender.int32(c.nation_code);
        appender.string(&c.phone);
        appender.int64(c.acctbal);
        appender.string(c.mktsegment);
        appender.string(c.comment);
        appender.end_row(sink)
    }

    /// `append_part` and `append_psupp`.
    fn append_part_psupp(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let p = &self.part;
        let appender = &mut self.appenders.part;
        appender.int64(p.partkey);
        appender.string(&p.name);
        appender.string(&p.mfgr);
        appender.string(&p.brand);
        appender.string(p.type_);
        appender.int32(p.size);
        appender.string(p.container);
        appender.int64(p.retailprice);
        appender.string(p.comment);
        appender.end_row(sink)?;

        let appender = &mut self.appenders.psupp;
        for supplier in &p.s[..SUPP_PER_PART] {
            appender.int64(supplier.partkey);
            appender.int64(supplier.suppkey);
            appender.int64(supplier.qty);
            appender.int64(supplier.scost);
            appender.string(supplier.comment);
            appender.end_row(sink)?;
        }
        Ok(())
    }

    /// `append_nation`.
    fn append_nation(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let c = &self.code;
        let appender = &mut self.appenders.nation;
        appender.int32(c.code);
        appender.string(c.text);
        appender.int32(c.join);
        appender.string(c.comment);
        appender.end_row(sink)
    }

    /// `append_region`.
    fn append_region(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        let c = &self.code;
        let appender = &mut self.appenders.region;
        appender.int32(c.code);
        appender.string(c.text);
        appender.string(c.comment);
        appender.end_row(sink)
    }

    /// `gen_tbl`: generates `count` rows of table `tnum` after the first `offset`.
    fn gen_tbl(&mut self, tnum: Table, count: i64, offset: i64, sink: &mut dyn ChunkSink) -> Result<()> {
        for i in offset + 1..=offset + count {
            row_start(&mut self.ctx);
            match tnum {
                Table::Line | Table::Order | Table::OrderLine => {
                    mk_order(i, &mut self.order, &mut self.ctx);
                    self.append_order_line(sink)?;
                }
                Table::Supp => {
                    mk_supp(i, &mut self.supplier, &mut self.ctx);
                    self.append_supp(sink)?;
                }
                Table::Cust => {
                    mk_cust(i, &mut self.customer, &mut self.ctx);
                    self.append_cust(sink)?;
                }
                Table::Psupp | Table::Part | Table::PartPsupp => {
                    mk_part(i, &mut self.part, &mut self.ctx);
                    self.append_part_psupp(sink)?;
                }
                Table::Nation => {
                    mk_nation(i, &mut self.code, &mut self.ctx);
                    self.append_nation(sink)?;
                }
                Table::Region => {
                    mk_region(i, &mut self.code, &mut self.ctx);
                    self.append_region(sink)?;
                }
            }
            row_stop_h(tnum, &mut self.ctx);
        }
        Ok(())
    }

    /// `AppendData`: generates every table, or part `current_step` of `children`
    /// parts of each.
    fn append_data(
        &mut self,
        children: usize,
        current_step: Option<usize>,
        sink: &mut dyn ChunkSink,
    ) -> Result<()> {
        for table in GENERATED_TABLES {
            let mut rowcnt = self.ctx.tdefs[table.index()].base;
            if table.index() < Table::Nation.index() {
                rowcnt *= self.ctx.scale_factor;
            }
            match current_step {
                Some(step) if children > 1 => {
                    let part_size = (rowcnt as f64 / children as f64).ceil() as i64;
                    let part_offset = part_size * step as i64;
                    let part_end = part_offset + part_size;
                    let rows = if part_end > rowcnt {
                        rowcnt - part_offset
                    } else {
                        part_size
                    };
                    skip(table, children, part_offset, &mut self.ctx);
                    if rows > 0 {
                        // Generate part of the table.
                        self.gen_tbl(table, rows, part_offset, sink)?;
                    }
                }
                // Generate the full table.
                _ => self.gen_tbl(table, rowcnt, 0, sink)?,
            }
        }
        Ok(())
    }

    /// Hands the last rows of every table to `sink`.
    fn flush(&mut self, sink: &mut dyn ChunkSink) -> Result<()> {
        for appender in self.appenders.all_mut() {
            appender.flush(sink)?;
        }
        Ok(())
    }
}

/// `skip`: advances the streams of `table` past its first `step` rows.
fn skip(table: Table, children: usize, step: i64, ctx: &mut DbgenContext) {
    match table {
        Table::Cust => sd_cust(step, ctx),
        Table::Supp => sd_supp(step, ctx),
        Table::Nation => sd_nation(step, ctx),
        Table::Region => sd_region(step, ctx),
        Table::OrderLine => {
            sd_line(children, step, ctx);
            sd_order(step, ctx);
        }
        Table::PartPsupp => {
            sd_part(step, ctx);
            sd_psupp(step, ctx);
        }
        Table::Part | Table::Psupp | Table::Order | Table::Line => {}
    }
}

/// The parts of the tables threads generate per scale factor (`CHILDREN_PER_SCALE_FACTOR`).
const CHILDREN_PER_SCALE_FACTOR: f64 = 20.0;

/// `DBGenWrapper::LoadTPCHData`: generates the data of scale factor `flt_scale` into the
/// tables (named with `suffix`) of `target`: all of it, or part `current_step` of
/// `children` parts. Without explicit parts, `threads` threads generate parts of the
/// tables and the parts are appended in order.
pub(crate) fn load_tpch_data(
    target: &DbgenTarget,
    flt_scale: f64,
    suffix: &[u8],
    children: usize,
    current_step: Option<usize>,
    threads: usize,
) -> Result<()> {
    if flt_scale == 0.0 {
        return Ok(());
    }
    let mut base_context = DbgenContext::default();
    let tdefs = &mut base_context.tdefs;
    tdefs[Table::Part.index()].base = 200_000;
    tdefs[Table::Psupp.index()].base = 200_000;
    tdefs[Table::Supp.index()].base = 10_000;
    tdefs[Table::Cust.index()].base = 150_000;
    tdefs[Table::Order.index()].base = 150_000 * ORDERS_PER_CUST;
    tdefs[Table::Line.index()].base = 150_000 * ORDERS_PER_CUST;
    tdefs[Table::OrderLine.index()].base = 150_000 * ORDERS_PER_CUST;
    tdefs[Table::PartPsupp.index()].base = 200_000;
    tdefs[Table::Nation.index()].base = dss::NATIONS_MAX;
    tdefs[Table::Region.index()].base = dss::NATIONS_MAX;

    if flt_scale < MIN_SCALE {
        base_context.scale_factor = 1;
        let int_scale = (1000.0 * flt_scale) as i32;
        for tdef in &mut tdefs[Table::Part.index()..Table::Region.index()] {
            tdef.base = (i64::from(int_scale) * tdef.base) / 1000;
            if tdef.base < 1 {
                tdef.base = 1;
            }
        }
    } else {
        base_context.scale_factor = flt_scale as i64;
    }

    if current_step.is_some_and(|step| step >= children) {
        return Ok(());
    }

    // `load_dists`; the counts are set after the distributions are read.
    let dists = distributions();
    base_context.tdefs[Table::Nation.index()].base = dists.nations.count() as i64;
    base_context.tdefs[Table::Region.index()].base = dists.regions.count() as i64;

    let mut appends = TableAppends::new(target, suffix)?;
    let explicit_partial_generation = children > 1 && current_step.is_some();
    if explicit_partial_generation || threads <= 1 {
        // The parallelism of explicit parts is managed outside of dbgen: only generate
        // the part asked for.
        let mut appender = DataAppender::new(base_context);
        let generated = appender
            .append_data(children, current_step, &mut appends)
            .and_then(|()| appender.flush(&mut appends));
        appends.finish();
        return generated;
    }
    // Twenty parts per scale factor by default.
    let child_count = if flt_scale < 1.0 {
        1
    } else {
        ((CHILDREN_PER_SCALE_FACTOR * flt_scale) as usize).min(MAX_CHILDREN)
    };
    let generated = generate_parallel(&base_context, child_count, threads, &mut appends);
    appends.finish();
    generated
}

/// The progress of a parallel generation, shared by its threads.
#[derive(Debug, Default)]
struct ParallelState {
    /// The number of parts appended (or `usize::MAX` once the generation stopped).
    appended: Mutex<usize>,
    progressed: Condvar,
}

impl ParallelState {
    /// Waits until part `step` may be generated: at most `window` parts ahead of the
    /// appends. `false` once the generation stopped.
    fn wait_for_turn(&self, step: usize, window: usize) -> bool {
        let mut appended = self.appended.lock().unwrap_or_else(PoisonError::into_inner);
        while *appended != usize::MAX && step >= *appended + window {
            appended = self
                .progressed
                .wait(appended)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *appended != usize::MAX
    }

    fn set_appended(&self, count: usize) {
        *self.appended.lock().unwrap_or_else(PoisonError::into_inner) = count;
        self.progressed.notify_all();
    }
}

/// Generates the `child_count` parts of the tables on `threads` threads and appends
/// them in order (DuckDB's batches of `ParallelTPCHAppendTask`s, whose appenders are
/// flushed in step order).
fn generate_parallel(
    base_context: &DbgenContext,
    child_count: usize,
    threads: usize,
    appends: &mut TableAppends,
) -> Result<()> {
    let threads = threads.min(child_count);
    // Parts generated ahead of the appends wait in memory; this bounds them.
    let window = threads * 2;
    let next_step = AtomicUsize::new(0);
    let state = ParallelState::default();
    let (sender, receiver) = mpsc::channel::<(usize, Result<CollectedChunks>)>();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let sender = sender.clone();
            let (next_step, state) = (&next_step, &state);
            scope.spawn(move || {
                loop {
                    let step = next_step.fetch_add(1, Ordering::Relaxed);
                    if step >= child_count || !state.wait_for_turn(step, window) {
                        break;
                    }
                    let mut appender = DataAppender::new(base_context.clone());
                    let mut chunks = CollectedChunks::default();
                    let generated = appender
                        .append_data(child_count, Some(step), &mut chunks)
                        .and_then(|()| appender.flush(&mut chunks))
                        .map(|()| chunks);
                    if sender.send((step, generated)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let appended = append_in_order(&receiver, child_count, &state, appends);
        // Stop the threads still waiting for their turn.
        state.set_appended(usize::MAX);
        drop(receiver);
        appended
    })
}

/// Appends the parts the threads send, in step order.
fn append_in_order(
    receiver: &mpsc::Receiver<(usize, Result<CollectedChunks>)>,
    child_count: usize,
    state: &ParallelState,
    appends: &mut TableAppends,
) -> Result<()> {
    let mut pending = BTreeMap::new();
    let mut next = 0;
    while next < child_count {
        let Ok((step, generated)) = receiver.recv() else {
            return Err(Error::internal("dbgen threads stopped early"));
        };
        pending.insert(step, generated?);
        while let Some(part) = pending.remove(&next) {
            for (table, chunk) in part.chunks {
                appends.push(table, chunk)?;
            }
            next += 1;
            state.set_appended(next);
        }
    }
    Ok(())
}
