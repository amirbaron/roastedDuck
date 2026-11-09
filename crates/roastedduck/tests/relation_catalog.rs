//! Relational catalog lookup must not execute a query or finish the caller's result.
use roastedduck::{Database, Error, QueryResult, Result};

#[test]
fn table_lookup_distinguishes_views_and_preserves_a_stream() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(b"CREATE TABLE \"table_\xFF\" AS SELECT 42 i")?;
    connection.query(b"CREATE VIEW \"view_\xFF\" AS SELECT * FROM \"table_\xFF\"")?;
    let sql = "SELECT i FROM range(10001)t(i)";
    let statement = connection.extract_statements(sql)?.remove(0);
    let pending = connection.pending_query(sql, statement, true)?;
    let QueryResult::Stream(stream) = connection.execute_pending(&pending)? else {
        return Err(Error::internal("Expected streaming result"));
    };
    let mut count = connection.fetch(&stream)?.expect("first chunk").len();
    assert!(connection.relation_is_base_table(b"", b"main", b"table_\xFF")?);
    assert!(!connection.relation_is_base_table(b"", b"main", b"view_\xFF")?);
    assert!(!connection.relation_is_base_table(b"", b"main", b"absent")?);
    while let Some(chunk) = connection.fetch(&stream)? {
        count += chunk.len();
    }
    assert_eq!(count, 10001);
    Ok(())
}

#[test]
fn table_lookup_preserves_explicit_transaction_rollback() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("BEGIN")?;
    connection.query("CREATE TABLE in_transaction AS SELECT 42 i")?;
    assert!(connection.relation_is_base_table(b"", b"main", b"in_transaction")?);
    connection.query("ROLLBACK")?;
    assert!(!connection.relation_is_base_table(b"", b"main", b"in_transaction")?);
    connection.query("SELECT 42")?;
    Ok(())
}
