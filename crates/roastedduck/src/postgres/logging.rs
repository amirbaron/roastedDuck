use roastedduck_common::Result;
use roastedduck_function::{
    FunctionClient,
    logging::{LogLevel, LogScope, LogTypeInfo},
};
use roastedduck_types::{LogicalType, Value};
use std::sync::Arc;
use std::time::Instant;

pub(crate) fn log_type() -> LogTypeInfo {
    LogTypeInfo {
        name: "PostgresQueryLog".into(),
        level: LogLevel::Debug,
        structured_type: Some(LogicalType::struct_type(vec![
            ("query".into(), LogicalType::VARCHAR),
            ("duration_ms".into(), LogicalType::BIGINT),
        ])),
    }
}
pub(super) fn query<T>(
    client: &Arc<FunctionClient>,
    sql: &str,
    call: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let start = Instant::now();
    let result = call();
    if let Some(logger) = client.logger()
        && logger.should_log(LogScope::Connection, "PostgresQueryLog", LogLevel::Debug)
    {
        let ty = LogicalType::struct_type(vec![
            ("query".into(), LogicalType::VARCHAR),
            ("duration_ms".into(), LogicalType::BIGINT),
        ]);
        let value = Value::from_children(
            ty,
            vec![
                Value::varchar(sql),
                Value::bigint(start.elapsed().as_millis().min(i64::MAX as u128) as i64),
            ],
        );
        logger.write_log_bytes(
            LogScope::Connection,
            "PostgresQueryLog",
            LogLevel::Debug,
            &value.to_varchar_bytes()?,
        )?;
    }
    result
}
