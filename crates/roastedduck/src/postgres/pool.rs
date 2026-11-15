//! Runtime settings and SQL control for PostgreSQL-owned connection pools.
use roastedduck_ast::SetScope;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{
    ExtensionOption, FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_postgres::pool::{AcquireMode, Config};
use roastedduck_types::{DataChunk, LogicalType, Value};
use std::any::Any;
use std::sync::{Arc, Weak};

fn setting(client: &FunctionClient, name: &str) -> Option<Value> {
    client.settings().and_then(|s| s.current_setting(name).ok())
}
pub(crate) fn from_client(client: &FunctionClient) -> Result<Config> {
    let mut config = Config::default();
    if let Some(value) = setting(client, "pg_pool_acquire_mode") {
        config.acquire_mode = AcquireMode::parse(&value.to_varchar()?)?;
    }
    for (name, target) in [
        ("pg_pool_max_connections", &mut config.max_connections),
        ("pg_pool_wait_timeout_millis", &mut config.wait_timeout_millis),
        ("pg_pool_max_lifetime_millis", &mut config.max_lifetime_millis),
        ("pg_pool_idle_timeout_millis", &mut config.idle_timeout_millis),
    ] {
        if let Some(value) = setting(client, name).and_then(|v| v.as_u64()) {
            *target = value;
        }
    }
    if let Some(value) = setting(client, "pg_pool_enable_thread_local_cache").and_then(|v| v.as_bool()) {
        config.thread_local_cache = value;
    }
    if let Some(value) = setting(client, "pg_pool_enable_reaper_thread").and_then(|v| v.as_bool()) {
        config.reaper = value;
    }
    if let Some(value) = setting(client, "pg_pool_health_check_query") {
        config.health_check_query = if value.is_null() {
            String::new()
        } else {
            value.to_varchar()?
        };
    }
    Ok(config)
}
fn description(name: &str) -> String {
    let (text, example) = match name {
        "pg_connection_limit" => (
            "The maximum amount of concurrent Postgres connections. This option is deprecated; use pg_pool_max_connections for newly attached databases and postgres_configure_pool for attached databases.",
            None,
        ),
        "pg_connection_cache" => (
            "Whether or not to use connection pooling. This option is deprecated; use pg_pool_max_connections=0 for newly attached databases and postgres_configure_pool(max_connections=0) for attached databases.",
            None,
        ),
        "pg_pool_acquire_mode" => (
            "How to acquire connections from the pool: 'force' (always connect, ignore pool limit), 'wait' (block until available), 'try' (fail immediately if unavailable) (default: force)",
            None,
        ),
        "pg_pool_max_connections" => (
            "Maximum number of connections cached per attached Postgres database; parallel scans may temporarily exceed this limit. ",
            Some("max_connections=42"),
        ),
        "pg_pool_wait_timeout_millis" => (
            "Maximum milliseconds to wait when all connections are in use. ",
            Some("wait_timeout_millis=60000"),
        ),
        "pg_pool_enable_thread_local_cache" => (
            "Pin cached connections to their returning thread; they count towards the pool limit and are unavailable to other threads. ",
            Some("enable_thread_local_cache=FALSE"),
        ),
        "pg_pool_max_lifetime_millis" => (
            "Maximum session lifetime in milliseconds, checked on acquisition, return and by an enabled background reaper. Zero disables lifetime expiry. ",
            Some("max_lifetime_millis=600000"),
        ),
        "pg_pool_idle_timeout_millis" => (
            "Maximum idle time in milliseconds, checked on acquisition and by an enabled background reaper. Zero disables idle expiry. ",
            Some("idle_timeout_millis=300000"),
        ),
        "pg_pool_enable_reaper_thread" => (
            "Run background expiry checks when lifetime or idle timeout is nonzero. ",
            Some("enable_reaper_thread=TRUE"),
        ),
        "pg_pool_health_check_query" => (
            "Query used to check a connection's health. An empty string disables the query. ",
            Some("health_check_query='SELECT 42'"),
        ),
        _ => unreachable!("known pool option"),
    };
    match example {
        Some(example) => format!(
            "{text}This option only applies to newly attached Postgres databases, to configure a database that is already attached use \"FROM postgres_configure_pool(catalog_name='my_attached_postgres_db', {example})\""
        ),
        None => text.to_owned(),
    }
}

pub(crate) fn options() -> Vec<ExtensionOption> {
    let defaults = Config::default();
    let mut options = Vec::new();
    for (name, ty, value, global) in [
        (
            "pg_connection_limit",
            LogicalType::UBIGINT,
            Value::ubigint(defaults.max_connections),
            false,
        ),
        (
            "pg_connection_cache",
            LogicalType::BOOLEAN,
            Value::boolean(true),
            false,
        ),
        (
            "pg_pool_acquire_mode",
            LogicalType::VARCHAR,
            Value::varchar("force"),
            true,
        ),
        (
            "pg_pool_max_connections",
            LogicalType::UBIGINT,
            Value::ubigint(defaults.max_connections),
            true,
        ),
        (
            "pg_pool_wait_timeout_millis",
            LogicalType::UBIGINT,
            Value::ubigint(defaults.wait_timeout_millis),
            true,
        ),
        (
            "pg_pool_enable_thread_local_cache",
            LogicalType::BOOLEAN,
            Value::boolean(defaults.thread_local_cache),
            true,
        ),
        (
            "pg_pool_max_lifetime_millis",
            LogicalType::UBIGINT,
            Value::ubigint(defaults.max_lifetime_millis),
            true,
        ),
        (
            "pg_pool_idle_timeout_millis",
            LogicalType::UBIGINT,
            Value::ubigint(defaults.idle_timeout_millis),
            true,
        ),
        (
            "pg_pool_enable_reaper_thread",
            LogicalType::BOOLEAN,
            Value::boolean(defaults.reaper),
            true,
        ),
        (
            "pg_pool_health_check_query",
            LogicalType::VARCHAR,
            Value::varchar(defaults.health_check_query),
            true,
        ),
    ] {
        options.push(ExtensionOption {
            name: name.into(),
            description: description(name),
            input_type: ty,
            default_value: value,
            set_function: None,
            global_by_default: global,
        });
    }
    options
}
/// Context-sensitive callbacks that `ExtensionOption`'s value-only callback cannot express.
pub(crate) fn validate_setting(
    client: &FunctionClient,
    name: &[u8],
    value: &Value,
    scope: SetScope,
) -> Result<()> {
    if name.eq_ignore_ascii_case(b"pg_connection_limit") || name.eq_ignore_ascii_case(b"pg_connection_cache")
    {
        if scope == SetScope::Local {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "{name} can only be set globally"
            )));
        }
    } else if name.eq_ignore_ascii_case(b"pg_pool_acquire_mode") {
        let mode = AcquireMode::parse(&value.to_varchar()?)?;
        if mode != AcquireMode::Force
            && setting(client, "pg_pool_max_connections").and_then(|v| v.as_u64()) == Some(0)
        {
            return Err(Error::invalid_input(format!(
                "pg_pool_pool_acquire_mode='{}' requires pg_pool_max_connections > 0 (pooling enabled)",
                mode.name()
            )));
        }
    }
    Ok(())
}
pub(crate) fn legacy_limit(name: &[u8], value: &Value) -> Option<Value> {
    if name.eq_ignore_ascii_case(b"pg_connection_limit") {
        Some(value.clone())
    } else if name.eq_ignore_ascii_case(b"pg_connection_cache") {
        Some(Value::ubigint(if value.as_bool() == Some(false) {
            0
        } else {
            Config::default().max_connections
        }))
    } else {
        None
    }
}

#[derive(Debug)]
struct Data {
    instance: Weak<crate::database::DatabaseInstance>,
    options: Vec<(ByteString, Value)>,
}
impl FunctionData for Data {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
const COLUMNS: &[(&str, bool)] = &[
    ("catalog_name", false),
    ("acquire_mode", false),
    ("available_connections", false),
    ("max_connections", false),
    ("wait_timeout_millis", false),
    ("cache_hits", false),
    ("cache_misses", false),
    ("try_failures", false),
    ("thread_local_cache_enabled", true),
    ("thread_local_cache_hits", false),
    ("thread_local_cache_misses", false),
    ("max_lifetime_millis", false),
    ("idle_timeout_millis", false),
    ("reaper_thread_running", true),
    ("reaper_thread_period_millis", false),
    ("health_check_query", false),
];
fn types() -> Vec<LogicalType> {
    COLUMNS
        .iter()
        .enumerate()
        .map(|(i, (_, boolean))| {
            if *boolean {
                LogicalType::BOOLEAN
            } else if matches!(i, 0 | 1 | 15) {
                LogicalType::VARCHAR
            } else {
                LogicalType::UBIGINT
            }
        })
        .collect()
}
pub(crate) fn function() -> TableFunctionSet {
    let mut function=TableFunction::new("postgres_configure_pool",vec![]).with_bind(|input| {
        let context=input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>()).ok_or_else(||Error::internal("Pool configuration requires client context"))?;
        let options=input.named_parameters.iter().filter(|(_,v)|!v.is_null()).map(|(n,v)|(n.to_ascii_lowercase(),v.clone())).collect::<Vec<_>>();
        for (name,value) in &options {
            if name=="acquire_mode" { AcquireMode::parse(&value.to_varchar()?).map_err(|e| Error::binder(e.raw_message().into_owned()))?; }
        }
        if !options.is_empty() && !options.iter().any(|(n,_)| n=="catalog_name") {
            return Err(Error::binder("'catalog_name' argument must be specified to change any option value on the connection pool of this catalog"));
        }
        Ok(TableFunctionBindOutput { names:COLUMNS.iter().map(|(n,_)|(*n).into()).collect(),return_types:types(),bind_data:Some(Arc::new(Data { instance:crate::database::instance_state(context)?.instance(),options })) })
    }).with_scan(|data,_,state,output| {
        if state.position>0 { output.set_len(0);return Ok(false); }
        let data=data.and_then(|d|d.as_any().downcast_ref::<Data>()).ok_or_else(||Error::internal("Missing pool configuration data"))?;
        let instance=data.instance.upgrade().ok_or_else(||Error::internal("Pool configuration after database shutdown"))?;
        let get=|name:&str| data.options.iter().find(|(n,_)|n.as_bytes()==name.as_bytes()).map(|(_,v)|v);
        let name=get("catalog_name").map(Value::to_varchar_bytes).transpose()?;
        let pools=instance.databases.all().into_iter().filter_map(|db| {
            if name.as_ref().is_some_and(|name|name.as_bytes()!=db.name().as_ref()) { return None; }
            match &db.storage { crate::attached::DatabaseStorage::Postgres(pg)=>Some((db.name().to_vec(),pg.pool.clone())),_=>None }
        }).collect::<Vec<_>>();
        if let Some(name)=&name && pools.is_empty() { return Err(Error::invalid_input(roastedduck_common::byte_format!("Catalog not found, name: '{name}'"))); }
        for (_,pool) in &pools {
            if let Some(v)=get("acquire_mode") { let mode=AcquireMode::parse(&v.to_varchar()?)?;pool.update(|c|c.acquire_mode=mode); }
            if let Some(v)=get("max_connections").and_then(Value::as_u64) { pool.set_max_connections(v); }
            if let Some(v)=get("wait_timeout_millis").and_then(Value::as_u64) { pool.update(|c|c.wait_timeout_millis=v); }
            if let Some(v)=get("enable_thread_local_cache").and_then(Value::as_bool) { pool.update(|c|c.thread_local_cache=v); }
            if let Some(v)=get("max_lifetime_millis").and_then(Value::as_u64) { pool.update(|c|c.max_lifetime_millis=v); }
            if let Some(v)=get("idle_timeout_millis").and_then(Value::as_u64) { pool.update(|c|c.idle_timeout_millis=v); }
            if let Some(v)=get("enable_reaper_thread").and_then(Value::as_bool) { pool.set_reaper(v)?; }
            if let Some(v)=get("health_check_query") { let query=v.to_varchar()?;pool.update(|c|c.health_check_query=query); }
        }
        *output=DataChunk::with_types(&types(),pools.len().max(1));
        for (row,(name,pool)) in pools.iter().enumerate() {
            let (config,metrics)=pool.snapshot();
            let values=vec![Value::try_varchar(name)?,Value::varchar(config.acquire_mode.name()),Value::ubigint(metrics.available_connections),Value::ubigint(config.max_connections),Value::ubigint(config.wait_timeout_millis),Value::ubigint(metrics.cache_hits),Value::ubigint(metrics.cache_misses),Value::ubigint(metrics.try_failures),Value::boolean(config.thread_local_cache),Value::ubigint(metrics.thread_local_cache_hits),Value::ubigint(metrics.thread_local_cache_misses),Value::ubigint(config.max_lifetime_millis),Value::ubigint(config.idle_timeout_millis),Value::boolean(metrics.reaper_running),Value::ubigint(config.reaper_period_millis()),Value::varchar(config.health_check_query)];
            for (column,value) in values.iter().enumerate() { output.column_mut(column).set_value(row,value)?; }
        }
        output.set_len(pools.len());state.position=1;Ok(false)
    });
    for name in ["catalog_name", "acquire_mode", "health_check_query"] {
        function
            .named_parameters
            .push((name.into(), LogicalType::VARCHAR));
    }
    for name in [
        "max_connections",
        "wait_timeout_millis",
        "max_lifetime_millis",
        "idle_timeout_millis",
    ] {
        function
            .named_parameters
            .push((name.into(), LogicalType::UBIGINT));
    }
    for name in ["enable_thread_local_cache", "enable_reaper_thread"] {
        function
            .named_parameters
            .push((name.into(), LogicalType::BOOLEAN));
    }
    let mut set = TableFunctionSet::new("postgres_configure_pool");
    set.functions.push(function);
    set
}
