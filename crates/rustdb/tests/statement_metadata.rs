//! Metadata/client statement operations must preserve native execution and transactions.
use rustdb::{Database, Error, ErrorKind, QueryResult, Result};
use rustdb_ast::StatementType;
use rustdb_common::ByteString;

#[test]
fn table_names_and_loading_preserve_active_stream() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let sql = "SELECT i FROM range(10001) t(i)";
    let statement = connection.extract_statements(sql)?.remove(0);
    let pending = connection.pending_query(sql, statement, true)?;
    let QueryResult::Stream(stream) = connection.execute_pending(&pending)? else {
        return Err(Error::internal("Expected stream"));
    };
    let first = connection
        .fetch(&stream)?
        .ok_or_else(|| Error::internal("Missing first batch"))?;
    let mut count = first.len();
    let mut sum: i64 = (0..first.len())
        .filter_map(|row| first.column(0).get_value(row).as_i64())
        .sum();
    assert_eq!(
        connection
            .table_names("SELECT * FROM absent_table", false)?
            .into_iter()
            .collect::<Vec<_>>(),
        vec![ByteString::from("absent_table")]
    );
    assert!(connection.table_names("PRAGMA version", false)?.is_empty());
    assert!(connection.table_names("SELECT 1;SELECT 2", false).is_err());
    connection.load_extension("core_functions")?;
    while let Some(chunk) = connection.fetch(&stream)? {
        count += chunk.len();
        sum += (0..chunk.len())
            .filter_map(|row| chunk.column(0).get_value(row).as_i64())
            .sum::<i64>();
    }
    assert_eq!((count, sum), (10001, 50_005_000));
    Ok(())
}
#[test]
fn table_names_preserve_explicit_transaction_and_rollback() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE t(i INTEGER)")?;
    connection.query("BEGIN")?;
    connection.query("INSERT INTO t VALUES(42)")?;
    assert_eq!(connection.table_names("SELECT * FROM t", false)?.len(), 1);
    assert!(connection.table_names("SELECT FROM", false).is_err());
    assert_eq!(
        connection.query("SELECT i FROM t")?.value(0, 0).as_i32(),
        Some(42)
    );
    connection.query("ROLLBACK")?;
    assert_eq!(connection.query("FROM t")?.row_count(), 0);
    Ok(())
}
#[test]
fn owned_pragma_statement_uses_native_preprocessor() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let parsed = connection.extract_statements("PRAGMA version")?;
    assert_eq!(parsed[0].statement_type(), StatementType::PragmaStatement);
    let processed = connection.preprocess_statements("PRAGMA version", parsed)?;
    assert_eq!(processed.len(), 1);
    assert_eq!(processed[0].statement_type(), StatementType::SelectStatement);
    let result = connection.execute_statements(processed)?;
    assert_eq!(result.value(0, 0).as_str(), Some("v1.5.6"));
    Ok(())
}
#[test]
fn direct_load_obeys_external_access_setting() -> Result<()> {
    let mut config = rustdb::Config::default();
    config.set_option_by_name("enable_external_access", rustdb::Value::boolean(false))?;
    let mut connection = Database::open(":memory:", &config)?.connect()?;
    connection.load_extension("core_functions")?;
    let error = connection
        .load_extension("/not-present/native-extension.duckdb_extension")
        .err()
        .ok_or_else(|| Error::internal("Loading a file must be denied"))?;
    assert_eq!(error.kind(), ErrorKind::Permission);
    Ok(())
}
