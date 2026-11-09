//! Public pending progress follows the frozen C API materialized/streaming controls.
use roastedduck::{Connection, Database, Error, PendingExecutionResult, QueryResult, Result};

fn connection() -> Result<Connection> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("SET threads=1; SET enable_progress_bar=true; SET enable_progress_bar_print=false")?;
    Ok(connection)
}

fn scalar(connection: &mut Connection, result: QueryResult) -> Result<i64> {
    match result {
        QueryResult::Materialized(result) => result
            .value(0, 0)
            .as_i64()
            .ok_or_else(|| Error::internal("Expected a BIGINT scalar")),
        QueryResult::Stream(stream) => {
            let chunk = connection
                .fetch(&stream)?
                .ok_or_else(|| Error::internal("Missing scalar chunk"))?;
            let value = chunk
                .column(0)
                .get_value(0)
                .as_i64()
                .ok_or_else(|| Error::internal("Expected a streaming BIGINT"))?;
            assert!(connection.fetch(&stream)?.is_none());
            Ok(value)
        }
    }
}

#[test]
fn unsupported_source_does_not_publish_restarted_progress_as_known() -> Result<()> {
    for allow_stream in [false, true] {
        let mut connection = connection()?;
        let sql = "SELECT 42::BIGINT AS answer";
        let statement = connection.parse_statements(sql)?.remove(0);
        let pending = connection.pending_query(sql, statement, allow_stream)?;
        let client = connection.client_handle();
        assert_eq!(client.query_progress().percentage, 0.0);
        let mut ready = false;
        for _ in 0..100 {
            let state = connection.execute_task(&pending)?;
            let progress = client.query_progress();
            assert_eq!(progress.percentage, -1.0);
            assert_eq!(progress.rows_processed, 0);
            assert_eq!(progress.total_rows_to_process, 0);
            if matches!(
                state,
                PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished
            ) {
                ready = true;
                break;
            }
        }
        assert!(ready, "Bounded constant query must finish");
        let result = connection.execute_pending(&pending)?;
        assert_eq!(scalar(&mut connection, result)?, 42);
        assert_eq!(connection.query("SELECT 43")?.value(0, 0).as_i32(), Some(43));
    }
    Ok(())
}

#[test]
fn zero_percent_publishes_known_rows_and_finished_pipelines_have_zero_counters() -> Result<()> {
    for allow_stream in [false, true] {
        let mut connection = connection()?;
        let sql = "SELECT sum(i % 97)::BIGINT FROM range(1000000)t(i)";
        let statement = connection.parse_statements(sql)?.remove(0);
        let pending = connection.pending_query(sql, statement, allow_stream)?;
        let client = connection.client_handle();
        let mut known_zero = false;
        let mut ready = false;
        for _ in 0..1000 {
            let state = connection.execute_task(&pending)?;
            let progress = client.query_progress();
            known_zero |= progress.percentage == 0.0 && progress.total_rows_to_process > 0;
            if progress.total_rows_to_process > 0 {
                assert_eq!(progress.total_rows_to_process, 1_000_002);
            }
            if matches!(
                state,
                PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished
            ) {
                ready = true;
                assert_eq!(progress.rows_processed, 0);
                assert_eq!(progress.total_rows_to_process, 0);
                break;
            }
        }
        assert!(ready, "Bounded range query must finish");
        assert!(
            known_zero,
            "A known source publishes totals before percentage increases"
        );
        let result = connection.execute_pending(&pending)?;
        assert_eq!(scalar(&mut connection, result)?, 47_999_055);
    }
    Ok(())
}

#[test]
fn partial_stream_close_retains_measurement_until_prepare_resets_it() -> Result<()> {
    let mut connection = connection()?;
    let sql = "SELECT count(*)::BIGINT FROM range(0)";
    let statement = connection.parse_statements(sql)?.remove(0);
    let pending = connection.pending_query(sql, statement, true)?;
    let client = connection.client_handle();
    for _ in 0..100 {
        if matches!(
            connection.execute_task(&pending)?,
            PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished
        ) {
            break;
        }
    }
    let QueryResult::Stream(stream) = connection.execute_pending(&pending)? else {
        return Err(Error::internal("Expected streaming count result"));
    };
    assert!(connection.fetch(&stream)?.is_some());
    assert_eq!(client.query_progress().percentage, 0.0);
    connection.close_query(stream.id());
    assert_eq!(client.query_progress().percentage, 0.0);
    let _next = connection.prepare("SELECT 43")?;
    assert_eq!(client.query_progress().percentage, -1.0);
    assert_eq!(connection.query("SELECT 43")?.value(0, 0).as_i32(), Some(43));
    Ok(())
}
