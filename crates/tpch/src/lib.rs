//! The TPC-H extension (DuckDB's `extension/tpch`): `CALL dbgen(sf = ...)` generates the
//! TPC-H tables, `tpch_queries()` and `tpch_answers()` list the 22 queries and their
//! answers, and `PRAGMA tpch(n)` runs query `n`.
//!
//! The generator is a port of the TPC's `dbgen` as DuckDB embeds it ([`dbgen`]); the
//! tables it fills are identical to DuckDB's.

mod dbgen;
pub mod tpch_constants;

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

use crate::dbgen::DbgenTarget;
use crate::tpch_constants::{
    TPCH_ANSWERS_SF0_01, TPCH_ANSWERS_SF0_1, TPCH_ANSWERS_SF1, TPCH_QUERIES, TPCH_QUERIES_COUNT,
};

/// The extension's name (`TpchExtension::Name`).
pub const EXTENSION_NAME: &str = "tpch";

/// The bind data of a `dbgen` call (DuckDB's `DBGenFunctionData`).
#[derive(Debug)]
struct DbgenFunctionData {
    finished: AtomicBool,
    sf: f64,
    target: DbgenTarget,
    suffix: String,
    children: usize,
    step: Option<usize>,
    /// The number of threads of the connection (`TaskScheduler::NumberOfThreads`).
    threads: usize,
}

impl FunctionData for DbgenFunctionData {
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
        .ok_or_else(|| Error::internal("tpch function bound without a client context"))
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

/// `DbgenBind`.
fn dbgen_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    // The current catalog and schema.
    let mut catalog = context.default_catalog();
    let mut schema = context.default_schema().to_owned();
    let mut sf = 0.0;
    let mut suffix = String::new();
    let mut children = 1;
    let mut step = None;
    for (name, value) in input.named_parameters {
        if value.is_null() {
            return Err(Error::binder("Cannot use NULL as function argument"));
        }
        match name.as_str() {
            "sf" => sf = value.as_f64().unwrap_or_default(),
            "catalog" => catalog = value.to_varchar()?,
            "schema" => schema = value.to_varchar()?,
            "suffix" => suffix = value.to_varchar()?,
            "children" => children = value.as_u32().unwrap_or_default() as usize,
            "step" => step = value.as_u32().map(|step| step as usize),
            // `overwrite` is accepted and ignored.
            _ => {}
        }
    }
    if children != 1 && step.is_none() {
        return Err(Error::invalid_input(
            "Step must be defined when children are defined",
        ));
    }
    // `RegisterDBModify`: the call writes to the catalog (see `modified_database`).
    let catalog = context
        .catalog(&catalog)
        .cloned()
        .ok_or_else(|| Error::binder(format!("Catalog \"{catalog}\" does not exist!")))?;
    let data = DbgenFunctionData {
        finished: AtomicBool::new(false),
        sf,
        target: DbgenTarget {
            catalog,
            schema,
            transaction: Arc::clone(&context.transaction),
        },
        suffix,
        children,
        step,
        threads: thread_count(context),
    };
    Ok(TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned()],
        bind_data: Some(Arc::new(data)),
    })
}

/// `DbgenFunction`: creates the tables and generates their data; returns no rows.
fn dbgen_function(bind_data: Option<&dyn FunctionData>, chunk: &mut DataChunk) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<DbgenFunctionData>())
        .ok_or_else(|| Error::internal("dbgen without bind data"))?;
    chunk.set_len(0);
    if data.finished.swap(true, Ordering::AcqRel) {
        return Ok(false);
    }
    dbgen::create_tpch_schema(&data.target, &data.suffix)?;
    dbgen::load_tpch_data(
        &data.target,
        data.sf,
        &data.suffix,
        data.children,
        data.step,
        data.threads,
    )?;
    Ok(false)
}

/// The `dbgen` table function.
fn dbgen_table_function() -> TableFunctionSet {
    let mut function = TableFunction::new("dbgen", Vec::new())
        .with_bind(dbgen_bind)
        .with_scan(|bind_data, _, _, chunk| dbgen_function(bind_data, chunk));
    function.named_parameters = vec![
        ("sf".to_owned(), LogicalType::DOUBLE),
        ("overwrite".to_owned(), LogicalType::BOOLEAN),
        ("catalog".to_owned(), LogicalType::VARCHAR),
        ("schema".to_owned(), LogicalType::VARCHAR),
        ("suffix".to_owned(), LogicalType::VARCHAR),
        ("children".to_owned(), LogicalType::UINTEGER),
        ("step".to_owned(), LogicalType::UINTEGER),
    ];
    let mut set = TableFunctionSet::new("dbgen");
    set.add(function);
    set
}

/// A table function without arguments returning `rows(offset)` from offset 0 until it
/// returns `None` (`TPCHInit`'s offset, with `TPCHQueryFunction` /
/// `TPCHQueryAnswerFunction`).
fn rows_function(
    name: &'static str,
    columns: &'static [(&'static str, LogicalType)],
    rows: fn(usize) -> Option<Vec<Value>>,
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
                for (vector, value) in vectors.iter_mut().zip(&row) {
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

/// The scale factors with answers.
const ANSWER_SCALE_FACTORS: [f64; 3] = [0.01, 0.1, 1.0];

/// `TPCHQueryFunction`: row `offset` of `tpch_queries()`.
fn query_row(offset: usize) -> Option<Vec<Value>> {
    let query = TPCH_QUERIES.get(offset)?;
    Some(vec![Value::integer(offset as i32 + 1), Value::varchar(query)])
}

/// `TPCHQueryAnswerFunction`: row `offset` of `tpch_answers()`, every query of a scale
/// factor before the next scale factor.
fn answer_row(offset: usize) -> Option<Vec<Value>> {
    let query = offset % TPCH_QUERIES_COUNT;
    let sf = *ANSWER_SCALE_FACTORS.get(offset / TPCH_QUERIES_COUNT)?;
    let answer = get_answer(sf, query as i32 + 1).ok()?;
    Some(vec![
        Value::integer(query as i32 + 1),
        Value::double(sf),
        Value::varchar(answer),
    ])
}

/// The extension's table functions (`LoadInternal`): `dbgen`, `tpch_queries` and
/// `tpch_answers`.
pub fn tpch_table_functions() -> Vec<TableFunctionSet> {
    vec![
        dbgen_table_function(),
        rows_function("tpch_queries", QUERY_COLUMNS, query_row),
        rows_function("tpch_answers", ANSWER_COLUMNS, answer_row),
    ]
}

/// The query number `query` checked against the 22 queries.
fn query_index(query: i32) -> Result<usize> {
    usize::try_from(query)
        .ok()
        .filter(|&query| (1..=TPCH_QUERIES_COUNT).contains(&query))
        .map(|query| query - 1)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Syntax,
                format!("Out of range TPC-H query number {query}"),
            )
        })
}

/// `DBGenWrapper::GetQuery`: the text of TPC-H query `query` (1 to 22).
pub fn get_query(query: i32) -> Result<&'static str> {
    Ok(TPCH_QUERIES[query_index(query)?])
}

/// `DBGenWrapper::GetAnswer`: the answer of query `query` at scale factor 0.01, 0.1 or 1,
/// as CSV.
#[expect(clippy::float_cmp, reason = "DuckDB matches the scale factors exactly")]
pub fn get_answer(sf: f64, query: i32) -> Result<&'static str> {
    let index = query_index(query)?;
    let answers = if sf == 0.01 {
        &TPCH_ANSWERS_SF0_01
    } else if sf == 0.1 {
        &TPCH_ANSWERS_SF0_1
    } else if sf == 1.0 {
        &TPCH_ANSWERS_SF1
    } else {
        return Err(Error::not_implemented(format!(
            "Don't have TPC-H answers for SF {sf:.6}!"
        )));
    };
    Ok(answers[index])
}

/// `PragmaTpchQuery`: the query `PRAGMA tpch(n)` runs. The pragma takes one BIGINT
/// (`PragmaFunction::PragmaCall("tpch", ..., {LogicalType::BIGINT})`), bound with
/// implicit casts only, and reads it as an `int32_t`.
pub fn pragma_tpch_query(parameters: &[Value]) -> Result<String> {
    let parameter = match parameters {
        [parameter] if implicit_cast_cost(parameter.logical_type(), &LogicalType::BIGINT) >= 0 => parameter,
        _ => {
            let types: Vec<String> = parameters
                .iter()
                .map(|parameter| parameter.logical_type().to_string())
                .collect();
            return Err(Error::binder(format!(
                "No function matches the given name and argument types 'tpch({})'. You might need to add explicit type casts.\n\tCandidate functions:\n\tPRAGMA tpch(BIGINT)\n",
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
