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
        "Binder Error: Parameter/argument count mismatch for prepared statement. Expected 1, got 2"
    );
    con.query("PREPARE q AS SELECT $2::INT").unwrap();
    assert_eq!(
        error(&mut con, "EXECUTE q(0)"),
        "Binder Error: Could not find parameter with identifier 2"
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
        "Binder Error: Max expression depth limit of 7 exceeded. Use \"SET max_expression_depth TO x\" to increase \
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
