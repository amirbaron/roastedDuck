//! End-to-end statements through `Connection::query`: parse, bind, plan, execute.
#![allow(clippy::unwrap_used)]

use rustdb::{Connection, Database, Value};

fn connect() -> Connection {
    Database::open_in_memory().unwrap().connect().unwrap()
}

/// The result rows rendered as strings (NULL as "NULL").
fn rows(connection: &mut Connection, sql: &str) -> Vec<Vec<String>> {
    let result = connection.query(sql).unwrap();
    (0..result.row_count())
        .map(|row| {
            (0..result.column_count())
                .map(|column| {
                    let value = result.value(column, row);
                    if value.is_null() {
                        "NULL".to_owned()
                    } else {
                        value.to_varchar().unwrap()
                    }
                })
                .collect()
        })
        .collect()
}

fn error(connection: &mut Connection, sql: &str) -> String {
    connection.query(sql).unwrap_err().to_string()
}

#[test]
fn constants_values_filters_and_limits() {
    let mut con = connect();
    assert_eq!(rows(&mut con, "SELECT 1 + 2 AS x, 'a'"), vec![vec!["3", "a"]]);
    assert_eq!(
        rows(
            &mut con,
            "SELECT b, a FROM (VALUES (1, 'x'), (2, 'y'), (3, 'z')) t(a, b) WHERE a >= 2 LIMIT 1"
        ),
        vec![vec!["y", "2"]]
    );
    assert_eq!(
        rows(&mut con, "SELECT range * 10 FROM range(2, 5) WHERE range <> 3"),
        vec![vec!["20"], vec!["40"]]
    );
    let result = con.query("SELECT 42 AS answer").unwrap();
    assert_eq!(result.names(), ["answer"]);
    assert_eq!(result.value(0, 0), Value::integer(42));
}

#[test]
fn binder_errors_carry_query_context() {
    let mut con = connect();
    assert_eq!(
        error(&mut con, "SELECT x"),
        "Binder Error: Referenced column \"x\" was not found because the FROM clause is missing\n\nLINE 1: SELECT \
         x\n               ^"
    );
    assert_eq!(
        error(&mut con, "SELECT * FROM range('a')"),
        "Binder Error: No function matches the given name and argument types 'range(VARCHAR)'. You might need to \
         add explicit type casts.\n\tCandidate functions:\n\trange(BIGINT)\n\trange(BIGINT, BIGINT)\n\trange(BIGINT, \
         BIGINT, BIGINT)\n\trange(TIMESTAMP, TIMESTAMP, INTERVAL)\n\trange(TIMESTAMP WITH TIME ZONE, TIMESTAMP WITH \
         TIME ZONE, INTERVAL)\n\n\nLINE 1: SELECT * FROM range('a')\n                      ^"
    );
    assert_eq!(
        error(&mut con, "SELECT 'abc'::INT"),
        "Conversion Error: Could not convert string 'abc' to INT32\n\nLINE 1: SELECT 'abc'::INT\n                    ^"
    );
}

#[test]
fn prepared_statements() {
    let mut con = connect();
    assert_eq!(
        error(&mut con, "SELECT $1"),
        "Invalid Input Error: Expected 1 parameters, but none were supplied"
    );
    con.query("PREPARE p AS SELECT $1 + 1").unwrap();
    assert_eq!(rows(&mut con, "EXECUTE P(41)"), vec![vec!["42"]]);
    assert_eq!(
        error(&mut con, "EXECUTE p(1, 2)"),
        "Invalid Input Error: Parameter argument/count mismatch, identifiers of the excess parameters: 2"
    );
    con.query("PREPARE q AS SELECT $2::INT").unwrap();
    assert_eq!(
        error(&mut con, "EXECUTE q(0)"),
        "Invalid Input Error: Values were not provided for the following prepared statement parameters: 2"
    );
    con.query("DEALLOCATE p").unwrap();
    assert_eq!(
        error(&mut con, "EXECUTE p(1)"),
        "Binder Error: Prepared statement \"p\" does not exist"
    );
}

#[test]
fn transactions_and_settings() {
    let mut con = connect();
    assert_eq!(
        error(&mut con, "COMMIT"),
        "TransactionContext Error: cannot commit - no transaction is active"
    );
    con.query("BEGIN").unwrap();
    assert_eq!(
        error(&mut con, "BEGIN"),
        "TransactionContext Error: cannot start a transaction within a transaction"
    );
    // Like any transaction error, that aborted the transaction.
    assert_eq!(
        error(&mut con, "SELECT 1"),
        "TransactionContext Error: Current transaction is aborted (please ROLLBACK)"
    );
    con.query("ROLLBACK").unwrap();
    con.query("BEGIN").unwrap();
    // A conversion error aborts the transaction; binder errors do not.
    assert!(con.query("SELECT x").is_err());
    assert_eq!(rows(&mut con, "SELECT 1"), vec![vec!["1"]]);
    assert!(con.query("SELECT 'a'::INT").is_err());
    assert_eq!(
        error(&mut con, "SELECT 1"),
        "TransactionContext Error: Current transaction is aborted (please ROLLBACK)"
    );
    con.query("ROLLBACK").unwrap();
    assert_eq!(rows(&mut con, "SELECT 2"), vec![vec!["2"]]);
    assert_eq!(
        error(&mut con, "SET foo_bar = 1"),
        "Catalog Error: unrecognized configuration parameter \"foo_bar\"\n\nDid you mean: \"force_bitpacking_mode\""
    );
    con.query("SET default_order = 'desc'").unwrap();
    let nested = "SELECT (1+(1+(1+(1+(1+(1+(1+1)))))))";
    assert_eq!(rows(&mut con, nested), vec![vec!["8"]]);
    con.query("SET max_expression_depth TO 7").unwrap();
    assert_eq!(
        error(&mut con, nested),
        "Parser Error: Max expression depth limit of 7 exceeded. Use \"SET max_expression_depth TO x\" to increase \
         the maximum expression depth."
    );
}

#[test]
fn describe_and_subqueries() {
    let mut con = connect();
    assert_eq!(
        rows(&mut con, "DESCRIBE SELECT 1 AS a, 'x' AS b"),
        vec![
            vec!["a", "INTEGER", "YES", "NULL", "NULL", "NULL"],
            vec!["b", "VARCHAR", "YES", "NULL", "NULL", "NULL"]
        ]
    );
}

#[test]
fn materialized_and_recursive_ctes() {
    let mut con = connect();
    assert_eq!(
        rows(
            &mut con,
            "WITH t AS (SELECT 1 x), u AS (SELECT x + 1 y FROM t) SELECT * FROM u, t"
        ),
        vec![vec!["2", "1"]]
    );
    // An unreferenced CTE is still materialised.
    assert_eq!(
        rows(&mut con, "WITH t AS MATERIALIZED (SELECT 1 x) SELECT 2"),
        vec![vec!["2"]]
    );
    assert_eq!(
        rows(
            &mut con,
            "WITH t AS MATERIALIZED (SELECT range x FROM range(2)) SELECT a.x, b.x FROM t a, t b ORDER BY 1, 2"
        ),
        vec![vec!["0", "0"], vec!["0", "1"], vec!["1", "0"], vec!["1", "1"]]
    );
    assert_eq!(
        rows(
            &mut con,
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 4) SELECT * FROM r"
        ),
        vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"]]
    );
    // UNION stops once an iteration produces only rows seen before.
    assert_eq!(
        rows(
            &mut con,
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION SELECT 3 - n FROM r) SELECT * FROM r ORDER BY n"
        ),
        vec![vec!["1"], vec!["2"]]
    );
}

/// Asserts that `sql` returns `expected` (expected values produced by DuckDB 1.5.6).
fn assert_rows(connection: &mut Connection, sql: &str, expected: &[&[&str]]) {
    let expected: Vec<Vec<String>> = expected
        .iter()
        .map(|row| row.iter().map(|&value| value.to_owned()).collect())
        .collect();
    assert_eq!(rows(connection, sql), expected, "{sql}");
}

#[test]
fn window_ranking_functions() {
    let mut con = connect();
    assert_rows(
        &mut con,
        "SELECT p, i, row_number() OVER w, rank() OVER w, dense_rank() OVER w, percent_rank() OVER w, \
         cume_dist() OVER w, ntile(2) OVER w FROM (VALUES (1, 10), (1, 20), (1, 20), (2, 5), (1, 30), (2, 5)) \
         t(p, i) WINDOW w AS (PARTITION BY p ORDER BY i) ORDER BY p, i",
        &[
            &["1", "10", "1", "1", "1", "0.0", "0.25", "1"],
            &["1", "20", "2", "2", "2", "0.3333333333333333", "0.75", "1"],
            &["1", "20", "3", "2", "2", "0.3333333333333333", "0.75", "2"],
            &["1", "30", "4", "4", "3", "1.0", "1.0", "2"],
            &["2", "5", "1", "1", "1", "0.0", "1.0", "1"],
            &["2", "5", "2", "1", "1", "0.0", "1.0", "2"],
        ],
    );
    // Without an outer ORDER BY, hash groups come out largest first, then by
    // descending radix bin (DuckDB's single-threaded order).
    assert_rows(
        &mut con,
        "SELECT p, row_number() OVER (PARTITION BY p) FROM range(7) a(p)",
        &[
            &["3", "1"],
            &["6", "1"],
            &["4", "1"],
            &["1", "1"],
            &["2", "1"],
            &["5", "1"],
            &["0", "1"],
        ],
    );
    assert_eq!(
        error(&mut con, "SELECT ntile(0) OVER (ORDER BY i) FROM range(3) t(i)"),
        "Invalid Input Error: Argument for ntile must be greater than zero"
    );
}

#[test]
fn window_value_functions_and_frames() {
    let mut con = connect();
    assert_rows(
        &mut con,
        "SELECT i, lag(i) OVER (ORDER BY i), lead(i, 2, -1) OVER (ORDER BY i), first_value(i) OVER w, \
         last_value(i) OVER w, nth_value(i, 2) OVER w FROM range(6) t(i) WINDOW w AS (ORDER BY i ROWS BETWEEN 1 \
         PRECEDING AND 2 FOLLOWING EXCLUDE CURRENT ROW)",
        &[
            &["0", "NULL", "2", "1", "2", "2"],
            &["1", "0", "3", "0", "3", "2"],
            &["2", "1", "4", "1", "4", "3"],
            &["3", "2", "5", "2", "5", "4"],
            &["4", "3", "-1", "3", "5", "5"],
            &["5", "4", "-1", "4", "4", "NULL"],
        ],
    );
    assert_rows(
        &mut con,
        "SELECT i, j, first_value(i * 10 + j) OVER w, last_value(i * 10 + j) OVER w FROM (VALUES (0, 0), (0, 1), \
         (1, 0), (1, 1), (2, 0), (3, 0)) t(i, j) WINDOW w AS (ORDER BY i GROUPS BETWEEN 1 PRECEDING AND CURRENT \
         ROW EXCLUDE GROUP) ORDER BY i, j",
        &[
            &["0", "0", "NULL", "NULL"],
            &["0", "1", "NULL", "NULL"],
            &["1", "0", "0", "1"],
            &["1", "1", "0", "1"],
            &["2", "0", "10", "11"],
            &["3", "0", "20", "20"],
        ],
    );
    assert_rows(
        &mut con,
        "SELECT x, last_value(x) OVER (ORDER BY x RANGE BETWEEN 1.5 PRECEDING AND 0.5 FOLLOWING), first_value(x) \
         OVER (ORDER BY x DESC RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM (VALUES (1.0), (2.0), (2.5), (4.0), \
         (NULL)) t(x) ORDER BY x",
        &[
            &["1.0", "1.0", "2.0"],
            &["2.0", "2.5", "2.5"],
            &["2.5", "2.5", "2.5"],
            &["4.0", "4.0", "4.0"],
            &["NULL", "NULL", "NULL"],
        ],
    );
    assert_rows(
        &mut con,
        "SELECT x, first_value(x IGNORE NULLS) OVER w, last_value(x IGNORE NULLS) OVER w, lag(x IGNORE NULLS) \
         OVER (ORDER BY i), lead(x IGNORE NULLS) OVER (ORDER BY i) FROM (VALUES (1, NULL), (2, 5), (3, NULL), (4, \
         7), (5, NULL)) t(i, x) WINDOW w AS (ORDER BY i ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) ORDER BY i",
        &[
            &["NULL", "5", "5", "NULL", "5"],
            &["5", "5", "5", "NULL", "7"],
            &["NULL", "5", "7", "5", "7"],
            &["7", "7", "7", "5", "NULL"],
            &["NULL", "7", "7", "7", "NULL"],
        ],
    );
    assert_rows(
        &mut con,
        "SELECT i, x, fill(x) OVER (ORDER BY i) FROM (VALUES (1, NULL), (2, 10), (3, NULL), (4, NULL), (5, 40), \
         (6, NULL)) t(i, x) ORDER BY i",
        &[
            &["1", "NULL", "0"],
            &["2", "10", "10"],
            &["3", "NULL", "20"],
            &["4", "NULL", "30"],
            &["5", "40", "40"],
            &["6", "NULL", "50"],
        ],
    );
    assert_eq!(
        error(
            &mut con,
            "SELECT first_value(i) OVER (ORDER BY i RANGE BETWEEN -1 PRECEDING AND CURRENT ROW) FROM range(3) t(i)"
        ),
        "Out of Range Error: Invalid RANGE PRECEDING value"
    );
    assert_eq!(
        error(
            &mut con,
            "SELECT first_value(i) OVER (ORDER BY i GROUPS BETWEEN CURRENT ROW AND -2 FOLLOWING) FROM range(3) t(i)"
        ),
        "Out of Range Error: Invalid GROUPS FOLLOWING value"
    );
    // Rows shifted in a run are not checked for overflow one by one.
    assert_rows(
        &mut con,
        "SELECT i, lead(i, 9223372036854775807) OVER (ORDER BY i) FROM range(3) t(i)",
        &[&["0", "NULL"], &["1", "NULL"], &["2", "NULL"]],
    );
}

#[test]
fn window_secondary_orders() {
    let mut con = connect();
    assert_rows(
        &mut con,
        "SELECT i, row_number(ORDER BY i DESC) OVER (), rank(ORDER BY 3 - i) OVER (ORDER BY i ROWS BETWEEN 1 \
         PRECEDING AND 1 FOLLOWING), first_value(i ORDER BY i DESC) OVER (ORDER BY i ROWS BETWEEN 1 PRECEDING AND \
         1 FOLLOWING), lead(i, 1, -1 ORDER BY i DESC) OVER (ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND \
         UNBOUNDED FOLLOWING) FROM range(5) t(i) ORDER BY i",
        &[
            &["0", "5", "2", "1", "-1"],
            &["1", "4", "2", "2", "0"],
            &["2", "3", "2", "3", "1"],
            &["3", "2", "2", "4", "2"],
            &["4", "1", "1", "4", "3"],
        ],
    );
}

#[test]
fn streaming_windows_cross_chunk_boundaries() {
    let mut con = connect();
    assert_rows(
        &mut con,
        "SELECT i, lead(i, 3) OVER (), lag(i, 2) OVER () FROM range(4100) t(i) LIMIT 4 OFFSET 2045",
        &[
            &["2045", "2048", "2043"],
            &["2046", "2049", "2044"],
            &["2047", "2050", "2045"],
            &["2048", "2051", "2046"],
        ],
    );
    assert_rows(
        &mut con,
        "SELECT i, row_number() OVER (), rank() OVER (), lead(i) OVER () FROM range(3) t(i)",
        &[
            &["0", "1", "1", "1"],
            &["1", "2", "1", "2"],
            &["2", "3", "1", "NULL"],
        ],
    );
}

#[test]
fn except_all_and_intersect_all_keep_duplicates() {
    let mut con = connect();
    assert_rows(
        &mut con,
        "SELECT * FROM (VALUES (1), (1), (2), (3), (3), (3), (NULL), (NULL)) t(x) EXCEPT ALL SELECT * FROM \
         (VALUES (1), (3), (NULL)) s(x) ORDER BY 1",
        &[&["1"], &["2"], &["3"], &["3"], &["NULL"]],
    );
    assert_rows(
        &mut con,
        "SELECT * FROM (VALUES (1), (1), (2), (3), (3), (3)) t(x) INTERSECT ALL SELECT * FROM (VALUES (1), (3), \
         (3), (4)) s(x) ORDER BY 1",
        &[&["1"], &["3"], &["3"]],
    );
}
