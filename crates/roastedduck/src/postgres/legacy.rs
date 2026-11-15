//! Standalone table binding. Connections and snapshots are independent of ATTACH.
use super::catalog::scan::{self, Columns, Data};
use roastedduck_common::{Error, Result};
use roastedduck_function::{TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet};
use roastedduck_postgres::{
    Driver,
    codec::Codec,
    config::Parameters,
    pool::{Config, Pool},
};
use roastedduck_types::{LogicalType, Value};
use std::sync::Arc;

fn bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = input
        .binder
        .and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
        .ok_or_else(|| Error::internal("Postgres scan requires a client context"))?;
    if input.inputs.iter().any(Value::is_null) {
        return Err(Error::invalid_input("Parameters to postgres_scan cannot be NULL"));
    }
    let dsn = input.inputs[0].to_varchar()?;
    let schema = input.inputs[1].to_varchar()?;
    let table = input.inputs[2].to_varchar()?;
    let driver = Driver::new()?;
    let pool = Pool::new(
        Config {
            max_connections: 0,
            reaper: false,
            health_check_query: String::new(),
            ..Config::default()
        },
        Arc::new(move || driver.connect(&Parameters::parse(&dsn)?)),
    )?;
    let client = &context.function_client;
    let lease = pool.acquire(client.interrupt_flag().as_deref())?;
    let session = lease.session()?;
    let read = |sql: &str| {
        super::logging::query(client, sql, || {
            session
                .text_query(sql, client.interrupt_flag().as_deref())
                .map_err(|e| super::transaction::query_error(sql, e))
        })
    };
    let version = super::version::Version::read(session, client);
    let info_schema = client
        .settings()
        .and_then(|s| s.current_setting("pg_use_information_schema_introspection").ok())
        .and_then(|v| v.as_bool())
        == Some(true);
    let sql = if info_schema {
        format!(
            "SELECT column_name,data_type FROM information_schema.columns WHERE table_schema={} AND table_name={} AND table_schema NOT IN ('information_schema','pg_catalog','pg_toast') ORDER BY ordinal_position",
            super::literal(&schema),
            super::literal(&table)
        )
    } else {
        format!(
            "SELECT a.attname,a.atttypid,a.atttypmod,a.attndims,c.relpages FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid WHERE n.nspname={} AND c.relname={} AND c.relkind IN ('r','v','m','f','p') AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum",
            super::literal(&schema),
            super::literal(&table)
        )
    };
    let rows = read(&sql)?.rows;
    if rows.is_empty() {
        return Err(Error::invalid_input(format!(
            "Table {table} does not contain any columns."
        )));
    }
    let options = super::type_options(client);
    let mut names = Vec::new();
    let mut codecs = Vec::new();
    let mut pages = 0;
    for row in rows {
        let field = |i: usize| {
            row.get(i)
                .and_then(Option::as_deref)
                .ok_or_else(|| Error::internal("Missing Postgres metadata field"))
        };
        let name = field(0)?.to_owned();
        let codec = if info_schema {
            Codec::from_information_schema(field(1)?, options)?
        } else {
            let oid = field(1)?
                .parse()
                .map_err(|_| Error::internal("Invalid Postgres type OID"))?;
            let modifier = field(2)?
                .parse()
                .map_err(|_| Error::internal("Invalid Postgres type modifier"))?;
            let dimensions = field(3)?
                .parse::<i32>()
                .map_err(|_| Error::internal("Invalid Postgres dimensions"))?
                .max(0) as usize;
            pages = field(4)?
                .parse::<i64>()
                .map_err(|_| Error::internal("Invalid Postgres table pages"))?;
            let column =
                session.catalog_column(name.clone(), oid, modifier, client.interrupt_flag().as_deref())?;
            Codec::new(&column.data_type, modifier, dimensions, options)?
        };
        names.push(name);
        codecs.push(codec);
    }
    drop(lease);
    let database = crate::Database::from_instance(&crate::database::instance_state(context)?.instance())
        .ok_or_else(|| Error::internal("Postgres scan after shutdown"))?;
    let data = Data::standalone(
        pool,
        database.instance().env.buffer_manager.clone(),
        client.clone(),
        Columns {
            names: names.clone(),
            codecs: codecs.clone(),
        },
        format!("{}.{}", super::identifier(&schema), super::identifier(&table)),
        table,
        version,
        pages,
    );
    Ok(TableFunctionBindOutput {
        names: names.into_iter().map(Into::into).collect(),
        return_types: codecs.iter().map(|c| c.logical_type.clone()).collect(),
        bind_data: Some(Arc::new(data)),
    })
}
pub(super) fn functions() -> Vec<TableFunctionSet> {
    ["postgres_scan", "postgres_scan_pushdown"]
        .into_iter()
        .map(|name| {
            let function = scan::function(
                name,
                vec![LogicalType::VARCHAR; 3],
                name == "postgres_scan_pushdown",
            )
            .with_bind(bind);
            let mut set = TableFunctionSet::new(name);
            set.add(function);
            set
        })
        .collect()
}
