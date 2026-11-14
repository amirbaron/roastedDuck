//! Nested materialized CTEs must retain their scope while a dependent join is
//! pushed below an independent definition (DuckDB 1.5.6 FTS deliminator regression).

use roastedduck::{Database, Result, Value};

#[test]
fn lifting_independent_ctes_preserves_later_cte_references() -> Result<()> {
    let database = Database::open_in_memory()?;
    let mut connection = database.connect()?;
    connection.query("CREATE TABLE documents(id VARCHAR)")?;
    connection.query("INSERT INTO documents VALUES ('a'), ('a'), ('b'), ('missing'), (NULL)")?;
    connection.query("CREATE TABLE indexed_docs(name VARCHAR, term VARCHAR, score INTEGER)")?;
    connection.query(
        "INSERT INTO indexed_docs VALUES ('a','duck',2), ('a','duck',3), \
         ('b','duck',7), ('b','skip',100), (NULL,'duck',10)",
    )?;
    let query = "SELECT id, (\
        WITH tokens AS MATERIALIZED (SELECT 'duck' AS term), \
        qterms AS MATERIALIZED (SELECT term FROM tokens WHERE term <> 'skip'), \
        scores AS (SELECT name, sum(score) AS score \
                   FROM indexed_docs JOIN qterms USING(term) GROUP BY name) \
        SELECT score FROM scores WHERE name = documents.id\
        ) AS score FROM documents ORDER BY id NULLS LAST";

    // Verify the actual result as well as the transformation: duplicate outer
    // rows are retained, grouped scores are correct, and unmatched/NULL keys
    // remain NULL under the scalar subquery's semantics.
    let result = connection.query(query)?;
    assert_eq!(result.row_count(), 5);
    for (row, (id, score)) in [
        (Some("a"), Some("5")),
        (Some("a"), Some("5")),
        (Some("b"), Some("7")),
        (Some("missing"), None),
        (None, None),
    ]
    .into_iter()
    .enumerate()
    {
        let actual_id = result.value(0, row);
        let actual_score = result.value(1, row);
        assert_eq!(actual_id.is_null(), id.is_none());
        assert_eq!(actual_score.is_null(), score.is_none());
        if let Some(id) = id {
            assert_eq!(actual_id, Value::varchar(id));
        }
        if let Some(score) = score {
            assert_eq!(actual_score.to_varchar()?, score);
        }
    }
    connection.query("PRAGMA explain_output = OPTIMIZED_ONLY")?;
    let plan = connection.query(format!("EXPLAIN {query}"))?;
    let text = plan.value(1, 0).to_varchar()?;
    assert!(!text.contains("DELIM_JOIN"), "{text}");
    assert!(!text.contains("DELIM_GET"), "{text}");

    // Retaining definitions must not erase genuine correlation in a later CTE.
    // Its filter still depends on each outer key, including NULL and duplicates.
    let correlated = connection.query(
        "SELECT id, (WITH tokens AS MATERIALIZED (SELECT 'duck' AS term), \
         qterms AS MATERIALIZED (SELECT term FROM tokens WHERE documents.id = 'a') \
         SELECT count(*) FROM qterms) AS matches FROM documents ORDER BY id NULLS LAST",
    )?;
    assert_eq!(correlated.row_count(), 5);
    for (row, count) in [1, 1, 0, 0, 0].into_iter().enumerate() {
        assert_eq!(correlated.value(1, row), Value::bigint(count));
    }
    Ok(())
}
