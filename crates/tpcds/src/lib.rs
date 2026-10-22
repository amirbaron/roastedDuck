//! The TPC-DS extension (DuckDB's `extension/tpcds`): `CALL dsdgen(sf = ...)` generates the
//! TPC-DS tables, `tpcds_queries()` and `tpcds_answers()` list the 99 queries and their
//! answers, and `PRAGMA tpcds(n)` runs query `n`.
//!
//! The generator is a port of the TPC's `dsdgen` as DuckDB embeds it ([`dsdgen`]); the
//! tables it fills are identical to DuckDB's.

mod dsdgen;
pub mod tpcds_constants;

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::cast::ValueCast;
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, LogicalType, Value, Vector, implicit_cast_cost};

use crate::dsdgen::DsdgenTarget;
use crate::tpcds_constants::{TPCDS_ANSWERS_SF0_01, TPCDS_ANSWERS_SF1, TPCDS_QUERIES, TPCDS_QUERIES_COUNT};

/// The extension's name (`TpcdsExtension::Name`).
pub const EXTENSION_NAME: &str = "tpcds";

/// The bind data of a `dsdgen` call (DuckDB's `DSDGenFunctionData`).
#[derive(Debug)]
struct DsdgenFunctionData {
    finished: AtomicBool,
    sf: f64,
    target: DsdgenTarget,
    suffix: String,
    overwrite: bool,
    keys: bool,
    /// The number of threads of the connection (`TaskScheduler::NumberOfThreads`).
    threads: usize,
}

impl FunctionData for DsdgenFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn modified_database(&self) -> Option<String> {
        Some(self.target.catalog.name().to_string())
    }
}

/// The binder's client context of a table function call.
fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("tpcds function bound without a client context"))
}

/// The number of threads queries of `context` run on.
fn thread_count(context: &ClientContext) -> usize {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting("threads").ok())
        .and_then(|threads| threads.as_i128())
        .and_then(|threads| usize::try_from(threads).ok())
        .unwrap_or(1)
}

/// `DsdgenBind`.
fn dsdgen_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    // The current catalog and schema.
    let mut catalog = context.default_catalog();
    let mut schema = context.default_schema().to_owned();
    let mut sf = 0.0;
    let mut suffix = String::new();
    let mut overwrite = false;
    let mut keys = false;
    for (name, value) in input.named_parameters {
        if value.is_null() {
            return Err(Error::binder("Cannot use NULL as function argument"));
        }
        match name.as_str() {
            "sf" => sf = value.as_f64().unwrap_or_default(),
            "catalog" => catalog = value.to_varchar()?,
            "schema" => schema = value.to_varchar()?,
            "suffix" => suffix = value.to_varchar()?,
            "overwrite" => overwrite = value.as_bool().unwrap_or_default(),
            "keys" => keys = value.as_bool().unwrap_or_default(),
            _ => {}
        }
    }
    // `RegisterDBModify`: the call writes to the catalog (see `modified_database`).
    let catalog = context
        .catalog(&catalog)
        .cloned()
        .ok_or_else(|| Error::binder(format!("Catalog \"{catalog}\" does not exist!")))?;
    let data = DsdgenFunctionData {
        finished: AtomicBool::new(false),
        sf,
        target: DsdgenTarget {
            catalog,
            schema,
            transaction: Arc::clone(&context.transaction),
        },
        suffix,
        overwrite,
        keys,
        threads: thread_count(context),
    };
    Ok(TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned()],
        bind_data: Some(Arc::new(data)),
    })
}

/// `DsdgenFunction`: creates the tables and generates their data; returns no rows.
fn dsdgen_function(bind_data: Option<&dyn FunctionData>, chunk: &mut DataChunk) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<DsdgenFunctionData>())
        .ok_or_else(|| Error::internal("dsdgen without bind data"))?;
    chunk.set_len(0);
    if data.finished.swap(true, Ordering::AcqRel) {
        return Ok(false);
    }
    dsdgen::create_tpcds_schema(&data.target, &data.suffix, data.keys, data.overwrite)?;
    dsdgen::dsdgen(&data.target, data.sf, &data.suffix, data.threads)?;
    Ok(false)
}

/// The `dsdgen` table function.
fn dsdgen_table_function() -> TableFunctionSet {
    let mut function = TableFunction::new("dsdgen", Vec::new())
        .with_bind(dsdgen_bind)
        .with_scan(|bind_data, _, _, chunk| dsdgen_function(bind_data, chunk));
    function.named_parameters = vec![
        ("sf".to_owned(), LogicalType::DOUBLE),
        ("overwrite".to_owned(), LogicalType::BOOLEAN),
        ("keys".to_owned(), LogicalType::BOOLEAN),
        ("catalog".to_owned(), LogicalType::VARCHAR),
        ("schema".to_owned(), LogicalType::VARCHAR),
        ("suffix".to_owned(), LogicalType::VARCHAR),
    ];
    let mut set = TableFunctionSet::new("dsdgen");
    set.add(function);
    set
}

/// A table function without arguments returning `rows(offset)` from offset 0 until it
/// returns `None` (`TPCDSInit`'s offset, with `TPCDSQueryFunction` /
/// `TPCDSQueryAnswerFunction`).
fn rows_function(
    name: &'static str,
    columns: &'static [(&'static str, LogicalType)],
    rows: fn(usize) -> Option<Result<Vec<Value>>>,
) -> TableFunctionSet {
    let function = TableFunction::new(name, Vec::new())
        .with_bind(move |_| {
            Ok(TableFunctionBindOutput {
                return_types: columns.iter().map(|(_, ty)| ty.clone()).collect(),
                names: columns.iter().map(|(name, _)| (*name).to_owned()).collect(),
                bind_data: None,
            })
        })
        .with_scan(move |_, _, state, chunk| {
            let mut vectors: Vec<Vector> = columns
                .iter()
                .map(|(_, ty)| Vector::flat(ty.clone(), STANDARD_VECTOR_SIZE))
                .collect();
            let mut count = 0;
            while count < STANDARD_VECTOR_SIZE {
                let Some(row) = rows(state.position as usize) else {
                    break;
                };
                for (vector, value) in vectors.iter_mut().zip(&row?) {
                    vector.set_value(count, value)?;
                }
                state.position += 1;
                count += 1;
            }
            *chunk = DataChunk::new(vectors, count);
            Ok(count == STANDARD_VECTOR_SIZE)
        });
    let mut set = TableFunctionSet::new(name);
    set.add(function);
    set
}

const QUERY_COLUMNS: &[(&str, LogicalType)] = &[
    ("query_nr", LogicalType::INTEGER),
    ("query", LogicalType::VARCHAR),
];

const ANSWER_COLUMNS: &[(&str, LogicalType)] = &[
    ("query_nr", LogicalType::INTEGER),
    ("scale_factor", LogicalType::DOUBLE),
    ("answer", LogicalType::VARCHAR),
];

/// The scale factors `TPCDSQueryAnswerFunction` lists. There are no answers at SF 10, so
/// listing them fails as in DuckDB.
const ANSWER_SCALE_FACTORS: [f64; 2] = [1.0, 10.0];

/// `TPCDSQueryFunction`: row `offset` of `tpcds_queries()`.
fn query_row(offset: usize) -> Option<Result<Vec<Value>>> {
    let query = TPCDS_QUERIES.get(offset)?;
    Some(Ok(vec![Value::integer(offset as i32 + 1), Value::varchar(query)]))
}

/// `TPCDSQueryAnswerFunction`: row `offset` of `tpcds_answers()`, every query of a scale
/// factor before the next scale factor.
fn answer_row(offset: usize) -> Option<Result<Vec<Value>>> {
    let query = offset % TPCDS_QUERIES_COUNT;
    let sf = *ANSWER_SCALE_FACTORS.get(offset / TPCDS_QUERIES_COUNT)?;
    Some(get_answer(sf, query as i32 + 1).map(|answer| {
        vec![
            Value::integer(query as i32 + 1),
            Value::double(sf),
            Value::varchar(answer),
        ]
    }))
}

/// The extension's table functions (`LoadInternal`): `dsdgen`, `tpcds_queries` and
/// `tpcds_answers`.
pub fn tpcds_table_functions() -> Vec<TableFunctionSet> {
    vec![
        dsdgen_table_function(),
        rows_function("tpcds_queries", QUERY_COLUMNS, query_row),
        rows_function("tpcds_answers", ANSWER_COLUMNS, answer_row),
    ]
}

/// The query number `query` checked against the 99 queries.
fn query_index(query: i32) -> Result<usize> {
    usize::try_from(query)
        .ok()
        .filter(|&query| (1..=TPCDS_QUERIES_COUNT).contains(&query))
        .map(|query| query - 1)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Syntax,
                format!("Out of range TPC-DS query number {query}"),
            )
        })
}

/// `DSDGenWrapper::GetQuery`: the text of TPC-DS query `query` (1 to 99).
pub fn get_query(query: i32) -> Result<&'static str> {
    Ok(TPCDS_QUERIES[query_index(query)?])
}

/// `DSDGenWrapper::GetAnswer`: the answer of query `query` at scale factor 0.01 or 1, as
/// CSV.
#[expect(clippy::float_cmp, reason = "DuckDB matches the scale factors exactly")]
pub fn get_answer(sf: f64, query: i32) -> Result<&'static str> {
    let index = query_index(query)?;
    let answers = if sf == 0.01 {
        &TPCDS_ANSWERS_SF0_01
    } else if sf == 1.0 {
        &TPCDS_ANSWERS_SF1
    } else {
        return Err(Error::not_implemented(format!(
            "Don't have TPC-DS answers for SF {sf:.6}!"
        )));
    };
    Ok(answers[index])
}

/// `PragmaTpcdsQuery`: the query `PRAGMA tpcds(n)` runs. The pragma takes one BIGINT
/// (`PragmaFunction::PragmaCall("tpcds", ..., {LogicalType::BIGINT})`), bound with
/// implicit casts only, and reads it as an `int32_t`.
pub fn pragma_tpcds_query(parameters: &[Value]) -> Result<String> {
    let parameter = match parameters {
        [parameter] if implicit_cast_cost(parameter.logical_type(), &LogicalType::BIGINT) >= 0 => parameter,
        _ => {
            let types: Vec<String> = parameters
                .iter()
                .map(|parameter| parameter.logical_type().to_string())
                .collect();
            return Err(Error::binder(format!(
                "No function matches the given name and argument types 'tpcds({})'. You might need to add explicit type casts.\n\tCandidate functions:\n\tPRAGMA tpcds(BIGINT)\n",
                types.join(", ")
            )));
        }
    };
    let value = parameter
        .default_cast_as(&LogicalType::BIGINT)?
        .as_i64()
        .ok_or_else(|| Error::internal("Calling GetValueInternal on a value that is NULL"))?;
    let query = i32::try_from(value).map_err(|_| {
        Error::invalid_input(format!(
            "Type INT64 with value {value} can't be cast because the value is out of range for the destination type INT32"
        ))
    })?;
    get_query(query).map(str::to_owned)
}
