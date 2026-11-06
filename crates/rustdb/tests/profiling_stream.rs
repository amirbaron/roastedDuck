//! Profiling must finish real streaming counters without collecting the output again.
use rustdb::{Connection, Database, Error, QueryResult, Result, StreamQueryResult};

fn stream(connection: &mut Connection, query: &str) -> Result<StreamQueryResult> {
    let statement = rustdb_parser::parse(query)?
        .pop()
        .ok_or_else(|| Error::internal("Missing SELECT"))?;
    let pending = connection.pending_query(query, statement, true)?;
    match connection.execute_pending(&pending)? {
        QueryResult::Stream(stream) => Ok(stream),
        QueryResult::Materialized(_) => Err(Error::internal("Expected a streaming result")),
    }
}

#[test]
fn exhausted_stream_flushes_rows_bytes_and_scan_metrics() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("SET threads=1")?;
    connection.enable_profiling();
    let result = stream(&mut connection, "SELECT i FROM range(10001) t(i)")?;
    let mut rows = 0;
    while let Some(chunk) = connection.fetch(&result)? {
        rows += chunk.len();
    }
    assert_eq!(rows, 10001);
    let profile = connection
        .profiling_tree()
        .ok_or_else(|| Error::internal("Missing profile"))?;
    let metric = |name: &str| {
        profile
            .metrics
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(metric("ROWS_RETURNED"), Some("10001"));
    assert_eq!(metric("RESULT_SET_SIZE"), Some("80008"));
    assert_eq!(metric("CUMULATIVE_ROWS_SCANNED"), Some("10001"));
    connection.disable_profiling();
    assert!(
        connection
            .profiling_information("json")?
            .as_ref()
            .windows(8)
            .any(|v| v == b"disabled")
    );
    Ok(())
}

#[test]
fn closing_unread_stream_allows_another_query() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("SET threads=1")?;
    connection.enable_profiling();
    let result = stream(&mut connection, "SELECT i FROM range(100000000) t(i)")?;
    let chunk = connection
        .fetch(&result)?
        .ok_or_else(|| Error::internal("Missing first chunk"))?;
    assert!(chunk.len() <= 2048);
    connection.close_query(result.id());
    connection.disable_profiling();
    assert_eq!(connection.query("SELECT 42")?.value(0, 0).as_i32(), Some(42));
    Ok(())
}
