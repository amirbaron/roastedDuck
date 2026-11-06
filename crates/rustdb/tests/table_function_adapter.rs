//! Replacing a client adapter must not change a running query's bound plan.
use rustdb::{Database, Error, QueryResult, Result};
use rustdb_function::{TableFunction, TableFunctionReplacement, TableFunctionSet};
use std::sync::Arc;

fn adapter(offset: i64) -> TableFunctionSet {
    let mut function = TableFunction::new("client_adapter_probe", Vec::new());
    function.bind_replace = Some(Arc::new(move |_| {
        Ok(Some(TableFunctionReplacement {
            query: format!("SELECT i + {offset} AS i FROM range(10001)t(i)"),
            error: "Expected adapter query".to_owned(),
        }))
    }));
    let mut set = TableFunctionSet::new("client_adapter_probe");
    set.add(function);
    set
}

#[test]
fn replacing_adapter_keeps_running_stream_and_changes_subsequent_bind() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut original = database.connect()?;
    original.create_table_function_set(adapter(0))?;
    let sql = "SELECT * FROM client_adapter_probe()";
    let statement = original.extract_statements(sql)?.remove(0);
    let pending = original.pending_query(sql, statement, true)?;
    let QueryResult::Stream(stream) = original.execute_pending(&pending)? else {
        return Err(Error::internal("Expected stream"));
    };
    let first = original.fetch(&stream)?.expect("first chunk");
    let mut count = first.len();
    let mut sum: i64 = (0..first.len())
        .map(|row| first.column(0).get_value(row).as_i64().expect("integer"))
        .sum();
    let mut replacement = database.connect()?;
    replacement.replace_table_function_set(adapter(10))?;
    while let Some(chunk) = original.fetch(&stream)? {
        count += chunk.len();
        sum += (0..chunk.len())
            .map(|row| chunk.column(0).get_value(row).as_i64().expect("integer"))
            .sum::<i64>();
    }
    assert_eq!((count, sum), (10001, 50_005_000));
    assert_eq!(
        replacement
            .query("SELECT min(i) FROM client_adapter_probe()")?
            .value(0, 0)
            .as_i64(),
        Some(10)
    );
    assert_eq!(
        original
            .query("SELECT min(i) FROM client_adapter_probe()")?
            .value(0, 0)
            .as_i64(),
        Some(10)
    );
    Ok(())
}
