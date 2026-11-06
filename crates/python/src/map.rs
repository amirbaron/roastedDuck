//! Native table-in/table-out dataframe callbacks, retaining Python owners with the query tree.
use crate::connection::lock;
use crate::{host, pandas};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rustdb_ast::{
    ExpressionType, ExternalDependency, OrdinalityType, ParsedExpression, ParsedExpressionKind,
    SelectStatement, SubqueryExpression, SubqueryType, TableFunctionRef, TableRef, TableRefKind,
};
use rustdb_common::{ByteString, Error, Result, STANDARD_VECTOR_SIZE, byte_format};
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value};
use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

static IDS: AtomicU64 = AtomicU64::new(1);
static INPUTS: OnceLock<Mutex<HashMap<u64, Weak<Callback>>>> = OnceLock::new();
fn inputs() -> &'static Mutex<HashMap<u64, Weak<Callback>>> {
    INPUTS.get_or_init(Mutex::default)
}
#[derive(Debug)]
struct Callback {
    id: u64,
    function: Py<PyAny>,
    schema: Py<PyAny>,
}
impl Drop for Callback {
    fn drop(&mut self) {
        lock(inputs()).remove(&self.id);
    }
}
#[derive(Debug)]
struct BoundData {
    owner: Arc<Callback>,
    in_names: Vec<ByteString>,
    in_types: Vec<LogicalType>,
    out_names: Vec<ByteString>,
    out_types: Vec<LogicalType>,
    sample: usize,
    properties: rustdb_arrow::ClientProperties,
}
impl FunctionData for BoundData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(super) fn table(
    source: SelectStatement,
    function: &Bound<'_, PyAny>,
    schema: Option<&Bound<'_, PyAny>>,
) -> TableRef {
    let owner = Arc::new(Callback {
        id: IDS.fetch_add(1, Ordering::Relaxed),
        function: function.clone().unbind(),
        schema: schema.map_or_else(|| function.py().None(), |s| s.clone().unbind()),
    });
    lock(inputs()).insert(owner.id, Arc::downgrade(&owner));
    let subquery = ParsedExpression::new(
        ExpressionType::Subquery,
        ParsedExpressionKind::Subquery(SubqueryExpression {
            subquery_type: SubqueryType::Scalar,
            subquery: Box::new(source),
            child: None,
            comparison_type: ExpressionType::Invalid,
        }),
    );
    let mut table = rustdb_parser::build::table_ref(TableRefKind::TableFunction(TableFunctionRef {
        function: Box::new(rustdb_parser::build::function(
            "python_map_function",
            vec![
                subquery,
                rustdb_parser::build::constant(Value::ubigint(owner.id).reinterpret(LogicalType::POINTER)),
                rustdb_parser::build::constant(Value::ubigint(owner.id).reinterpret(LogicalType::POINTER)),
            ],
        )),
        column_name_alias: Vec::new(),
        with_ordinality: OrdinalityType::WithoutOrdinality,
    }));
    table.alias = "python_map_function".into();
    table.external_dependency = ExternalDependency::new(owner);
    table
}

fn call(py: Python<'_>, data: &BoundData, chunk: Option<&DataChunk>) -> PyResult<Py<PyAny>> {
    let metadata = host::Metadata {
        names: data.in_names.clone(),
        types: data.in_types.clone(),
        timezone: data.properties.time_zone.clone(),
        consumed: false,
        materialized: false,
        materialized_empty: false,
        arrow: data.properties.clone(),
    };
    let frame = host::map_frame(py, &metadata, chunk)?;
    let pd = py.import("pandas")?;
    let result = data.owner.function.bind(py).call1((frame,)).map_err(|error| {
        error.print_and_set_sys_last_vars(py);
        crate::error::InvalidInputException::new_err(
            "Invalid Input Error: Python error. See above for a stack trace.",
        )
    })?;
    if result.is_none() {
        return Err(crate::error::InvalidInputException::new_err(
            "Invalid Input Error: No return value from Python function",
        ));
    }
    if !pandas::is_dataframe(&result)? {
        return Err(crate::error::InvalidInputException::new_err(format!(
            "Invalid Input Error: Expected the UDF to return an object of type 'pandas.DataFrame', found '{}' instead",
            result.getattr("__class__")?.str()?
        )));
    }
    if let Ok(arrow_dtype) = pd.getattr("ArrowDtype") {
        for dtype in result.getattr("dtypes")?.try_iter()? {
            if dtype?.is_instance(&arrow_dtype)? {
                return Err(crate::error::InvalidInputException::new_err(
                    "Invalid Input Error: Produced DataFrame has columns that are backed by PyArrow, which is not supported yet in 'map'",
                ));
            }
        }
    }
    Ok(result.unbind())
}
fn boundary(error: &PyErr) -> Error {
    pandas::python_error(error)
}
fn binding(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let id = input
        .inputs
        .get(1)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::invalid_input("Missing map function identity"))?;
    let owner = lock(inputs())
        .get(&id)
        .and_then(Weak::upgrade)
        .ok_or_else(|| Error::invalid_input("Map function is no longer available"))?;
    let mut data = BoundData {
        owner,
        in_names: input.input_table_names.to_vec(),
        in_types: input.input_table_types.to_vec(),
        out_names: Vec::new(),
        out_types: Vec::new(),
        sample: 1000,
        properties: rustdb_arrow::ClientProperties::default(),
    };
    if let Some(settings) = input
        .binder
        .and_then(|b| b.downcast_ref::<rustdb_planner::ClientContext>())
        .and_then(|c| c.function_client.settings())
    {
        if let Ok(value) = settings.current_setting("pandas_analyze_sample") {
            data.sample = value.as_u64().unwrap_or(1000) as usize;
        }
        if let Ok(value) = settings.current_setting("TimeZone") {
            data.properties.time_zone = value.to_varchar().unwrap_or_else(|_| "UTC".to_owned());
        }
    }
    Python::attach(|py| -> Result<()> {
        let schema = data.owner.schema.bind(py);
        if schema.is_none() {
            let frame = call(py, &data, None).map_err(|e| boundary(&e))?;
            let output = pandas::bind_dataframe(frame.bind(py), data.sample)?;
            data.out_names = output.names;
            data.out_types = output.return_types;
            if data.out_types.len() == data.in_types.len() {
                for index in 0..data.out_types.len() {
                    if data.out_types[index].id() == LogicalTypeId::SqlNull {
                        if data.out_names[index] != data.in_names[index] {
                            return Err(Error::invalid_input(
                                "Returned dataframe contains NULL type, and we could not infer the desired type",
                            ));
                        }
                        data.out_types[index] = data.in_types[index].clone();
                    }
                }
            }
        } else {
            let schema = schema
                .cast::<PyDict>()
                .map_err(|_| Error::invalid_input("'schema' should be given as a Dict[str, DuckDBType]"))?;
            for (name, kind) in schema.iter() {
                // pybind11 permits None for shared_ptr<DuckDBPyType>, then Type()
                // raises DuckDB's checked null-pointer internal exception.
                if kind.is_none() {
                    return Err(Error::internal(
                        "Attempted to dereference shared_ptr that is NULL!",
                    ));
                }

                data.out_names.push(ByteString::from(
                    name.str()
                        .and_then(|s| s.extract::<String>())
                        .map_err(|e| boundary(&e))?,
                ));
                data.out_types.push(crate::udf::annotation(&kind).map_err(|_| {
                    Error::new(rustdb_common::ErrorKind::Invalid, format!("Unable to cast Python instance of type {} to C++ type '?' (#define PYBIND11_DETAILED_ERROR_MESSAGES or compile in debug mode for details)", kind.get_type().repr().map_or_else(|_| "?".to_owned(), |s| s.to_string())))
                        .with_subtype("python_callback")
                })?);
            }
        }
        Ok(())
    })?;
    Ok(TableFunctionBindOutput {
        names: data.out_names.clone(),
        return_types: data.out_types.clone(),
        bind_data: Some(Arc::new(data)),
    })
}
fn types(types: &[LogicalType]) -> ByteString {
    ByteString::join(
        &types.iter().map(LogicalType::to_bytes).collect::<Vec<_>>(),
        b", ",
    )
}
fn execute(
    data: Option<&dyn FunctionData>,
    _: &mut TableScanState,
    chunk: &DataChunk,
    output: &mut DataChunk,
) -> Result<InOutResult> {
    let data = data
        .and_then(|d| d.as_any().downcast_ref::<BoundData>())
        .ok_or_else(|| Error::internal("Missing map bind data"))?;
    if chunk.is_empty() {
        output.reset();
        return Ok(InOutResult::NeedMoreInput);
    }
    Python::attach(|py| -> Result<()> {
        let frame = call(py, data, Some(chunk)).map_err(|e| boundary(&e))?;
        let bound = pandas::bind_dataframe(frame.bind(py), data.sample)?;
        if bound.return_types.len() != data.out_types.len() {
            return Err(Error::invalid_input(format!(
                "Expected {} columns from UDF, got {}",
                data.out_types.len(),
                bound.return_types.len()
            )));
        }
        if bound.return_types != data.out_types {
            return Err(Error::invalid_input(byte_format!(
                "UDF column type mismatch, expected [{}], got [{}]",
                types(&data.out_types),
                types(&bound.return_types)
            )));
        }
        if bound.names != data.out_names {
            return Err(Error::invalid_input(byte_format!(
                "UDF column name mismatch, expected [{}], got [{}]",
                ByteString::join(&data.out_names, b", "),
                ByteString::join(&bound.names, b", ")
            )));
        }
        let bound = bound
            .bind_data
            .as_deref()
            .ok_or_else(|| Error::internal("Missing mapped dataframe bind data"))?;
        if pandas::row_count(bound)? > STANDARD_VECTOR_SIZE {
            return Err(Error::invalid_input(format!(
                "UDF returned more than {STANDARD_VECTOR_SIZE} rows, which is not allowed."
            )));
        }
        pandas::scan(Some(bound), &[], &mut TableScanState::default(), output)?;
        Ok(())
    })?;
    Ok(InOutResult::NeedMoreInput)
}
pub(super) fn functions() -> TableFunctionSet {
    let mut set = TableFunctionSet::new("python_map_function");
    set.add(
        TableFunction::new(
            "python_map_function",
            vec![
                LogicalType::simple(LogicalTypeId::Table),
                LogicalType::POINTER,
                LogicalType::POINTER,
            ],
        )
        .with_bind(binding)
        .with_in_out(execute),
    );
    set
}
