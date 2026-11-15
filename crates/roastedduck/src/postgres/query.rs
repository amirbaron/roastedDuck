use super::catalog::scan::{self, Columns, Data};
use super::catalog::{PgCatalog, type_options};
use roastedduck_common::{Error, Result};
use roastedduck_function::{
    FunctionClient, FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet,
};
use roastedduck_postgres::codec::Codec;
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::any::Any;
use std::sync::Arc;

fn context<'a>(input: &'a TableFunctionBindInput<'_>) -> Result<&'a roastedduck_planner::ClientContext> {
    input
        .binder
        .and_then(|b| b.downcast_ref::<roastedduck_planner::ClientContext>())
        .ok_or_else(|| Error::internal("Postgres function requires client context"))
}
fn catalog(context: &roastedduck_planner::ClientContext, name: &str) -> Result<Arc<PgCatalog>> {
    let database = crate::Database::from_instance(&crate::database::instance_state(context)?.instance())
        .ok_or_else(|| Error::internal("Postgres function after shutdown"))?;
    let attached = database.instance().databases.get(name).ok_or_else(|| {
        Error::binder(format!(
            "Failed to find attached database \"{name}\" referenced in postgres_query"
        ))
    })?;
    match &attached.storage {
        crate::attached::DatabaseStorage::Postgres(catalog) => Ok(catalog.clone()),
        _ => Err(Error::binder(format!(
            "Attached database \"{name}\" does not refer to a Postgres database"
        ))),
    }
}
fn use_transaction(input: &TableFunctionBindInput<'_>) -> Result<bool> {
    input
        .named_parameters
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(b"use_transaction"))
        .map_or(Ok(true), |(_, v)| {
            v.as_bool()
                .ok_or_else(|| Error::binder("use_transaction cannot be NULL"))
        })
}

// The pinned StringUtil::RTrim treats nonpositive C char bytes as trailing.
fn rtrim(value: &str) -> &str {
    let end = value
        .as_bytes()
        .iter()
        .rposition(|b| {
            (*b as std::ffi::c_char) > 0 && !matches!(*b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
        })
        .map_or(0, |p| p + 1);
    &value[..end]
}
fn query() -> TableFunction {
    let mut function = scan::function("postgres_query",vec![LogicalType::VARCHAR;2],false).with_bind(|input| {
        if input.inputs.iter().any(Value::is_null) { return Err(Error::binder("Parameters to postgres_query cannot be NULL")); }
        let context = context(input)?;
        let catalog = catalog(context,&input.inputs[0].to_varchar()?)?;
        let mut sql = input.inputs[1].to_varchar()?;
        sql.truncate(rtrim(&sql).len());
        while sql.ends_with(';') { sql.pop(); sql.truncate(rtrim(&sql).len()); }
        let use_transaction = use_transaction(input)?;
        let mut values = Vec::new();
        if let Some((_,value)) = input.named_parameters.iter().find(|(n,_)| n.eq_ignore_ascii_case(b"params")) {
            if value.is_null() { return Err(Error::binder("Parameters to postgres_query cannot be NULL")); }
            if value.logical_type().id() != roastedduck_types::LogicalTypeId::Struct { return Err(Error::binder("Query parameters must be specified in a STRUCT")); }
            values = value.children().unwrap_or_default().to_vec();
        }
        let description = catalog.transaction(&context.function_client,&context.transaction)?.describe_query(&sql,use_transaction)?;
        if description.columns.is_empty() { return Err(Error::binder(format!("No fields returned by query \"{sql}\" - the query must be a SELECT statement that returns at least one column"))); }
        let options = type_options(&context.function_client);
        let codecs = description.columns.iter().map(|c| Codec::for_query(&c.data_type,c.type_modifier,options)).collect::<Result<Vec<_>>>()?;
        if values.len() != description.parameter_types.len() { return Err(Error::binder(format!("Incorrect number of parameters specified, expected: {}, actual: {}, query: \"{sql}\"",description.parameter_types.len(),values.len()))); }
        let parameters=values.iter().map(roastedduck_postgres::parameters::encode).collect::<Result<Vec<_>>>()?;
        let names = description.columns.into_iter().map(|c| c.name).collect::<Vec<_>>();
        Ok(TableFunctionBindOutput { return_types: codecs.iter().map(|c| c.logical_type.clone()).collect(), names: names.iter().map(|n| n.as_str().into()).collect(),
            bind_data: Some(Arc::new(Data::query(catalog, context.function_client.clone(),&sql,Columns{names,codecs},parameters,use_transaction))) })
    });
    // PGQueryFunction only inherits scan callbacks and projection pushdown.
    function.cardinality = None;
    function.static_explain = None;
    function.named_parameters = vec![
        ("use_transaction".into(), LogicalType::BOOLEAN),
        ("params".into(), LogicalType::ANY),
    ];
    function
}

#[derive(Debug)]
struct ExecuteData {
    catalog: Arc<PgCatalog>,
    client: Arc<FunctionClient>,
    sql: String,
    use_transaction: bool,
}
impl FunctionData for ExecuteData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn execute() -> TableFunction {
    let mut function = TableFunction::new("postgres_execute", vec![LogicalType::VARCHAR; 2])
        .with_bind(|input| {
            if input.inputs.iter().any(Value::is_null) {
                return Err(Error::binder("Parameters to postgres_execute cannot be NULL"));
            }
            let context = context(input)?;
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN],
                names: vec!["Success".into()],
                bind_data: Some(Arc::new(ExecuteData {
                    catalog: catalog(context, &input.inputs[0].to_varchar()?)?,
                    client: context.function_client.clone(),
                    sql: input.inputs[1].to_varchar()?,
                    use_transaction: use_transaction(input)?,
                })),
            })
        })
        .with_scan(|data, _, state, output| {
            *output = DataChunk::new(vec![Vector::flat(LogicalType::BOOLEAN, 0)], 0);
            if state.position != 0 {
                return Ok(false);
            }
            let data = data
                .and_then(|d| d.as_any().downcast_ref::<ExecuteData>())
                .ok_or_else(|| Error::internal("Postgres execute without data"))?;
            data.catalog
                .current(&data.client)?
                .execute(&data.sql, data.use_transaction)?;
            state.position = 1;
            Ok(false)
        });
    function.named_parameters = vec![("use_transaction".into(), LogicalType::BOOLEAN)];
    function
}

#[derive(Debug)]
struct ClearData(Vec<Arc<PgCatalog>>);
impl FunctionData for ClearData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn clear_cache() -> TableFunction {
    TableFunction::new("pg_clear_cache", vec![])
        .with_bind(|input| {
            let context = context(input)?;
            let database =
                crate::Database::from_instance(&crate::database::instance_state(context)?.instance())
                    .ok_or_else(|| Error::internal("Postgres cache clear after shutdown"))?;
            let catalogs = database
                .instance()
                .databases
                .all()
                .iter()
                .filter_map(|d| match &d.storage {
                    crate::attached::DatabaseStorage::Postgres(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN],
                names: vec!["Success".into()],
                bind_data: Some(Arc::new(ClearData(catalogs))),
            })
        })
        .with_scan(|data, _, state, output| {
            *output = DataChunk::new(vec![Vector::flat(LogicalType::BOOLEAN, 0)], 0);
            if state.position != 0 {
                return Ok(false);
            }
            let data = data
                .and_then(|d| d.as_any().downcast_ref::<ClearData>())
                .ok_or_else(|| Error::internal("Postgres cache clear without data"))?;
            for catalog in &data.0 {
                catalog.clear_cache();
            }
            state.position = 1;
            Ok(false)
        })
}
pub(crate) fn query_functions() -> Vec<TableFunctionSet> {
    [query(), execute(), clear_cache()]
        .into_iter()
        .map(|function| {
            let mut set = TableFunctionSet::new(function.name.clone());
            set.add(function);
            set
        })
        .collect()
}
