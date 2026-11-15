//! Runtime integration for roastedDuck's compatible PostgreSQL implementation.
use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{ByteString, Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::{
    ExtensionOption, FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_postgres::{
    BinaryParameter, Driver, Rows, Session,
    codec::{Codec, Options},
    config::Parameters,
};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

fn identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

struct AttachData {
    instance: std::sync::Weak<crate::database::DatabaseInstance>,
    client: Arc<FunctionClient>,
    dsn: String,
    source_schema: String,
    sink_schema: String,
    overwrite: bool,
    filter_pushdown: bool,
}
impl std::fmt::Debug for AttachData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresAttach")
            .field("source_schema", &self.source_schema)
            .field("sink_schema", &self.sink_schema)
            .finish_non_exhaustive()
    }
}
impl FunctionData for AttachData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The legacy function deliberately creates views through a separate connection,
/// as upstream does. This is distinct from ATTACH's remote catalog implementation.
pub(crate) fn attach_function() -> TableFunctionSet {
    let mut function = TableFunction::new("postgres_attach", vec![LogicalType::VARCHAR])
        .with_bind(|input| {
            let context = input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
                .ok_or_else(|| Error::internal("postgres_attach requires a client context"))?;
            if input.inputs[0].is_null() { return Err(Error::invalid_input("Parameters to postgres_attach cannot be NULL")); }
            let named = |name: &str| input.named_parameters.iter().find(|(n, _)| n.as_bytes().eq_ignore_ascii_case(name.as_bytes())).map(|(_, v)| v);
            let text = |name: &str, default: &str| named(name).map_or_else(|| Ok(default.to_owned()), Value::to_varchar);
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN], names: vec!["Success".into()],
                bind_data: Some(Arc::new(AttachData {
                    instance: crate::database::instance_state(context)?.instance(),
                    client: Arc::clone(&context.function_client), dsn: input.inputs[0].to_varchar()?,
                    source_schema: text("source_schema", "public")?, sink_schema: text("sink_schema", "main")?,
                    overwrite: named("overwrite").and_then(Value::as_bool).unwrap_or(false),
                    filter_pushdown: named("filter_pushdown").and_then(Value::as_bool).unwrap_or(false),
                })),
            })
        })
        .with_scan(|bind_data, _, state, output| {
            *output = DataChunk::new(vec![Vector::flat(LogicalType::BOOLEAN, 0)], 0);
            if state.position != 0 { return Ok(false); }
            let data = bind_data.and_then(|d| d.as_any().downcast_ref::<AttachData>()).ok_or_else(|| Error::internal("postgres_attach without bind data"))?;
            let driver = Driver::new()?;
            let session = driver.connect(&Parameters::parse(&data.dsn)?)?;
            let mut tables = session.stream_interruptible(
                "SELECT relname::text FROM pg_catalog.pg_class JOIN pg_catalog.pg_namespace ON pg_class.relnamespace=pg_namespace.oid JOIN pg_catalog.pg_attribute ON pg_class.oid=pg_attribute.attrelid WHERE relkind='r' AND attnum>0 AND nspname=$1::text GROUP BY relname ORDER BY relname",
                &[BinaryParameter(Some(data.source_schema.as_bytes().to_vec()))], data.client.interrupt_flag().as_deref())?;
            let database = crate::Database::from_instance(&data.instance).ok_or_else(|| Error::internal("postgres_attach after database shutdown"))?;
            let mut connection = database.connect()?;
            loop {
                let rows = tables.next_batch_interruptible(STANDARD_VECTOR_SIZE, data.client.interrupt_flag().as_deref())?;
                if rows.is_empty() { break; }
                for row in rows {
                    let table = row[0].as_deref().and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(|| Error::internal("Invalid Postgres table name"))?;
                    let sink = if data.sink_schema.is_empty() { identifier(table) } else { format!("{}.{}", identifier(&data.sink_schema), identifier(table)) };
                    let create = if data.overwrite { "CREATE OR REPLACE VIEW" } else { "CREATE VIEW IF NOT EXISTS" };
                    let scan = if data.filter_pushdown { "postgres_scan_pushdown" } else { "postgres_scan" };
                    let sql = format!("{create} {sink} AS SELECT * FROM {scan}({},{},{})", literal(&data.dsn), literal(&data.source_schema), literal(table));
                    // The pinned AttachFunction discards each dconn.Query result,
                    // including creation errors; preserve that legacy behavior.
                    let _ = connection.query(&sql);
                }
            }
            state.position = 1;
            Ok(false)
        });
    function.named_parameters = vec![
        ("overwrite".into(), LogicalType::BOOLEAN),
        ("filter_pushdown".into(), LogicalType::BOOLEAN),
        ("source_schema".into(), LogicalType::VARCHAR),
        ("sink_schema".into(), LogicalType::VARCHAR),
        ("suffix".into(), LogicalType::VARCHAR),
    ];
    // `suffix` is accepted but unused by the pinned implementation.
    let mut set = TableFunctionSet::new("postgres_attach");
    set.add(function);
    set
}

pub(crate) fn options() -> Vec<ExtensionOption> {
    [
        ("pg_array_as_varchar", "Read Postgres arrays as varchar - enables reading mixed dimensional arrays", false),
        ("pg_numeric_as_varchar", "Read Postgres numerics without precision and scale or precision > 38 as varchar instead of double", false),
        ("pg_numeric_nan_as_null", "Read Postgres numeric NaN value as NULL instead of throwing an error", true),
    ].into_iter().map(|(name, description, default)| ExtensionOption {
        name: name.into(), description: description.into(), input_type: LogicalType::BOOLEAN,
        default_value: Value::boolean(default), set_function: None, global_by_default: false,
    }).collect()
}

fn type_options(client: &FunctionClient) -> Options {
    let setting = |name: &str, default: bool| {
        client
            .settings()
            .and_then(|s| s.current_setting(name).ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };
    Options {
        array_as_varchar: setting("pg_array_as_varchar", false),
        numeric_as_varchar: setting("pg_numeric_as_varchar", false),
        // Legacy PostgresBind does not assign bind_data.type_config; the parser
        // therefore retains numeric_nan_as_null=true even after SET. Attached
        // tables and postgres_query use their explicit codec options instead.
        numeric_nan_as_null: true,
        resolve_custom_types: false,
    }
}

#[derive(Debug)]
struct ScanData {
    driver: Driver,
    parameters: Parameters,
    relation: String,
    names: Vec<String>,
    codecs: Vec<Codec>,
    client: Arc<FunctionClient>,
}
impl FunctionData for ScanData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
struct ScanState {
    // Keep the session alive until its stream is exhausted or the scan is dropped.
    _session: Session,
    rows: Rows,
    columns: Vec<usize>,
}

pub(crate) fn scan_functions() -> Vec<TableFunctionSet> {
    ["postgres_scan", "postgres_scan_pushdown"].into_iter().map(|name| {
        let mut function = TableFunction::new(name, vec![LogicalType::VARCHAR; 3])
            .with_bind(|input| {
                let context = input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
                    .ok_or_else(|| Error::internal("Postgres scan requires a client context"))?;
                if input.inputs.iter().any(Value::is_null) { return Err(Error::invalid_input("Parameters to postgres_scan cannot be NULL")); }
                let dsn = input.inputs[0].to_varchar()?;
                let schema = input.inputs[1].to_varchar()?;
                let table = input.inputs[2].to_varchar()?;
                let relation = format!("{}.{}", identifier(&schema), identifier(&table));
                let parameters = Parameters::parse(&dsn)?;
                let driver = Driver::new()?;
                let session = driver.connect(&parameters)?;
                let columns = session.describe(&format!("SELECT * FROM {relation}"))?;
                if columns.is_empty() { return Err(Error::invalid_input(format!("Table {table} does not contain any columns."))); }
                // attndims survives a declared multidimensional array even when
                // RowDescription alone reports the common array OID.
                let mut dimensions = session.stream(
                    "SELECT attname::text,attndims::int4 FROM pg_catalog.pg_attribute WHERE attrelid=$1::text::regclass AND attnum>0 AND NOT attisdropped ORDER BY attnum", &[BinaryParameter(Some(relation.as_bytes().to_vec()))])?;
                let mut dimensions_by_name = std::collections::HashMap::new();
                loop {
                    let batch = dimensions.next_batch(STANDARD_VECTOR_SIZE)?;
                    if batch.is_empty() { break; }
                    for row in batch {
                        let name = row[0].as_ref().and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(|| Error::internal("Invalid Postgres column name"))?;
                        let bytes: [u8; 4] = row[1].as_deref().and_then(|b| b.try_into().ok()).ok_or_else(|| Error::internal("Invalid Postgres array dimensions"))?;
                        dimensions_by_name.insert(name.to_owned(), i32::from_be_bytes(bytes).max(0) as usize);
                    }
                }
                let options = type_options(&context.function_client);
                let codecs = columns.iter().map(|c| Codec::new(&c.data_type, c.type_modifier, dimensions_by_name.get(&c.name).copied().unwrap_or(0), options)).collect::<Result<Vec<_>>>()?;
                let names = columns.into_iter().map(|c| c.name).collect::<Vec<_>>();
                Ok(TableFunctionBindOutput {
                    return_types: codecs.iter().map(|c| c.logical_type.clone()).collect(),
                    names: names.iter().map(|s| ByteString::from(s.as_str())).collect(),
                    bind_data: Some(Arc::new(ScanData { driver, parameters, relation, names, codecs, client: Arc::clone(&context.function_client) })),
                })
            })
            .with_scan(|bind_data, column_ids, state, output| {
                let data = bind_data.and_then(|b| b.as_any().downcast_ref::<ScanData>()).ok_or_else(|| Error::internal("Postgres scan without bind data"))?;
                if data.client.interrupt_flag().is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed)) {
                    return Err(Error::new(ErrorKind::Interrupt, "Interrupted!"));
                }
                if state.local.is_none() {
                    let columns = column_ids.iter().map(|id| usize::try_from(*id).map_err(|_| Error::internal("Invalid Postgres projected column"))).collect::<Result<Vec<_>>>()?;
                    // COUNT(*) can request no physical columns. Keep one wire
                    // column to carry cardinality without returning it to the engine.
                    let expressions = if columns.is_empty() { vec!["1".to_owned()] } else {
                        columns.iter().map(|i| {
                            let name = data.names.get(*i).ok_or_else(|| Error::internal("Invalid Postgres projected column"))?;
                            let column = identifier(name);
                            data.codecs[*i].projection(&column)
                        }).collect::<Result<Vec<_>>>()?
                    };
                    let session = data.driver.connect(&data.parameters)?;
                    let rows = session.stream_interruptible(&format!("SELECT {} FROM {}", expressions.join(","), data.relation), &[], data.client.interrupt_flag().as_deref())?;
                    state.local = Some(Box::new(ScanState { _session: session, rows, columns }));
                }
                let local = state.local.as_mut().and_then(|s| s.downcast_mut::<ScanState>()).ok_or_else(|| Error::internal("Invalid Postgres scan state"))?;
                let rows = local.rows.next_batch_interruptible(STANDARD_VECTOR_SIZE, data.client.interrupt_flag().as_deref())?;
                let mut vectors = local.columns.iter().map(|i| Vector::flat(data.codecs[*i].logical_type.clone(), rows.len())).collect::<Vec<_>>();
                for (row_index, row) in rows.iter().enumerate() {
                    for (output_index, column) in local.columns.iter().enumerate() {
                        let value = data.codecs[*column].decode(row[output_index].as_deref())?;
                        vectors[output_index].set_value(row_index, &value)?;
                    }
                }
                *output = DataChunk::new(vectors, rows.len());
                state.position += rows.len() as u64;
                Ok(rows.len() == STANDARD_VECTOR_SIZE)
            });
        function.projection_pushdown = true;
        let mut set = TableFunctionSet::new(name);
        set.add(function);
        set
    }).collect()
}
