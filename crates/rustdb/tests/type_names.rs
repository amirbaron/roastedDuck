//! Type-name lookup must preserve a live result and the caller's transaction.
use rustdb::{Database, Error, ErrorKind, LogicalTypeId, QueryResult, Result};

#[test]
fn byte_type_names_resolve_catalog_entries_without_unicode_roundtrip() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query(b"CREATE TYPE \"kind_\xFF\" AS ENUM ('yes','no')")?;
    let kind = connection.type_from_name_bytes(b"\"kind_\xFF\"[]")?;
    assert_eq!(kind.id(), LogicalTypeId::List);
    let child = kind.child_type().expect("list child");
    assert_eq!(child.id(), LogicalTypeId::Enum);
    assert_eq!(child.enum_values().expect("enum values")[0].as_bytes(), b"yes");
    Ok(())
}

#[test]
fn callback_type_context_survives_streaming_and_is_released_at_end() -> Result<()> {
    use rustdb::{Connection, LogicalType};
    use rustdb_function::{ScalarFunction, ScalarFunctionSet, Stability};
    use std::sync::{Arc, Mutex, Weak};
    type ContextWeak = Weak<dyn std::any::Any + Send + Sync>;
    let observed: Arc<Mutex<Option<ContextWeak>>> = Arc::default();
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TYPE callback_kind AS ENUM ('first','second')")?;
    let id = connection.connection_id();
    let observer = Arc::clone(&observed);
    let mut function = ScalarFunction::new(
        "callback_type_lookup",
        vec![LogicalType::BIGINT],
        LogicalType::BIGINT,
        move |input, _, output| {
            let kind = Connection::type_from_current_query(id, b"callback_kind")
                .ok_or_else(|| Error::internal("Missing current callback connection"))??;
            assert_eq!(kind.id(), LogicalTypeId::Enum);
            let client = rustdb_function::file_system::opener().expect("callback client");
            let context = client.transaction_context().expect("callback context");
            *observer.lock().expect("observer") = Some(Arc::downgrade(&context));
            *output = input.column(0).clone();
            Ok(())
        },
    );
    function.stability = Stability::Volatile;
    let mut functions = ScalarFunctionSet::new("callback_type_lookup");
    functions.add(function);
    connection.create_scalar_function_set(functions)?;
    let sql = "SELECT callback_type_lookup(i) FROM range(10001)t(i)";
    let statement = connection.extract_statements(sql)?.remove(0);
    let pending = connection.pending_query(sql, statement, true)?;
    let QueryResult::Stream(stream) = connection.execute_pending(&pending)? else {
        return Err(Error::internal("Expected stream"));
    };
    let first = connection.fetch(&stream)?.expect("first chunk");
    let mut count = first.len();
    assert!(
        observed
            .lock()
            .expect("observer")
            .as_ref()
            .expect("observed context")
            .upgrade()
            .is_some()
    );
    connection.type_from_name("INTEGER")?;
    while let Some(chunk) = connection.fetch(&stream)? {
        count += chunk.len();
    }
    assert_eq!(count, 10001);
    assert!(
        observed
            .lock()
            .expect("observer")
            .as_ref()
            .expect("observed context")
            .upgrade()
            .is_none()
    );
    Ok(())
}

#[test]
fn type_lookup_preserves_stream_after_success_and_failure() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    let sql = "SELECT i FROM range(10001) t(i)";
    let statement = rustdb_parser::parse(sql)?
        .pop()
        .ok_or_else(|| Error::internal("Missing SELECT"))?;
    let pending = connection.pending_query(sql, statement, true)?;
    let QueryResult::Stream(stream) = connection.execute_pending(&pending)? else {
        return Err(Error::internal("Expected a streaming result"));
    };
    let first = connection
        .fetch(&stream)?
        .ok_or_else(|| Error::internal("Missing first chunk"))?;
    let mut count = first.len();
    let mut sum: i64 = (0..first.len())
        .filter_map(|row| first.column(0).get_value(row).as_i64())
        .sum();
    assert_eq!(
        connection.type_from_name("MAP(VARCHAR, INTEGER[])")?.id(),
        LogicalTypeId::Map
    );
    let error = connection
        .type_from_name("missing_test_type")
        .err()
        .ok_or_else(|| Error::internal("Expected a missing type error"))?;
    assert_eq!(error.kind(), ErrorKind::Catalog);
    assert!(!error.to_string().contains("LINE 1:"));
    while let Some(chunk) = connection.fetch(&stream)? {
        count += chunk.len();
        sum += (0..chunk.len())
            .filter_map(|row| chunk.column(0).get_value(row).as_i64())
            .sum::<i64>();
    }
    assert_eq!(count, 10001);
    assert_eq!(sum, 50_005_000);
    Ok(())
}

#[test]
fn type_lookup_sees_uncommitted_types_and_preserves_rollback() -> Result<()> {
    let mut connection = Database::open_in_memory()?.connect()?;
    connection.query("CREATE TABLE t(i INTEGER)")?;
    connection.query("BEGIN")?;
    connection.query("INSERT INTO t VALUES (42)")?;
    connection.query("CREATE TYPE local_mood AS ENUM ('calm', 'busy')")?;
    let kind = connection.type_from_name("local_mood[]")?;
    assert_eq!(kind.id(), LogicalTypeId::List);
    assert_eq!(
        kind.child_type().map(rustdb::LogicalType::id),
        Some(LogicalTypeId::Enum)
    );
    assert!(connection.type_from_name("missing_test_type").is_err());
    assert_eq!(
        connection.query("SELECT i FROM t")?.value(0, 0).as_i32(),
        Some(42)
    );
    connection.query("ROLLBACK")?;
    assert_eq!(connection.query("SELECT * FROM t")?.row_count(), 0);
    assert!(connection.type_from_name("local_mood").is_err());
    Ok(())
}
