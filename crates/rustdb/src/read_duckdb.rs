//! `read_duckdb` (DuckDB's `function/table/read_duckdb.cpp`: `DuckDBMultiFileInfo`,
//! `DuckDBReader` and the parts of `MultiFileFunction` they rely on): reads a table of
//! one or more DuckDB database files. Each file is attached read-only and hidden for as
//! long as it is read, and its table is scanned with the storage's table scan.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rustdb_catalog::{CatalogEntry, EntryKind, Transaction};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_csv::multi_file::{
    COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_FILENAME, FileList, MultiFileBindData, MultiFileOptions,
    bind_options, combine_union_types, create_file_list, hive_parse, multi_file_parameters, parse_paths,
};
use rustdb_function::cast::{BoundCastInfo, CastFunctionSet, CastParameters};
use rustdb_function::{
    FunctionData, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan, TableFunction,
    TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_planner::ClientContext;
use rustdb_planner::operator::{COLUMN_IDENTIFIER_EMPTY, COLUMN_IDENTIFIER_ROW_ID};
use rustdb_storage::{DataTable, ParallelTableScanState, StorageIndex, TableFilter, TableFilterSet};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::AccessMode;
use crate::attached::{AttachOptions, AttachedDatabase};
use crate::database::{Database, DatabaseInstance};

const FUNCTION_NAME: &str = "read_duckdb";

/// The table selection of a `read_duckdb` call (`DuckDBFileReaderOptions`).
#[derive(Clone, Debug, Default)]
struct ReaderOptions {
    schema_name: String,
    table_name: String,
}

impl ReaderOptions {
    /// `DuckDBFileReaderOptions::Matches`.
    fn matches(&self, schema: &str, table: &str) -> bool {
        (self.schema_name.is_empty() || schema.eq_ignore_ascii_case(&self.schema_name))
            && (self.table_name.is_empty() || table.eq_ignore_ascii_case(&self.table_name))
    }

    /// `DuckDBFileReaderOptions::HasSelection`.
    fn has_selection(&self) -> bool {
        !self.table_name.is_empty() || !self.schema_name.is_empty()
    }

    /// `DuckDBFileReaderOptions::PrintOptions` (a schema alone keeps its trailing `", "`).
    fn print_options(&self) -> String {
        let mut options = String::new();
        if !self.schema_name.is_empty() {
            options = format!("schema_name=\"{}\"", self.schema_name);
        }
        if !options.is_empty() {
            options += ", ";
        }
        if !self.table_name.is_empty() {
            options = format!("{options}table_name=\"{}\"", self.table_name);
        }
        options
    }

    /// `DuckDBFileReaderOptions::GetCandidates`: the tables of the file, qualified by
    /// their schema when the name occurs in several schemas.
    fn candidates(&self, tables: &[(String, String)]) -> String {
        if tables.is_empty() {
            return String::new();
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (_, table) in tables {
            *counts.entry(table.to_lowercase()).or_default() += 1;
        }
        let candidates: Vec<String> = tables
            .iter()
            .map(|(schema, table)| {
                if counts.get(&table.to_lowercase()).copied().unwrap_or(0) > 1 {
                    format!("{schema}.{table}")
                } else {
                    table.clone()
                }
            })
            .collect();
        let mut search_term = self.schema_name.clone();
        if !search_term.is_empty() {
            search_term.push('.');
        }
        search_term += &self.table_name;
        rustdb_common::string_util::candidates_error_message(&candidates, &search_term, "Candidates")
    }
}

/// A file attached for reading (`AttachedDatabaseWrapper`): detached again once no reader
/// uses it.
#[derive(Debug)]
struct ReaderDatabase {
    database: Arc<AttachedDatabase>,
    instance: Weak<DatabaseInstance>,
}

impl Drop for ReaderDatabase {
    /// `AttachedDatabaseWrapper::~AttachedDatabaseWrapper`.
    fn drop(&mut self) {
        if let Some(instance) = self.instance.upgrade() {
            instance.databases.detach_if_current(&self.database, false);
        }
    }
}

/// The reader of one database file (`DuckDBReader`): the attached file and the table
/// it reads.
#[derive(Debug)]
struct DuckDbReader {
    path: String,
    #[allow(dead_code, reason = "keeps the file attached while the reader lives")]
    database: Arc<ReaderDatabase>,
    /// The table's entry (`AttachedDatabaseWrapper::table_entry`).
    table: Arc<CatalogEntry>,
    /// The table's columns (generated ones included): name and type.
    columns: Vec<(String, LogicalType)>,
}

impl DuckDbReader {
    /// `DuckDBReader::DuckDBReader`: attaches the file and selects its table.
    fn open(
        instance: &Weak<DatabaseInstance>,
        transaction: &Transaction,
        path: &str,
        options: &ReaderOptions,
    ) -> Result<Self> {
        let database = attach_reader_database(instance, path)?;
        let catalog = &database.database.catalog;
        let mut tables = Vec::new();
        let mut candidate_tables = Vec::new();
        // `Catalog::ScanSchemas` + `SchemaCatalogEntry::Scan(TABLE_ENTRY)`.
        for schema in catalog.schemas().scan(transaction) {
            let Some(contents) = schema.as_schema() else {
                continue;
            };
            for entry in contents.tables.scan(transaction) {
                if !matches!(entry.kind, EntryKind::Table(_)) {
                    continue;
                }
                if options.matches(schema.name(), entry.name()) {
                    tables.push(Arc::clone(&entry));
                }
                candidate_tables.push((schema.name().to_owned(), entry.name().to_owned()));
            }
        }
        if tables.len() != 1 {
            let error_msg = if tables.is_empty() {
                "does not have any tables"
            } else {
                "has multiple tables"
            };
            let extra_info = if options.has_selection() {
                format!(" matching {}", options.print_options())
            } else {
                "\nSelect a table using `table_name='<name>'".to_owned()
            };
            return Err(Error::binder(format!(
                "Database \"{path}\" {error_msg}{extra_info}{}",
                options.candidates(&candidate_tables)
            )));
        }
        let table = tables.swap_remove(0);
        let EntryKind::Table(contents) = &table.kind else {
            return Err(Error::internal("read_duckdb selected a non-table"));
        };
        let columns = contents
            .columns
            .iter()
            .map(|column| (column.name.clone(), column.logical_type.clone()))
            .collect();
        Ok(Self {
            path: path.to_owned(),
            database: Arc::new(database),
            table,
            columns,
        })
    }

    /// The table's definition.
    fn table(&self) -> Result<&rustdb_catalog::TableEntry> {
        match &self.table.kind {
            EntryKind::Table(table) => Ok(table),
            _ => Err(Error::internal("read_duckdb reader without a table")),
        }
    }

    /// The table's storage (`TableCatalogEntry::GetStorage`).
    fn storage(&self) -> Result<Arc<DataTable>> {
        self.table()?
            .storage
            .clone()
            .ok_or_else(|| Error::internal("read_duckdb table without storage"))
    }

    /// `DuckDBReader::NumRows`.
    fn num_rows(&self) -> Result<u64> {
        Ok(self.storage()?.total_rows())
    }
}

/// `DuckDBReader::GetAttachedDatabase`: attaches the file read-only and hidden, under a
/// name no user can attach (DuckDB uses invalid UTF-8; the Rust name uses U+0080), or
/// takes the attachment another reader of the file made.
fn attach_reader_database(instance: &Weak<DatabaseInstance>, path: &str) -> Result<ReaderDatabase> {
    let database = Database::from_instance(instance)
        .ok_or_else(|| Error::internal("read_duckdb on a closed database"))?;
    let name = format!("\u{80}__duckdb_reader_{path}");
    let options = AttachOptions {
        access: AccessMode::ReadOnly,
        hidden: true,
        ..AttachOptions::default()
    };
    loop {
        if let Some(attached) = database.attach(
            &name,
            path,
            &options,
            rustdb_ast::OnCreateConflict::IgnoreOnConflict,
        )? {
            return Ok(ReaderDatabase {
                database: attached,
                instance: instance.clone(),
            });
        }
        // `IGNORE_ON_CONFLICT` returns the database already attached under the name; it
        // may be detached in between, then the attach is retried.
        if let Some(existing) = database.instance().databases.get(&name) {
            return Ok(ReaderDatabase {
                database: existing,
                instance: instance.clone(),
            });
        }
    }
}

/// The bind data of a `read_duckdb` call (`MultiFileBindData` with `DuckDBReadBindData`).
#[derive(Debug)]
struct ReadDuckDbData {
    instance: Weak<DatabaseInstance>,
    /// The transaction the call was bound in: the attached files are read-only, so
    /// every transaction sees the same rows.
    transaction: Arc<Transaction>,
    files: FileList,
    /// The file list `EXPLAIN ANALYZE` shows: the patterns given, until filters prune
    /// the expanded files (`GetDisplayFileList`).
    display_paths: Option<Vec<String>>,
    names: Vec<String>,
    types: Vec<LogicalType>,
    file_options: MultiFileOptions,
    reader_bind: MultiFileBindData,
    options: ReaderOptions,
    /// The reader of the first file, opened while binding and taken by the first scan
    /// (`initial_reader`).
    initial_reader: Mutex<Option<Arc<DuckDbReader>>>,
    /// With `union_by_name`, the readers of all files (`union_readers`).
    union_readers: Vec<Arc<DuckDbReader>>,
    /// `DuckDBReadBindData::initial_file_cardinality`.
    initial_file_cardinality: Option<u64>,
    casts: Arc<CastFunctionSet>,
    /// The filters pushed into the scan, keyed by position in the scanned column ids.
    filters: Option<Arc<TableFilterSet>>,
    /// `index_scan_max_count` and `index_scan_percentage`.
    index_scan: (u64, f64),
}

impl FunctionData for ReadDuckDbData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ReadDuckDbData {
    /// A copy with the pushed-down table filters `filters`.
    fn with_filters(&self, filters: Option<Arc<TableFilterSet>>) -> Self {
        Self {
            instance: self.instance.clone(),
            transaction: Arc::clone(&self.transaction),
            files: self.files.clone(),
            display_paths: self.display_paths.clone(),
            names: self.names.clone(),
            types: self.types.clone(),
            file_options: self.file_options.clone(),
            reader_bind: self.reader_bind.clone(),
            options: self.options.clone(),
            initial_reader: Mutex::new(self.take_initial_reader()),
            union_readers: self.union_readers.clone(),
            initial_file_cardinality: self.initial_file_cardinality,
            casts: Arc::clone(&self.casts),
            filters,
            index_scan: self.index_scan,
        }
    }

    /// A copy that reads only the files at positions `keep` (`PruneReaders`).
    fn with_files(&self, keep: &[usize]) -> Self {
        let files: Vec<String> = keep
            .iter()
            .filter_map(|&index| self.files.files.get(index).cloned())
            .collect();
        let mut result = self.with_filters(self.filters.clone());
        result
            .union_readers
            .retain(|reader| files.iter().any(|file| *file == reader.path));
        let initial = result
            .initial_reader
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if initial
            .as_ref()
            .is_some_and(|reader| !files.iter().any(|file| *file == reader.path))
        {
            *initial = None;
        }
        result.files = FileList { files };
        result.display_paths = None;
        result
    }

    /// Takes the reader opened while binding (`MultiFileFunction::MultiFileInitGlobal`
    /// moves it out of the bind data, so the file is detached once it was read).
    fn take_initial_reader(&self) -> Option<Arc<DuckDbReader>> {
        self.initial_reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The type of scanned column `id`, virtual columns included.
    fn column_type(&self, id: u64) -> LogicalType {
        match id {
            COLUMN_IDENTIFIER_FILENAME => LogicalType::VARCHAR,
            COLUMN_IDENTIFIER_FILE_INDEX => LogicalType::UBIGINT,
            COLUMN_IDENTIFIER_ROW_ID => LogicalType::BIGINT,
            _ => usize::try_from(id)
                .ok()
                .and_then(|index| self.types.get(index).cloned())
                .unwrap_or(LogicalType::BOOLEAN),
        }
    }
}

/// The bind data of a `read_duckdb` scan.
fn bind_data(bind_data: Option<&dyn FunctionData>) -> Result<&ReadDuckDbData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<ReadDuckDbData>())
        .ok_or_else(|| Error::internal("read_duckdb scan without bind data"))
}

/// The value of a VARCHAR option (`StringValue::Get`).
fn string_option(value: &Value) -> Result<String> {
    if value.is_null() {
        return Err(Error::internal("Calling StringValue::Get on a NULL value"));
    }
    value.to_varchar()
}

/// The value of an option of the client, if set (`ClientContext::TryGetCurrentSetting`).
fn setting(context: &ClientContext, name: &str) -> Option<Value> {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting(name).ok())
}

/// `MultiFileFunction::MultiFileBind` with `DuckDBMultiFileInfo`.
fn read_duckdb_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("read_duckdb bound without a client context"))?;
    let instance = crate::database::instance_state(context)?.instance();
    let first = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("read_duckdb without a file argument"))?;
    // `DuckDBMultiFileInfo::GetGlobInput`: a path without glob matching no file is read as
    // a directory of `*.db` files.
    let paths = parse_paths(FUNCTION_NAME, first)?;
    let files = create_file_list(FUNCTION_NAME, &paths, "db")?;
    let mut file_options = MultiFileOptions::default();
    let mut options = ReaderOptions::default();
    let parse_type = |text: &str| rustdb_csv::read_csv::transform_string_to_logical_type(context, text);
    for (key, value) in input.named_parameters {
        if file_options.parse_option(key, value, &parse_type)? {
            continue;
        }
        // `DuckDBMultiFileInfo::ParseOption`.
        match key.as_str() {
            "schema_name" => options.schema_name = string_option(value)?,
            "table_name" => options.table_name = string_option(value)?,
            _ => return Err(Error::not_implemented(format!("Unimplemented option {key}"))),
        }
    }
    file_options.auto_detect_hive_partitioning(&files, &context.casts)?;

    // `MultiFileReader::BindReader` / `BindUnionReader`.
    let transaction = Arc::clone(&context.transaction);
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut union_readers = Vec::new();
    let initial_reader = if file_options.union_by_name {
        let mut union_names_map = std::collections::HashMap::new();
        for path in &files.files {
            let reader = Arc::new(DuckDbReader::open(&instance, &transaction, path, &options)?);
            let (file_names, file_types): (Vec<String>, Vec<LogicalType>) =
                reader.columns.iter().cloned().unzip();
            combine_union_types(
                &file_names,
                &file_types,
                &mut types,
                &mut names,
                &mut union_names_map,
            );
            union_readers.push(reader);
        }
        union_readers.first().cloned()
    } else {
        let first = files
            .first_file()
            .ok_or_else(|| Error::internal("read_duckdb without files"))?;
        let reader = DuckDbReader::open(&instance, &transaction, first, &options)?;
        for (name, ty) in &reader.columns {
            names.push(name.clone());
            types.push(ty.clone());
        }
        Some(Arc::new(reader))
    };
    let reader_bind = bind_options(&file_options, &files, &mut types, &mut names)?;
    // `DuckDBMultiFileInfo::FinalizeBindData`.
    let initial_file_cardinality = initial_reader
        .as_ref()
        .map(|reader| reader.num_rows())
        .transpose()?;
    let index_scan = (
        setting(context, "index_scan_max_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(rustdb_execution::operators::DEFAULT_INDEX_SCAN_MAX_COUNT),
        setting(context, "index_scan_percentage")
            .and_then(|value| value.as_f64())
            .unwrap_or(rustdb_execution::operators::DEFAULT_INDEX_SCAN_PERCENTAGE),
    );
    let data = ReadDuckDbData {
        instance,
        transaction,
        files,
        display_paths: Some(paths),
        names: names.clone(),
        types: types.clone(),
        file_options,
        reader_bind,
        options,
        initial_reader: Mutex::new(initial_reader),
        union_readers,
        initial_file_cardinality,
        casts: Arc::clone(&context.casts),
        filters: None,
        index_scan,
    };
    Ok(TableFunctionBindOutput {
        return_types: types,
        names,
        bind_data: Some(Arc::new(data)),
    })
}

/// Where an output column of a file's scan comes from (the mapping
/// `MultiFileColumnMapper` creates).
#[derive(Debug)]
enum ColumnSource {
    /// Column `index` of the storage scan, as stored.
    Storage(usize),
    /// Column `index` of the storage scan, cast to the global type.
    Cast { index: usize, cast: Box<ColumnCast> },
    /// The same value for every row (filename, hive partitions, missing columns).
    Constant(Value, Vector),
}

/// The cast of a file's column to the type the scan produces (an expression of the
/// `MultiFileColumnMapper`); a cast that does not exist fails when it would run.
#[derive(Debug)]
struct ColumnCast {
    cast: Result<BoundCastInfo>,
    local_name: String,
    local_type: LogicalType,
    target: LogicalType,
}

impl ColumnCast {
    /// Casts a chunk of the column, explaining a failure like `GetExtendedMultiFileError`.
    fn execute(&self, file_name: &str, vector: &Vector, count: usize) -> Result<Vector> {
        let result = self.cast.as_ref().map_err(Clone::clone).and_then(|cast| {
            let mut result = Vector::flat(self.target.clone(), count);
            cast.execute(vector, &mut result, count, &mut CastParameters::new())?;
            Ok(result)
        });
        result.map_err(|error| {
            if error.kind() == rustdb_common::ErrorKind::Internal {
                return error;
            }
            let (local_name, local_type, target) = (&self.local_name, &self.local_type, &self.target);
            Error::conversion(format!(
                "Error while reading file \"{file_name}\": failed to cast column \"{local_name}\" from type {local_type} to {target}: : {}\n\nIn file \"{file_name}\" the column \"{local_name}\" has type {local_type}, but we are trying to read it as type {target}.\nThis can happen when reading multiple duckdb files. The schema information is taken from the first duckdb file by default. Possible solutions:\n* Enable the union_by_name=True option to combine the schema of all duckdb files (https://duckdb.org/docs/stable/data/multiple_files/combining_schemas)\n* Use a COPY statement to automatically derive types from an existing table.",
                error.raw_message()
            ))
        })
    }
}

/// The scan of one file, shared by the threads reading it: the storage scan of its table
/// and how its columns map onto the scanned columns.
#[derive(Debug)]
struct FileScan {
    reader: Arc<DuckDbReader>,
    storage: Arc<DataTable>,
    /// The storage columns scanned.
    column_ids: Vec<StorageIndex>,
    /// One per scanned column id of the call.
    sources: Vec<ColumnSource>,
    /// The filters the storage scan applies, keyed by position in `column_ids`.
    filters: Option<TableFilterSet>,
    /// The filters on cast columns, applied to the output, keyed by output position.
    chunk_filters: Vec<(usize, TableFilter)>,
    /// Whether every output column is the storage column at the same position.
    direct: bool,
    parallel: ParallelTableScanState,
    /// For an index scan, the matching row ids and the next one to fetch.
    index_rows: Option<Vec<i64>>,
    next_index_row: AtomicUsize,
    /// The batch index the file's batches start after.
    batch_offset: u64,
}

impl FileScan {
    /// The number of batch indexes the file uses.
    fn batch_count(&self) -> u64 {
        match &self.index_rows {
            Some(rows) => rows.len().div_ceil(STANDARD_VECTOR_SIZE) as u64,
            None => self.storage.row_group_count() as u64 + 1,
        }
    }
}

/// `MultiFileColumnMapper::ThrowColumnNotFoundError`.
fn column_not_found(data: &ReadDuckDbData, reader: &DuckDbReader, name: &str) -> Error {
    let candidates: Vec<&str> = reader.columns.iter().map(|(name, _)| name.as_str()).collect();
    Error::invalid_input(format!(
        "Failed to read file \"{}\": schema mismatch in glob: column \"{name}\" was read from the original file \"{}\", but could not be found in file \"{}\".\nCandidate names: {}\nIf you are trying to read files with different schemas, try setting union_by_name=True",
        reader.path,
        data.files.first_file().unwrap_or_default(),
        reader.path,
        candidates.join(", ")
    ))
}

/// Maps the scanned columns onto file `file_index` and prepares the scan of its table
/// (`InitializeReader` + `DuckDBReader::TryInitializeScan`). `None` when the filters
/// on constant columns exclude the whole file.
fn prepare_file(
    data: &ReadDuckDbData,
    file_index: usize,
    reader: Arc<DuckDbReader>,
    column_ids: &[u64],
    batch_offset: u64,
) -> Result<Option<FileScan>> {
    let storage = reader.storage()?;
    let table = reader.table()?;
    let path = reader.path.clone();
    let hive_partitions = if data.reader_bind.hive_partitioning_indexes.is_empty() {
        BTreeMap::default()
    } else {
        hive_parse(&path)
    };
    let constant = |value: Value| -> Result<ColumnSource> {
        let vector = Vector::constant(&value)?;
        Ok(ColumnSource::Constant(value, vector))
    };
    let mut storage_ids: Vec<StorageIndex> = Vec::new();
    let mut sources = Vec::with_capacity(column_ids.len());
    for &global_id in column_ids {
        match global_id {
            COLUMN_IDENTIFIER_FILENAME => {
                sources.push(constant(Value::varchar(&path))?);
                continue;
            }
            COLUMN_IDENTIFIER_FILE_INDEX => {
                sources.push(constant(Value::ubigint(file_index as u64))?);
                continue;
            }
            COLUMN_IDENTIFIER_ROW_ID => {
                storage_ids.push(StorageIndex::row_id());
                sources.push(ColumnSource::Storage(storage_ids.len() - 1));
                continue;
            }
            COLUMN_IDENTIFIER_EMPTY => {
                sources.push(constant(Value::null(LogicalType::BOOLEAN))?);
                continue;
            }
            _ => {}
        }
        let Some(global) = usize::try_from(global_id)
            .ok()
            .filter(|&global| global < data.names.len())
        else {
            return Err(Error::internal(format!(
                "read_duckdb scan of unknown column {global_id}"
            )));
        };
        if data.reader_bind.filename_idx == Some(global) {
            sources.push(constant(Value::varchar(&path))?);
            continue;
        }
        if let Some((key, _)) = data
            .reader_bind
            .hive_partitioning_indexes
            .iter()
            .find(|(_, index)| *index == global)
        {
            let value = hive_partitions.get(key).cloned().unwrap_or_default();
            sources.push(constant(data.file_options.hive_partition_value(
                &value,
                key,
                &data.casts,
            )?)?);
            continue;
        }
        let name = &data.names[global];
        let global_type = &data.types[global];
        let Some(local) = reader
            .columns
            .iter()
            .position(|(column, _)| column.eq_ignore_ascii_case(name))
        else {
            if data.file_options.union_by_name {
                sources.push(constant(Value::null(global_type.clone()))?);
                continue;
            }
            return Err(column_not_found(data, &reader, name));
        };
        // `DuckDBReader::TryInitializeScan`: generated columns cannot be read.
        let Some(physical) = table.logical_to_physical(local) else {
            return Err(Error::not_implemented(format!(
                "Unsupported: read_duckdb cannot read generated column {}",
                reader.columns[local].0
            )));
        };
        storage_ids.push(StorageIndex::new(physical as u64));
        let index = storage_ids.len() - 1;
        let local_type = &reader.columns[local].1;
        if local_type == global_type {
            sources.push(ColumnSource::Storage(index));
        } else {
            sources.push(ColumnSource::Cast {
                index,
                cast: Box::new(ColumnCast {
                    cast: data.casts.get_cast_function(local_type, global_type),
                    local_name: reader.columns[local].0.clone(),
                    local_type: local_type.clone(),
                    target: global_type.clone(),
                }),
            });
        }
    }
    // Filters on constant columns decide for the whole file; filters on columns read as
    // stored go into the storage scan, the others apply to the cast output.
    let mut storage_filters = TableFilterSet::default();
    let mut chunk_filters = Vec::new();
    if let Some(filters) = &data.filters {
        for (&position, filter) in &filters.filters {
            match sources.get(position) {
                Some(ColumnSource::Constant(value, _)) => {
                    if !rustdb_parquet::filter::constant_passes(filter, value)? {
                        return Ok(None);
                    }
                }
                Some(ColumnSource::Storage(index)) => storage_filters.push_filter(*index, filter.clone()),
                _ => chunk_filters.push((position, filter.clone())),
            }
        }
    }
    let direct = storage_ids.len() == sources.len()
        && sources
            .iter()
            .enumerate()
            .all(|(position, source)| matches!(source, ColumnSource::Storage(index) if *index == position))
        && chunk_filters.is_empty();
    if storage_ids.is_empty() {
        // Nothing is read from the file: scan the row ids for the row count.
        storage_ids.push(StorageIndex::row_id());
    }
    let filters = (!storage_filters.is_empty()).then_some(storage_filters);
    // `TableScanInitGlobal`: a single filter on an indexed column fetches the matching
    // rows through the index when few match.
    let mut index_rows = None;
    if let Some(filters) = &filters
        && filters.filters.len() == 1
        && storage.has_indexes()
    {
        let (max_count, percentage) = data.index_scan;
        let from_percentage = (storage.total_rows() as f64 * percentage) as u64;
        let max_count = usize::try_from(max_count.max(from_percentage)).unwrap_or(usize::MAX);
        index_rows = storage.try_index_scan(&storage_ids, filters, max_count)?;
    }
    let parallel = if index_rows.is_some() {
        ParallelTableScanState::default()
    } else {
        storage.initialize_parallel_scan(&data.transaction, None)
    };
    Ok(Some(FileScan {
        reader,
        storage,
        column_ids: storage_ids,
        sources,
        filters,
        chunk_filters,
        direct,
        parallel,
        index_rows,
        next_index_row: AtomicUsize::new(0),
        batch_offset,
    }))
}

/// The shared state of a `read_duckdb` scan: the files in turn, each scanned by every
/// thread (`MultiFileGlobalState` with `DuckDBReadGlobalState`).
#[derive(Debug)]
struct ScanGlobal {
    cursor: Mutex<Cursor>,
    max_threads: usize,
}

/// The file being scanned.
#[derive(Debug, Default)]
struct Cursor {
    /// The next file to open.
    next_file: usize,
    file: Option<Arc<FileScan>>,
    /// The batch index the next file's batches start after.
    batch_offset: u64,
    /// The reader opened while binding, for the first file.
    initial_reader: Option<Arc<DuckDbReader>>,
    /// The files opened so far (`file_index`).
    files_opened: usize,
    /// Whether opening a file failed, so the other threads stop.
    failed: bool,
}

impl Cursor {
    /// The reader of file `index` (`DuckDBMultiFileInfo::CreateReader`), reusing those
    /// opened while binding.
    fn reader(&mut self, data: &ReadDuckDbData, index: usize) -> Result<Arc<DuckDbReader>> {
        let path = &data.files.files[index];
        if let Some(reader) = self.initial_reader.take()
            && reader.path == *path
        {
            return Ok(reader);
        }
        if let Some(reader) = data.union_readers.iter().find(|reader| reader.path == *path) {
            return Ok(Arc::clone(reader));
        }
        Ok(Arc::new(DuckDbReader::open(
            &data.instance,
            &data.transaction,
            path,
            &data.options,
        )?))
    }

    /// Moves on to the next file that is not excluded by the filters (`TryOpenNextFile`);
    /// `file` is `None` once all files were read.
    fn advance(&mut self, data: &ReadDuckDbData, column_ids: &[u64]) -> Result<()> {
        self.file = None;
        while self.next_file < data.files.files.len() {
            let index = self.next_file;
            self.next_file += 1;
            let reader = self.reader(data, index)?;
            self.files_opened += 1;
            if let Some(file) = prepare_file(data, index, reader, column_ids, self.batch_offset)? {
                self.batch_offset += file.batch_count();
                self.file = Some(Arc::new(file));
                return Ok(());
            }
        }
        Ok(())
    }
}

/// The state of one thread of a `read_duckdb` scan (`DuckDBReadLocalState`).
#[derive(Debug, Default)]
struct ScanLocal {
    /// The file the thread scans; it holds the file attached
    /// (`DuckDBReadLocalState::attached_database`).
    file: Option<Arc<FileScan>>,
    state: rustdb_storage::TableScanState,
    /// Whether `state` holds a morsel of `file`.
    has_morsel: bool,
    scan_chunk: DataChunk,
    batch_index: u64,
}

/// The global state of a `read_duckdb` scan.
fn scan_global(global: &ParallelScanGlobal) -> Result<&ScanGlobal> {
    global
        .downcast_ref::<ScanGlobal>()
        .ok_or_else(|| Error::internal("read_duckdb scan state"))
}

/// `MultiFileFunction::MultiFileInitGlobal`: takes the reader opened while binding and
/// prepares the first file; a single file is scanned by up to one thread per row group.
fn read_duckdb_init_global(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    _: &[rustdb_types::ColumnIndex],
) -> Result<ParallelScanGlobal> {
    let data = bind_data(bind)?;
    let mut cursor = Cursor {
        initial_reader: data.take_initial_reader(),
        ..Cursor::default()
    };
    cursor.advance(data, column_ids)?;
    let max_threads = if data.files.files.len() > 1 {
        usize::MAX
    } else {
        cursor.file.as_ref().map_or(1, |file| file.storage.max_threads())
    };
    Ok(Arc::new(ScanGlobal {
        cursor: Mutex::new(cursor),
        max_threads,
    }))
}

/// The output chunk of a storage chunk of `file`: one vector per scanned column id, with
/// the filters on cast columns applied (`MultiFileReader::FinalizeChunk`).
fn assemble_chunk(file: &FileScan, scanned: &DataChunk, chunk: &mut DataChunk) -> Result<()> {
    let mut count = scanned.len();
    let mut columns = Vec::with_capacity(file.sources.len());
    for source in &file.sources {
        columns.push(match source {
            ColumnSource::Storage(index) => scanned.column(*index).clone(),
            ColumnSource::Cast { index, cast } => {
                cast.execute(&file.reader.path, scanned.column(*index), count)?
            }
            ColumnSource::Constant(_, vector) => vector.clone(),
        });
    }
    if !file.chunk_filters.is_empty() {
        let mut sel: Vec<u32> = (0..count as u32).collect();
        for (position, filter) in &file.chunk_filters {
            if let Some(vector) = columns.get(*position) {
                filter.select(vector, count, &mut sel)?;
            }
        }
        if sel.len() != count {
            count = sel.len();
            let selection = rustdb_types::SelectionVector::new(sel);
            for vector in &mut columns {
                *vector = vector.slice(&selection);
            }
        }
    }
    chunk.set_columns(columns, count);
    Ok(())
}

/// `MultiFileFunction::MultiFileScan` on one thread (`DuckDBReader::Scan`): the next
/// chunk of the thread's morsel, taking the next morsel or file when it is done.
fn read_duckdb_parallel_scan(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    global: &ParallelScanGlobal,
    local: &mut ParallelScanLocal,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data(bind)?;
    let global = scan_global(global)?;
    let local = local
        .downcast_mut::<ScanLocal>()
        .ok_or_else(|| Error::internal("read_duckdb scan local state"))?;
    let transaction = &data.transaction;
    loop {
        if let Some(file) = &local.file {
            if let Some(rows) = &file.index_rows {
                let start = file
                    .next_index_row
                    .fetch_add(STANDARD_VECTOR_SIZE, Ordering::Relaxed);
                if start < rows.len() {
                    let end = (start + STANDARD_VECTOR_SIZE).min(rows.len());
                    local.batch_index = file.batch_offset + (start / STANDARD_VECTOR_SIZE) as u64;
                    let target = if file.direct {
                        &mut *chunk
                    } else {
                        &mut local.scan_chunk
                    };
                    file.storage
                        .fetch(transaction, target, &file.column_ids, &rows[start..end])?;
                    if target.is_empty() {
                        continue;
                    }
                    if !file.direct {
                        assemble_chunk(file, &local.scan_chunk, chunk)?;
                    }
                    return Ok(true);
                }
            } else {
                if local.has_morsel {
                    let target = if file.direct {
                        &mut *chunk
                    } else {
                        &mut local.scan_chunk
                    };
                    if file.storage.scan_morsel(transaction, target, &mut local.state)? {
                        if !file.direct {
                            assemble_chunk(file, &local.scan_chunk, chunk)?;
                        }
                        if chunk.is_empty() {
                            continue;
                        }
                        return Ok(true);
                    }
                }
                local.has_morsel =
                    file.storage
                        .next_parallel_scan(transaction, &file.parallel, &mut local.state)?;
                if local.has_morsel {
                    local.batch_index = file.batch_offset + local.state.batch_index();
                    continue;
                }
            }
        }
        // The thread's file has no more work: take the next one, opening it if this was
        // the file being handed out (`TryOpenNextFile`).
        let exhausted = local.file.take();
        let next = {
            let mut cursor = global.cursor.lock().unwrap_or_else(PoisonError::into_inner);
            if cursor.failed {
                return Ok(false);
            }
            let current_done = match (&cursor.file, &exhausted) {
                (Some(current), Some(exhausted)) => Arc::ptr_eq(current, exhausted),
                _ => false,
            };
            if current_done && let Err(error) = cursor.advance(data, column_ids) {
                cursor.failed = true;
                return Err(error);
            }
            cursor.file.clone()
        };
        // `DuckDBMultiFileInfo::FinishReading`: the finished file is released.
        drop(exhausted);
        let Some(file) = next else {
            return Ok(false);
        };
        local.state = rustdb_storage::TableScanState::default();
        local
            .state
            .initialize(file.column_ids.clone(), file.filters.as_ref());
        local.has_morsel = false;
        local.file = Some(file);
    }
}

/// The single-threaded scan: the parallel scan on one thread.
fn read_duckdb_scan(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    state: &mut rustdb_function::TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    if state.local.is_none() {
        let global = read_duckdb_init_global(bind, column_ids, &[])?;
        let local: ParallelScanLocal = Box::<ScanLocal>::default();
        state.local = Some(Box::new((global, local)));
    }
    let (global, local) = state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<(ParallelScanGlobal, ParallelScanLocal)>())
        .ok_or_else(|| Error::internal("read_duckdb scan state"))?;
    if read_duckdb_parallel_scan(bind, column_ids, global, local, chunk)? {
        return Ok(true);
    }
    let data = bind_data(bind)?;
    *chunk = DataChunk::new(
        column_ids
            .iter()
            .map(|&id| Vector::constant_null(data.column_type(id)))
            .collect(),
        0,
    );
    Ok(false)
}

/// `MultiFileDynamicToString`.
fn read_duckdb_dynamic_to_string(
    bind: Option<&dyn FunctionData>,
    global: &ParallelScanGlobal,
) -> Vec<(String, String)> {
    let (Ok(data), Ok(global)) = (bind_data(bind), scan_global(global)) else {
        return Vec::new();
    };
    let files_loaded = global
        .cursor
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .files_opened;
    let display = data.display_paths.as_ref().unwrap_or(&data.files.files);
    rustdb_csv::multi_file::dynamic_to_string(files_loaded, display)
}

/// The `read_duckdb` scan by several threads.
fn read_duckdb_parallel() -> ParallelTableScan {
    ParallelTableScan {
        init_global: Arc::new(read_duckdb_init_global),
        init_local: Arc::new(|_| Ok(Box::new(ScanLocal::default()) as ParallelScanLocal)),
        scan: Arc::new(read_duckdb_parallel_scan),
        max_threads: Arc::new(|global| scan_global(global).map_or(1, |global| global.max_threads)),
        batch_index: Arc::new(|_, local| {
            local
                .downcast_ref::<ScanLocal>()
                .map_or(0, |local| local.batch_index)
        }),
        partition_data: None,
        dynamic_to_string: Some(Arc::new(read_duckdb_dynamic_to_string)),
        row_group_metrics: None,
    }
}

/// `MultiFileGetVirtualColumns` with `DuckDBMultiFileInfo::GetVirtualColumns`:
/// `filename` (unless the `filename` option names a real column), `file_index` and the
/// table's `rowid`.
fn read_duckdb_virtual_columns(bind: Option<&dyn FunctionData>) -> Vec<(u64, String, LogicalType)> {
    let filename_option = bind_data(bind).is_ok_and(|data| data.reader_bind.filename_idx.is_some());
    let mut result = Vec::with_capacity(3);
    if !filename_option {
        result.push((
            COLUMN_IDENTIFIER_FILENAME,
            "filename".to_owned(),
            LogicalType::VARCHAR,
        ));
    }
    result.push((
        COLUMN_IDENTIFIER_FILE_INDEX,
        "file_index".to_owned(),
        LogicalType::UBIGINT,
    ));
    result.push((COLUMN_IDENTIFIER_ROW_ID, "rowid".to_owned(), LogicalType::BIGINT));
    result
}

/// The file filter pushdown of `read_duckdb` (`MultiFileComplexFilterPushdown`).
fn read_duckdb_file_filter_pushdown() -> rustdb_function::FileFilterPushdown {
    rustdb_function::FileFilterPushdown {
        files: Arc::new(|bind| {
            let data = bind.as_any().downcast_ref::<ReadDuckDbData>()?;
            (data.file_options.hive_partitioning || data.file_options.filename)
                .then(|| data.files.files.clone())
        }),
        known_value: Arc::new(|bind, path, name, ty| {
            let Some(data) = bind.as_any().downcast_ref::<ReadDuckDbData>() else {
                return Ok(None);
            };
            rustdb_csv::multi_file::known_column_value(&data.file_options, &data.casts, path, name, ty)
        }),
        with_files: Arc::new(|bind, keep| {
            let data = bind
                .as_any()
                .downcast_ref::<ReadDuckDbData>()
                .ok_or_else(|| Error::internal("read_duckdb file pruning without bind data"))?;
            Ok(Arc::new(data.with_files(keep)) as Arc<dyn FunctionData>)
        }),
    }
}

/// One overload of `read_duckdb` (`ReadDuckDBTableFunction::GetFunction`).
fn read_duckdb_function(argument: LogicalType) -> TableFunction {
    let mut function = TableFunction::new(FUNCTION_NAME, vec![argument])
        .with_bind(read_duckdb_bind)
        .with_scan(read_duckdb_scan)
        // `DuckDBMultiFileInfo::GetCardinality`: the first file's rows per file.
        .with_cardinality(|bind| {
            let data = bind_data(bind).ok()?;
            let file_count = data.files.files.len() as u64;
            Some(
                data.initial_file_cardinality
                    .map_or(file_count, |rows| file_count * rows),
            )
        });
    // `ReadDuckDBAddNamedParameters`, after the multi-file parameters `MultiFileFunction`
    // adds first.
    function.named_parameters = multi_file_parameters();
    function.named_parameters.extend([
        ("schema_name".to_owned(), LogicalType::VARCHAR),
        ("table_name".to_owned(), LogicalType::VARCHAR),
    ]);
    function.projection_pushdown = true;
    function.filter_pushdown = true;
    function.filter_prune = true;
    function.pushdown_filters = Some(Arc::new(|bind, _, filters| {
        let data = bind
            .as_any()
            .downcast_ref::<ReadDuckDbData>()
            .ok_or_else(|| Error::internal("read_duckdb filter pushdown without bind data"))?;
        let filters = filters
            .downcast_ref::<TableFilterSet>()
            .ok_or_else(|| Error::internal("read_duckdb filter pushdown without a filter set"))?;
        Ok(Arc::new(data.with_filters(Some(Arc::new(filters.clone())))) as Arc<dyn FunctionData>)
    }));
    function.empty_column = true;
    function.get_virtual_columns = Some(Arc::new(read_duckdb_virtual_columns));
    function.projected_output = true;
    function.parallel_scan = Some(read_duckdb_parallel());
    function.file_filter_pushdown = Some(read_duckdb_file_filter_pushdown());
    // `DuckDBGetRowIdColumns`: late materialization fetches rows by file and row id.
    function.get_row_id_columns = Some(Arc::new(|_| {
        vec![COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_ROW_ID]
    }));
    function
}

/// `read_duckdb` (`MultiFileReader::CreateFunctionSet`: the VARCHAR and LIST(VARCHAR)
/// overloads).
pub(crate) fn read_duckdb_functions() -> TableFunctionSet {
    let mut set = TableFunctionSet::new(FUNCTION_NAME);
    set.add(read_duckdb_function(LogicalType::VARCHAR));
    set.add(read_duckdb_function(LogicalType::list(LogicalType::VARCHAR)));
    set
}
