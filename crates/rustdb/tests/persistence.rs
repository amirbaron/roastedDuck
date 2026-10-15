//! Database files: data written by checkpoints is there after reopening.
#![allow(clippy::unwrap_used)]

use rustdb::{Config, Connection, Database};

/// A fresh database path in the temporary directory.
fn temp_path(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rustdb-persistence-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().into_owned()
}

fn open(path: &str) -> (Database, Connection) {
    let database = Database::open(path, &Config::default()).unwrap();
    let connection = database.connect().unwrap();
    (database, connection)
}

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

#[test]
fn tables_survive_a_restart() {
    let path = temp_path("tables");
    let big = "x".repeat(10_000);
    {
        let (_database, mut con) = open(&path);
        con.query("CREATE TABLE t(i INTEGER PRIMARY KEY, s VARCHAR, d DOUBLE, b BOOLEAN)")
            .unwrap();
        con.query(&format!(
            "INSERT INTO t VALUES (1, 'a', 1.5, true), (2, NULL, NULL, false), (3, '{big}', -2.0, NULL)"
        ))
        .unwrap();
        con.query("CREATE SCHEMA s").unwrap();
        con.query("CREATE TABLE s.u(x BIGINT)").unwrap();
        con.query("INSERT INTO s.u SELECT * FROM range(5000)").unwrap();
        con.query("DELETE FROM s.u WHERE x > 10 AND x < 4990").unwrap();
        con.query("CREATE VIEW v AS SELECT i FROM t WHERE i > 1").unwrap();
        con.query("CREATE SEQUENCE seq START 10").unwrap();
        con.query("SELECT nextval('seq')").unwrap();
    }
    {
        let (_database, mut con) = open(&path);
        assert_eq!(
            rows(&mut con, "SELECT i, s IS NULL, d, b FROM t ORDER BY i"),
            vec![
                vec!["1", "false", "1.5", "true"],
                vec!["2", "true", "NULL", "false"],
                vec!["3", "false", "-2.0", "NULL"],
            ]
        );
        assert_eq!(
            rows(&mut con, "SELECT s FROM t WHERE i = 3"),
            vec![vec![big.clone()]]
        );
        let remaining: Vec<Vec<String>> = (0..=10).chain(4990..5000).map(|x| vec![x.to_string()]).collect();
        assert_eq!(rows(&mut con, "SELECT x FROM s.u ORDER BY x"), remaining);
        assert_eq!(
            rows(&mut con, "SELECT * FROM v ORDER BY i"),
            vec![vec!["2"], vec!["3"]]
        );
        assert_eq!(rows(&mut con, "SELECT nextval('seq')"), vec![vec!["11"]]);
        // The primary key is enforced on reloaded data.
        assert!(con.query("INSERT INTO t VALUES (1, 'dup', 0, false)").is_err());
        con.query("INSERT INTO t VALUES (4, 'd', 4.0, true)").unwrap();
        con.query("CHECKPOINT").unwrap();
        con.query("DROP TABLE s.u").unwrap();
    }
    {
        let (_database, mut con) = open(&path);
        assert_eq!(
            rows(&mut con, "SELECT i FROM t ORDER BY i"),
            vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"]]
        );
        assert!(con.query("SELECT * FROM s.u").is_err());
    }
}
