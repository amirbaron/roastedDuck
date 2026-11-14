//! Real SQL behavior of roastedDuck's compatible implementation of VSS.

use crate::{Connection, Database, Result, Value};

fn setup() -> Result<Connection> {
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("LOAD vss")?;
    connection.query("CREATE TABLE vectors(id INTEGER, vec FLOAT[2], label VARCHAR)")?;
    connection.query("INSERT INTO vectors VALUES (1,[1,0],'one'), (2,[0,1],'two'), (3,[-1,0],'three')")?;
    connection.query("CREATE INDEX vectors_hnsw ON vectors USING HNSW(vec)")?;
    Ok(connection)
}

#[test]
fn graph_scan_returns_projected_rows_and_real_index_metadata() -> Result<()> {
    let mut connection = setup()?;
    let sql = "SELECT label,id FROM vectors ORDER BY array_distance(vec,[1,0]::FLOAT[2]) LIMIT 2";
    let result = connection.query(sql)?;
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.value(0, 0), Value::varchar("one"));
    assert_eq!(result.value(1, 0), Value::integer(1));
    assert_eq!(result.value(1, 1), Value::integer(2));
    let plan = connection.query(format!("EXPLAIN {sql}"))?;
    assert!(plan.value(1, 0).to_varchar()?.contains("HNSW_INDEX_SCAN"));
    let info = connection.query(
        "SELECT index_name,metric,dimensions,count,capacity >= count,approx_memory_usage > 0 \
         FROM pragma_hnsw_index_info() WHERE index_name='vectors_hnsw'",
    )?;
    assert_eq!(info.row_count(), 1);
    assert_eq!(info.value(0, 0), Value::varchar("vectors_hnsw"));
    assert_eq!(info.value(1, 0), Value::varchar("l2sq"));
    assert_eq!(info.value(2, 0), Value::bigint(2));
    assert_eq!(info.value(3, 0), Value::bigint(3));
    assert_eq!(info.value(4, 0), Value::boolean(true));
    assert_eq!(info.value(5, 0), Value::boolean(true));
    connection.query("PRAGMA hnsw_compact_index('vectors_hnsw')")?;
    assert_eq!(connection.query(sql)?.value(1, 0), Value::integer(1));
    Ok(())
}

#[test]
fn prepared_scan_sees_local_inserts_updates_and_rollback() -> Result<()> {
    let mut connection = setup()?;
    connection.query(
        "PREPARE nearest AS SELECT id FROM vectors ORDER BY array_distance(vec,[2,0]::FLOAT[2]) LIMIT 1",
    )?;
    assert_eq!(
        connection.query("EXECUTE nearest")?.value(0, 0),
        Value::integer(1)
    );
    connection.query("BEGIN")?;
    connection.query("INSERT INTO vectors VALUES (4,[2,0],'local')")?;
    assert_eq!(
        connection.query("EXECUTE nearest")?.value(0, 0),
        Value::integer(4)
    );
    connection.query("UPDATE vectors SET vec=[3,0] WHERE id=4")?;
    connection.query("DELETE FROM vectors WHERE id=1")?;
    assert_eq!(
        connection.query("EXECUTE nearest")?.value(0, 0),
        Value::integer(4)
    );
    connection.query("ROLLBACK")?;
    assert_eq!(
        connection.query("EXECUTE nearest")?.value(0, 0),
        Value::integer(1)
    );
    Ok(())
}

#[test]
fn min_by_uses_candidates_and_retains_k_during_local_fallback() -> Result<()> {
    let mut connection = setup()?;
    let sql = "SELECT min_by(id,array_distance(vec,[2,0]::FLOAT[2]),2) FROM vectors";
    assert_eq!(connection.query(sql)?.value(0, 0).to_varchar()?, "[1, 2]");
    assert!(
        connection
            .query(format!("EXPLAIN {sql}"))?
            .value(1, 0)
            .to_varchar()?
            .contains("HNSW_INDEX_SCAN")
    );
    connection.query("BEGIN")?;
    connection.query("INSERT INTO vectors VALUES (4,[2,0],'local')")?;
    assert_eq!(connection.query(sql)?.value(0, 0).to_varchar()?, "[4, 1]");
    connection.query("ROLLBACK")?;
    Ok(())
}

#[test]
fn indexed_offset_matches_the_pinned_extension_quirk() -> Result<()> {
    let mut connection = setup()?;
    let sql = "SELECT id FROM vectors ORDER BY array_distance(vec,[1,0]::FLOAT[2]) LIMIT 1 OFFSET 1";
    // Pinned b833341 removes TopN and passes only its limit to HNSW, ignoring
    // OFFSET. Oracle evidence: vss-oracle-probes-2/verification.json, offset.
    assert_eq!(connection.query(sql)?.value(0, 0), Value::integer(1));
    connection.query("SET disabled_optimizers='extension'")?;
    assert_eq!(connection.query(sql)?.value(0, 0), Value::integer(2));
    Ok(())
}

#[test]
fn null_query_element_preserves_the_pinned_internal_error() -> Result<()> {
    let mut connection = setup()?;
    let result =
        connection.query("SELECT id FROM vectors ORDER BY array_distance(vec,[1,NULL]::FLOAT[2]) LIMIT 1");
    let Err(error) = result else {
        return Err(crate::Error::internal(
            "NULL query element unexpectedly succeeded",
        ));
    };
    assert_eq!(error.kind(), crate::ErrorKind::Internal);
    assert_eq!(
        error.raw_message(),
        "Calling GetValueInternal on a value that is NULL"
    );
    Ok(())
}

#[test]
fn fetched_null_element_still_evaluates_the_sql_distance() -> Result<()> {
    let mut connection = setup()?;
    connection.query("INSERT INTO vectors VALUES (4,[1,NULL],'null element')")?;
    let result =
        connection.query("SELECT id FROM vectors ORDER BY array_distance(vec,[1,0]::FLOAT[2]) LIMIT 4");
    let Err(error) = result else {
        return Err(crate::Error::internal(
            "stored NULL vector element unexpectedly succeeded",
        ));
    };
    assert_eq!(error.kind(), crate::ErrorKind::InvalidInput);
    assert_eq!(
        error.raw_message(),
        "array_distance: left argument can not contain NULL values"
    );
    Ok(())
}

#[test]
fn selective_predicates_expand_duplicate_candidates_without_underfilling() -> Result<()> {
    let mut connection = setup()?;
    connection.query("CREATE TABLE duplicates(id INTEGER, vec FLOAT[2])")?;
    connection.query("INSERT INTO duplicates SELECT i,[0,0]::FLOAT[2] FROM range(1,129) r(i)")?;
    connection.query("CREATE INDEX duplicates_hnsw ON duplicates USING HNSW(vec)")?;
    for threshold in [112, 126] {
        let sql = format!(
            "SELECT array_distance(vec,[0,0]::FLOAT[2]) AS distance,id>{threshold} AS eligible \
             FROM duplicates WHERE id>{threshold} ORDER BY distance LIMIT 5"
        );
        let actual = connection.query(&sql)?;
        connection.query("SET disabled_optimizers='extension'")?;
        let exact = connection.query(&sql)?;
        connection.query("SET disabled_optimizers=''")?;
        assert_eq!(actual.row_count(), exact.row_count());
        assert_eq!(actual.row_count(), if threshold == 112 { 5 } else { 2 });
        // All nearest vectors tie. Check count, SQL predicate, and exact distance,
        // without constraining which equivalent row IDs the Rust graph returns.
        for row in 0..actual.row_count() {
            assert_eq!(actual.value(0, row), exact.value(0, row));
            assert_eq!(actual.value(1, row), Value::boolean(true));
        }
        let profile = connection
            .query(format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"))?
            .value(1, 0)
            .to_varchar()?;
        assert!(
            profile.contains("HNSW Index") && profile.contains("duplicates_hnsw"),
            "{profile}"
        );
        assert!(profile.contains("HNSW graph traversal"));
        assert!(!profile.contains("Exact scan"));
    }
    Ok(())
}

#[test]
fn lateral_join_traverses_index_and_reports_local_fallback() -> Result<()> {
    let mut connection = setup()?;
    // DuckDB 1.5.6 otherwise turns this window into arg_min before the VSS
    // join rule runs. These settings/cardinality exercise the pinned rule on
    // both engines (join-eligible-probes/limit1-verification.json).
    connection.query("INSERT INTO vectors SELECT i,[i+10,20]::FLOAT[2],'extra' FROM range(4,33)r(i)")?;
    connection.query("SET disabled_optimizers='top_n_window_elimination'")?;
    connection.query(
        "CREATE TABLE queries(qid INTEGER, q FLOAT[2]); INSERT INTO queries VALUES (10,[1,0]),(20,[0,1])",
    )?;
    let sql = "SELECT q.qid, v.id FROM queries q, LATERAL (SELECT id FROM vectors v ORDER BY array_distance(v.vec,q.q) LIMIT 1) v ORDER BY q.qid";
    let result = connection.query(sql)?;
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.value(1, 0), Value::integer(1));
    assert_eq!(result.value(1, 1), Value::integer(2));
    assert!(
        connection
            .query(format!("EXPLAIN {sql}"))?
            .value(1, 0)
            .to_varchar()?
            .contains("HNSW_INDEX_JOIN")
    );
    assert!(
        connection
            .query(format!("EXPLAIN ANALYZE {sql}"))?
            .value(1, 0)
            .to_varchar()?
            .contains("HNSW graph traversal")
    );
    connection.query("BEGIN")?;
    connection.query("DELETE FROM vectors WHERE id=1; INSERT INTO vectors VALUES (4,[1,0],'local')")?;
    let result = connection.query(sql)?;
    assert_eq!(result.value(1, 0), Value::integer(4));
    assert!(
        connection
            .query(format!("EXPLAIN ANALYZE {sql}"))?
            .value(1, 0)
            .to_varchar()?
            .contains("Exact scan")
    );
    connection.query("ROLLBACK")?;
    assert_eq!(connection.query(sql)?.value(1, 0), Value::integer(1));
    Ok(())
}
