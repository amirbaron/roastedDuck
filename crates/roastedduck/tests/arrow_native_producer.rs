//! Projection-aware native factories: independent frozen C++ callback observations.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Controlled native Arrow fixtures"
)]
use roastedduck::{Database, Result, Value};
use roastedduck_arrow::ffi::{ArrowArray, ArrowArrayStream, ArrowArrayStreamWrapper, ArrowSchema};
use roastedduck_arrow::{
    ArrowStreamParameters, ClientProperties, TableFilter, arrow_table_functions, retain_factory_owner,
    to_arrow_array, to_arrow_schema,
};
use roastedduck_function::{FunctionData, TableFunctionBindInput, TableFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, Vector};
use serde_json::{Value as Json, json};
use std::any::Any;
use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_int};
use std::sync::{Arc, Mutex};
const NAMES: [&str; 3] = ["a", "b", "c"];
fn cell(row: usize, column: usize) -> i64 {
    row as i64
        + match column {
            0 => 0,
            1 => 100,
            _ => 1000,
        }
}
#[derive(Debug, Default)]
struct Factory {
    events: Mutex<Vec<Json>>,
}
impl FunctionData for Factory {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
struct StreamState {
    chunk: Option<DataChunk>,
    names: Vec<roastedduck_common::ByteString>,
    error: Option<CString>,
}
unsafe extern "C" fn stream_schema(stream: *mut ArrowArrayStream, out: *mut ArrowSchema) -> c_int {
    // SAFETY: this stream owns a StreamState and the consumer supplies an output schema.
    let state = unsafe { &mut *(*stream).private_data.cast::<StreamState>() };
    let types = vec![LogicalType::BIGINT; state.names.len()];
    match to_arrow_schema(
        unsafe { &mut *out },
        &types,
        &state.names,
        &ClientProperties::default(),
    ) {
        Ok(()) => 0,
        Err(error) => {
            state.error = Some(CString::new(error.to_string()).unwrap());
            1
        }
    }
}
unsafe extern "C" fn stream_next(stream: *mut ArrowArrayStream, out: *mut ArrowArray) -> c_int {
    // SAFETY: the retained stream owns its state; output is an Arrow C data slot.
    let state = unsafe { &mut *(*stream).private_data.cast::<StreamState>() };
    let Some(chunk) = state.chunk.take() else {
        unsafe { out.write(ArrowArray::empty()) };
        return 0;
    };
    match to_arrow_array(&chunk, &ClientProperties::default()) {
        Ok(array) => {
            unsafe { out.write(array) };
            0
        }
        Err(error) => {
            state.error = Some(CString::new(error.to_string()).unwrap());
            1
        }
    }
}
unsafe extern "C" fn stream_error(stream: *mut ArrowArrayStream) -> *const c_char {
    // SAFETY: this live stream owns its error text.
    let state = unsafe { &*(*stream).private_data.cast::<StreamState>() };
    state
        .error
        .as_ref()
        .map_or(std::ptr::null(), |error| error.as_ptr())
}
unsafe extern "C" fn stream_release(stream: *mut ArrowArrayStream) {
    // SAFETY: the stream's private data is allocated once and released once.
    unsafe {
        drop(Box::from_raw((*stream).private_data.cast::<StreamState>()));
        (*stream).release = None;
    }
}
unsafe fn factory_schema(_: *mut ArrowArrayStream, out: &mut ArrowSchema) -> Result<()> {
    let names = NAMES.iter().map(|name| (*name).into()).collect::<Vec<_>>();
    to_arrow_schema(
        out,
        &vec![LogicalType::BIGINT; 3],
        &names,
        &ClientProperties::default(),
    )
}
fn pass(filter: &TableFilter, vector: &Vector, row: u32) -> Result<bool> {
    // Both independent fixtures deliberately ignore optional pruning hints.
    match filter {
        TableFilter::Optional(_) | TableFilter::SelectivityOptional(_) | TableFilter::Dynamic(_) => Ok(true),
        TableFilter::And(children) => {
            for child in children {
                if !pass(child, vector, row)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        TableFilter::Or(children) => {
            for child in children {
                if pass(child, vector, row)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => {
            let mut selected = vec![row];
            filter.select(vector, 8, &mut selected)?;
            Ok(!selected.is_empty())
        }
    }
}
unsafe fn factory_produce(
    pointer: usize,
    parameters: &ArrowStreamParameters,
) -> Result<Box<ArrowArrayStreamWrapper>> {
    // SAFETY: the bind data retains this exact Factory Arc through all callbacks.
    let factory = unsafe { &*(pointer as *const Factory) };
    let names = parameters.projected_columns.columns.clone();
    let columns = names
        .iter()
        .map(|name| {
            NAMES
                .iter()
                .position(|candidate| candidate.as_bytes() == name.as_bytes())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mapping = parameters
        .projected_columns
        .filter_to_col
        .iter()
        .map(|(&a, &b)| (a, b))
        .collect::<BTreeMap<_, _>>();
    let mut filters = BTreeMap::new();
    let mut rows = (0..8u32).collect::<Vec<_>>();
    if let Some(set) = &parameters.filters {
        for (&position, filter) in &set.filters {
            let column = mapping[&position];
            let mut vector = Vector::flat(LogicalType::BIGINT, 8);
            for row in 0..8 {
                vector.values_mut::<i64>()[row] = cell(row, column);
            }
            let mut keep = Vec::new();
            for row in rows {
                if pass(filter, &vector, row)? {
                    keep.push(row);
                }
            }
            rows = keep;
            filters.insert(
                position,
                std::str::from_utf8(filter.to_sql(NAMES[column]).as_bytes())
                    .unwrap()
                    .to_owned(),
            );
        }
    }
    factory.events.lock().unwrap().push(json!({"columns":names.iter().map(|name|std::str::from_utf8(name.as_bytes()).unwrap()).collect::<Vec<_>>(),"mapping":mapping,"filters":filters,"rows":rows.len()}));
    let mut vectors = Vec::new();
    for column in columns {
        let mut vector = Vector::flat(LogicalType::BIGINT, rows.len());
        for (position, &row) in rows.iter().enumerate() {
            vector.values_mut::<i64>()[position] = cell(row as usize, column);
        }
        vectors.push(vector);
    }
    let state = Box::new(StreamState {
        chunk: Some(DataChunk::new(vectors, rows.len())),
        names,
        error: None,
    });
    Ok(Box::new(ArrowArrayStreamWrapper {
        arrow_array_stream: ArrowArrayStream {
            get_schema: Some(stream_schema),
            get_next: Some(stream_next),
            get_last_error: Some(stream_error),
            release: Some(stream_release),
            private_data: Box::into_raw(state).cast(),
        },
    }))
}
fn function(factory: &Arc<Factory>) -> TableFunctionSet {
    let mut function = arrow_table_functions()
        .into_iter()
        .find(|set| set.name.as_ref() == "arrow_scan")
        .unwrap()
        .functions
        .remove(0);
    let bind = function.bind.take().unwrap();
    let owner = Arc::clone(factory);
    let pointers = [
        Arc::as_ptr(factory) as usize,
        factory_produce as *const () as usize,
        factory_schema as *const () as usize,
    ]
    .into_iter()
    .map(|p| Value::ubigint(p as u64).reinterpret(LogicalType::POINTER))
    .collect::<Vec<_>>();
    function.name = "native_factory".into();
    function.arguments.clear();
    function.bind = Some(Arc::new(move |input| {
        let mut output = bind(&TableFunctionBindInput {
            inputs: &pointers,
            ..*input
        })?;
        if let Some(data) = &output.bind_data {
            output.bind_data = Some(retain_factory_owner(data.as_ref(), owner.clone())?);
        }
        Ok(output)
    }));
    let mut set = TableFunctionSet::new("native_factory");
    set.add(function);
    set
}
fn runtime_filters(value: &Json) -> Option<String> {
    if value
        .get("extra_info")
        .and_then(|info| info.get("Function"))
        .and_then(Json::as_str)
        == Some("NATIVE_FACTORY")
    {
        return value["extra_info"]["Dynamic Filters"].as_str().map(str::to_owned);
    }
    match value {
        Json::Object(fields) => fields.values().find_map(runtime_filters),
        Json::Array(values) => values.iter().find_map(runtime_filters),
        _ => None,
    }
}

#[test]
fn native_projection_filter_and_dynamic_requests_match_cpp() -> Result<()> {
    let factory = Arc::new(Factory::default());
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.create_table_function_set(function(&factory))?;
    connection.query("SET disabled_optimizers='join_order,build_side_probe_side'")?;
    connection.query(
        "CREATE TABLE keys AS SELECT*FROM(VALUES(0,'drop'),(2,'keep'),(5,'keep'),(7,'drop'))t(id,flag)",
    )?;
    connection.query("CREATE TABLE large_keys AS SELECT i*1000 AS id,i%2 AS flag FROM range(4096)t(i)")?;
    connection.query("CREATE TABLE sparse_keys AS SELECT i*1000 AS id,i%2 AS flag FROM range(256)t(i)")?;
    connection.query(
        "CREATE TABLE one_keys AS SELECT*FROM(VALUES(0,'drop'),(2,'one'),(5,'drop'),(7,'drop'))t(id,flag)",
    )?;
    connection.query("CREATE VIEW native_source AS SELECT*FROM native_factory()")?;
    connection.query("PREPARE factory_prepared AS SELECT v.c FROM native_source v JOIN keys k ON v.a=k.id WHERE k.flag=$1 ORDER BY v.c")?;
    let mut differences = Vec::new();
    for line in include_str!("data/arrow_factory_1.5.6.jsonl").lines() {
        let expected: Json = serde_json::from_str(line).unwrap();
        let name = expected["case"].as_str().unwrap();
        let sql = match name {
            "all" => "SELECT*FROM native_source ORDER BY a",
            "second" => "SELECT b FROM native_source ORDER BY b",
            "reorder" => "SELECT c,b FROM native_source ORDER BY c",
            "filter_other" => "SELECT c FROM native_source WHERE a>=3 ORDER BY c",
            "equality" => "SELECT a FROM native_source WHERE b=104",
            "count" => "SELECT count(*)FROM native_source",
            "empty" => "SELECT count(*)FROM native_source WHERE a<0",
            "aggregates" => "SELECT sum(c),min(b),max(a)FROM native_source",
            "dynamic_join" => {
                "SELECT v.c FROM native_source v JOIN keys k ON v.a=k.id WHERE k.flag='keep' ORDER BY v.c"
            }
            "dynamic_static" => {
                "SELECT v.c FROM native_source v JOIN keys k ON v.a=k.id WHERE k.flag='keep' AND v.b>=103 ORDER BY v.c"
            }
            "prepared_keep" | "prepared_keep_again" => "EXECUTE factory_prepared('keep')",
            "prepared_drop" => "EXECUTE factory_prepared('drop')",
            "prepared_empty" => "EXECUTE factory_prepared('none')",
            "dynamic_profile" => {
                "EXPLAIN(ANALYZE,FORMAT JSON)SELECT v.c FROM native_source v JOIN keys k ON v.a=k.id WHERE k.flag='keep' ORDER BY v.c"
            }
            "sparse_join" => {
                "SELECT v.c FROM native_source v JOIN large_keys k ON v.a=k.id WHERE k.flag=0 ORDER BY v.c"
            }
            "sparse_small" => {
                "SELECT v.c FROM native_source v JOIN sparse_keys k ON v.a=k.id WHERE k.flag=0 ORDER BY v.c"
            }
            "single_join" => {
                "SELECT v.c FROM native_source v JOIN one_keys k ON v.a=k.id WHERE k.flag='one' ORDER BY v.c"
            }
            _ => panic!("unknown frozen factory control"),
        };
        factory.events.lock().unwrap().clear();
        let result = connection.query(sql)?;
        let rows = if name == "dynamic_profile" {
            let text = result.value(1, 0);
            let profile: Json = serde_json::from_str(text.as_str().unwrap()).unwrap();
            vec![vec![
                runtime_filters(&profile).expect("profiled factory dynamic filters"),
            ]]
        } else {
            (0..result.row_count())
                .map(|row| {
                    (0..result.column_count())
                        .map(|col| result.value(col, row).to_varchar().unwrap())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let events = factory.events.lock().unwrap().clone();
        let actual = json!({"case":name,"rows":rows,"producer":events});
        eprintln!("ARROW_FACTORY {actual}");
        if actual != expected {
            eprintln!("EXPECTED {expected}");
            differences.push(name.to_owned());
        }
    }
    assert!(
        differences.is_empty(),
        "different native factory observations: {differences:?}"
    );
    Ok(())
}
