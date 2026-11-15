use super::catalog::{PgCatalog, type_options};
use super::transaction::{Cursor, Decoder};
use roastedduck_common::{Error, Result};
use roastedduck_function::{
    FunctionClient, FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet,
};
use roastedduck_postgres::{QueryParameter, codec::Codec};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

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

#[derive(Debug)]
struct QueryData {
    catalog: Arc<PgCatalog>,
    client: Arc<FunctionClient>,
    sql: String,
    codecs: Vec<Codec>,
    names: Vec<String>,
    parameters: Vec<QueryParameter>,
    use_transaction: bool,
    text: bool,
}
impl FunctionData for QueryData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn query() -> TableFunction {
    let mut function = TableFunction::new("postgres_query",vec![LogicalType::VARCHAR;2]).with_bind(|input| {
        if input.inputs.iter().any(Value::is_null) { return Err(Error::binder("Parameters to postgres_query cannot be NULL")); }
        let context = context(input)?;
        let catalog = catalog(context,&input.inputs[0].to_varchar()?)?;
        let mut sql = input.inputs[1].to_varchar()?.trim_end().trim_end_matches(';').trim_end().to_owned();
        while sql.ends_with(';') { sql.pop(); sql.truncate(sql.trim_end().len()); }
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
            bind_data: Some(Arc::new(QueryData { catalog, client: context.function_client.clone(), sql, codecs, names, parameters, use_transaction, text: super::use_text_protocol(&context.function_client) })) })
    }).with_scan(|data,column_ids,state,output| {
        let data = data.and_then(|d| d.as_any().downcast_ref::<QueryData>()).ok_or_else(|| Error::internal("Postgres query without bind data"))?;
        if state.local.is_none() {
            let mut projections = Vec::new(); let mut codecs = Vec::new();
            for id in column_ids {
                let index = *id as usize;
                let codec = data.codecs.get(index).ok_or_else(|| Error::internal("Invalid Postgres query column"))?;
                projections.push(codec.projection(&super::identifier(&data.names[index]))?); codecs.push(if data.text {Decoder::Text(codec.logical_type.clone())} else {Decoder::Value(codec.clone())});
            }
            if projections.is_empty() { projections.push("NULL".into()); }
            let sql = format!("SELECT {} FROM ({}) AS __unnamed_subquery",projections.join(","),data.sql);
            let pg = data.catalog.current(&data.client)?;
            if !data.text && !data.parameters.is_empty() {
                pg.execute(&format!("COPY ({sql}) TO STDOUT (FORMAT BINARY)"),data.use_transaction)?;
                return Err(Error::internal("Postgres COPY unexpectedly accepted unbound parameters"));
            }
            let cursor = pg.cursor_query(&sql,&data.parameters,codecs,data.use_transaction,data.text)?;
            state.local = Some(Box::new(cursor));
        }
        let cursor = state.local.as_mut().and_then(|s| s.downcast_mut::<Arc<Mutex<Cursor>>>()).ok_or_else(|| Error::internal("Invalid Postgres query cursor"))?;
        if let Some(chunk) = cursor.lock().unwrap_or_else(PoisonError::into_inner).next()? {
            state.position += chunk.len() as u64; *output = chunk; Ok(true)
        } else { output.set_len(0); Ok(false) }
    });
    function.projection_pushdown = true;
    function.projected_output = true;
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
