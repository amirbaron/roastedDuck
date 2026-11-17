//! Worker threads can initialize a pending query's HNSW scan before the running
//! query installs its own context. The planning context must still be valid then.
use roastedduck::{Database, Error, QueryResult, Result, Value};

const QUERY: &str = "SELECT id FROM vectors ORDER BY array_distance(v,[1,0,0]::FLOAT[3]) LIMIT 1";

#[test]
fn pending_hnsw_scans_always_find_their_transaction_context() -> Result<()> {
    let mut failures = Vec::new();
    for _ in 0..400 {
        let database = Database::open_in_memory()?;
        let mut connection = database.connect()?;
        connection.query("SET threads=16")?;
        connection.query("LOAD vss")?;
        connection.query("CREATE TABLE vectors(id INTEGER, v FLOAT[3])")?;
        connection.query("INSERT INTO vectors VALUES (1,[1,0,0]),(2,[0,1,0])")?;
        connection.query("CREATE INDEX h ON vectors USING HNSW(v)")?;
        for _ in 0..20 {
            let statement = roastedduck_parser::parse(QUERY)?
                .pop()
                .ok_or_else(|| Error::internal("Missing SELECT"))?;
            let outcome =
                connection
                    .pending_query(QUERY, statement, true)
                    .and_then(|pending| match connection.execute_pending(&pending)? {
                        QueryResult::Stream(stream) => connection.materialize(&stream),
                        QueryResult::Materialized(result) => Ok(result),
                    });
            match outcome {
                Ok(result) => {
                    assert_eq!(result.row_count(), 1);
                    assert_eq!(result.value(0, 0), Value::integer(1));
                }
                Err(error) => failures.push(error.raw_message().into_owned()),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 8000 queries failed: {:?}",
        failures.len(),
        failures.first()
    );
    Ok(())
}
