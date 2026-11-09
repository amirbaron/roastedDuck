//! The `arrow_scan` and `arrow_scan_dumb` table functions (DuckDB's
//! `function/table/arrow.cpp`): scans of an Arrow stream given by three pointers, a
//! stream factory and the functions producing its stream and its schema.
//!
//! `arrow_scan` hands the projected columns and the pushed-down filters to the stream
//! producer, which may apply them; `arrow_scan_dumb` asks for every column. Threads take
//! the stream's arrays in turn and convert them [`STANDARD_VECTOR_SIZE`] rows at a time.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{
    FunctionData, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan, TableFunction,
    TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet, TableScanState,
};
use roastedduck_storage::{COLUMN_IDENTIFIER_ROW_ID, TableFilterSet};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

use crate::conversion::{ArrowArrayScanState, convert_column};
use crate::duck_schema::{ArrowTableSchema, ArrowType};
use crate::ffi::{
    ArrowArrayStream, ArrowArrayStreamWrapper, ArrowArrayWrapper, ArrowSchema, ArrowSchemaWrapper,
};

/// The columns a scan reads, for the stream producer (DuckDB's `ArrowProjectedColumns`).
#[derive(Clone, Debug, Default)]
pub struct ArrowProjectedColumns {
    /// The name of the column at each scanned position.
    pub projection_map: HashMap<usize, roastedduck_common::ByteString>,
    /// The names of the scanned columns, in order.
    pub columns: Vec<roastedduck_common::ByteString>,
    /// The column of each scanned position, which filters are keyed by.
    pub filter_to_col: HashMap<usize, usize>,
}

/// What a scan asks of the stream producer (DuckDB's `ArrowStreamParameters`): the
/// producer may leave out other columns and rows failing the filters.
#[derive(Clone, Debug, Default)]
pub struct ArrowStreamParameters {
    pub projected_columns: ArrowProjectedColumns,
    pub filters: Option<TableFilterSet>,
}

/// Produces the stream of a factory (DuckDB's `stream_factory_produce_t`); the second
/// `arrow_scan` argument points to one.
pub type StreamFactoryProduce = unsafe fn(
    stream_factory_ptr: usize,
    parameters: &ArrowStreamParameters,
) -> Result<Box<ArrowArrayStreamWrapper>>;

/// Reads the schema of a factory's stream (DuckDB's `stream_factory_get_schema_t`); the
/// third `arrow_scan` argument points to one.
pub type StreamFactoryGetSchema =
    unsafe fn(stream_factory_ptr: *mut ArrowArrayStream, schema: &mut ArrowSchema) -> Result<()>;

/// The bind data of an Arrow scan (DuckDB's `ArrowScanFunctionData`).
#[derive(Clone, Debug)]
struct ArrowScanFunctionData {
    all_types: Vec<LogicalType>,
    /// The stream's schema, which the column types were read from.
    schema_root: Arc<ArrowSchemaWrapper>,
    stream_factory_ptr: usize,
    scanner_producer: StreamFactoryProduce,
    arrow_table: Arc<ArrowTableSchema>,
    /// Whether the scan reads only the projected columns (not for `arrow_scan_dumb`).
    projection_pushdown_enabled: bool,
    /// The filters pushed into the scan, for the producer.
    filters: Option<TableFilterSet>,
    _factory_owner: Option<Arc<dyn FunctionData>>,
}

impl FunctionData for ArrowScanFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The bind data of a scan call.
fn bind_data(bind_data: Option<&dyn FunctionData>) -> Result<&ArrowScanFunctionData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<ArrowScanFunctionData>())
        .ok_or_else(|| Error::internal("arrow_scan without bind data"))
}

/// Reads the columns of an Arrow schema into `arrow_table`: unnamed columns are named
/// `v<index>` and duplicate names made unique (`PopulateArrowTableSchema`).
///
/// # Safety
/// `arrow_schema` must be a struct schema of the C data interface.
pub unsafe fn populate_arrow_table_schema(
    arrow_table: &mut ArrowTableSchema,
    arrow_schema: &ArrowSchema,
) -> Result<()> {
    // SAFETY: guaranteed by the caller.
    let children: Vec<&ArrowSchema> = unsafe { arrow_schema.child_schemas() }.collect();
    let mut names = Vec::with_capacity(children.len());
    for (index, schema) in children.iter().enumerate() {
        if schema.release.is_none() {
            return Err(Error::invalid_input("arrow_scan: released schema passed"));
        }
        // SAFETY: as above.
        let name = unsafe { schema.name_string() };
        names.push(if name.is_empty() {
            roastedduck_common::byte_format!("v{index}")
        } else {
            name
        });
    }
    roastedduck_planner::deduplicate_columns(&mut names);
    for (schema, name) in children.into_iter().zip(names) {
        // SAFETY: as above.
        let arrow_type = unsafe { ArrowType::from_schema_with_dictionary(schema) }?;
        arrow_table.add_column(arrow_type, name);
    }
    Ok(())
}

/// The pointer an `arrow_scan` argument holds.
fn pointer_argument(value: &Value) -> Result<usize> {
    value
        .as_u64()
        .map(|pointer| pointer as usize)
        .ok_or_else(|| Error::internal("arrow_scan: argument is not a pointer"))
}

/// `ArrowScanBind`: the stream's schema gives the columns.
fn arrow_scan_bind(
    input: &TableFunctionBindInput<'_>,
    projection_pushdown_enabled: bool,
) -> Result<TableFunctionBindOutput> {
    let [factory, produce, get_schema] = input.inputs else {
        return Err(Error::internal("arrow_scan takes three pointers"));
    };
    if factory.is_null() || produce.is_null() || get_schema.is_null() {
        return Err(Error::binder("arrow_scan: pointers cannot be null"));
    }
    let stream_factory_ptr = pointer_argument(factory)?;
    // SAFETY: by the contract of `arrow_scan`, the second and third arguments point to a
    // stream factory's produce and get-schema functions, as in DuckDB (which, like here,
    // cannot check what it calls); both are non-null (checked above).
    let (scanner_producer, stream_factory_get_schema) = unsafe {
        (
            std::mem::transmute::<usize, StreamFactoryProduce>(pointer_argument(produce)?),
            std::mem::transmute::<usize, StreamFactoryGetSchema>(pointer_argument(get_schema)?),
        )
    };
    let mut schema_root = ArrowSchemaWrapper::default();
    // SAFETY: the factory is the one the get-schema function reads (contract above).
    unsafe {
        stream_factory_get_schema(
            stream_factory_ptr as *mut ArrowArrayStream,
            &mut schema_root.arrow_schema,
        )
    }?;
    let mut arrow_table = ArrowTableSchema::default();
    // SAFETY: the producer filled in a schema of the C data interface.
    unsafe { populate_arrow_table_schema(&mut arrow_table, &schema_root.arrow_schema) }?;
    let names = arrow_table.names().to_vec();
    let return_types = arrow_table.types().to_vec();
    if return_types.is_empty() {
        return Err(Error::invalid_input(
            "Provided table/dataframe must have at least one column",
        ));
    }
    Ok(TableFunctionBindOutput {
        names,
        return_types: return_types.clone(),
        bind_data: Some(Arc::new(ArrowScanFunctionData {
            all_types: return_types,
            schema_root: Arc::new(schema_root),
            stream_factory_ptr,
            scanner_producer,
            arrow_table: Arc::new(arrow_table),
            projection_pushdown_enabled,
            filters: None,
            _factory_owner: None,
        })),
    })
}

/// The stream of a scan of `column_ids`, asked for those columns and the filters
/// (`ProduceArrowScan`).
fn produce_arrow_scan(
    function: &ArrowScanFunctionData,
    column_ids: &[u64],
) -> Result<Box<ArrowArrayStreamWrapper>> {
    let mut parameters = ArrowStreamParameters::default();
    // SAFETY: the schema was read from the stream at bind time and is owned by the bind data.
    let schemas: Vec<&ArrowSchema> = unsafe { function.schema_root.arrow_schema.child_schemas() }.collect();
    for (index, &column) in column_ids.iter().enumerate() {
        if column == COLUMN_IDENTIFIER_ROW_ID {
            continue;
        }
        let column = column as usize;
        let schema = schemas
            .get(column)
            .ok_or_else(|| Error::internal("arrow_scan: column out of range"))?;
        // SAFETY: as above.
        let name = unsafe { schema.name_string() };
        let projected = &mut parameters.projected_columns;
        projected.projection_map.insert(index, name.clone());
        projected.columns.push(name);
        projected.filter_to_col.insert(index, column);
    }
    parameters.filters.clone_from(&function.filters);
    // SAFETY: the producer was given as the stream factory's (see `arrow_scan_bind`).
    unsafe { (function.scanner_producer)(function.stream_factory_ptr, &parameters) }
}

/// The stream shared by the threads of a scan (DuckDB's `ArrowScanGlobalState`).
#[derive(Debug)]
struct ArrowScanGlobalState {
    stream: Mutex<StreamState>,
}

/// The stream and how far the scan got.
#[derive(Debug)]
struct StreamState {
    stream: Box<ArrowArrayStreamWrapper>,
    batch_index: u64,
    done: bool,
}

/// The Arrow array a thread converts (DuckDB's `ArrowScanLocalState`).
#[derive(Debug)]
struct ArrowScanLocalState {
    chunk: Arc<ArrowArrayWrapper>,
    /// The rows of `chunk` converted so far.
    chunk_offset: usize,
    batch_index: u64,
    array_states: HashMap<usize, ArrowArrayScanState>,
}

impl ArrowScanLocalState {
    /// Starts on a new Arrow array (`Reset`).
    fn reset(&mut self) {
        self.chunk_offset = 0;
        for state in self.array_states.values_mut() {
            state.reset();
        }
    }
}

fn global_state(global: &ParallelScanGlobal) -> Result<&ArrowScanGlobalState> {
    global
        .downcast_ref::<ArrowScanGlobalState>()
        .ok_or_else(|| Error::internal("arrow_scan global state"))
}

/// `ArrowScanInitGlobal`
fn arrow_scan_init_global(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    _: &[roastedduck_types::ColumnIndex],
    _: Option<&roastedduck_function::logging::OperatorLogger>,
) -> Result<ParallelScanGlobal> {
    let stream = produce_arrow_scan(bind_data(bind)?, column_ids)?;
    Ok(Arc::new(ArrowScanGlobalState {
        stream: Mutex::new(StreamState {
            stream,
            batch_index: 0,
            done: false,
        }),
    }))
}

/// `ArrowScanInitLocal`: the first array is taken on the first scan.
fn arrow_scan_init_local() -> ParallelScanLocal {
    Box::new(ArrowScanLocalState {
        chunk: Arc::new(ArrowArrayWrapper::default()),
        chunk_offset: 0,
        batch_index: 0,
        array_states: HashMap::new(),
    })
}

/// Gives the thread the next non-empty array of the stream; false at its end
/// (`ArrowScanParallelStateNext`).
fn arrow_scan_parallel_state_next(
    state: &mut ArrowScanLocalState,
    global: &ArrowScanGlobalState,
) -> Result<bool> {
    let mut stream = global.stream.lock().unwrap_or_else(PoisonError::into_inner);
    if stream.done {
        return Ok(false);
    }
    state.reset();
    stream.batch_index += 1;
    state.batch_index = stream.batch_index;
    let mut current_chunk = stream.stream.get_next_chunk()?;
    while current_chunk.arrow_array.length == 0 && current_chunk.arrow_array.release.is_some() {
        current_chunk = stream.stream.get_next_chunk()?;
    }
    state.chunk = current_chunk;
    // A released array ends the stream.
    if state.chunk.arrow_array.release.is_none() {
        stream.done = true;
        return Ok(false);
    }
    Ok(true)
}

/// Converts the next `output_size` rows of the thread's array into one vector per
/// scanned column; `column_ids` empty reads every column in order
/// (`ArrowTableFunction::ArrowToDuckDB`, with the array projected by the producer).
fn arrow_to_duckdb(
    scan_state: &mut ArrowScanLocalState,
    data: &ArrowScanFunctionData,
    column_ids: &[u64],
    column_count: usize,
    output_size: usize,
) -> Result<Vec<Vector>> {
    let parent_array = &scan_state.chunk.arrow_array;
    let mut output = Vec::with_capacity(column_count);
    for index in 0..column_count {
        let column = column_ids.get(index).copied().unwrap_or(index as u64);
        if column == COLUMN_IDENTIFIER_ROW_ID {
            // Arrow scans have no row ids.
            output.push(Vector::constant_null(LogicalType::BIGINT));
            continue;
        }
        let column = column as usize;
        if index >= usize::try_from(parent_array.n_children).unwrap_or(0) {
            return Err(Error::invalid_input(
                "arrow_scan: array has fewer columns than the schema",
            ));
        }
        // SAFETY: the stream's arrays follow the C data interface; `index` is a child.
        let array = unsafe { &**parent_array.children.add(index) };
        if array.release.is_none() {
            return Err(Error::invalid_input("arrow_scan: released array passed"));
        }
        if array.length != parent_array.length {
            return Err(Error::invalid_input("arrow_scan: array length mismatch"));
        }
        let arrow_type = data
            .arrow_table
            .columns()
            .get(column)
            .ok_or_else(|| Error::internal("arrow_scan: column out of range"))?;
        let array_state = scan_state.array_states.entry(column).or_default();
        // The vectors keep the Arrow array alive where they point into it.
        if array_state.owned_data.is_none() {
            array_state.owned_data = Some(Arc::clone(&scan_state.chunk));
        }
        let mut vector = Vector::flat(data.all_types[column].clone(), output_size);
        // SAFETY: by the producer's contract the stream's arrays hold the projected
        // columns, of the types read from its schema (DuckDB relies on it the same way).
        unsafe {
            convert_column(
                &mut vector,
                array,
                scan_state.chunk_offset,
                array_state,
                output_size,
                arrow_type,
                parent_array.offset,
            )
        }?;
        output.push(vector);
    }
    Ok(output)
}

/// `ArrowScanFunction` on one thread: the next rows of its array, taking the next array
/// when it is done.
fn arrow_scan_parallel(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    global: &ParallelScanGlobal,
    local: &mut ParallelScanLocal,
    output: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data(bind)?;
    let global = global_state(global)?;
    let state = local
        .downcast_mut::<ArrowScanLocalState>()
        .ok_or_else(|| Error::internal("arrow_scan local state"))?;
    // Out of rows in this array?
    if state.chunk_offset >= usize::try_from(state.chunk.arrow_array.length).unwrap_or(0)
        && !arrow_scan_parallel_state_next(state, global)?
    {
        return Ok(false);
    }
    let length = usize::try_from(state.chunk.arrow_array.length).unwrap_or(0);
    let output_size = STANDARD_VECTOR_SIZE.min(length - state.chunk_offset);
    // `arrow_scan_dumb` reads every column in order.
    let conversion_ids = if data.projection_pushdown_enabled {
        column_ids
    } else {
        &[]
    };
    let column_count = if data.projection_pushdown_enabled {
        column_ids.len()
    } else {
        data.all_types.len()
    };
    let columns = arrow_to_duckdb(state, data, conversion_ids, column_count, output_size)?;
    *output = DataChunk::new(columns, output_size);
    state.chunk_offset += output_size;
    Ok(true)
}

/// The scan on a single thread, keeping a thread's state in the scan state.
struct SingleThreadScan {
    global: ParallelScanGlobal,
    local: ParallelScanLocal,
}

/// `ArrowScanFunction` for callers scanning on one thread.
fn arrow_scan(
    bind: Option<&dyn FunctionData>,
    column_ids: &[u64],
    state: &mut TableScanState,
    output: &mut DataChunk,
) -> Result<bool> {
    if state.local.is_none() {
        let global = arrow_scan_init_global(bind, column_ids, &[], None)?;
        let local = arrow_scan_init_local();
        state.local = Some(Box::new(SingleThreadScan { global, local }));
    }
    let scan = state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<SingleThreadScan>())
        .ok_or_else(|| Error::internal("arrow_scan scan state"))?;
    if arrow_scan_parallel(bind, column_ids, &scan.global, &mut scan.local, output)? {
        return Ok(true);
    }
    let data = bind_data(bind)?;
    let columns = column_ids
        .iter()
        .map(|&column| {
            let ty = usize::try_from(column)
                .ok()
                .and_then(|column| data.all_types.get(column))
                .cloned()
                .unwrap_or(LogicalType::BIGINT);
            Vector::constant_null(ty)
        })
        .collect();
    *output = DataChunk::new(columns, 0);
    Ok(false)
}

/// Hands the filters pushed into an `arrow_scan` to its stream producer.
fn arrow_scan_pushdown_filters(
    bind: &dyn FunctionData,
    _: &[u64],
    filters: &dyn Any,
) -> Result<Arc<dyn FunctionData>> {
    let data = bind
        .as_any()
        .downcast_ref::<ArrowScanFunctionData>()
        .ok_or_else(|| Error::internal("arrow_scan filter pushdown without bind data"))?;
    let filters = filters
        .downcast_ref::<TableFilterSet>()
        .ok_or_else(|| Error::internal("arrow_scan filter pushdown without a filter set"))?;
    Ok(Arc::new(ArrowScanFunctionData {
        filters: Some(filters.clone()),
        ..data.clone()
    }))
}

/// Retain an external factory through all cloned/rebound scan data.
pub fn retain_factory_owner(
    data: &dyn FunctionData,
    owner: Arc<dyn FunctionData>,
) -> Result<Arc<dyn FunctionData>> {
    let data = bind_data(Some(data))?;
    Ok(Arc::new(ArrowScanFunctionData {
        _factory_owner: Some(owner),
        ..data.clone()
    }))
}
fn arrow_type_has_view(ty: &ArrowType) -> bool {
    use crate::duck_schema::ArrowVariableSizeType as V;
    use roastedduck_types::LogicalTypeId as L;
    if ty.dictionary().is_some_and(arrow_type_has_view) {
        return true;
    }
    match ty.duck_type(false).id() {
        L::Varchar | L::Blob => ty.string_info().is_ok_and(|(size, _)| size == V::View),
        L::Struct | L::Union => ty
            .struct_children()
            .is_ok_and(|children| children.iter().any(arrow_type_has_view)),
        L::List | L::Map => ty
            .list_info()
            .is_ok_and(|(child, _, _)| arrow_type_has_view(child)),
        L::Array => ty.array_info().is_ok_and(|(child, _)| arrow_type_has_view(child)),
        _ => false,
    }
}
fn arrow_type_can_filter(ty: &ArrowType) -> bool {
    use roastedduck_types::{LogicalTypeId as L, PhysicalType};
    let ty = ty.dictionary().unwrap_or(ty);
    match ty.duck_type(true).id() {
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Date
        | L::Time
        | L::TimeNs
        | L::Timestamp
        | L::TimestampMs
        | L::TimestampNs
        | L::TimestampSec
        | L::TimestampTz
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::Float
        | L::Double
        | L::Varchar
        | L::Blob => true,
        L::Decimal => ty.duck_type(true).physical_type() == PhysicalType::Int128,
        L::Struct => ty
            .struct_children()
            .is_ok_and(|children| children.iter().all(arrow_type_can_filter)),
        _ => false,
    }
}
fn arrow_filter_supported(data: &dyn FunctionData, column: usize, filter: &dyn Any) -> bool {
    use roastedduck_storage::TableFilter as F;
    fn supported(filter: &F) -> bool {
        match filter {
            F::Expression(_) => false,
            F::And(children) | F::Or(children) => children.iter().all(supported),
            F::StructExtract { child_filter, .. } => supported(child_filter),
            F::Optional(Some(child)) => supported(child),
            F::SelectivityOptional(child) => supported(&child.child),
            _ => true,
        }
    }
    let Ok(data) = bind_data(Some(data)) else {
        return false;
    };
    let Some(ty) = data.arrow_table.columns().get(column) else {
        return false;
    };
    if data.arrow_table.columns().iter().any(arrow_type_has_view) || !arrow_type_can_filter(ty) {
        return false;
    }
    filter.downcast_ref::<F>().is_some_and(supported)
}

/// The scan by several threads, each taking whole Arrow arrays.
fn arrow_scan_parallel_scan() -> ParallelTableScan {
    ParallelTableScan {
        init_global: Arc::new(arrow_scan_init_global),
        init_local: Arc::new(|_| Ok(arrow_scan_init_local())),
        scan: Arc::new(arrow_scan_parallel),
        // As many threads as the database has (`ArrowScanMaxThreads`).
        max_threads: Arc::new(|_| usize::MAX),
        // `ArrowGetPartitionData`
        batch_index: Arc::new(|_, local| {
            local
                .downcast_ref::<ArrowScanLocalState>()
                .map_or(0, |state| state.batch_index)
        }),
        // Partition columns are not supported (`ArrowGetPartitionData` throws).
        partition_data: None,
        dynamic_to_string: None,
        row_group_metrics: None,
    }
}

/// One of the Arrow scan functions, taking three pointers.
fn arrow_scan_function(name: &str, projection_pushdown: bool) -> TableFunction {
    let mut function = TableFunction::new(name, vec![LogicalType::POINTER; 3])
        .with_bind(move |input| arrow_scan_bind(input, projection_pushdown))
        .with_scan(arrow_scan);
    function.parallel_scan = Some(arrow_scan_parallel_scan());
    function.projected_output = projection_pushdown;
    function.projection_pushdown = projection_pushdown;
    function.filter_pushdown = projection_pushdown;
    function.filter_prune = projection_pushdown;
    if projection_pushdown {
        function.pushdown_filters = Some(Arc::new(arrow_scan_pushdown_filters));
        function.filter_support = Some(Arc::new(arrow_filter_supported));
    }
    function
}

/// `arrow_scan` and `arrow_scan_dumb` (`ArrowTableFunction::RegisterFunction`).
pub fn arrow_table_functions() -> Vec<TableFunctionSet> {
    [("arrow_scan", true), ("arrow_scan_dumb", false)]
        .into_iter()
        .map(|(name, projection_pushdown)| {
            let mut set = TableFunctionSet::new(name);
            set.add(arrow_scan_function(name, projection_pushdown));
            set
        })
        .collect()
}
